use crate::auth::{Auth, Credentials};
use crate::linear_api::{viewer_organization, ApiError};
use crate::oauth::{self, generate_pkce, generate_state, OAuthError};
use crate::session::Linear;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

const CALLBACK_TIMEOUT: Duration = Duration::from_secs(120);

/// A started OAuth attempt waiting for the user to finish in the browser.
struct PendingAuth {
    attempt_id: String,
    verifier: String,
    state: String,
    listener: TcpListener,
}

struct AuthCompletion {
    attempt_id: String,
    task: JoinHandle<Result<(), String>>,
}

/// Connects a Linear workspace with a personal API key or OAuth.
pub struct Connector {
    linear: Linear,
    pending: Option<PendingAuth>,
    completion: Option<AuthCompletion>,
    completed_attempt: Option<String>,
    last_error: Option<String>,
}

impl Connector {
    pub fn new(linear: Linear) -> Self {
        Self {
            linear,
            pending: None,
            completion: None,
            completed_attempt: None,
            last_error: None,
        }
    }

    pub fn oauth_available(&self) -> bool {
        self.linear.oauth.client_id.is_some()
    }

    pub fn authorizing(&self) -> bool {
        self.completion.is_some()
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    fn ensure_disconnected(&self) -> Result<(), String> {
        if self.linear.credentials.load()?.is_some() {
            return Err(
                "disconnect the current Linear workspace before connecting another one".into(),
            );
        }
        Ok(())
    }

    pub async fn connect_api_key(&mut self, key: &str) -> Result<Value, String> {
        self.ensure_disconnected()?;
        let key = key.trim();
        if key.is_empty() {
            return Err("paste a Linear personal API key".into());
        }
        let organization = viewer_organization(&self.linear.http, &self.linear.graphql_url, key)
            .await
            .map_err(|error| match error {
                ApiError::Unauthorized => "Linear rejected the API key".to_string(),
                other => other.to_string(),
            })?;
        self.linear.credentials.save(&Credentials {
            auth: Auth::ApiKey { key: key.into() },
            organization_id: organization.id,
            organization_name: organization.name.clone(),
            url_key: organization.url_key,
        })?;
        self.last_error = None;
        Ok(json!({ "connected": true, "organization_name": organization.name }))
    }

    pub async fn start(&mut self, attempt_id: Option<String>) -> Result<Value, String> {
        let Some(client_id) = self.linear.oauth.client_id.clone() else {
            return Err(
                "this build of the Linear plugin has no OAuth client; connect with a personal API key"
                    .into(),
            );
        };
        if self.pending.is_some() || self.completion.is_some() {
            return Err("an authorization flow is already waiting for a callback".into());
        }
        self.ensure_disconnected()?;
        let address = &self.linear.oauth.callback_address;
        let listener = TcpListener::bind(address).await.map_err(|error| {
            format!("failed to bind the OAuth callback on {address}: {error}. Is another PlaneAI instance running?")
        })?;
        let (verifier, challenge) = generate_pkce();
        let state = generate_state();
        let url = self
            .linear
            .oauth
            .authorization_url(&client_id, &challenge, &state);
        self.last_error = None;
        self.completed_attempt = None;
        self.pending = Some(PendingAuth {
            attempt_id: attempt_id
                .filter(|id| !id.is_empty())
                .unwrap_or_else(generate_state),
            verifier,
            state,
            listener,
        });
        Ok(json!({ "authorization_url": url.to_string() }))
    }

    pub fn complete(&mut self, attempt_id: Option<&str>) -> Result<Value, String> {
        let pending = self
            .pending
            .take()
            .ok_or("start authorization before completing it")?;
        if attempt_id.is_some_and(|id| id != pending.attempt_id) {
            self.pending = Some(pending);
            return Err("authorization attempt does not match the active flow".into());
        }
        let linear = self.linear.clone();
        self.completion = Some(AuthCompletion {
            attempt_id: pending.attempt_id.clone(),
            task: tokio::spawn(async move { finish(pending, &linear).await }),
        });
        Ok(json!({ "authorizing": true }))
    }

    /// Records the outcome of a finished OAuth attempt.
    pub async fn reap(&mut self) {
        if !self
            .completion
            .as_ref()
            .is_some_and(|completion| completion.task.is_finished())
        {
            return;
        }
        let completion = self.completion.take().expect("checked above");
        self.last_error = match completion.task.await {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(error),
            Err(error) => Some(error.to_string()),
        };
        self.completed_attempt = Some(completion.attempt_id);
    }

    /// Cancels the matching attempt, or every attempt without an id; true when one matched.
    async fn cancel_attempt(&mut self, attempt_id: Option<&str>) -> bool {
        let matches = |candidate: &str| attempt_id.is_none_or(|id| id == candidate);
        let pending = self
            .pending
            .as_ref()
            .is_some_and(|pending| matches(&pending.attempt_id));
        if pending {
            self.pending = None;
        }
        let running = self
            .completion
            .as_ref()
            .is_some_and(|completion| matches(&completion.attempt_id));
        if running {
            let completion = self.completion.take().expect("checked above");
            completion.task.abort();
            let _ = completion.task.await;
        }
        let completed = self.completed_attempt.as_deref().is_some_and(matches);
        if completed {
            self.completed_attempt = None;
        }
        pending || running || completed
    }

    /// A cancelled attempt must not leave a connection behind, even one it already saved.
    pub async fn cancel(&mut self, attempt_id: Option<&str>) -> Result<Value, String> {
        let cancelled = self.cancel_attempt(attempt_id).await;
        if cancelled {
            self.linear.credentials.clear()?;
        }
        Ok(json!({ "cancelled": cancelled }))
    }

    pub async fn disconnect(&mut self) -> Result<Value, String> {
        self.cancel_attempt(None).await;
        self.linear.credentials.clear()?;
        self.last_error = None;
        Ok(json!({ "connected": false }))
    }

    #[cfg(test)]
    pub fn callback_port(&self) -> Option<u16> {
        self.pending
            .as_ref()
            .map(|pending| pending.listener.local_addr().unwrap().port())
    }
}

async fn finish(pending: PendingAuth, linear: &Linear) -> Result<(), String> {
    let client_id = linear
        .oauth
        .client_id
        .as_deref()
        .ok_or("this build has no OAuth client")?;
    let code = tokio::time::timeout(
        CALLBACK_TIMEOUT,
        oauth::wait_for_callback(&pending.listener, &pending.state),
    )
    .await
    .map_err(|_| OAuthError::Timeout.to_string())?
    .map_err(|error| error.to_string())?;
    drop(pending.listener);
    let tokens = oauth::exchange_code(
        &linear.http,
        &linear.oauth,
        client_id,
        &code,
        &pending.verifier,
    )
    .await
    .map_err(|error| error.to_string())?;
    let auth = Auth::OAuth {
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
        expires_at: tokens.expires_at,
    };
    let organization =
        viewer_organization(&linear.http, &linear.graphql_url, &auth.authorization())
            .await
            .map_err(|error| error.to_string())?;
    linear.credentials.save(&Credentials {
        auth,
        organization_id: organization.id,
        organization_name: organization.name,
        url_key: organization.url_key,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::tests::api_key_credentials;
    use crate::session::tests::linear;
    use tokio::io::AsyncWriteExt;
    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn viewer(server: &MockServer, authorization: &str) {
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(header("authorization", authorization))
            .and(body_string_contains("viewer"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": { "viewer": { "organization": { "id": "org-1", "name": "Acme", "urlKey": "acme" } } }
            })))
            .mount(server)
            .await;
    }

    fn connector(server: &MockServer, secrets: &std::path::Path) -> Connector {
        let mut linear = linear(server, secrets);
        linear.oauth.callback_address = "127.0.0.1:0".into();
        Connector::new(linear)
    }

    #[tokio::test]
    async fn an_api_key_is_validated_with_the_viewer_before_it_is_saved() {
        let server = MockServer::start().await;
        viewer(&server, "lin_api_good").await;
        Mock::given(method("POST"))
            .and(header("authorization", "lin_api_bad"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({ "errors": [{ "message": "no", "extensions": { "code": "AUTHENTICATION_ERROR" } }] })))
            .mount(&server)
            .await;
        let secrets = tempfile::tempdir().unwrap();
        let mut connector = connector(&server, secrets.path());

        assert_eq!(
            connector.connect_api_key("lin_api_bad").await,
            Err("Linear rejected the API key".to_string())
        );
        assert_eq!(connector.linear.credentials.load().unwrap(), None);

        assert_eq!(
            connector
                .connect_api_key("  lin_api_good \n")
                .await
                .unwrap(),
            json!({ "connected": true, "organization_name": "Acme" })
        );
        assert_eq!(
            connector.linear.credentials.load().unwrap(),
            Some(api_key_credentials("lin_api_good", "org-1"))
        );
        assert_eq!(
            connector.connect_api_key("lin_api_good").await,
            Err("disconnect the current Linear workspace before connecting another one".into())
        );
    }

    #[tokio::test]
    async fn oauth_is_unavailable_without_a_built_client_id() {
        let server = MockServer::start().await;
        let secrets = tempfile::tempdir().unwrap();
        let mut connector = connector(&server, secrets.path());
        connector.linear.oauth.client_id = None;
        assert!(!connector.oauth_available());
        assert_eq!(
            connector.start(None).await,
            Err("this build of the Linear plugin has no OAuth client; connect with a personal API key".into())
        );
    }

    #[tokio::test]
    async fn oauth_flow_exchanges_the_callback_code_and_saves_the_workspace() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .and(body_string_contains("code=the-code"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "access", "refresh_token": "refresh", "expires_in": 86399
            })))
            .mount(&server)
            .await;
        viewer(&server, "Bearer access").await;
        let secrets = tempfile::tempdir().unwrap();
        let mut connector = connector(&server, secrets.path());

        let started = connector.start(Some("attempt-1".into())).await.unwrap();
        let url = url::Url::parse(started["authorization_url"].as_str().unwrap()).unwrap();
        assert_eq!(
            connector
                .linear
                .oauth
                .validate_authorization_url(url.as_str()),
            Ok(())
        );
        let state = url
            .query_pairs()
            .find(|(key, _)| key == "state")
            .unwrap()
            .1
            .to_string();
        let port = connector.callback_port().unwrap();
        assert_eq!(
            connector.complete(Some("other")),
            Err("authorization attempt does not match the active flow".into())
        );
        assert_eq!(
            connector.complete(Some("attempt-1")).unwrap(),
            json!({ "authorizing": true })
        );
        assert!(connector.authorizing());

        let mut browser = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        browser
            .write_all(
                format!("GET /callback?code=the-code&state={state} HTTP/1.1\r\n\r\n").as_bytes(),
            )
            .await
            .unwrap();
        for _ in 0..200 {
            connector.reap().await;
            if !connector.authorizing() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(!connector.authorizing());
        assert_eq!(connector.last_error(), None);
        let stored = connector.linear.credentials.load().unwrap().unwrap();
        assert_eq!(stored.organization_name, "Acme");
        assert_eq!(stored.auth.kind(), "oauth");
    }

    #[tokio::test]
    async fn cancelling_authorization_removes_credentials_written_by_the_task() {
        let server = MockServer::start().await;
        let secrets = tempfile::tempdir().unwrap();
        let mut connector = connector(&server, secrets.path());
        let (written, observed) = tokio::sync::oneshot::channel();
        let store = connector.linear.credentials.clone();
        connector.completion = Some(AuthCompletion {
            attempt_id: "test".into(),
            task: tokio::spawn(async move {
                store.save(&api_key_credentials("k", "org"))?;
                let _ = written.send(());
                std::future::pending::<()>().await;
                Ok(())
            }),
        });
        observed.await.unwrap();
        assert_eq!(
            connector.cancel(None).await.unwrap(),
            json!({ "cancelled": true })
        );
        assert_eq!(connector.linear.credentials.load().unwrap(), None);
    }

    #[tokio::test]
    async fn cancelling_a_reaped_attempt_removes_its_credentials() {
        let server = MockServer::start().await;
        let secrets = tempfile::tempdir().unwrap();
        let mut connector = connector(&server, secrets.path());
        connector
            .linear
            .credentials
            .save(&api_key_credentials("k", "org"))
            .unwrap();
        connector.completed_attempt = Some("completed-attempt".into());
        assert_eq!(
            connector.cancel(Some("completed-attempt")).await.unwrap(),
            json!({ "cancelled": true })
        );
        assert_eq!(connector.linear.credentials.load().unwrap(), None);
    }

    #[tokio::test]
    async fn stale_cancellation_does_not_remove_a_newer_attempts_credentials() {
        let server = MockServer::start().await;
        let secrets = tempfile::tempdir().unwrap();
        let mut connector = connector(&server, secrets.path());
        connector
            .linear
            .credentials
            .save(&api_key_credentials("k", "org"))
            .unwrap();
        connector.completed_attempt = Some("newer-attempt".into());
        assert_eq!(
            connector.cancel(Some("stale-attempt")).await.unwrap(),
            json!({ "cancelled": false })
        );
        assert!(connector.linear.credentials.load().unwrap().is_some());
        assert_eq!(
            connector.completed_attempt.as_deref(),
            Some("newer-attempt")
        );
    }

    #[tokio::test]
    async fn a_second_start_is_refused_while_an_attempt_waits() {
        let server = MockServer::start().await;
        let secrets = tempfile::tempdir().unwrap();
        let mut connector = connector(&server, secrets.path());
        connector.start(None).await.unwrap();
        assert_eq!(
            connector.start(None).await,
            Err("an authorization flow is already waiting for a callback".into())
        );
        connector.disconnect().await.unwrap();
        assert!(connector.start(None).await.is_ok());
    }
}
