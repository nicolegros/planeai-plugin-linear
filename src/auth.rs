use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs::OpenOptions;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

/// How requests to Linear authenticate.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Auth {
    /// A personal API key, sent as the raw `Authorization` header.
    ApiKey { key: String },
    /// An OAuth access token, sent as `Bearer`; renewed with the refresh token when Linear issued one.
    #[serde(rename = "oauth")]
    OAuth {
        access_token: String,
        refresh_token: Option<String>,
        expires_at: Option<DateTime<Utc>>,
    },
}

impl Auth {
    pub fn authorization(&self) -> String {
        match self {
            Self::ApiKey { key } => key.clone(),
            Self::OAuth { access_token, .. } => format!("Bearer {access_token}"),
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::ApiKey { .. } => "api_key",
            Self::OAuth { .. } => "oauth",
        }
    }
}

impl fmt::Debug for Auth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ApiKey { .. } => f.write_str("ApiKey { .. }"),
            Self::OAuth { expires_at, .. } => f
                .debug_struct("OAuth")
                .field("expires_at", expires_at)
                .finish_non_exhaustive(),
        }
    }
}

/// The connected workspace and how to reach it; stored in the secrets directory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Credentials {
    pub auth: Auth,
    pub organization_id: String,
    pub organization_name: String,
    pub url_key: String,
}

#[derive(Debug, Clone)]
pub struct CredentialStore {
    dir: PathBuf,
}

impl CredentialStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn path(&self) -> PathBuf {
        self.dir.join("credentials.json")
    }

    pub fn load(&self) -> Result<Option<Credentials>, String> {
        match std::fs::read(self.path()) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|error| format!("stored Linear credentials are unreadable: {error}")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!("failed to read Linear credentials: {error}")),
        }
    }

    /// Writes a 0600 temporary file and renames it over the credentials, so readers never see a partial file.
    pub fn save(&self, credentials: &Credentials) -> Result<(), String> {
        let content = serde_json::to_vec(credentials).expect("credentials serialize");
        let temporary = self.dir.join(format!(
            ".credentials-{}.tmp",
            crate::oauth::generate_state()
        ));
        let result = write_private(&temporary, &content)
            .and_then(|()| std::fs::rename(&temporary, self.path()));
        if let Err(error) = result {
            let _ = std::fs::remove_file(&temporary);
            return Err(format!("failed to save Linear credentials: {error}"));
        }
        Ok(())
    }

    pub fn clear(&self) -> Result<(), String> {
        match std::fs::remove_file(self.path()) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("failed to remove Linear credentials: {error}")),
        }
    }
}

fn write_private(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path)?;
    file.write_all(content)?;
    file.sync_all()
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn api_key_credentials(key: &str, organization_id: &str) -> Credentials {
        Credentials {
            auth: Auth::ApiKey {
                key: key.to_string(),
            },
            organization_id: organization_id.to_string(),
            organization_name: "Acme".to_string(),
            url_key: "acme".to_string(),
        }
    }

    #[test]
    fn api_keys_are_sent_raw_and_oauth_tokens_as_bearer() {
        assert_eq!(
            Auth::ApiKey {
                key: "lin_api_x".into()
            }
            .authorization(),
            "lin_api_x"
        );
        assert_eq!(
            Auth::OAuth {
                access_token: "token".into(),
                refresh_token: None,
                expires_at: None
            }
            .authorization(),
            "Bearer token"
        );
    }

    #[test]
    fn debug_output_never_contains_secrets() {
        let rendered = format!(
            "{:?}",
            Credentials {
                auth: Auth::OAuth {
                    access_token: "secret-access".into(),
                    refresh_token: Some("secret-refresh".into()),
                    expires_at: None
                },
                ..api_key_credentials("secret-key", "org")
            }
        );
        assert!(!rendered.contains("secret"), "{rendered}");
    }

    #[test]
    fn credentials_are_replaced_atomically_with_private_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialStore::new(dir.path().to_path_buf());
        assert_eq!(store.load().unwrap(), None);
        store.save(&api_key_credentials("first", "org-a")).unwrap();
        store.save(&api_key_credentials("second", "org-b")).unwrap();

        assert_eq!(
            store.load().unwrap(),
            Some(api_key_credentials("second", "org-b"))
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(store.path())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, vec!["credentials.json"]);
        store.clear().unwrap();
        store.clear().unwrap();
        assert_eq!(store.load().unwrap(), None);
    }

    #[test]
    fn stored_credentials_round_trip_their_tagged_auth() {
        let credentials = Credentials {
            auth: Auth::OAuth {
                access_token: "a".into(),
                refresh_token: Some("r".into()),
                expires_at: None,
            },
            ..api_key_credentials("k", "org")
        };
        assert_eq!(
            serde_json::to_value(&credentials).unwrap(),
            serde_json::json!({
                "auth": { "kind": "oauth", "access_token": "a", "refresh_token": "r", "expires_at": null },
                "organization_id": "org", "organization_name": "Acme", "url_key": "acme"
            })
        );
    }
}
