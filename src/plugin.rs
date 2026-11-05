use crate::auth::CredentialStore;
use crate::connect::Connector;
use crate::host::Host;
use crate::linear_api::GRAPHQL_URL;
use crate::model::IssueId;
use crate::oauth::OAuthConfig;
use crate::rpc::{Handler, HostClient, RpcError, METHOD_NOT_FOUND};
use crate::session::Linear;
use crate::settings::Settings;
use crate::store::Store;
use crate::sync::sync_now;
use crate::writeback::{task_lifecycle, LifecycleBatch, WritebackOutcome};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Duration;

pub const PLUGIN_ID: &str = "linear";
pub const PLUGIN_NAME: &str = "Linear";
pub const PLUGIN_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const HOST_API_VERSION: &str = "planeai.plugin-host.v7";
const INVALID_PARAMS: i64 = -32602;
const NETWORK_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Deserialize)]
struct Handshake {
    host_api_version: String,
}

#[derive(Deserialize)]
struct IssueParams {
    issue_id: IssueId,
}

#[derive(Deserialize)]
struct AttemptParams {
    #[serde(default)]
    attempt_id: Option<String>,
}

#[derive(Deserialize)]
struct ApiKeyParams {
    key: String,
}

#[derive(Deserialize)]
struct BrowserParams {
    url: String,
}

#[derive(Deserialize)]
struct LifecycleParams {
    batch: LifecycleBatch,
}

/// A sidecar request, parsed from its method and params.
enum Call {
    Handshake(Handshake),
    Shutdown,
    Status,
    SettingsGet,
    SettingsUpdate(Value),
    SyncNow,
    SidebarItems,
    AssignmentSelect(IssueParams),
    AssignmentCurrent,
    DeparturesList,
    DeparturesDequeue(IssueParams),
    DeparturesResolve(IssueParams),
    ConnectApiKey(ApiKeyParams),
    ConnectStart(AttemptParams),
    ConnectComplete(AttemptParams),
    ConnectCancel(AttemptParams),
    Disconnect,
    OpenBrowser(BrowserParams),
    TaskLifecycle(LifecycleParams),
}

impl Call {
    fn parse(method: &str, params: Value) -> Result<Self, RpcError> {
        fn p<T: DeserializeOwned>(params: Value) -> Result<T, RpcError> {
            let params = if params.is_null() { json!({}) } else { params };
            serde_json::from_value(params)
                .map_err(|error| RpcError::new(INVALID_PARAMS, format!("invalid params: {error}")))
        }
        Ok(match method {
            "plugin.handshake" => Self::Handshake(p(params)?),
            "plugin.shutdown" => Self::Shutdown,
            "plugin.taskLifecycle" => Self::TaskLifecycle(p(params)?),
            "linear.status" => Self::Status,
            "linear.settings.get" => Self::SettingsGet,
            "linear.settings.update" => Self::SettingsUpdate(params),
            "linear.syncNow" => Self::SyncNow,
            "linear.sidebar.items" => Self::SidebarItems,
            "linear.assignment.select" => Self::AssignmentSelect(p(params)?),
            "linear.assignment.current" => Self::AssignmentCurrent,
            "linear.departures.list" => Self::DeparturesList,
            "linear.departures.dequeue" => Self::DeparturesDequeue(p(params)?),
            "linear.departures.resolve" => Self::DeparturesResolve(p(params)?),
            "linear.connect.apiKey" => Self::ConnectApiKey(p(params)?),
            "linear.connect.start" => Self::ConnectStart(p(params)?),
            "linear.connect.complete" => Self::ConnectComplete(p(params)?),
            "linear.connect.cancel" => Self::ConnectCancel(p(params)?),
            "linear.disconnect" => Self::Disconnect,
            "linear.open_browser" => Self::OpenBrowser(p(params)?),
            _ => return Err(RpcError::new(METHOD_NOT_FOUND, "method not found")),
        })
    }
}

pub struct LinearPlugin {
    data_dir: PathBuf,
    linear: Linear,
    connector: Connector,
    selected_issue: Option<IssueId>,
    last_writeback: Option<WritebackOutcome>,
}

