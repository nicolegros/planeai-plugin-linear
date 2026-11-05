use crate::host::{Host, TaskFields};
use crate::linear_api::fetch_issues;
use crate::model::{LinearIssue, TaskKey, TaskStatus};
use crate::session::Linear;
use crate::settings::{Settings, Source, SourceName};
use crate::store::Store;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Debug, Default, PartialEq, Serialize)]
pub struct SyncTotals {
    pub created: usize,
    pub updated: usize,
    pub departed: usize,
    pub errors: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub error_messages: Vec<String>,
}

impl SyncTotals {
    fn fail(&mut self, source: &SourceName, error: impl std::fmt::Display) {
        eprintln!("linear sync source={source} failed: {error}");
        self.errors += 1;
        self.error_messages.push(format!("{source}: {error}"));
    }
}

/// An explicit mapping for the state name wins; otherwise the state type decides.
pub fn map_status(issue: &LinearIssue, status_map: &BTreeMap<String, TaskStatus>) -> TaskStatus {
    status_map
        .get(&issue.state_name)
        .copied()
        .unwrap_or_else(|| issue.state_type.task_status())
}

fn task_fields(issue: &LinearIssue, status: TaskStatus) -> TaskFields {
    TaskFields {
        title: issue.title.clone(),
        description: issue.description.clone(),
        status,
        priority: issue.priority.task_priority(),
        tags: issue.labels.clone(),
    }
}

enum Applied {
    Created,
    Updated,
    Unchanged,
}

