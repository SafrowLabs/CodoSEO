//! Web configuration, read from the environment (spec section 11).

use std::net::SocketAddr;

use codoseo_core::plan::Plan;
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

/// Cloudflare Turnstile keys, plus the verify endpoint so tests can point it at a fake.
#[derive(Debug, Clone)]
pub struct TurnstileConfig {
    pub site_key: String,
    pub secret: String,
    pub verify_url: Url,
}

/// Dodo Payments billing (cloud only). All four keys are needed; with any missing, billing is
/// off: the billing pages say so and checkout is disabled.
#[derive(Debug, Clone)]
pub struct DodoConfig {
    /// Bearer token for the Dodo API (`DODO_API_KEY`).
    pub api_key: String,
    /// `whsec_<base64 key>` the webhooks are signed with (`DODO_WEBHOOK_SECRET`).
    pub webhook_secret: String,
    pub product_pro: String,
    pub product_agency: String,
    /// `https://test.dodopayments.com` or `https://live.dodopayments.com` (`DODO_ENV`), or
    /// `DODO_API_URL` when set (tests point it at a fake).
    pub api_url: Url,
}

impl DodoConfig {
    /// The plan a Dodo product sells, or `None` for a product that isn't ours.
    pub fn plan_for_product(&self, product_id: &str) -> Option<Plan> {
        if product_id == self.product_pro {
            Some(Plan::Pro)
        } else if product_id == self.product_agency {
            Some(Plan::Agency)
        } else {
            None
        }
    }

