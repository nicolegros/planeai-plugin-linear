use crate::model::{TaskKey, TaskStatus};
use crate::rpc::{HostClient, RpcError};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// A PlaneAI task as `host.task.get` returns it, parsed at the host boundary.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct HostTask {
    pub key: TaskKey,
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub status: TaskStatus,
    #[serde(default)]
    pub priority: i64,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// The task fields the plugin mirrors from Linear.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TaskFields {
    pub title: String,
    pub description: String,
    pub status: TaskStatus,
    pub priority: i64,
    /// Sorted, as PlaneAI returns them.
    pub tags: Vec<String>,
}

impl HostTask {
    pub fn fields(&self) -> TaskFields {
        let mut tags = self.tags.clone();
        tags.sort();
        TaskFields {
            title: self.title.clone(),
            description: self.description.clone(),
            status: self.status,
            priority: self.priority,
            tags,
        }
    }
}

/// The host callbacks this plugin uses, typed.
#[derive(Clone, Copy)]
pub struct Host<'a>(pub &'a HostClient);

impl Host<'_> {
    pub async fn task(&self, key: &TaskKey) -> Result<Option<HostTask>, RpcError> {
        let reply = self.0.call("host.task.get", json!({ "key": key })).await?;
        let task = reply.get("task").cloned().unwrap_or(Value::Null);
        serde_json::from_value(task)
            .map_err(|error| RpcError::failed(format!("invalid host task reply: {error}")))
    }

    pub async fn create_task(&self, key: &TaskKey, fields: &TaskFields) -> Result<(), RpcError> {
        self.0
            .call("host.task.create", with_key(key, fields))
            .await
            .map(drop)
    }

    pub async fn update_task(&self, key: &TaskKey, fields: &TaskFields) -> Result<(), RpcError> {
        self.0
            .call("host.task.update", with_key(key, fields))
            .await
            .map(drop)
    }

    pub async fn mark_done(&self, key: &TaskKey) -> Result<(), RpcError> {
        self.0
            .call(
                "host.task.update",
                json!({ "key": key, "status": TaskStatus::Done }),
            )
            .await
            .map(drop)
    }

    pub async fn settings(&self) -> Result<Value, RpcError> {
        let reply = self.0.call("host.settings.get", Value::Null).await?;
        match reply.get("settings") {
            Some(settings @ Value::Object(_)) => Ok(settings.clone()),
            _ => Err(RpcError::failed(
                "host settings reply did not carry a settings object",
            )),
        }
    }

    pub async fn replace_settings(&self, settings: &Value) -> Result<(), RpcError> {
        self.0
            .call("host.settings.replace", json!({ "settings": settings }))
            .await
            .map(drop)
    }
}

fn with_key(key: &TaskKey, fields: &TaskFields) -> Value {
    let mut params = serde_json::to_value(fields).expect("task fields serialize");
    params["key"] = json!(key);
    params
}
