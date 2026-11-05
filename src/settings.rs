use crate::model::TaskStatus;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use url::Url;

pub const DEFAULT_SYNC_INTERVAL_MS: u64 = 60_000;
/// Keeps a misconfigured interval from exhausting Linear's hourly request budget.
pub const MIN_SYNC_INTERVAL_MS: u64 = 10_000;

/// The host-owned settings document, parsed and validated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    #[serde(default = "default_sync_interval_ms")]
    pub sync_interval_ms: u64,
    #[serde(default)]
    pub sources: BTreeMap<SourceName, Source>,
}

fn default_sync_interval_ms() -> u64 {
    DEFAULT_SYNC_INTERVAL_MS
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            sync_interval_ms: DEFAULT_SYNC_INTERVAL_MS,
            sources: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SourceName(pub String);

impl fmt::Display for SourceName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub query: SourceQuery,
    /// Linear workflow state name to PlaneAI status; unmapped states fall back to the state type.
    #[serde(default)]
    pub status_map: BTreeMap<String, TaskStatus>,
    #[serde(default)]
    pub writeback: Option<Writeback>,
}

/// Linear's analog of JQL: a saved custom view or a raw `IssueFilter`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceQuery {
    /// A custom view URL (`https://linear.app/<org>/view/<name>-<slugId>`), slug id, or uuid.
    View {
        view: String,
    },
    Filter {
        filter: Value,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Writeback {
    #[serde(default)]
    pub on_start: Option<String>,
    #[serde(default)]
    pub on_complete: Option<String>,
    #[serde(default)]
    pub comment: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsError(pub String);

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Settings {
    /// Parses the settings document, rejecting malformed values instead of defaulting them.
    pub fn parse(value: &Value) -> Result<Self, SettingsError> {
        let settings: Settings = serde_json::from_value(value.clone())
            .map_err(|error| SettingsError(format!("invalid Linear plugin settings: {error}")))?;
        settings.validate()?;
        Ok(settings)
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).expect("settings always serialize")
    }

    fn validate(&self) -> Result<(), SettingsError> {
        if self.sync_interval_ms < MIN_SYNC_INTERVAL_MS {
            return Err(SettingsError(format!(
                "sync_interval_ms must be at least {MIN_SYNC_INTERVAL_MS}"
            )));
        }
        for (name, source) in &self.sources {
            if name.0.trim().is_empty() || name.0 != name.0.trim() {
                return Err(SettingsError(format!(
                    "Linear source name {:?} must be nonblank without surrounding spaces",
                    name.0
                )));
            }
            source
                .query
                .lookup()
                .map_err(|error| SettingsError(format!("Linear source {:?}: {error}", name.0)))?;
            if let Some(writeback) = &source.writeback {
                for state in [&writeback.on_start, &writeback.on_complete]
                    .into_iter()
                    .flatten()
                {
                    if state.trim().is_empty() {
                        return Err(SettingsError(format!(
                            "Linear source {:?} has a blank writeback state; leave it unset instead",
                            name.0
                        )));
                    }
                }
            }
        }
        Ok(())
    }
}

/// What a source query asks Linear for, derived from the stored query.
#[derive(Debug, Clone, PartialEq)]
pub enum QueryLookup {
    View { id: String },
    Filter { filter: Value },
}

impl SourceQuery {
    pub fn lookup(&self) -> Result<QueryLookup, String> {
        match self {
            Self::View { view } => view_lookup_id(view).map(|id| QueryLookup::View { id }),
            Self::Filter { filter } => match filter {
                Value::Object(fields) if !fields.is_empty() => Ok(QueryLookup::Filter {
                    filter: filter.clone(),
                }),
                Value::Object(_) => Err("requires a non-empty IssueFilter object".to_string()),
                _ => Err("filter must be an IssueFilter JSON object".to_string()),
            },
        }
    }
}

fn view_lookup_id(view: &str) -> Result<String, String> {
    let view = view.trim();
    if view.is_empty() {
        return Err("requires a non-empty custom view URL or id".to_string());
    }
    let segment = if view.starts_with("http://") || view.starts_with("https://") {
        let url = Url::parse(view).map_err(|_| "custom view URL is not a valid URL".to_string())?;
        let segments: Vec<_> = url
            .path_segments()
            .map(Iterator::collect)
            .unwrap_or_default();
        match (url.scheme(), url.host_str(), segments.as_slice()) {
            ("https", Some("linear.app"), [_, "view", slug, ..]) if !slug.is_empty() => {
                slug.to_string()
            }
            _ => {
                return Err(
                    "custom view URL must look like https://linear.app/<workspace>/view/<view>"
                        .to_string(),
                )
            }
        }
    } else if view.contains(['/', ' ']) {
        return Err("custom view must be a Linear view URL, slug id, or uuid".to_string());
    } else {
        view.to_string()
    };
    if is_uuid(&segment) {
        return Ok(segment);
    }
    // A view URL slug is `<name>-<slugId>`; `customView(id:)` accepts the slug id.
    Ok(segment
        .rsplit_once('-')
        .map(|(_, slug_id)| slug_id.to_string())
        .filter(|slug_id| !slug_id.is_empty())
        .unwrap_or(segment))
}

fn is_uuid(value: &str) -> bool {
    let groups: Vec<_> = value.split('-').map(str::len).collect();
    groups == [8, 4, 4, 4, 12] && value.chars().all(|c| c == '-' || c.is_ascii_hexdigit())
}

/// How memberships move when settings change: renamed sources keep their issues, removed ones depart.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SettingsTransition {
    pub renamed: Vec<(SourceName, SourceName)>,
    pub removed: Vec<SourceName>,
}

