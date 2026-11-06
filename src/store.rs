use crate::model::{IssueId, LinearIssue, TaskKey, TaskStatus};
use crate::settings::{Settings, SettingsTransition, SourceName};
use chrono::Utc;
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ValueRef};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
#[error("Linear plugin database: {0}")]
pub struct StoreError(#[from] rusqlite::Error);

pub type StoreResult<T> = Result<T, StoreError>;

struct StoredStatus(TaskStatus);

impl FromSql for StoredStatus {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        TaskStatus::parse(value.as_str()?)
            .map(StoredStatus)
            .ok_or(FromSqlError::InvalidType)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SidebarItem {
    pub issue_id: IssueId,
    pub identifier: String,
    pub title: String,
    pub status: TaskStatus,
}

/// A synced issue with the PlaneAI task it is linked to.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LinkedIssue {
    pub issue_id: IssueId,
    pub identifier: String,
    pub task_key: TaskKey,
    pub title: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DepartedIssue {
    pub issue_id: IssueId,
    pub identifier: String,
    pub task_key: TaskKey,
    pub title: String,
}

const MIGRATIONS: &[(i64, &str)] = &[(
    1,
    "CREATE TABLE linear_issues (
        issue_id TEXT PRIMARY KEY,
        identifier TEXT NOT NULL,
        title TEXT NOT NULL,
        description TEXT NOT NULL DEFAULT '',
        url TEXT NOT NULL DEFAULT '',
        state_name TEXT NOT NULL,
        state_type TEXT NOT NULL,
        mapped_status TEXT NOT NULL,
        priority INTEGER NOT NULL DEFAULT 0,
        labels TEXT NOT NULL DEFAULT '[]',
        team_id TEXT NOT NULL,
        sync_status TEXT NOT NULL DEFAULT 'synced',
        last_synced_at TEXT NOT NULL
    );
    CREATE TABLE linear_issue_sources (
        issue_id TEXT NOT NULL REFERENCES linear_issues(issue_id),
        source_name TEXT NOT NULL,
        sync_status TEXT NOT NULL DEFAULT 'synced',
        departure_prompt_eligible INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (issue_id, source_name)
    );
    CREATE INDEX linear_issue_sources_source_active ON linear_issue_sources(source_name, sync_status);
    CREATE TABLE linear_task_links (
        issue_id TEXT PRIMARY KEY,
        task_key TEXT NOT NULL UNIQUE
    );
    CREATE TABLE linear_departure_queue (
        issue_id TEXT PRIMARY KEY,
        task_key TEXT NOT NULL,
        identifier TEXT NOT NULL,
        title TEXT NOT NULL,
        queued_at TEXT NOT NULL
    );
    CREATE TABLE linear_cache_metadata (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
    CREATE TABLE linear_settings_snapshot (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        settings TEXT NOT NULL
    );",
)];

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> StoreResult<Self> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Self::migrated(conn)
    }

    #[cfg(test)]
    pub fn in_memory() -> Self {
        Self::migrated(Connection::open_in_memory().unwrap()).unwrap()
    }

    fn migrated(conn: Connection) -> StoreResult<Self> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS linear_plugin_schema_migrations (version INTEGER PRIMARY KEY);",
        )?;
        let current: i64 = conn.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM linear_plugin_schema_migrations",
            [],
            |row| row.get(0),
        )?;
        for (version, sql) in MIGRATIONS.iter().filter(|(v, _)| *v > current) {
            let transaction = conn.unchecked_transaction()?;
            transaction.execute_batch(sql)?;
            transaction.execute(
                "INSERT INTO linear_plugin_schema_migrations (version) VALUES (?1)",
                [version],
            )?;
            transaction.commit()?;
        }
        Ok(Self { conn })
    }

    #[cfg(test)]
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    pub fn cache_matches_organization(&self, organization_id: &str) -> StoreResult<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT value FROM linear_cache_metadata WHERE key = 'organization_id'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .as_deref()
            == Some(organization_id))
    }

    /// Wipes everything synced from another workspace before syncing this one.
    pub fn ensure_cache_organization(&self, organization_id: &str) -> StoreResult<()> {
        if self.cache_matches_organization(organization_id)? {
            return Ok(());
        }
        let transaction = self.conn.unchecked_transaction()?;
        transaction.execute_batch(
            "DELETE FROM linear_task_links;
             DELETE FROM linear_departure_queue;
             DELETE FROM linear_issue_sources;
             DELETE FROM linear_issues;",
        )?;
        transaction.execute(
            "INSERT INTO linear_cache_metadata (key, value) VALUES ('organization_id', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![organization_id],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn upsert_issue(&self, issue: &LinearIssue, mapped: TaskStatus) -> StoreResult<()> {
        let labels = serde_json::to_string(&issue.labels).expect("labels serialize");
        self.conn.execute(
            "INSERT INTO linear_issues (issue_id, identifier, title, description, url, state_name, state_type, mapped_status, priority, labels, team_id, sync_status, last_synced_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'synced', ?12)
             ON CONFLICT(issue_id) DO UPDATE SET identifier = excluded.identifier, title = excluded.title,
               description = excluded.description, url = excluded.url, state_name = excluded.state_name,
               state_type = excluded.state_type, mapped_status = excluded.mapped_status, priority = excluded.priority,
               labels = excluded.labels, team_id = excluded.team_id, sync_status = 'synced', last_synced_at = excluded.last_synced_at",
            params![
                issue.id.0,
                issue.identifier,
                issue.title,
                issue.description,
                issue.url,
                issue.state_name,
                issue.state_type.as_str(),
                mapped.as_str(),
                issue.priority.task_priority(),
                labels,
                issue.team_id,
                Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn task_link(&self, issue_id: &IssueId) -> StoreResult<Option<TaskKey>> {
        Ok(self
            .conn
            .query_row(
                "SELECT task_key FROM linear_task_links WHERE issue_id = ?1",
                params![issue_id.0],
                |row| row.get(0).map(TaskKey),
            )
            .optional()?)
    }

    pub fn link_task(&self, issue_id: &IssueId, task_key: &TaskKey) -> StoreResult<()> {
        self.conn.execute(
            "INSERT INTO linear_task_links (issue_id, task_key) VALUES (?1, ?2)
             ON CONFLICT(issue_id) DO UPDATE SET task_key = excluded.task_key",
            params![issue_id.0, task_key.0],
        )?;
        Ok(())
    }

    pub fn unlink_task(&self, issue_id: &IssueId) -> StoreResult<()> {
        self.conn.execute(
            "DELETE FROM linear_task_links WHERE issue_id = ?1",
            params![issue_id.0],
        )?;
        Ok(())
    }

    pub fn issue_for_task(&self, task_key: &str) -> StoreResult<Option<IssueId>> {
        Ok(self
            .conn
            .query_row(
                "SELECT issue_id FROM linear_task_links WHERE task_key = ?1",
                params![task_key],
                |row| row.get(0).map(IssueId),
            )
            .optional()?)
    }

    pub fn sync_membership(&self, issue_id: &IssueId, source: &SourceName) -> StoreResult<()> {
        self.conn.execute(
            "INSERT INTO linear_issue_sources (issue_id, source_name, sync_status, departure_prompt_eligible)
             VALUES (?1, ?2, 'synced', 0)
             ON CONFLICT(issue_id, source_name) DO UPDATE SET sync_status = 'synced', departure_prompt_eligible = 0",
            params![issue_id.0, source.0],
        )?;
        self.refresh_issue_status(issue_id)
    }

    pub fn active_source_issues(&self, source: &SourceName) -> StoreResult<Vec<IssueId>> {
        let mut statement = self.conn.prepare(
            "SELECT issue_id FROM linear_issue_sources WHERE source_name = ?1 AND sync_status = 'synced' ORDER BY issue_id",
        )?;
        let rows = statement.query_map(params![source.0], |row| row.get(0).map(IssueId))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The issue left a source's results: it may prompt once it has left every source.
    pub fn mark_departed(&self, issue_id: &IssueId, source: &SourceName) -> StoreResult<()> {
        self.conn.execute(
            "UPDATE linear_issue_sources SET sync_status = 'departed', departure_prompt_eligible = 1
             WHERE issue_id = ?1 AND source_name = ?2",
            params![issue_id.0, source.0],
        )?;
        self.refresh_issue_status(issue_id)
    }

    /// A removed source departs its issues without prompting: the user removed it, not Linear.
    fn depart_source(&self, source: &SourceName) -> StoreResult<()> {
        let issues = self.active_source_issues(source)?;
        self.conn.execute(
            "UPDATE linear_issue_sources SET sync_status = 'departed', departure_prompt_eligible = 0
             WHERE source_name = ?1 AND sync_status = 'synced'",
            params![source.0],
        )?;
        for issue in issues {
            self.refresh_issue_status(&issue)?;
        }
        Ok(())
    }

    fn rename_source(&self, from: &SourceName, to: &SourceName) -> StoreResult<()> {
        self.conn.execute(
            "INSERT INTO linear_issue_sources (issue_id, source_name, sync_status, departure_prompt_eligible)
             SELECT issue_id, ?1, sync_status, departure_prompt_eligible FROM linear_issue_sources WHERE source_name = ?2
             ON CONFLICT(issue_id, source_name) DO UPDATE SET sync_status =
                 CASE WHEN linear_issue_sources.sync_status = 'synced' OR excluded.sync_status = 'synced'
                 THEN 'synced' ELSE 'departed' END",
            params![to.0, from.0],
        )?;
        self.conn.execute(
            "DELETE FROM linear_issue_sources WHERE source_name = ?1",
            params![from.0],
        )?;
        Ok(())
    }

    fn refresh_issue_status(&self, issue_id: &IssueId) -> StoreResult<()> {
        self.conn.execute(
            "UPDATE linear_issues SET sync_status = CASE WHEN EXISTS(
                SELECT 1 FROM linear_issue_sources WHERE issue_id = ?1 AND sync_status = 'synced'
             ) THEN 'synced' ELSE 'departed' END WHERE issue_id = ?1",
            params![issue_id.0],
        )?;
        Ok(())
    }

    pub fn settings_snapshot(&self) -> StoreResult<Option<Settings>> {
        let stored: Option<String> = self
            .conn
            .query_row(
                "SELECT settings FROM linear_settings_snapshot WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(stored
            .and_then(|text| serde_json::from_str(&text).ok())
            .and_then(|value| Settings::parse(&value).ok()))
    }

    /// Moves memberships from the last applied settings to `settings`, atomically with its snapshot.
    pub fn apply_settings(&self, settings: &Settings) -> StoreResult<SettingsTransition> {
        let previous = self.settings_snapshot()?.unwrap_or_default();
        let transition = SettingsTransition::between(&previous, settings);
        let transaction = self.conn.unchecked_transaction()?;
        for (from, to) in &transition.renamed {
            self.rename_source(from, to)?;
        }
        for source in &transition.removed {
            self.depart_source(source)?;
        }
        transaction.execute(
            "INSERT INTO linear_settings_snapshot (id, settings) VALUES (1, ?1)
             ON CONFLICT(id) DO UPDATE SET settings = excluded.settings",
            params![settings.to_value().to_string()],
        )?;
        transaction.commit()?;
        Ok(transition)
    }

    /// Issues gone from every source after at least one source dropped them.
    pub fn departure_candidates(&self) -> StoreResult<Vec<DepartedIssue>> {
        let mut statement = self.conn.prepare(
            "SELECT issue.issue_id, issue.identifier, link.task_key, issue.title
             FROM linear_issues issue JOIN linear_task_links link ON link.issue_id = issue.issue_id
             WHERE issue.sync_status = 'departed' AND EXISTS (
                SELECT 1 FROM linear_issue_sources source
                WHERE source.issue_id = issue.issue_id AND source.departure_prompt_eligible = 1
             ) ORDER BY issue.identifier",
        )?;
        let rows = statement.query_map([], departed_issue)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn replace_departure_queue(&self, issues: &[DepartedIssue]) -> StoreResult<()> {
        let transaction = self.conn.unchecked_transaction()?;
        transaction.execute("DELETE FROM linear_departure_queue", [])?;
        let now = Utc::now().to_rfc3339();
        for issue in issues {
            transaction.execute(
                "INSERT INTO linear_departure_queue (issue_id, task_key, identifier, title, queued_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![issue.issue_id.0, issue.task_key.0, issue.identifier, issue.title, now],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn departure_queue(&self) -> StoreResult<Vec<DepartedIssue>> {
        let mut statement = self.conn.prepare(
            "SELECT issue_id, identifier, task_key, title FROM linear_departure_queue ORDER BY queued_at, identifier",
        )?;
        let rows = statement.query_map([], departed_issue)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn queued_departure(&self, issue_id: &IssueId) -> StoreResult<Option<DepartedIssue>> {
        Ok(self
            .conn
            .query_row(
                "SELECT issue_id, identifier, task_key, title FROM linear_departure_queue WHERE issue_id = ?1",
                params![issue_id.0],
                departed_issue,
            )
            .optional()?)
    }

    pub fn dequeue_departure(&self, issue_id: &IssueId) -> StoreResult<()> {
        self.conn.execute(
            "DELETE FROM linear_departure_queue WHERE issue_id = ?1",
            params![issue_id.0],
        )?;
        Ok(())
    }

    pub fn sidebar_items(&self) -> StoreResult<Vec<SidebarItem>> {
        let mut statement = self.conn.prepare(
            "SELECT issue_id, identifier, title, mapped_status FROM linear_issues
             WHERE sync_status = 'synced' ORDER BY last_synced_at DESC, identifier",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(SidebarItem {
                issue_id: IssueId(row.get(0)?),
                identifier: row.get(1)?,
                title: row.get(2)?,
                status: row.get::<_, StoredStatus>(3)?.0,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn synced_issue(&self, issue_id: &IssueId) -> StoreResult<Option<LinkedIssue>> {
        Ok(self
            .conn
            .query_row(
                "SELECT issue.issue_id, issue.identifier, link.task_key, issue.title, issue.description
                 FROM linear_issues issue JOIN linear_task_links link ON link.issue_id = issue.issue_id
                 WHERE issue.issue_id = ?1 AND issue.sync_status = 'synced'",
                params![issue_id.0],
                |row| {
                    Ok(LinkedIssue {
                        issue_id: IssueId(row.get(0)?),
                        identifier: row.get(1)?,
                        task_key: TaskKey(row.get(2)?),
                        title: row.get(3)?,
                        description: row.get(4)?,
                    })
                },
            )
            .optional()?)
    }

    /// The first source by name that still returns the issue; it decides writeback.
    pub fn first_synced_source(&self, issue_id: &IssueId) -> StoreResult<Option<SourceName>> {
        Ok(self
            .conn
            .query_row(
                "SELECT source_name FROM linear_issue_sources WHERE issue_id = ?1 AND sync_status = 'synced'
                 ORDER BY source_name LIMIT 1",
                params![issue_id.0],
                |row| row.get(0).map(SourceName),
            )
            .optional()?)
    }

    pub fn issue_identifier(&self, issue_id: &IssueId) -> StoreResult<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT identifier FROM linear_issues WHERE issue_id = ?1",
                params![issue_id.0],
                |row| row.get(0),
            )
            .optional()?)
    }
}

fn departed_issue(row: &rusqlite::Row<'_>) -> rusqlite::Result<DepartedIssue> {
    Ok(DepartedIssue {
        issue_id: IssueId(row.get(0)?),
        identifier: row.get(1)?,
        task_key: TaskKey(row.get(2)?),
        title: row.get(3)?,
    })
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::model::{Priority, StateType};
    use serde_json::json;

    pub fn issue(id: &str, identifier: &str) -> LinearIssue {
        LinearIssue {
            id: IssueId(id.to_string()),
            identifier: identifier.to_string(),
            title: format!("Title {identifier}"),
            description: "Description".to_string(),
            url: format!("https://linear.app/acme/issue/{identifier}"),
            state_name: "Todo".to_string(),
            state_type: StateType::Unstarted,
            priority: Priority::High,
            labels: vec!["sync".to_string()],
            team_id: "team-1".to_string(),
        }
    }

    fn source(name: &str) -> SourceName {
        SourceName(name.to_string())
    }

    fn synced(store: &Store, id: &str, sources: &[&str]) -> IssueId {
        let issue = issue(id, &format!("ENG-{id}"));
        store.upsert_issue(&issue, TaskStatus::Todo).unwrap();
        store
            .link_task(&issue.id, &TaskKey(issue.identifier.clone()))
            .unwrap();
        for name in sources {
            store.sync_membership(&issue.id, &source(name)).unwrap();
        }
        issue.id
    }

    fn issue_status(store: &Store, id: &str) -> String {
        store
            .connection()
            .query_row(
                "SELECT sync_status FROM linear_issues WHERE issue_id = ?1",
                [id],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn eligible(store: &Store, id: &str, name: &str) -> i64 {
        store.connection().query_row(
            "SELECT departure_prompt_eligible FROM linear_issue_sources WHERE issue_id = ?1 AND source_name = ?2",
            [id, name], |row| row.get(0),
        ).unwrap()
    }

    fn settings(sources: &[(&str, &str)]) -> Settings {
        let sources: serde_json::Map<_, _> = sources
            .iter()
            .map(|(name, view)| {
                (
                    name.to_string(),
                    json!({ "query": { "kind": "view", "view": view } }),
                )
            })
            .collect();
        Settings::parse(&json!({ "sources": sources })).unwrap()
    }

    #[test]
    fn migrations_are_versioned_and_idempotent() {
        let store = Store::in_memory();
        let again = Store::migrated(
            Connection::open_in_memory()
                .and_then(|conn| {
                    conn.execute_batch(
                    "CREATE TABLE linear_plugin_schema_migrations (version INTEGER PRIMARY KEY);",
                )?;
                    Ok(conn)
                })
                .unwrap(),
        )
        .unwrap();
        let version = |store: &Store| -> i64 {
            store
                .connection()
                .query_row(
                    "SELECT MAX(version) FROM linear_plugin_schema_migrations",
                    [],
                    |row| row.get(0),
                )
                .unwrap()
        };
        assert_eq!(version(&store), 1);
        assert_eq!(version(&Store::migrated(store.conn).unwrap()), 1);
        assert_eq!(version(&again), 1);
    }

    #[test]
    fn source_records_are_isolated_and_removed_sources_depart_only_their_records() {
        let store = Store::in_memory();
        synced(&store, "1", &["one"]);
        synced(&store, "2", &["two"]);
        store
            .apply_settings(&settings(&[("one", "a"), ("two", "b")]))
            .unwrap();

        store.apply_settings(&settings(&[("two", "b")])).unwrap();

        assert_eq!(store.active_source_issues(&source("one")).unwrap(), vec![]);
        assert_eq!(
            store.active_source_issues(&source("two")).unwrap(),
            vec![IssueId("2".into())]
        );
        assert_eq!(issue_status(&store, "1"), "departed");
        assert_eq!(issue_status(&store, "2"), "synced");
    }

    #[test]
    fn departing_one_overlapping_source_keeps_the_issue_synced() {
        let store = Store::in_memory();
        let id = synced(&store, "1", &["one", "two"]);
        store.mark_departed(&id, &source("one")).unwrap();
        assert_eq!(issue_status(&store, "1"), "synced");
        store.mark_departed(&id, &source("two")).unwrap();
        assert_eq!(issue_status(&store, "1"), "departed");
    }

    #[test]
    fn source_removal_never_marks_a_departure_prompt_eligible() {
        let store = Store::in_memory();
        let id = synced(&store, "1", &["removed"]);
        store
            .apply_settings(&settings(&[("removed", "a")]))
            .unwrap();
        store.apply_settings(&settings(&[])).unwrap();
        assert_eq!(eligible(&store, "1", "removed"), 0);
        assert_eq!(store.departure_candidates().unwrap(), vec![]);

        store.sync_membership(&id, &source("removed")).unwrap();
        store.mark_departed(&id, &source("removed")).unwrap();
        assert_eq!(eligible(&store, "1", "removed"), 1);
        assert_eq!(
            store.departure_candidates().unwrap(),
            vec![DepartedIssue {
                issue_id: id,
                identifier: "ENG-1".into(),
                task_key: TaskKey("ENG-1".into()),
                title: "Title ENG-1".into(),
            }]
        );
    }

    #[test]
    fn source_rename_moves_memberships_without_departing_issues() {
        let store = Store::in_memory();
        synced(&store, "1", &["old"]);
        store.apply_settings(&settings(&[("old", "a")])).unwrap();

        let transition = store.apply_settings(&settings(&[("new", "a")])).unwrap();

        assert_eq!(transition.renamed, vec![(source("old"), source("new"))]);
        assert_eq!(store.active_source_issues(&source("old")).unwrap(), vec![]);
        assert_eq!(
            store.active_source_issues(&source("new")).unwrap(),
            vec![IssueId("1".into())]
        );
        assert_eq!(issue_status(&store, "1"), "synced");
        assert_eq!(
            store.settings_snapshot().unwrap(),
            Some(settings(&[("new", "a")]))
        );
    }

    #[test]
    fn cache_is_retained_for_the_same_organization_and_cleared_for_a_new_one() {
        let store = Store::in_memory();
        store.ensure_cache_organization("org-a").unwrap();
        let id = synced(&store, "1", &["source"]);

        store.ensure_cache_organization("org-a").unwrap();
        assert_eq!(store.task_link(&id).unwrap(), Some(TaskKey("ENG-1".into())));

        store.ensure_cache_organization("org-b").unwrap();
        assert_eq!(store.task_link(&id).unwrap(), None);
        assert_eq!(store.sidebar_items().unwrap(), vec![]);
        assert!(store.cache_matches_organization("org-b").unwrap());
    }

    #[test]
    fn departure_queue_lists_and_dismisses_unresolved_items() {
        let store = Store::in_memory();
        let departed = DepartedIssue {
            issue_id: IssueId("1".into()),
            identifier: "ENG-1".into(),
            task_key: TaskKey("ENG-1".into()),
            title: "Gone".into(),
        };
        store
            .replace_departure_queue(std::slice::from_ref(&departed))
            .unwrap();
        assert_eq!(store.departure_queue().unwrap(), vec![departed.clone()]);
        store.dequeue_departure(&departed.issue_id).unwrap();
        assert_eq!(store.departure_queue().unwrap(), vec![]);
    }

    #[test]
    fn synced_issue_lookup_skips_departed_issues() {
        let store = Store::in_memory();
        let id = synced(&store, "1", &["one"]);
        assert_eq!(
            store.synced_issue(&id).unwrap(),
            Some(LinkedIssue {
                issue_id: id.clone(),
                identifier: "ENG-1".into(),
                task_key: TaskKey("ENG-1".into()),
                title: "Title ENG-1".into(),
                description: "Description".into(),
            })
        );
        store.mark_departed(&id, &source("one")).unwrap();
        assert_eq!(store.synced_issue(&id).unwrap(), None);
        assert_eq!(
            store.synced_issue(&IssueId("missing".into())).unwrap(),
            None
        );
    }
}
