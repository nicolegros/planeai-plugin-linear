use serde::{Deserialize, Serialize};
use std::fmt;

/// A Linear issue's uuid, stable across team moves.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IssueId(pub String);

/// A PlaneAI task key such as `ENG-123`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TaskKey(pub String);

impl fmt::Display for IssueId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Display for TaskKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Todo,
    InProgress,
    InReview,
    Done,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Todo => "todo",
            Self::InProgress => "in_progress",
            Self::InReview => "in_review",
            Self::Done => "done",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "todo" => Some(Self::Todo),
            "in_progress" => Some(Self::InProgress),
            "in_review" => Some(Self::InReview),
            "done" => Some(Self::Done),
            _ => None,
        }
    }
}

/// Linear's `WorkflowState.type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateType {
    Triage,
    Backlog,
    Unstarted,
    Started,
    Completed,
    Canceled,
    Duplicate,
    #[serde(other)]
    Unknown,
}

impl StateType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Triage => "triage",
            Self::Backlog => "backlog",
            Self::Unstarted => "unstarted",
            Self::Started => "started",
            Self::Completed => "completed",
            Self::Canceled => "canceled",
            Self::Duplicate => "duplicate",
            Self::Unknown => "unknown",
        }
    }

    pub fn task_status(self) -> TaskStatus {
        match self {
            Self::Triage | Self::Backlog | Self::Unstarted | Self::Unknown => TaskStatus::Todo,
            Self::Started => TaskStatus::InProgress,
            Self::Completed | Self::Canceled | Self::Duplicate => TaskStatus::Done,
        }
    }
}

/// Linear's issue priority: 0 none, 1 urgent, 2 high, 3 medium, 4 low.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    None,
    Urgent,
    High,
    Medium,
    Low,
}

impl Priority {
    pub fn from_linear(value: f64) -> Option<Self> {
        match value {
            v if v == 0.0 => Some(Self::None),
            v if v == 1.0 => Some(Self::Urgent),
            v if v == 2.0 => Some(Self::High),
            v if v == 3.0 => Some(Self::Medium),
            v if v == 4.0 => Some(Self::Low),
            _ => None,
        }
    }

    /// PlaneAI dispatches larger priorities first, the reverse of Linear's numbering.
    pub fn task_priority(self) -> i64 {
        match self {
            Self::Urgent => 4,
            Self::High => 3,
            Self::Medium => 2,
            Self::Low => 1,
            Self::None => 0,
        }
    }
}

/// An issue as fetched from Linear, parsed at the GraphQL boundary.
#[derive(Debug, Clone, PartialEq)]
pub struct LinearIssue {
    pub id: IssueId,
    pub identifier: String,
    pub title: String,
    pub description: String,
    pub url: String,
    pub state_name: String,
    pub state_type: StateType,
    pub priority: Priority,
    pub labels: Vec<String>,
    pub team_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_priority_maps_to_planeai_order() {
        let mapped: Vec<_> = [0.0, 1.0, 2.0, 3.0, 4.0]
            .into_iter()
            .map(|value| Priority::from_linear(value).unwrap().task_priority())
            .collect();
        assert_eq!(mapped, vec![0, 4, 3, 2, 1]);
        assert_eq!(Priority::from_linear(5.0), None);
    }

    #[test]
    fn state_types_map_to_task_statuses() {
        let mapped: Vec<_> = [
            "triage",
            "backlog",
            "unstarted",
            "started",
            "completed",
            "canceled",
            "duplicate",
            "something-new",
        ]
        .into_iter()
        .map(|kind| {
            serde_json::from_value::<StateType>(serde_json::json!(kind))
                .unwrap()
                .task_status()
                .as_str()
        })
        .collect();
        assert_eq!(
            mapped,
            vec![
                "todo",
                "todo",
                "todo",
                "in_progress",
                "done",
                "done",
                "done",
                "todo"
            ]
        );
    }
}