    /// The Dodo product that sells `plan`.
    pub fn product_for(&self, plan: Plan) -> Option<&str> {
        match plan {
            Plan::Pro => Some(&self.product_pro),
            Plan::Agency => Some(&self.product_agency),
            Plan::Free | Plan::SelfHosted => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub mode: Mode,
    /// The public address of the app; used for magic links, OAuth callbacks and the `Origin`
    /// check on every POST.
    pub base_url: Url,
    pub bind: SocketAddr,
    /// Keys the channel-target encryption (`codoseo_notify::ChannelKey`).
    pub secret_key: String,
    pub smtp_url: Option<String>,
    /// The sender of every email (`MAIL_FROM`).
    pub mail_from: String,
    pub github: Option<GithubConfig>,
    /// The fixed address cloud crawls come from, listed on the bot page (`CODOSEO_BOT_IP`).
    pub bot_ip: Option<String>,
    /// Turnstile on the audit form: `TURNSTILE_SITE_KEY` and `TURNSTILE_SECRET`. Cloud only.
    pub turnstile: Option<TurnstileConfig>,
    /// The request header carrying the visitor's address behind the cloud's proxy
    /// (`CLIENT_IP_HEADER`, default `CF-Connecting-IP`). Read in cloud mode only.
    pub client_ip_header: String,
    /// Who may open `/admin` in the cloud: canonical emails from `ADMIN_EMAILS`.
    pub admin_emails: Vec<String>,
    /// Where RankOrg links go (`RANKORG_URL`).
    pub rankorg_url: Url,
    /// Billing through Dodo Payments; `None` in self-hosted mode or when keys are missing.
    pub billing: Option<DodoConfig>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{0} is not set")]
    Missing(&'static str),
    #[error("{name} is invalid: {reason}")]
    Invalid { name: &'static str, reason: String },
}

const DEFAULT_BIND: &str = "0.0.0.0:8080";
/// The default `MAIL_FROM`; shared with the worker, which sends the alert and digest mail.
pub const DEFAULT_MAIL_FROM: &str = "CodoSEO <hello@codoseo.com>";

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

        let turnstile = match (get("TURNSTILE_SITE_KEY"), get("TURNSTILE_SECRET")) {
            (Some(site_key), Some(secret)) if mode == Mode::Cloud => {
                let verify = get("TURNSTILE_VERIFY_URL").unwrap_or_else(|| {
                    "https://challenges.cloudflare.com/turnstile/v0/siteverify".to_owned()
                });
                Some(TurnstileConfig {
                    site_key,
                    secret,
                    verify_url: Url::parse(&verify).map_err(|e| ConfigError::Invalid {
                        name: "TURNSTILE_VERIFY_URL",
                        reason: e.to_string(),
                    })?,
                })
            }
            _ => None,
        };

        let admin_emails = get("ADMIN_EMAILS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|e| !e.is_empty())
            .map(crate::auth::email::canonical)
            .collect();
        let rankorg_url =
            Url::parse(&get("RANKORG_URL").unwrap_or_else(|| "https://rankorg.com".to_owned()))
                .map_err(|e| ConfigError::Invalid {
                    name: "RANKORG_URL",
                    reason: e.to_string(),
                })?;

        let billing = dodo_from(&get, mode)?;

        Ok(Config {
            mode,
            base_url,
            bind,
            secret_key,
            smtp_url: get("SMTP_URL"),
            mail_from: get("MAIL_FROM").unwrap_or_else(|| DEFAULT_MAIL_FROM.to_owned()),
            github,
            bot_ip: get("CODOSEO_BOT_IP"),
            turnstile,
            client_ip_header: get("CLIENT_IP_HEADER")
                .unwrap_or_else(|| "CF-Connecting-IP".to_owned()),
            admin_emails,
            rankorg_url,
            billing,
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

fn dodo_from(
    get: &impl Fn(&str) -> Option<String>,
    mode: Mode,
) -> Result<Option<DodoConfig>, ConfigError> {
    use base64::Engine as _;

    let (Some(api_key), Some(webhook_secret), Some(product_pro), Some(product_agency)) = (
        get("DODO_API_KEY"),
        get("DODO_WEBHOOK_SECRET"),
        get("DODO_PRODUCT_PRO"),
        get("DODO_PRODUCT_AGENCY"),
    ) else {
        return Ok(None);
    };
    if mode != Mode::Cloud {
        return Ok(None);
    }
    let invalid = |name, reason: &str| ConfigError::Invalid {
        name,
        reason: reason.to_owned(),
    };
    let key = webhook_secret
        .trim()
        .strip_prefix("whsec_")
        .ok_or_else(|| {
            invalid(
                "DODO_WEBHOOK_SECRET",
                "expected the whsec_ secret from Dodo",
            )
        })?;
    base64::engine::general_purpose::STANDARD
        .decode(key)
        .map_err(|_| invalid("DODO_WEBHOOK_SECRET", "the key after whsec_ is not base64"))?;
    let api_url = match get("DODO_API_URL") {
        Some(v) => v,
        None => match get("DODO_ENV").as_deref() {
            None | Some("test") => "https://test.dodopayments.com".to_owned(),
            Some("live") => "https://live.dodopayments.com".to_owned(),
            Some(other) => {
                return Err(ConfigError::Invalid {
                    name: "DODO_ENV",
                    reason: format!("expected test or live, got {other:?}"),
                });
            }
        },
    };
    Ok(Some(DodoConfig {
        api_key,
        webhook_secret: webhook_secret.trim().to_owned(),
        product_pro,
        product_agency,
        api_url: Url::parse(&api_url).map_err(|e| ConfigError::Invalid {
            name: "DODO_API_URL",
            reason: e.to_string(),
        })?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const CLOUD: [(&str, &str); 3] = [
        ("CODOSEO_MODE", "cloud"),
        ("BASE_URL", "https://codoseo.com"),
        ("SECRET_KEY", "k"),
    ];
    const DODO: [(&str, &str); 4] = [
        ("DODO_API_KEY", "key_1"),
        ("DODO_WEBHOOK_SECRET", "whsec_c2VjcmV0"),
        ("DODO_PRODUCT_PRO", "pdt_pro"),
        ("DODO_PRODUCT_AGENCY", "pdt_agency"),
    ];

    fn cloud_with(extra: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let all: Vec<_> = CLOUD.iter().chain(extra.iter()).copied().collect();
        cfg(&all)
    }

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
    fn turnstile_needs_both_keys_and_the_cloud() {
        let keys = [("TURNSTILE_SITE_KEY", "a"), ("TURNSTILE_SECRET", "b")];
        let cloud = [
            ("CODOSEO_MODE", "cloud"),
            ("BASE_URL", "https://codoseo.com"),
            ("SECRET_KEY", "k"),
        ];
        let both: Vec<_> = cloud.iter().chain(keys.iter()).copied().collect();
        let t = cfg(&both).unwrap().turnstile.expect("configured");
        assert_eq!(t.site_key, "a");
        assert!(t.verify_url.as_str().contains("challenges.cloudflare.com"));
        let one: Vec<_> = cloud.iter().chain(keys[..1].iter()).copied().collect();
        assert!(cfg(&one).unwrap().turnstile.is_none());
        // Self-hosted never shows Turnstile, even with keys set.
        assert!(cfg(&keys).unwrap().turnstile.is_none());
    }

    #[test]
    fn admins_are_matched_on_canonical_emails() {
        let c = cfg(&[("ADMIN_EMAILS", " Boss@Example.com , o.ther+x@gmail.com ,, ")]).unwrap();
        assert_eq!(c.admin_emails, ["boss@example.com", "other@gmail.com"]);
        assert!(cfg(&[]).unwrap().admin_emails.is_empty());
    }

    #[test]
    fn rankorg_has_a_default_and_rejects_nonsense() {
        assert_eq!(
            cfg(&[]).unwrap().rankorg_url.as_str(),
            "https://rankorg.com/"
        );
        assert!(cfg(&[("RANKORG_URL", "not a url")]).is_err());
    }

    #[test]
    fn the_client_ip_header_has_a_default() {
        assert_eq!(cfg(&[]).unwrap().client_ip_header, "CF-Connecting-IP");
        assert_eq!(
            cfg(&[("CLIENT_IP_HEADER", "X-Real-IP")])
                .unwrap()
                .client_ip_header,
            "X-Real-IP"
        );
    }

    #[test]
    fn mail_from_has_a_default() {
        assert_eq!(cfg(&[]).unwrap().mail_from, "CodoSEO <hello@codoseo.com>");
        assert_eq!(
            cfg(&[("MAIL_FROM", "Me <me@example.com>")])
                .unwrap()
                .mail_from,
            "Me <me@example.com>"
        );
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

    #[test]
    fn billing_needs_all_four_keys_and_the_cloud() {
        let d = cloud_with(&DODO).unwrap().billing.expect("configured");
        assert_eq!(d.api_key, "key_1");
        assert_eq!(d.plan_for_product("pdt_pro"), Some(Plan::Pro));
        assert_eq!(d.plan_for_product("pdt_agency"), Some(Plan::Agency));
        assert_eq!(d.plan_for_product("pdt_other"), None);
        assert_eq!(d.product_for(Plan::Pro), Some("pdt_pro"));
        assert_eq!(d.product_for(Plan::Free), None);
        for skip in 0..DODO.len() {
            let some: Vec<_> = DODO
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != skip)
                .map(|(_, kv)| *kv)
                .collect();
            assert!(
                cloud_with(&some).unwrap().billing.is_none(),
                "without {skip}"
            );
        }
        // Self-hosted never bills, even with every key set.
        assert!(cfg(&DODO).unwrap().billing.is_none());
    }

    #[test]
    fn dodo_runs_against_test_mode_unless_told_otherwise() {
        let url = |extra: &[(&str, &str)]| {
            let all: Vec<_> = DODO.iter().chain(extra.iter()).copied().collect();
            cloud_with(&all)
                .unwrap()
                .billing
                .unwrap()
                .api_url
                .to_string()
        };
        assert_eq!(url(&[]), "https://test.dodopayments.com/");
        assert_eq!(
            url(&[("DODO_ENV", "live")]),
            "https://live.dodopayments.com/"
        );
        assert_eq!(
            url(&[
                ("DODO_ENV", "live"),
                ("DODO_API_URL", "http://127.0.0.1:9/")
            ]),
            "http://127.0.0.1:9/"
        );
        let bad: Vec<_> = DODO
            .iter()
            .chain([("DODO_ENV", "prod")].iter())
            .copied()
            .collect();
        assert!(cloud_with(&bad).is_err());
    }

    #[test]
    fn the_webhook_secret_must_be_a_whsec_key() {
        let mut keys = DODO.to_vec();
        keys[1] = ("DODO_WEBHOOK_SECRET", "not-a-secret");
        assert!(cloud_with(&keys).is_err());
        keys[1] = ("DODO_WEBHOOK_SECRET", "whsec_%%%");
        assert!(cloud_with(&keys).is_err());
    }
}
