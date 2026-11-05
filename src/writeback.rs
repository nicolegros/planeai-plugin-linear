use crate::host::Host;
use crate::linear_api::{create_comment, set_issue_state, team_states};
use crate::model::IssueId;
use crate::session::{Linear, Session};
use crate::settings::{Settings, SourceName, Writeback};
use crate::store::Store;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// A `plugin.taskLifecycle` batch; only the events writeback acts on are distinguished.
#[derive(Debug, Deserialize)]
pub struct LifecycleBatch {
    pub events: Vec<LifecycleEvent>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LifecycleEvent {
    ChildAssigned {
        parent_key: String,
        is_first_child_assignment: bool,
    },
    StatusChanged {
        task_key: String,
        previous_status: String,
        new_status: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WritebackAction {
    Start,
    Complete,
}

impl LifecycleEvent {
    /// The task a writeback concerns: a parent's first child starts it, reaching done completes it.
    fn writeback(&self) -> Option<(&str, WritebackAction)> {
        match self {
            Self::ChildAssigned {
                parent_key,
                is_first_child_assignment: true,
            } => Some((parent_key, WritebackAction::Start)),
            Self::StatusChanged {
                task_key,
                previous_status,
                new_status,
            } if previous_status != "done" && new_status == "done" => {
                Some((task_key, WritebackAction::Complete))
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WritebackOutcome {
    pub at: DateTime<Utc>,
    pub issue_id: IssueId,
    pub identifier: String,
    pub source: SourceName,
    pub action: WritebackAction,
    pub ok: bool,
    pub transition_ok: bool,
    pub comment_ok: bool,
    pub error: Option<String>,
}

pub async fn task_lifecycle(
    linear: &Linear,
    database: &Path,
    host: Host<'_>,
    batch: &LifecycleBatch,
) -> Result<Vec<WritebackOutcome>, String> {
    let candidates: Vec<_> = batch
        .events
        .iter()
        .filter_map(LifecycleEvent::writeback)
        .collect();
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let store = Store::open(database).map_err(|error| error.to_string())?;
    let settings = host
        .settings()
        .await
        .map_err(|error| format!("failed to read Linear settings: {error}"))?;
    let settings = Settings::parse(&settings).map_err(|error| error.to_string())?;
    let mut outcomes = Vec::new();
    for (task_key, action) in candidates {
        let Some(issue_id) = store
            .issue_for_task(task_key)
            .map_err(|error| error.to_string())?
        else {
            continue;
        };
        let Some(source_name) = store
            .first_synced_source(&issue_id)
            .map_err(|error| error.to_string())?
        else {
            continue;
        };
        let Some(config) = settings
            .sources
            .get(&source_name)
            .and_then(|source| source.writeback.as_ref())
        else {
            continue;
        };
        let target = match action {
            WritebackAction::Start => config.on_start.as_deref(),
            WritebackAction::Complete => config.on_complete.as_deref(),
        };
        if target.is_none() && !config.comment {
            continue;
        }
        let identifier = store
            .issue_identifier(&issue_id)
            .map_err(|error| error.to_string())?
            .unwrap_or_default();
        let outcome = write_back(linear, &issue_id, target, config, action).await;
        if !outcome.errors.is_empty() {
            eprintln!(
                "linear lifecycle writeback failed issue={identifier} source={source_name} action={action:?}: {}",
                outcome.errors.join("; ")
            );
        }
        outcomes.push(WritebackOutcome {
            at: Utc::now(),
            issue_id,
            identifier,
            source: source_name,
            action,
            ok: outcome.errors.is_empty(),
            transition_ok: outcome.transition_ok,
            comment_ok: outcome.comment_ok,
            error: (!outcome.errors.is_empty()).then(|| outcome.errors.join("; ")),
        });
    }
    Ok(outcomes)
}

struct Parts {
    transition_ok: bool,
    comment_ok: bool,
    errors: Vec<String>,
}

/// Moves the issue and comments on it; each part fails on its own.
async fn write_back(
    linear: &Linear,
    issue_id: &IssueId,
    target: Option<&str>,
    config: &Writeback,
    action: WritebackAction,
) -> Parts {
    let mut parts = Parts {
        transition_ok: target.is_none(),
        comment_ok: !config.comment,
        errors: Vec::new(),
    };
    let mut session = match linear.session().await {
        Ok(session) => session,
        Err(error) => {
            parts.errors.push(error.to_string());
            return parts;
        }
    };
    if let Some(target) = target {
        match transition(&mut session, issue_id, target).await {
            Ok(()) => parts.transition_ok = true,
            Err(error) => parts.errors.push(error),
        }
    }
    if config.comment {
        let body = format!(
            "planeai: Task moved to {} at {}",
            match action {
                WritebackAction::Start => "Start",
                WritebackAction::Complete => "Complete",
            },
            Utc::now().format("%Y-%m-%dT%H:%M:%SZ")
        );
        match create_comment(&mut session, issue_id, &body).await {
            Ok(()) => parts.comment_ok = true,
            Err(error) => parts.errors.push(format!("Linear comment failed: {error}")),
        }
    }
    parts
}

async fn transition(
    session: &mut Session<'_>,
    issue_id: &IssueId,
    target: &str,
) -> Result<(), String> {
    let states = team_states(session, issue_id)
        .await
        .map_err(|error| format!("Linear workflow state lookup failed: {error}"))?;
    let state = states
        .iter()
        .find(|state| state.name.eq_ignore_ascii_case(target))
        .ok_or_else(|| format!("Linear workflow state {target:?} is not in the issue's team"))?;
    set_issue_state(session, issue_id, &state.id)
        .await
        .map_err(|error| format!("Linear transition failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn writebacks(events: serde_json::Value) -> Vec<(String, WritebackAction)> {
        let batch: LifecycleBatch =
            serde_json::from_value(json!({ "batch_id": "b", "events": events })).unwrap();
        batch
            .events
            .iter()
            .filter_map(LifecycleEvent::writeback)
            .map(|(key, action)| (key.to_string(), action))
            .collect()
    }

    #[test]
    fn only_first_children_and_transitions_into_done_write_back() {
        assert_eq!(
            writebacks(json!([
                { "type": "child_assigned", "child_key": "PLA-2", "parent_key": "ENG-1", "is_first_child_assignment": true },
                { "type": "child_assigned", "child_key": "PLA-3", "parent_key": "ENG-1", "is_first_child_assignment": false },
                { "type": "status_changed", "task_key": "ENG-1", "parent_key": null, "previous_status": "in_progress", "new_status": "done", "cause": "automatic_parent_completion" },
                { "type": "status_changed", "task_key": "ENG-1", "parent_key": null, "previous_status": "done", "new_status": "done", "cause": "direct" },
                { "type": "status_changed", "task_key": "ENG-1", "parent_key": null, "previous_status": "todo", "new_status": "in_review", "cause": "direct" },
                { "type": "something_new", "task_key": "ENG-1" }
            ])),
            vec![
                ("ENG-1".to_string(), WritebackAction::Start),
                ("ENG-1".to_string(), WritebackAction::Complete),
            ]
        );
    }
}
