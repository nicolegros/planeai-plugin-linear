use crate::auth::{Auth, CredentialStore, Credentials};
use crate::linear_api::{graphql, ApiError};
use crate::oauth::{self, OAuthConfig, RefreshError};
use chrono::{Duration, Utc};
use reqwest::Client;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Refresh an OAuth token this long before Linear says it expires.
const REFRESH_MARGIN: Duration = Duration::minutes(5);

/// Everything needed to talk to Linear: the HTTP client, endpoints, and stored credentials.
#[derive(Clone)]
pub struct Linear {
    pub http: Client,
    pub graphql_url: String,
    pub oauth: OAuthConfig,
    pub credentials: CredentialStore,
}

/// Authenticated access for one operation; renews an OAuth token when it nears expiry or Linear answers 401.
pub struct Session<'a> {
    linear: &'a Linear,
    credentials: Credentials,
    refreshed: bool,
}

impl Linear {
    pub async fn session(&self) -> Result<Session<'_>, ApiError> {
        let credentials = self
            .credentials
            .load()
            .map_err(ApiError::Reconnect)?
            .ok_or(ApiError::NotConnected)?;
        let mut session = Session {
            linear: self,
            credentials,
            refreshed: false,
        };
        if session.expiring() {
            session.refresh().await?;
        }
        Ok(session)
    }
}

