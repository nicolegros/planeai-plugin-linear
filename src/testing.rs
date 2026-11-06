//! A fake PlaneAI host on the sidecar's RPC pipe, plus Linear mocks.

use crate::auth::tests::api_key_credentials;
use crate::auth::CredentialStore;
use crate::oauth::OAuthConfig;
use crate::plugin::LinearPlugin;
use crate::rpc::serve;
use crate::session::Linear;
use crate::store::Store;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use tokio::io::{
    AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines, ReadHalf, WriteHalf,
};
use wiremock::matchers::{body_partial_json, body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Runs a test body on a `LocalSet`, where the harness serves the plugin.
pub async fn local<F: Future>(body: F) -> F::Output {
    tokio::task::LocalSet::new().run_until(body).await
}

/// PlaneAI's side of the v7 contract: tasks, ownership, settings, and every call it saw.
pub struct FakeHost {
    pub settings: Value,
    pub tasks: BTreeMap<String, Value>,
    /// Tasks the plugin created with `host.task.create`; only these accept `host.task.update`.
    pub owned: BTreeSet<String>,
    pub calls: Vec<(String, Value)>,
    /// Methods that fail with the given message.
    pub failures: BTreeMap<String, String>,
}

impl Default for FakeHost {
    fn default() -> Self {
        Self {
            settings: json!({}),
            tasks: BTreeMap::new(),
            owned: BTreeSet::new(),
            calls: Vec::new(),
            failures: BTreeMap::new(),
        }
    }
}

impl FakeHost {
    pub fn with_settings(settings: Value) -> Self {
        Self {
            settings,
            ..Self::default()
        }
    }

    pub fn methods(&self) -> Vec<&str> {
        self.calls
            .iter()
            .map(|(method, _)| method.as_str())
            .collect()
    }

    fn answer(&mut self, method: &str, params: Value) -> Result<Value, (i64, String)> {
        self.calls.push((method.to_string(), params.clone()));
        if let Some(message) = self.failures.get(method) {
            return Err((-32603, message.clone()));
        }
        let key = params["key"].as_str().unwrap_or_default().to_string();
        match method {
            "host.settings.get" => Ok(json!({ "settings": self.settings })),
            "host.settings.replace" => {
                self.settings = params["settings"].clone();
                Ok(json!({ "settings": self.settings }))
            }
            "host.task.get" => Ok(json!({ "task": self.tasks.get(&key) })),
            "host.task.create" => {
                if self.tasks.contains_key(&key) {
                    return Err((-32005, format!("task {key} already exists")));
                }
                let mut task =
                    json!({ "status": "todo", "priority": 0, "tags": [], "description": "" });
                merge(&mut task, &params);
                self.tasks.insert(key.clone(), task.clone());
                self.owned.insert(key);
                Ok(task)
            }
            "host.task.update" => {
                if !self.owned.contains(&key) {
                    return Err(match self.tasks.contains_key(&key) {
                        true => (-32005, format!("task {key} belongs to another owner")),
                        false => (-32003, "not granted".to_string()),
                    });
                }
                let task = self
                    .tasks
                    .get_mut(&key)
                    .ok_or((-32602, "unknown task".to_string()))?;
                merge(task, &params);
                Ok(task.clone())
            }
            _ => Err((-32601, "method not found".to_string())),
        }
    }
}

fn merge(task: &mut Value, params: &Value) {
    for (field, value) in params.as_object().unwrap() {
        let value = if field == "tags" {
            let mut tags: Vec<String> = serde_json::from_value(value.clone()).unwrap();
            tags.sort();
            json!(tags)
        } else {
            value.clone()
        };
        task[field] = value;
    }
}

/// The plugin served over a real duplex pipe; nested host calls are answered by `host`.
pub struct Harness {
    to_plugin: WriteHalf<DuplexStream>,
    from_plugin: Lines<BufReader<ReadHalf<DuplexStream>>>,
    next_id: u64,
    pub host: FakeHost,
}

impl Harness {
    pub fn start(mut plugin: LinearPlugin, host: FakeHost) -> Self {
        let (plugin_side, test_side) = tokio::io::duplex(1 << 20);
        let (plugin_read, plugin_write) = tokio::io::split(plugin_side);
        let (test_read, test_write) = tokio::io::split(test_side);
        tokio::task::spawn_local(async move {
            serve(BufReader::new(plugin_read), plugin_write, &mut plugin)
                .await
                .unwrap();
        });
        Self {
            to_plugin: test_write,
            from_plugin: BufReader::new(test_read).lines(),
            next_id: 0,
            host,
        }
    }

    async fn write(&mut self, frame: Value) {
        self.to_plugin
            .write_all(format!("{frame}\n").as_bytes())
            .await
            .unwrap();
    }

    /// Sends a request and answers its nested host calls until the plugin replies.
    pub async fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.next_id += 1;
        let id = self.next_id;
        self.write(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await;
        loop {
            let line = self
                .from_plugin
                .next_line()
                .await
                .unwrap()
                .expect("plugin stopped");
            let frame: Value = serde_json::from_str(&line).unwrap();
            if let Some(nested) = frame["method"].as_str() {
                let reply = match self.host.answer(nested, frame["params"].clone()) {
                    Ok(result) => json!({ "jsonrpc": "2.0", "id": frame["id"], "result": result }),
                    Err((code, message)) => json!({
                        "jsonrpc": "2.0", "id": frame["id"], "error": { "code": code, "message": message }
                    }),
                };
                self.write(reply).await;
                continue;
            }
            assert_eq!(frame["id"], json!(id));
            return match frame.get("error") {
                Some(error) => Err(error["message"].as_str().unwrap().to_string()),
                None => Ok(frame["result"].clone()),
            };
        }
    }
}

/// A plugin wired to a Linear mock server and private directories.
pub struct Fixture {
    pub server: MockServer,
    pub data: tempfile::TempDir,
    pub secrets: tempfile::TempDir,
}

impl Fixture {
    pub async fn new() -> Self {
        Self {
            server: MockServer::start().await,
            data: tempfile::tempdir().unwrap(),
            secrets: tempfile::tempdir().unwrap(),
        }
    }

    pub fn linear(&self) -> Linear {
        Linear {
            http: reqwest::Client::new(),
            graphql_url: format!("{}/graphql", self.server.uri()),
            oauth: OAuthConfig {
                client_id: Some("client-1".into()),
                token_url: format!("{}/oauth/token", self.server.uri()),
                callback_address: "127.0.0.1:0".into(),
                ..OAuthConfig::built()
            },
            credentials: CredentialStore::new(self.secrets.path().to_path_buf()),
        }
    }

    pub fn connect(&self, organization_id: &str) {
        self.linear()
            .credentials
            .save(&api_key_credentials("lin_api_key", organization_id))
            .unwrap();
    }

    pub fn start(&self, host: FakeHost) -> Harness {
        Harness::start(
            LinearPlugin::new(self.data.path().to_path_buf(), self.linear()),
            host,
        )
    }

    pub fn store(&self) -> Store {
        Store::open(&self.data.path().join("linear.sqlite")).unwrap()
    }

    /// Answers the filter query `filter` with one page of `issues`, replacing earlier issue mocks.
    pub async fn filter_returns(&self, filter: Value, issues: Vec<Value>) {
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(body_string_contains("FilteredIssues"))
            .and(body_partial_json(json!({ "variables": { "filter": filter } })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": { "issues": { "nodes": issues, "pageInfo": { "hasNextPage": false, "endCursor": null } } }
            })))
            .mount(&self.server)
            .await;
    }

    pub async fn view_returns(&self, view_id: &str, issues: Vec<Value>) {
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(body_string_contains("ViewIssues"))
            .and(body_partial_json(json!({ "variables": { "id": view_id } })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": { "customView": { "issues": { "nodes": issues, "pageInfo": { "hasNextPage": false, "endCursor": null } } } }
            })))
            .mount(&self.server)
            .await;
    }

    pub async fn reset_linear(&self) {
        self.server.reset().await;
    }
}

pub fn filter(team: &str) -> Value {
    json!({ "team": { "key": { "eq": team } } })
}

pub fn filter_source(team: &str) -> Value {
    json!({ "query": { "kind": "filter", "filter": filter(team) } })
}