/// Mirrors every source's issues into PlaneAI tasks; a failing source never blocks the others.
pub async fn sync_now(
    linear: &Linear,
    database: &Path,
    host: Host<'_>,
) -> Result<SyncTotals, String> {
    let settings = host
        .settings()
        .await
        .map_err(|error| format!("failed to read Linear settings: {error}"))?;
    let settings = Settings::parse(&settings).map_err(|error| error.to_string())?;
    let mut session = linear.session().await.map_err(|error| error.to_string())?;
    let store = Store::open(database).map_err(|error| error.to_string())?;
    store
        .ensure_cache_organization(&session.credentials().organization_id)
        .map_err(|error| error.to_string())?;
    // The UI may have replaced settings without the sidecar; catch up on renames and removals.
    store
        .apply_settings(&settings)
        .map_err(|error| error.to_string())?;

    let mut totals = SyncTotals::default();
    for (name, source) in &settings.sources {
        let lookup = source.query.lookup()?;
        let issues = match fetch_issues(&mut session, &lookup).await {
            Ok(issues) => issues,
            Err(error) => {
                totals.fail(name, error);
                continue;
            }
        };
        let mut seen = BTreeSet::new();
        let mut failure = None;
        for issue in &issues {
            seen.insert(issue.id.clone());
            match apply_issue(&store, host, name, source, issue).await {
                Ok(Applied::Created) => totals.created += 1,
                Ok(Applied::Updated) => totals.updated += 1,
                Ok(Applied::Unchanged) => {}
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }
        if let Some(error) = failure {
            totals.fail(name, error);
            continue;
        }
        let active = store
            .active_source_issues(name)
            .map_err(|error| error.to_string())?;
        for departed in active.iter().filter(|id| !seen.contains(*id)) {
            store
                .mark_departed(departed, name)
                .map_err(|error| error.to_string())?;
        }
    }
    // Only a complete snapshot may replace the queue: an issue moving between two
    // sources must never prompt because one of them failed to load.
    if totals.errors == 0 {
        rebuild_departure_queue(&store, host, &mut totals).await?;
    }
    Ok(totals)
}

async fn apply_issue(
    store: &Store,
    host: Host<'_>,
    source_name: &SourceName,
    source: &Source,
    issue: &LinearIssue,
) -> Result<Applied, String> {
    let status = map_status(issue, &source.status_map);
    let fields = task_fields(issue, status);
    let link = store
        .task_link(&issue.id)
        .map_err(|error| error.to_string())?;
    // A moved issue changes identifier; its task keeps the key it was created with.
    let key = link
        .clone()
        .unwrap_or_else(|| TaskKey(issue.identifier.clone()));
    let existing = host
        .task(&key)
        .await
        .map_err(|error| format!("failed to read local task {key}: {error}"))?;
    let applied = match (existing, &link) {
        (None, _) => {
            host.create_task(&key, &fields)
                .await
                .map_err(|error| format!("failed to create task {key}: {error}"))?;
            Applied::Created
        }
        (Some(task), Some(_)) if task.fields() == fields => Applied::Unchanged,
        (Some(_), Some(_)) => {
            host.update_task(&key, &fields)
                .await
                .map_err(|error| format!("failed to update task {key}: {error}"))?;
            Applied::Updated
        }
        // PlaneAI updates only tasks this plugin owns, so an unlinked task updates only
        // when an earlier sync created it and stopped before linking it.
        (Some(_), None) => {
            host.update_task(&key, &fields).await.map_err(|error| {
                format!(
                    "refusing to overwrite local task {key} without a Linear task link: {error}"
                )
            })?;
            Applied::Updated
        }
    };
    store
        .upsert_issue(issue, status)
        .and_then(|()| store.sync_membership(&issue.id, source_name))
        .and_then(|()| store.link_task(&issue.id, &key))
        .map_err(|error| error.to_string())?;
    Ok(applied)
}

async fn rebuild_departure_queue(
    store: &Store,
    host: Host<'_>,
    totals: &mut SyncTotals,
) -> Result<(), String> {
    let candidates = store
        .departure_candidates()
        .map_err(|error| error.to_string())?;
    let mut queue = Vec::new();
    for candidate in candidates {
        match host.task(&candidate.task_key).await {
            Ok(Some(task)) if task.status != TaskStatus::Done => queue.push(candidate),
            Ok(_) => {}
            Err(error) => {
                eprintln!(
                    "linear departed task lookup failed task={}: {error}",
                    candidate.task_key
                );
                totals.errors += 1;
            }
        }
    }
    totals.departed = queue.len();
    store
        .replace_departure_queue(&queue)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::issue;

    #[test]
    fn status_mapping_prefers_the_explicit_state_name_then_the_state_type() {
        let mut started = issue("1", "ENG-1");
        started.state_name = "In QA".into();
        started.state_type = crate::model::StateType::Started;
        let explicit = BTreeMap::from([("In QA".to_string(), TaskStatus::InReview)]);
        assert_eq!(map_status(&started, &explicit), TaskStatus::InReview);
        assert_eq!(
            map_status(&started, &BTreeMap::new()),
            TaskStatus::InProgress
        );
        let other_case = BTreeMap::from([("in qa".to_string(), TaskStatus::Done)]);
        assert_eq!(map_status(&started, &other_case), TaskStatus::InProgress);
    }

    #[test]
    fn sync_totals_serialize_error_messages_only_when_present() {
        assert_eq!(
            serde_json::to_value(SyncTotals::default()).unwrap(),
            serde_json::json!({ "created": 0, "updated": 0, "departed": 0, "errors": 0 })
        );
        let mut totals = SyncTotals::default();
        totals.fail(
            &SourceName("backlog".into()),
            "Linear request failed: HTTP 500",
        );
        assert_eq!(
            serde_json::to_value(totals).unwrap(),
            serde_json::json!({
                "created": 0, "updated": 0, "departed": 0, "errors": 1,
                "error_messages": ["backlog: Linear request failed: HTTP 500"]
            })
        );
    }

    mod over_rpc {
        use crate::linear_api::tests::raw_issue;
        use crate::testing::{filter, filter_source, local, FakeHost, Fixture};
        use serde_json::{json, Value};
        use wiremock::matchers::{body_string_contains, method};
        use wiremock::{Mock, ResponseTemplate};

        fn one_source() -> FakeHost {
            FakeHost::with_settings(json!({ "sources": { "eng": filter_source("ENG") } }))
        }

        fn todo(id: &str, identifier: &str) -> Value {
            raw_issue(id, identifier, ("Todo", "unstarted"), 2.0)
        }

        #[tokio::test]
        async fn sync_creates_linked_tasks_and_updates_only_changed_ones() {
            local(async {
                let fixture = Fixture::new().await;
                fixture.connect("org-1");
                fixture
                    .filter_returns(
                        filter("ENG"),
                        vec![
                            todo("uuid-1", "ENG-1"),
                            raw_issue("uuid-2", "ENG-2", ("In QA", "started"), 1.0),
                        ],
                    )
                    .await;
                let mut host = one_source();
                host.settings["sources"]["eng"]["status_map"] = json!({ "In QA": "in_review" });
                let mut plugin = fixture.start(host);

                assert_eq!(
                    plugin.call("linear.syncNow", Value::Null).await.unwrap(),
                    json!({ "created": 2, "updated": 0, "departed": 0, "errors": 0 })
                );
                assert_eq!(
                    plugin.host.tasks["ENG-2"],
                    json!({
                        "key": "ENG-2", "title": "Title ENG-2", "description": "Description ENG-2",
                        "status": "in_review", "priority": 4, "tags": ["alpha", "zeta"]
                    })
                );
                assert_eq!(plugin.host.tasks["ENG-1"]["status"], "todo");
                assert_eq!(plugin.host.tasks["ENG-1"]["priority"], 3);

                plugin.host.calls.clear();
                assert_eq!(
                    plugin.call("linear.syncNow", Value::Null).await.unwrap(),
                    json!({ "created": 0, "updated": 0, "departed": 0, "errors": 0 })
                );
                assert!(!plugin.host.methods().contains(&"host.task.update"));

                fixture.reset_linear().await;
                let mut renamed = todo("uuid-1", "ENG-1");
                renamed["title"] = json!("Renamed in Linear");
                fixture
                    .filter_returns(
                        filter("ENG"),
                        vec![
                            renamed,
                            raw_issue("uuid-2", "ENG-2", ("In QA", "started"), 1.0),
                        ],
                    )
                    .await;
                assert_eq!(
                    plugin.call("linear.syncNow", Value::Null).await.unwrap(),
                    json!({ "created": 0, "updated": 1, "departed": 0, "errors": 0 })
                );
                assert_eq!(plugin.host.tasks["ENG-1"]["title"], "Renamed in Linear");
                assert_eq!(
                    plugin
                        .call("linear.sidebar.items", Value::Null)
                        .await
                        .unwrap()["items"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|item| item["identifier"].as_str().unwrap())
                        .collect::<Vec<_>>()
                        .len(),
                    2
                );
            })
            .await;
        }

        #[tokio::test]
        async fn sync_refuses_to_overwrite_an_unlinked_local_task_and_keeps_other_sources() {
            local(async {
                let fixture = Fixture::new().await;
                fixture.connect("org-1");
                fixture.filter_returns(filter("ENG"), vec![todo("uuid-1", "ENG-1")]).await;
                fixture.filter_returns(filter("OPS"), vec![todo("uuid-9", "OPS-9")]).await;
                let mut host = FakeHost::with_settings(json!({ "sources": {
                    "eng": filter_source("ENG"), "ops": filter_source("OPS")
                } }));
                host.tasks.insert(
                    "ENG-1".into(),
                    json!({ "key": "ENG-1", "title": "Mine", "description": "", "status": "todo", "priority": 0, "tags": [] }),
                );
                let mut plugin = fixture.start(host);

                assert_eq!(
                    plugin.call("linear.syncNow", Value::Null).await.unwrap(),
                    json!({
                        "created": 1, "updated": 0, "departed": 0, "errors": 1,
                        "error_messages": ["eng: refusing to overwrite local task ENG-1 without a Linear task link: not granted"]
                    })
                );
                assert_eq!(plugin.host.tasks["ENG-1"]["title"], "Mine");
                assert_eq!(plugin.host.tasks["OPS-9"]["title"], "Title OPS-9");
            })
            .await;
        }

        #[tokio::test]
        async fn an_owned_task_without_a_link_is_relinked() {
            local(async {
                let fixture = Fixture::new().await;
                fixture.connect("org-1");
                fixture.filter_returns(filter("ENG"), vec![todo("uuid-1", "ENG-1")]).await;
                let mut host = one_source();
                host.tasks.insert(
                    "ENG-1".into(),
                    json!({ "key": "ENG-1", "title": "Half-synced", "description": "", "status": "todo", "priority": 0, "tags": [] }),
                );
                host.owned.insert("ENG-1".into());
                let mut plugin = fixture.start(host);

                assert_eq!(
                    plugin.call("linear.syncNow", Value::Null).await.unwrap(),
                    json!({ "created": 0, "updated": 1, "departed": 0, "errors": 0 })
                );
                assert_eq!(plugin.host.tasks["ENG-1"]["title"], "Title ENG-1");
            })
            .await;
        }

        #[tokio::test]
        async fn a_failing_source_is_isolated_and_holds_back_the_departure_queue() {
            local(async {
                let fixture = Fixture::new().await;
                fixture.connect("org-1");
                fixture.filter_returns(filter("ENG"), vec![todo("uuid-1", "ENG-1")]).await;
                let mut plugin = fixture.start(one_source());
                plugin.call("linear.syncNow", Value::Null).await.unwrap();

                fixture.reset_linear().await;
                Mock::given(method("POST"))
                    .and(body_string_contains("\"OPS\""))
                    .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                        "errors": [{ "message": "Invalid filter", "extensions": { "code": "INVALID_INPUT" } }]
                    })))
                    .mount(&fixture.server)
                    .await;
                fixture.filter_returns(filter("ENG"), vec![]).await;
                plugin.host.settings = json!({ "sources": { "eng": filter_source("ENG"), "ops": filter_source("OPS") } });

                assert_eq!(
                    plugin.call("linear.syncNow", Value::Null).await.unwrap(),
                    json!({
                        "created": 0, "updated": 0, "departed": 0, "errors": 1,
                        "error_messages": ["ops: Linear GraphQL error: Invalid filter"]
                    })
                );
                assert_eq!(
                    plugin.call("linear.departures.list", Value::Null).await.unwrap(),
                    json!({ "items": [] })
                );
            })
            .await;
        }

        #[tokio::test]
        async fn an_issue_leaving_every_source_queues_a_departure_unless_its_task_is_done() {
            local(async {
                let fixture = Fixture::new().await;
                fixture.connect("org-1");
                fixture
                    .filter_returns(filter("ENG"), vec![todo("uuid-1", "ENG-1"), todo("uuid-2", "ENG-2")])
                    .await;
                let mut plugin = fixture.start(one_source());
                plugin.call("linear.syncNow", Value::Null).await.unwrap();
                plugin.host.tasks.get_mut("ENG-2").unwrap()["status"] = json!("done");

                fixture.reset_linear().await;
                fixture.filter_returns(filter("ENG"), vec![]).await;
                assert_eq!(
                    plugin.call("linear.syncNow", Value::Null).await.unwrap(),
                    json!({ "created": 0, "updated": 0, "departed": 1, "errors": 0 })
                );
                assert_eq!(
                    plugin.call("linear.departures.list", Value::Null).await.unwrap(),
                    json!({ "items": [{ "issue_id": "uuid-1", "identifier": "ENG-1", "task_key": "ENG-1", "title": "Title ENG-1" }] })
                );
                assert_eq!(
                    plugin.call("linear.sidebar.items", Value::Null).await.unwrap(),
                    json!({ "items": [] })
                );
            })
            .await;
        }

        #[tokio::test]
        async fn an_issue_moving_between_sources_never_departs() {
            local(async {
                let fixture = Fixture::new().await;
                fixture.connect("org-1");
                fixture
                    .filter_returns(filter("ENG"), vec![todo("uuid-1", "ENG-1")])
                    .await;
                fixture.filter_returns(filter("OPS"), vec![]).await;
                let mut plugin = fixture.start(FakeHost::with_settings(json!({ "sources": {
                    "eng": filter_source("ENG"), "ops": filter_source("OPS")
                } })));
                plugin.call("linear.syncNow", Value::Null).await.unwrap();

                fixture.reset_linear().await;
                fixture.filter_returns(filter("ENG"), vec![]).await;
                fixture
                    .filter_returns(filter("OPS"), vec![todo("uuid-1", "ENG-1")])
                    .await;
                assert_eq!(
                    plugin.call("linear.syncNow", Value::Null).await.unwrap(),
                    json!({ "created": 0, "updated": 0, "departed": 0, "errors": 0 })
                );
            })
            .await;
        }

        #[tokio::test]
        async fn a_moved_issue_keeps_its_original_task_key() {
            local(async {
                let fixture = Fixture::new().await;
                fixture.connect("org-1");
                fixture.filter_returns(filter("ENG"), vec![todo("uuid-1", "ENG-1")]).await;
                let mut plugin = fixture.start(one_source());
                plugin.call("linear.syncNow", Value::Null).await.unwrap();

                fixture.reset_linear().await;
                let mut moved = todo("uuid-1", "OPS-40");
                moved["title"] = json!("Now in Ops");
                fixture.filter_returns(filter("ENG"), vec![moved]).await;
                assert_eq!(
                    plugin.call("linear.syncNow", Value::Null).await.unwrap(),
                    json!({ "created": 0, "updated": 1, "departed": 0, "errors": 0 })
                );
                assert_eq!(plugin.host.tasks.keys().collect::<Vec<_>>(), vec!["ENG-1"]);
                assert_eq!(plugin.host.tasks["ENG-1"]["title"], "Now in Ops");
                assert_eq!(
                    plugin.call("linear.sidebar.items", Value::Null).await.unwrap(),
                    json!({ "items": [{ "issue_id": "uuid-1", "identifier": "OPS-40", "title": "Now in Ops", "status": "todo" }] })
                );
            })
            .await;
        }

        #[tokio::test]
        async fn connecting_another_workspace_wipes_the_cache() {
            local(async {
                let fixture = Fixture::new().await;
                fixture.connect("org-1");
                fixture
                    .filter_returns(filter("ENG"), vec![todo("uuid-1", "ENG-1")])
                    .await;
                let mut plugin = fixture.start(one_source());
                plugin.call("linear.syncNow", Value::Null).await.unwrap();

                fixture.connect("org-2");
                assert_eq!(
                    plugin
                        .call("linear.sidebar.items", Value::Null)
                        .await
                        .unwrap(),
                    json!({ "items": [] })
                );
                fixture.reset_linear().await;
                fixture.filter_returns(filter("ENG"), vec![]).await;
                plugin.call("linear.syncNow", Value::Null).await.unwrap();
                assert!(fixture
                    .store()
                    .task_link(&crate::model::IssueId("uuid-1".into()))
                    .unwrap()
                    .is_none());
            })
            .await;
        }

        #[tokio::test]
        async fn a_custom_view_source_queries_the_view_slug_id() {
            local(async {
                let fixture = Fixture::new().await;
                fixture.connect("org-1");
                fixture.view_returns("0a1b2c3d4e5f", vec![todo("uuid-1", "ENG-1")]).await;
                let mut plugin = fixture.start(FakeHost::with_settings(json!({ "sources": { "mine": {
                    "query": { "kind": "view", "view": "https://linear.app/acme/view/my-issues-0a1b2c3d4e5f" }
                } } })));
                assert_eq!(
                    plugin.call("linear.syncNow", Value::Null).await.unwrap(),
                    json!({ "created": 1, "updated": 0, "departed": 0, "errors": 0 })
                );
            })
            .await;
        }

        #[tokio::test]
        async fn more_than_5000_issues_is_an_explicit_source_error() {
            local(async {
                let fixture = Fixture::new().await;
                fixture.connect("org-1");
                Mock::given(method("POST"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                        "data": { "issues": { "nodes": [], "pageInfo": { "hasNextPage": true, "endCursor": "next" } } }
                    })))
                    .expect(100)
                    .mount(&fixture.server)
                    .await;
                let mut plugin = fixture.start(one_source());
                assert_eq!(
                    plugin.call("linear.syncNow", Value::Null).await.unwrap(),
                    json!({
                        "created": 0, "updated": 0, "departed": 0, "errors": 1,
                        "error_messages": ["eng: Linear query exceeded the 5,000-issue synchronization limit"]
                    })
                );
            })
            .await;
        }

        #[tokio::test]
        async fn a_rename_written_by_the_ui_moves_memberships_at_the_next_sync() {
            local(async {
                let fixture = Fixture::new().await;
                fixture.connect("org-1");
                fixture
                    .filter_returns(filter("ENG"), vec![todo("uuid-1", "ENG-1")])
                    .await;
                let mut plugin = fixture.start(one_source());
                plugin.call("linear.syncNow", Value::Null).await.unwrap();

                plugin.host.settings =
                    json!({ "sources": { "engineering": filter_source("ENG") } });
                fixture.reset_linear().await;
                fixture.filter_returns(filter("ENG"), vec![]).await;
                assert_eq!(
                    plugin.call("linear.syncNow", Value::Null).await.unwrap()["departed"],
                    1,
                    "the renamed source still owns the issue, so leaving it prompts"
                );
                let store = fixture.store();
                let id = crate::model::IssueId("uuid-1".into());
                assert_eq!(
                    store
                        .active_source_issues(&crate::settings::SourceName("eng".into()))
                        .unwrap(),
                    vec![]
                );
                assert_eq!(store.departure_candidates().unwrap()[0].issue_id, id);
            })
            .await;
        }

        #[tokio::test]
        async fn sync_without_a_connection_fails() {
            local(async {
                let fixture = Fixture::new().await;
                let mut plugin = fixture.start(one_source());
                assert_eq!(
                    plugin.call("linear.syncNow", Value::Null).await,
                    Err("Linear is not connected".to_string())
                );
            })
            .await;
        }
    }
}
