//! Web configuration, read from the environment (spec section 11).

use std::net::SocketAddr;

use url::Url;

/// `CODOSEO_MODE`: self-hosted (the default) or the codoseo.com cloud.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    SelfHost,
    Cloud,
}

/// GitHub OAuth app credentials, plus the endpoints so tests can point them at a fake.
#[derive(Debug, Clone)]
pub struct GithubConfig {
    pub client_id: String,
    pub client_secret: String,
    pub authorize_url: Url,
    pub token_url: Url,
    pub api_url: Url,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub mode: Mode,
    /// The public address of the app; used for magic links, OAuth callbacks and the `Origin`
    /// check on every POST.
    pub base_url: Url,
    pub bind: SocketAddr,
    /// Signs nothing yet; reserved for M7's channel encryption key derivation.
    pub secret_key: String,
    pub smtp_url: Option<String>,
    pub github: Option<GithubConfig>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{0} is not set")]
    Missing(&'static str),
    #[error("{name} is invalid: {reason}")]
    Invalid { name: &'static str, reason: String },
}

const DEFAULT_BIND: &str = "0.0.0.0:8080";

impl Config {
    pub fn from_env() -> Result<Config, ConfigError> {
        Config::from_lookup(|k| std::env::var(k).ok())
    }

    /// Reads configuration through `lookup`, so tests can pass a map instead of the process
    /// environment.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Config, ConfigError> {
        let get = |k: &str| lookup(k).filter(|v| !v.trim().is_empty());

        let mode = match get("CODOSEO_MODE").as_deref() {
            None | Some("selfhost") | Some("self-host") | Some("self_hosted") => Mode::SelfHost,
            Some("cloud") => Mode::Cloud,
            Some(other) => {
                return Err(ConfigError::Invalid {
                    name: "CODOSEO_MODE",
                    reason: format!("expected selfhost or cloud, got {other:?}"),
                });
            }
        };

        let bind_text = get("CODOSEO_BIND").unwrap_or_else(|| DEFAULT_BIND.to_owned());
        let bind: SocketAddr = bind_text.parse().map_err(|e| ConfigError::Invalid {
            name: "CODOSEO_BIND",
            reason: format!("{e}"),
        })?;

        let base_url = match get("BASE_URL") {
            Some(v) => Url::parse(&v).map_err(|e| ConfigError::Invalid {
                name: "BASE_URL",
                reason: e.to_string(),
            })?,
            None if mode == Mode::Cloud => return Err(ConfigError::Missing("BASE_URL")),
            None => Url::parse(&format!("http://localhost:{}", bind.port()))
                .expect("localhost url is valid"),
        };

        let secret_key = match get("SECRET_KEY") {
            Some(v) => v,
            None if mode == Mode::Cloud => return Err(ConfigError::Missing("SECRET_KEY")),
            None => "codoseo-selfhost-dev-key".to_owned(),
        };

        let github = match (get("GITHUB_CLIENT_ID"), get("GITHUB_CLIENT_SECRET")) {
            (Some(client_id), Some(client_secret)) => Some(GithubConfig {
                client_id,
                client_secret,
                authorize_url: Url::parse("https://github.com/login/oauth/authorize")
                    .expect("static url"),
                token_url: Url::parse("https://github.com/login/oauth/access_token")
                    .expect("static url"),
                api_url: Url::parse("https://api.github.com/").expect("static url"),
            }),
            _ => None,
        };

        Ok(Config {
            mode,
            base_url,
            bind,
            secret_key,
            smtp_url: get("SMTP_URL"),
            github,
        })
    }

    /// Cookies get `Secure` whenever the app is served over https.
    pub fn secure_cookies(&self) -> bool {
        self.base_url.scheme() == "https"
    }

    /// The origin every POST must come from (`scheme://host[:port]`).
    pub fn origin(&self) -> String {
        self.base_url.origin().ascii_serialization()
    }

    /// A config for tests and local tools: self-hosted, `http://localhost:8080`.
    pub fn for_tests() -> Config {
        Config::from_lookup(|_| None).expect("defaults are valid")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn cfg(pairs: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Config::from_lookup(|k| map.get(k).cloned())
    }

    #[test]
    fn self_hosted_defaults() {
        let c = cfg(&[]).unwrap();
        assert_eq!(c.mode, Mode::SelfHost);
        assert_eq!(c.base_url.as_str(), "http://localhost:8080/");
        assert!(!c.secure_cookies());
        assert!(c.github.is_none());
    }

    #[test]
    fn cloud_requires_base_url_and_secret() {
        assert!(matches!(
            cfg(&[("CODOSEO_MODE", "cloud")]),
            Err(ConfigError::Missing("BASE_URL"))
        ));
        assert!(matches!(
            cfg(&[
                ("CODOSEO_MODE", "cloud"),
                ("BASE_URL", "https://codoseo.com")
            ]),
            Err(ConfigError::Missing("SECRET_KEY"))
        ));
        let c = cfg(&[
            ("CODOSEO_MODE", "cloud"),
            ("BASE_URL", "https://codoseo.com"),
            ("SECRET_KEY", "k"),
        ])
        .unwrap();
        assert!(c.secure_cookies());
        assert_eq!(c.origin(), "https://codoseo.com");
    }

    #[test]
    fn unknown_mode_is_rejected() {
        assert!(cfg(&[("CODOSEO_MODE", "nope")]).is_err());
    }

    #[test]
    fn github_needs_both_keys() {
        assert!(cfg(&[("GITHUB_CLIENT_ID", "a")]).unwrap().github.is_none());
        assert!(
            cfg(&[("GITHUB_CLIENT_ID", "a"), ("GITHUB_CLIENT_SECRET", "b")])
                .unwrap()
                .github
                .is_some()
        );
    }
}
