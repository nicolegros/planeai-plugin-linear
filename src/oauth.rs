use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use rand::RngExt;
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use url::Url;

include!(concat!(env!("OUT_DIR"), "/oauth_client.rs"));

pub const AUTHORIZE_URL: &str = "https://linear.app/oauth/authorize";
pub const TOKEN_URL: &str = "https://api.linear.app/oauth/token";
pub const SCOPES: &str = "read,write";
/// Jira's builtin plugin owns 19287.
pub const CALLBACK_ADDRESS: &str = "127.0.0.1:19297";
pub const REDIRECT_URI: &str = "http://localhost:19297/callback";
const CALLBACK_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_CALLBACK_REQUEST_LINE_BYTES: u64 = 8 * 1024;

#[derive(Debug, Clone)]
pub struct OAuthConfig {
    /// Absent in builds without `LINEAR_CLIENT_ID`; those offer only API keys.
    pub client_id: Option<String>,
    pub authorize_url: String,
    pub token_url: String,
    pub callback_address: String,
    pub redirect_uri: String,
}

impl OAuthConfig {
    pub fn built() -> Self {
        Self {
            client_id: BUILT_CLIENT_ID.map(str::to_string),
            authorize_url: AUTHORIZE_URL.to_string(),
            token_url: TOKEN_URL.to_string(),
            callback_address: CALLBACK_ADDRESS.to_string(),
            redirect_uri: REDIRECT_URI.to_string(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OAuthError {
    #[error("OAuth state mismatch")]
    StateMismatch,
    #[error("Linear authorization failed: {0}")]
    Provider(String),
    #[error("Timed out waiting for browser callback")]
    Timeout,
    #[error("OAuth callback failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("Linear token request failed: {0}")]
    Token(String),
}

pub fn generate_pkce() -> (String, String) {
    let bytes: Vec<u8> = (0..32).map(|_| rand::rng().random()).collect();
    let verifier = URL_SAFE_NO_PAD.encode(bytes);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

pub fn generate_state() -> String {
    let bytes: Vec<u8> = (0..16).map(|_| rand::rng().random()).collect();
    URL_SAFE_NO_PAD.encode(bytes)
}

impl OAuthConfig {
    pub fn authorization_url(&self, client_id: &str, challenge: &str, state: &str) -> Url {
        let mut url = Url::parse(&self.authorize_url).expect("authorize URL is valid");
        url.query_pairs_mut()
            .append_pair("client_id", client_id)
            .append_pair("redirect_uri", &self.redirect_uri)
            .append_pair("response_type", "code")
            .append_pair("scope", SCOPES)
            .append_pair("state", state)
            .append_pair("actor", "user")
            .append_pair("prompt", "consent")
            .append_pair("code_challenge", challenge)
            .append_pair("code_challenge_method", "S256");
        url
    }

    /// The browser capability opens only an authorize URL this plugin generated.
    pub fn validate_authorization_url(&self, value: &str) -> Result<(), String> {
        let invalid = || "invalid Linear authorization URL".to_string();
        let url = Url::parse(value).map_err(|_| invalid())?;
        let expected = Url::parse(&self.authorize_url).expect("authorize URL is valid");
        if url.origin() != expected.origin()
            || url.path() != expected.path()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err("the browser capability only opens Linear authorization URLs".to_string());
        }
        let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
        let is = |key: &str, expected: Option<&str>| query.get(key).map(String::as_str) == expected;
        if self.client_id.is_none()
            || !is("client_id", self.client_id.as_deref())
            || !is("redirect_uri", Some(&self.redirect_uri))
            || !is("response_type", Some("code"))
            || !is("scope", Some(SCOPES))
            || !is("actor", Some("user"))
            || !is("code_challenge_method", Some("S256"))
            || !query.contains_key("code_challenge")
            || !query.contains_key("state")
        {
            return Err(invalid());
        }
        Ok(())
    }
}

pub async fn wait_for_callback(
    listener: &TcpListener,
    expected_state: &str,
) -> Result<String, OAuthError> {
    loop {
        let (stream, _) = listener.accept().await?;
        let (reader, mut writer) = stream.into_split();
        let mut line = Vec::new();
        let read = tokio::time::timeout(
            CALLBACK_REQUEST_TIMEOUT,
            BufReader::new(reader)
                .take(MAX_CALLBACK_REQUEST_LINE_BYTES)
                .read_until(b'\n', &mut line),
        )
        .await;
        let line = match read {
            Ok(Ok(read)) if read > 0 && line.ends_with(b"\n") => String::from_utf8(line).ok(),
            Ok(Err(error)) => return Err(OAuthError::Io(error)),
            _ => None,
        };
        let query = line.as_deref().and_then(callback_query);
        let Some(query) = query else {
            respond(&mut writer, "400 Bad Request", "Invalid callback request.").await;
            continue;
        };
        if query.get("state").map(String::as_str) != Some(expected_state) {
            respond(
                &mut writer,
                "400 Bad Request",
                "Authentication failed: state mismatch.",
            )
            .await;
            return Err(OAuthError::StateMismatch);
        }
        if let Some(error) = query.get("error") {
            let description = query
                .get("error_description")
                .map(|description| format!(": {description}"))
                .unwrap_or_default();
            respond(
                &mut writer,
                "400 Bad Request",
                "Authentication failed. You can close this tab.",
            )
            .await;
            return Err(OAuthError::Provider(format!("{error}{description}")));
        }
        let Some(code) = query.get("code").cloned() else {
            respond(
                &mut writer,
                "400 Bad Request",
                "No authorization code received.",
            )
            .await;
            continue;
        };
        respond(
            &mut writer,
            "200 OK",
            "Linear is connected. You can close this tab.",
        )
        .await;
        return Ok(code);
    }
}

fn callback_query(request_line: &str) -> Option<HashMap<String, String>> {
    let mut parts = request_line.split_whitespace();
    let (Some("GET"), Some(path)) = (parts.next(), parts.next()) else {
        return None;
    };
    if !path.starts_with("/callback") {
        return None;
    }
    let url = Url::parse(&format!("http://localhost{path}")).ok()?;
    Some(url.query_pairs().into_owned().collect())
}

async fn respond(writer: &mut tokio::net::tcp::OwnedWriteHalf, status: &str, body: &str) {
    let _ = writer
        .write_all(
            format!(
                "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await;
}

/// An access token and, when Linear issued one, the refresh token that renews it.
#[derive(Debug, Clone, PartialEq)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
}

#[derive(Deserialize)]
struct TokenErrorResponse {
    error: Option<String>,
}

#[derive(Debug, PartialEq)]
pub enum RefreshError {
    /// Linear answered `invalid_grant`: the user must reconnect.
    Rejected,
    Failed(String),
}

pub async fn exchange_code(
    http: &Client,
    config: &OAuthConfig,
    client_id: &str,
    code: &str,
    verifier: &str,
) -> Result<Tokens, OAuthError> {
    let form = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", config.redirect_uri.as_str()),
        ("client_id", client_id),
        ("code_verifier", verifier),
    ];
    match token_request(http, &config.token_url, &form).await {
        Ok(tokens) => Ok(tokens),
        Err((_, message)) => Err(OAuthError::Token(message)),
    }
}

pub async fn refresh(
    http: &Client,
    config: &OAuthConfig,
    client_id: &str,
    refresh_token: &str,
) -> Result<Tokens, RefreshError> {
    let form = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", client_id),
    ];
    match token_request(http, &config.token_url, &form).await {
        Ok(tokens) => Ok(tokens),
        Err((Some("invalid_grant"), _)) => Err(RefreshError::Rejected),
        Err((_, message)) => Err(RefreshError::Failed(message)),
    }
}

async fn token_request(
    http: &Client,
    token_url: &str,
    form: &[(&str, &str)],
) -> Result<Tokens, (Option<&'static str>, String)> {
    let response = http
        .post(token_url)
        .form(form)
        .send()
        .await
        .map_err(|error| (None, error.to_string()))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| (None, error.to_string()))?;
    if !status.is_success() {
        let invalid_grant = status == StatusCode::BAD_REQUEST
            && serde_json::from_str::<TokenErrorResponse>(&body)
                .ok()
                .and_then(|error| error.error)
                .as_deref()
                == Some("invalid_grant");
        return Err((
            invalid_grant.then_some("invalid_grant"),
            format!("HTTP {status}"),
        ));
    }
    let token: TokenResponse = serde_json::from_str(&body)
        .map_err(|error| (None, format!("invalid token response: {error}")))?;
    Ok(Tokens {
        access_token: token.access_token,
        refresh_token: token.refresh_token,
        expires_at: token
            .expires_in
            .map(|seconds| Utc::now() + ChronoDuration::seconds(seconds)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    pub fn config(server: &str) -> OAuthConfig {
        OAuthConfig {
            client_id: Some("client-1".to_string()),
            authorize_url: AUTHORIZE_URL.to_string(),
            token_url: format!("{server}/oauth/token"),
            callback_address: "127.0.0.1:0".to_string(),
            redirect_uri: REDIRECT_URI.to_string(),
        }
    }

    #[test]
    fn pkce_is_url_safe_and_uses_s256() {
        let (verifier, challenge) = generate_pkce();
        assert_eq!(verifier.len(), 43);
        assert!(verifier
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert_eq!(
            challenge,
            URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
        );
    }

    #[test]
    fn authorization_url_requests_user_read_write_with_pkce() {
        let url = config("http://unused").authorization_url("client-1", "challenge", "state");
        assert_eq!(
            url.as_str(),
            "https://linear.app/oauth/authorize?client_id=client-1&redirect_uri=http%3A%2F%2Flocalhost%3A19297%2Fcallback&response_type=code&scope=read%2Cwrite&state=state&actor=user&prompt=consent&code_challenge=challenge&code_challenge_method=S256"
        );
    }

    #[test]
    fn browser_capability_accepts_only_generated_linear_authorization_urls() {
        let config = config("http://unused");
        let url = config.authorization_url("client-1", "challenge", "state");
        assert_eq!(config.validate_authorization_url(url.as_str()), Ok(()));
        assert!(config
            .validate_authorization_url("https://example.com/oauth/authorize?state=state")
            .is_err());
        assert!(config
            .validate_authorization_url("https://linear.app/oauth/authorize?state=state")
            .is_err());
        let foreign_client = config.authorization_url("someone-else", "challenge", "state");
        assert!(config
            .validate_authorization_url(foreign_client.as_str())
            .is_err());
        let without_client = OAuthConfig {
            client_id: None,
            ..config.clone()
        };
        assert!(without_client
            .validate_authorization_url(url.as_str())
            .is_err());
    }

    #[tokio::test]
    async fn callback_rejects_a_mismatched_state() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let wait = tokio::spawn(async move { wait_for_callback(&listener, "expected").await });
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        stream
            .write_all(b"GET /callback?code=abc&state=wrong HTTP/1.1\r\n\r\n")
            .await
            .unwrap();
        assert!(matches!(
            wait.await.unwrap(),
            Err(OAuthError::StateMismatch)
        ));
    }

    #[tokio::test]
    async fn callback_reports_a_provider_error() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let wait = tokio::spawn(async move { wait_for_callback(&listener, "s").await });
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        stream
            .write_all(
                b"GET /callback?error=access_denied&error_description=No&state=s HTTP/1.1\r\n\r\n",
            )
            .await
            .unwrap();
        assert_eq!(
            wait.await.unwrap().unwrap_err().to_string(),
            "Linear authorization failed: access_denied: No"
        );
    }

    #[tokio::test]
    async fn callback_ignores_malformed_requests_until_a_valid_callback_arrives() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let wait = tokio::spawn(async move { wait_for_callback(&listener, "expected").await });
        let mut malformed = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        malformed.write_all(b"not an HTTP request\n").await.unwrap();
        let mut valid = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        valid
            .write_all(b"GET /callback?code=abc&state=expected HTTP/1.1\r\n\r\n")
            .await
            .unwrap();
        assert_eq!(wait.await.unwrap().unwrap(), "abc");
    }

    #[tokio::test]
    async fn exchange_posts_a_pkce_form_without_a_client_secret() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .and(header("content-type", "application/x-www-form-urlencoded"))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains("code=the-code"))
            .and(body_string_contains("code_verifier=the-verifier"))
            .and(body_string_contains("client_id=client-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "access", "token_type": "Bearer", "expires_in": 86399,
                "scope": "read,write", "refresh_token": "refresh"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let tokens = exchange_code(
            &Client::new(),
            &config(&server.uri()),
            "client-1",
            "the-code",
            "the-verifier",
        )
        .await
        .unwrap();
        assert_eq!(tokens.access_token, "access");
        assert_eq!(tokens.refresh_token.as_deref(), Some("refresh"));
        let lifetime = tokens.expires_at.unwrap() - Utc::now();
        assert!(lifetime > ChronoDuration::hours(23) && lifetime <= ChronoDuration::seconds(86399));
        let body =
            String::from_utf8(server.received_requests().await.unwrap()[0].body.clone()).unwrap();
        assert!(!body.contains("client_secret"), "{body}");
    }

    #[tokio::test]
    async fn refresh_distinguishes_a_rejected_grant_from_a_failure() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_string_contains("refresh_token=dead"))
            .respond_with(ResponseTemplate::new(400).set_body_json(
                serde_json::json!({"error":"invalid_grant","error_description":"Invalid refresh token"}),
            ))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_string_contains("refresh_token=flaky"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let config = config(&server.uri());
        assert_eq!(
            refresh(&Client::new(), &config, "client-1", "dead").await,
            Err(RefreshError::Rejected)
        );
        assert_eq!(
            refresh(&Client::new(), &config, "client-1", "flaky").await,
            Err(RefreshError::Failed(
                "HTTP 503 Service Unavailable".to_string()
            ))
        );
    }
}