impl Session<'_> {
    pub fn credentials(&self) -> &Credentials {
        &self.credentials
    }

    pub async fn execute<T: DeserializeOwned>(
        &mut self,
        query: &str,
        variables: Value,
    ) -> Result<T, ApiError> {
        let outcome = self.send(query, variables.clone()).await;
        match outcome {
            Err(ApiError::Unauthorized) if !self.refreshed && self.can_refresh() => {
                self.refresh().await?;
                self.send(query, variables).await
            }
            other => other,
        }
    }

    async fn send<T: DeserializeOwned>(
        &self,
        query: &str,
        variables: Value,
    ) -> Result<T, ApiError> {
        graphql(
            &self.linear.http,
            &self.linear.graphql_url,
            &self.credentials.auth.authorization(),
            query,
            variables,
        )
        .await
    }

    fn can_refresh(&self) -> bool {
        matches!(
            self.credentials.auth,
            Auth::OAuth {
                refresh_token: Some(_),
                ..
            }
        )
    }

    fn expiring(&self) -> bool {
        match &self.credentials.auth {
            Auth::OAuth {
                refresh_token: Some(_),
                expires_at: Some(expires_at),
                ..
            } => *expires_at - Utc::now() < REFRESH_MARGIN,
            _ => false,
        }
    }

    async fn refresh(&mut self) -> Result<(), ApiError> {
        let Auth::OAuth {
            refresh_token: Some(refresh_token),
            ..
        } = &self.credentials.auth
        else {
            return Err(ApiError::Unauthorized);
        };
        let refresh_token = refresh_token.clone();
        let Some(client_id) = self.linear.oauth.client_id.as_deref() else {
            return Err(ApiError::Reconnect(
                "this build cannot renew Linear OAuth tokens; reconnect with an API key".into(),
            ));
        };
        let store = &self.linear.credentials;
        match oauth::refresh(
            &self.linear.http,
            &self.linear.oauth,
            client_id,
            &refresh_token,
        )
        .await
        {
            Ok(tokens) => {
                self.credentials.auth = Auth::OAuth {
                    access_token: tokens.access_token,
                    refresh_token: tokens.refresh_token.or(Some(refresh_token)),
                    expires_at: tokens.expires_at,
                };
                self.refreshed = true;
                store.save(&self.credentials).map_err(ApiError::Reconnect)
            }
            Err(RefreshError::Rejected) => {
                store.clear().map_err(ApiError::Reconnect)?;
                Err(ApiError::Reconnect(
                    "Linear OAuth refresh token was rejected; reconnect to Linear".into(),
                ))
            }
            Err(RefreshError::Failed(message)) => Err(ApiError::Request(format!(
                "Linear token refresh failed: {message}"
            ))),
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::auth::tests::api_key_credentials;
    use serde_json::json;
    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    pub fn linear(server: &MockServer, secrets: &std::path::Path) -> Linear {
        Linear {
            http: Client::new(),
            graphql_url: format!("{}/graphql", server.uri()),
            oauth: OAuthConfig {
                client_id: Some("client-1".into()),
                token_url: format!("{}/oauth/token", server.uri()),
                ..OAuthConfig::built()
            },
            credentials: CredentialStore::new(secrets.to_path_buf()),
        }
    }

    fn oauth_credentials(
        access: &str,
        refresh: Option<&str>,
        expires_in_minutes: Option<i64>,
    ) -> Credentials {
        Credentials {
            auth: Auth::OAuth {
                access_token: access.into(),
                refresh_token: refresh.map(str::to_string),
                expires_at: expires_in_minutes
                    .map(|minutes| Utc::now() + Duration::minutes(minutes)),
            },
            ..api_key_credentials("unused", "org-1")
        }
    }

    async fn viewer_ok(server: &MockServer, authorization: &str) {
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(header("authorization", authorization))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "data": { "ok": true } })),
            )
            .mount(server)
            .await;
    }

    async fn unauthorized(server: &MockServer, authorization: &str) {
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(header("authorization", authorization))
            .respond_with(
                ResponseTemplate::new(401).set_body_json(json!({ "errors": [{
                "message": "Authentication required, not authenticated",
                "extensions": { "code": "AUTHENTICATION_ERROR" }
            }] })),
            )
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn without_credentials_there_is_no_session() {
        let server = MockServer::start().await;
        let secrets = tempfile::tempdir().unwrap();
        let linear = linear(&server, secrets.path());
        assert_eq!(linear.session().await.err(), Some(ApiError::NotConnected));
    }

    #[tokio::test]
    async fn api_keys_and_long_lived_tokens_are_used_as_stored() {
        let server = MockServer::start().await;
        viewer_ok(&server, "lin_api_key").await;
        let secrets = tempfile::tempdir().unwrap();
        let linear = linear(&server, secrets.path());
        linear
            .credentials
            .save(&api_key_credentials("lin_api_key", "org-1"))
            .unwrap();
        let mut session = linear.session().await.unwrap();
        let data: Value = session.execute("query { ok }", json!({})).await.unwrap();
        assert_eq!(data, json!({ "ok": true }));

        viewer_ok(&server, "Bearer forever").await;
        linear
            .credentials
            .save(&oauth_credentials("forever", None, None))
            .unwrap();
        let mut session = linear.session().await.unwrap();
        let data: Value = session.execute("query { ok }", json!({})).await.unwrap();
        assert_eq!(data, json!({ "ok": true }));
    }

    #[tokio::test]
    async fn an_expiring_token_is_refreshed_and_the_rotated_refresh_token_persisted() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .and(body_string_contains("grant_type=refresh_token"))
            .and(body_string_contains("refresh_token=old-refresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "new-access", "refresh_token": "new-refresh", "expires_in": 86399
            })))
            .expect(1)
            .mount(&server)
            .await;
        viewer_ok(&server, "Bearer new-access").await;
        let secrets = tempfile::tempdir().unwrap();
        let linear = linear(&server, secrets.path());
        linear
            .credentials
            .save(&oauth_credentials(
                "old-access",
                Some("old-refresh"),
                Some(2),
            ))
            .unwrap();

        let mut session = linear.session().await.unwrap();
        let _: Value = session.execute("query { ok }", json!({})).await.unwrap();

        let stored = linear.credentials.load().unwrap().unwrap();
        let Auth::OAuth {
            access_token,
            refresh_token,
            expires_at,
        } = stored.auth
        else {
            panic!()
        };
        assert_eq!(access_token, "new-access");
        assert_eq!(refresh_token.as_deref(), Some("new-refresh"));
        assert!(expires_at.unwrap() - Utc::now() > Duration::hours(23));
        assert_eq!(stored.organization_id, "org-1");
    }

    #[tokio::test]
    async fn a_401_refreshes_once_and_retries() {
        let server = MockServer::start().await;
        unauthorized(&server, "Bearer revoked").await;
        viewer_ok(&server, "Bearer fresh").await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "fresh", "expires_in": 86399
            })))
            .expect(1)
            .mount(&server)
            .await;
        let secrets = tempfile::tempdir().unwrap();
        let linear = linear(&server, secrets.path());
        linear
            .credentials
            .save(&oauth_credentials("revoked", Some("keep"), Some(600)))
            .unwrap();

        let mut session = linear.session().await.unwrap();
        let data: Value = session.execute("query { ok }", json!({})).await.unwrap();

        assert_eq!(data, json!({ "ok": true }));
        let Auth::OAuth { refresh_token, .. } = linear.credentials.load().unwrap().unwrap().auth
        else {
            panic!()
        };
        assert_eq!(
            refresh_token.as_deref(),
            Some("keep"),
            "Linear did not rotate it, so the old one stays"
        );
    }

    #[tokio::test]
    async fn rejected_refresh_clears_credentials_for_reconnect() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(
                ResponseTemplate::new(400).set_body_json(json!({ "error": "invalid_grant" })),
            )
            .mount(&server)
            .await;
        let secrets = tempfile::tempdir().unwrap();
        let linear = linear(&server, secrets.path());
        linear
            .credentials
            .save(&oauth_credentials("old", Some("rejected"), Some(1)))
            .unwrap();

        assert_eq!(
            linear.session().await.err(),
            Some(ApiError::Reconnect(
                "Linear OAuth refresh token was rejected; reconnect to Linear".into()
            ))
        );
        assert_eq!(linear.credentials.load().unwrap(), None);
    }

    #[tokio::test]
    async fn an_unrefreshable_401_reports_unauthorized() {
        let server = MockServer::start().await;
        unauthorized(&server, "revoked-key").await;
        let secrets = tempfile::tempdir().unwrap();
        let linear = linear(&server, secrets.path());
        linear
            .credentials
            .save(&api_key_credentials("revoked-key", "org-1"))
            .unwrap();
        let mut session = linear.session().await.unwrap();
        assert_eq!(
            session.execute::<Value>("query { ok }", json!({})).await,
            Err(ApiError::Unauthorized)
        );
    }
}