impl LinearPlugin {
    pub fn new(data_dir: PathBuf, linear: Linear) -> Self {
        Self {
            data_dir,
            connector: Connector::new(linear.clone()),
            linear,
            selected_issue: None,
            last_writeback: None,
        }
    }

    pub fn from_environment() -> Result<Self, String> {
        let directory = |name: &str| {
            std::env::var_os(name)
                .map(PathBuf::from)
                .ok_or(format!("{name} was not provided by the host"))
        };
        let data_dir = directory("PLANEAI_PLUGIN_DATA_DIR")?;
        let secrets_dir = directory("PLANEAI_PLUGIN_SECRETS_DIR")?;
        std::fs::create_dir_all(&data_dir).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&secrets_dir).map_err(|error| error.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&secrets_dir, std::fs::Permissions::from_mode(0o700))
                .map_err(|error| error.to_string())?;
        }
        let http = reqwest::Client::builder()
            .timeout(NETWORK_TIMEOUT)
            .user_agent(concat!("planeai-plugin-linear/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| error.to_string())?;
        Ok(Self::new(
            data_dir,
            Linear {
                http,
                graphql_url: GRAPHQL_URL.to_string(),
                oauth: OAuthConfig::built(),
                credentials: CredentialStore::new(secrets_dir),
            },
        ))
    }

    fn database(&self) -> PathBuf {
        self.data_dir.join("linear.sqlite")
    }

    fn store(&self) -> Result<Store, String> {
        Store::open(&self.database()).map_err(|error| error.to_string())
    }

    async fn run(&mut self, call: Call, host: Host<'_>) -> Result<Value, String> {
        match call {
            Call::Handshake(handshake) if handshake.host_api_version == HOST_API_VERSION => {
                Ok(json!({
                    "plugin_id": PLUGIN_ID,
                    "plugin_name": PLUGIN_NAME,
                    "plugin_version": PLUGIN_VERSION,
                    "host_api_version": HOST_API_VERSION,
                    "lifecycle_event_subscriptions": ["task.lifecycle"],
                }))
            }
            Call::Handshake(_) => Err("unsupported plugin host API version".into()),
            Call::Shutdown => Ok(json!({ "stopping": true })),
            Call::Status => Ok(self.status().await),
            Call::SettingsGet => {
                let settings = host.settings().await.map_err(|error| error.to_string())?;
                Ok(Settings::parse(&settings)
                    .map_err(|error| error.to_string())?
                    .to_value())
            }
            Call::SettingsUpdate(document) => self.update_settings(&document, host).await,
            Call::SyncNow => sync_now(&self.linear, &self.database(), host)
                .await
                .map(|totals| serde_json::to_value(totals).expect("totals serialize")),
            Call::SidebarItems => self.sidebar_items(),
            Call::AssignmentSelect(IssueParams { issue_id }) => {
                self.store()?
                    .synced_issue(&issue_id)
                    .map_err(|error| error.to_string())?
                    .ok_or("this Linear issue is no longer available")?;
                self.selected_issue = Some(issue_id);
                Ok(json!({ "selected": true }))
            }
            Call::AssignmentCurrent => {
                let issue_id = self
                    .selected_issue
                    .as_ref()
                    .ok_or("choose a Linear issue in the sidebar first")?;
                let issue = self
                    .store()?
                    .synced_issue(issue_id)
                    .map_err(|error| error.to_string())?
                    .ok_or("this Linear issue is no longer available")?;
                Ok(serde_json::to_value(issue).expect("issue serializes"))
            }
            Call::DeparturesList => {
                let items = self
                    .store()?
                    .departure_queue()
                    .map_err(|error| error.to_string())?;
                Ok(json!({ "items": items }))
            }
            Call::DeparturesDequeue(IssueParams { issue_id }) => {
                self.store()?
                    .dequeue_departure(&issue_id)
                    .map_err(|error| error.to_string())?;
                Ok(json!({ "dequeued": true }))
            }
            Call::DeparturesResolve(IssueParams { issue_id }) => {
                let store = self.store()?;
                let departed = store
                    .queued_departure(&issue_id)
                    .map_err(|error| error.to_string())?
                    .ok_or("this Linear issue is no longer waiting for a decision")?;
                host.mark_done(&departed.task_key).await.map_err(|error| {
                    format!("failed to mark {} done: {error}", departed.task_key)
                })?;
                store
                    .dequeue_departure(&issue_id)
                    .map_err(|error| error.to_string())?;
                Ok(json!({ "resolved": true }))
            }
            Call::ConnectApiKey(ApiKeyParams { key }) => self.connector.connect_api_key(&key).await,
            Call::ConnectStart(AttemptParams { attempt_id }) => {
                self.connector.start(attempt_id).await
            }
            Call::ConnectComplete(AttemptParams { attempt_id }) => {
                self.connector.complete(attempt_id.as_deref())
            }
            Call::ConnectCancel(AttemptParams { attempt_id }) => {
                self.connector.cancel(attempt_id.as_deref()).await
            }
            Call::Disconnect => self.connector.disconnect().await,
            Call::OpenBrowser(BrowserParams { url }) => {
                self.linear.oauth.validate_authorization_url(&url)?;
                open::that(&url).map_err(|error| format!("failed to open browser: {error}"))?;
                Ok(json!({ "opened": true }))
            }
            Call::TaskLifecycle(LifecycleParams { batch }) => {
                let outcomes = task_lifecycle(&self.linear, &self.database(), host, &batch).await?;
                if let Some(last) = outcomes.last() {
                    self.last_writeback = Some(last.clone());
                }
                Ok(json!({ "outcomes": outcomes }))
            }
        }
    }

    async fn status(&mut self) -> Value {
        self.connector.reap().await;
        let (credentials, credentials_error) = match self.linear.credentials.load() {
            Ok(credentials) => (credentials, None),
            Err(error) => (None, Some(error)),
        };
        json!({
            "plugin_id": PLUGIN_ID,
            "plugin_name": PLUGIN_NAME,
            "plugin_version": PLUGIN_VERSION,
            "host_api_version": HOST_API_VERSION,
            "runtime_state": "running",
            "connected": credentials.is_some(),
            "authorizing": self.connector.authorizing(),
            "oauth_available": self.connector.oauth_available(),
            "auth_kind": credentials.as_ref().map(|credentials| credentials.auth.kind()),
            "organization": credentials.as_ref().map(|credentials| json!({
                "id": credentials.organization_id,
                "name": credentials.organization_name,
                "url_key": credentials.url_key,
            })),
            "last_error": self.connector.last_error().map(str::to_string).or(credentials_error),
            "last_writeback": self.last_writeback,
        })
    }

    /// Validates, persists through the host, then moves memberships; a failed move restores the old document.
    async fn update_settings(&self, document: &Value, host: Host<'_>) -> Result<Value, String> {
        let settings = Settings::parse(document).map_err(|error| error.to_string())?;
        let previous = host
            .settings()
            .await
            .map_err(|error| format!("failed to read Linear settings: {error}"))?;
        let saved = settings.to_value();
        host.replace_settings(&saved)
            .await
            .map_err(|error| format!("failed to save Linear settings: {error}"))?;
        let applied = self.store().and_then(|store| {
            store
                .apply_settings(&settings)
                .map_err(|error| error.to_string())
        });
        if let Err(error) = applied {
            if let Err(rollback) = host.replace_settings(&previous).await {
                eprintln!("linear could not restore settings after a failed update: {rollback}");
            }
            return Err(error);
        }
        Ok(saved)
    }

    fn sidebar_items(&self) -> Result<Value, String> {
        let Some(credentials) = self.linear.credentials.load()? else {
            return Ok(json!({ "items": [] }));
        };
        let store = self.store()?;
        if !store
            .cache_matches_organization(&credentials.organization_id)
            .map_err(|error| error.to_string())?
        {
            return Ok(json!({ "items": [] }));
        }
        let items = store.sidebar_items().map_err(|error| error.to_string())?;
        Ok(json!({ "items": items }))
    }
}

impl Handler for LinearPlugin {
    async fn handle(
        &mut self,
        method: &str,
        params: Value,
        host: &HostClient,
    ) -> Result<Value, RpcError> {
        let call = Call::parse(method, params)?;
        self.run(call, Host(host)).await.map_err(RpcError::failed)
    }
}