impl SettingsTransition {
    pub fn between(old: &Settings, new: &Settings) -> Self {
        let mut added: Vec<_> = new
            .sources
            .iter()
            .filter(|(name, _)| !old.sources.contains_key(*name))
            .collect();
        let mut used = BTreeSet::new();
        let mut transition = Self::default();
        for (old_name, old_source) in old
            .sources
            .iter()
            .filter(|(name, _)| !new.sources.contains_key(*name))
        {
            let target = added
                .iter()
                .position(|(name, source)| !used.contains(*name) && *source == old_source);
            match target {
                Some(index) => {
                    let (new_name, _) = added.remove(index);
                    used.insert(new_name);
                    transition
                        .renamed
                        .push((old_name.clone(), new_name.clone()));
                }
                None => transition.removed.push(old_name.clone()),
            }
        }
        transition
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn view_source(view: &str) -> Value {
        json!({ "query": { "kind": "view", "view": view } })
    }

    #[test]
    fn empty_document_parses_to_defaults() {
        assert_eq!(Settings::parse(&json!({})).unwrap(), Settings::default());
        assert_eq!(
            Settings::default().to_value(),
            json!({ "sync_interval_ms": 60000, "sources": {} })
        );
    }

    #[test]
    fn malformed_settings_are_rejected_instead_of_defaulting() {
        for document in [
            json!({ "sources": [] }),
            json!({ "sources": { "mine": { "query": { "kind": "jql", "jql": "x" } } } }),
            json!({ "sources": { "mine": { "query": { "kind": "view", "view": "abc" }, "status_map": { "In QA": "reviewing" } } } }),
            json!({ "sources": { "mine": { "query": { "kind": "view", "view": "abc" }, "jql": "x" } } }),
            json!({ "sync_interval_ms": "fast" }),
            json!({ "sync_interval_ms": 50 }),
        ] {
            assert!(Settings::parse(&document).is_err(), "{document}");
        }
    }

    #[test]
    fn settings_reject_blank_queries() {
        let blank_view = Settings::parse(&json!({ "sources": { "mine": view_source("  ") } }));
        assert_eq!(
            blank_view.unwrap_err().0,
            "Linear source \"mine\": requires a non-empty custom view URL or id"
        );
        let empty_filter = Settings::parse(
            &json!({ "sources": { "mine": { "query": { "kind": "filter", "filter": {} } } } }),
        );
        assert_eq!(
            empty_filter.unwrap_err().0,
            "Linear source \"mine\": requires a non-empty IssueFilter object"
        );
        let array_filter = Settings::parse(
            &json!({ "sources": { "mine": { "query": { "kind": "filter", "filter": [] } } } }),
        );
        assert_eq!(
            array_filter.unwrap_err().0,
            "Linear source \"mine\": filter must be an IssueFilter JSON object"
        );
    }

    #[test]
    fn view_queries_resolve_to_the_slug_id_linear_looks_up() {
        let lookup = |view: &str| {
            SourceQuery::View {
                view: view.to_string(),
            }
            .lookup()
        };
        assert_eq!(
            lookup("https://linear.app/acme/view/my-bugs-0a1b2c3d4e5f").unwrap(),
            QueryLookup::View {
                id: "0a1b2c3d4e5f".to_string()
            }
        );
        assert_eq!(
            lookup("0a1b2c3d4e5f").unwrap(),
            QueryLookup::View {
                id: "0a1b2c3d4e5f".to_string()
            }
        );
        assert_eq!(
            lookup("2f1c3c3e-9a0b-4c2d-8e1f-0123456789ab").unwrap(),
            QueryLookup::View {
                id: "2f1c3c3e-9a0b-4c2d-8e1f-0123456789ab".to_string()
            }
        );
        assert!(lookup("https://example.com/acme/view/x-1").is_err());
        assert!(lookup("https://linear.app/acme/issue/ENG-1").is_err());
    }

    #[test]
    fn writeback_states_must_not_be_blank() {
        let document = json!({ "sources": { "mine": {
            "query": { "kind": "view", "view": "abc" },
            "writeback": { "on_start": " ", "comment": true }
        } } });
        assert!(Settings::parse(&document).is_err());
    }

    #[test]
    fn settings_detect_a_pure_source_rename() {
        let old = Settings::parse(&json!({ "sources": { "old": view_source("abc") } })).unwrap();
        let new = Settings::parse(&json!({ "sources": { "new": view_source("abc") } })).unwrap();
        assert_eq!(
            SettingsTransition::between(&old, &new),
            SettingsTransition {
                renamed: vec![(SourceName("old".into()), SourceName("new".into()))],
                removed: vec![],
            }
        );
    }

    #[test]
    fn a_changed_source_under_a_new_name_is_a_removal() {
        let old = Settings::parse(&json!({ "sources": { "old": view_source("abc") } })).unwrap();
        let new = Settings::parse(&json!({ "sources": { "new": view_source("def") } })).unwrap();
        assert_eq!(
            SettingsTransition::between(&old, &new),
            SettingsTransition {
                renamed: vec![],
                removed: vec![SourceName("old".into())],
            }
        );
    }
}
