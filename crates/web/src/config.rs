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
#[derive(Clone)]
pub struct GithubConfig {
    pub client_id: String,
    pub client_secret: String,
    pub authorize_url: Url,
    pub token_url: Url,
    pub api_url: Url,
}

impl std::fmt::Debug for GithubConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GithubConfig")
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .field("authorize_url", &self.authorize_url.as_str())
            .field("token_url", &self.token_url.as_str())
            .field("api_url", &self.api_url.as_str())
            .finish()
    }
}

/// Cloudflare Turnstile keys, plus the verify endpoint so tests can point it at a fake.
#[derive(Clone)]
pub struct TurnstileConfig {
    pub site_key: String,
    pub secret: String,
    pub verify_url: Url,
}

impl std::fmt::Debug for TurnstileConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TurnstileConfig")
            .field("site_key", &self.site_key)
            .field("secret", &"<redacted>")
            .field("verify_url", &self.verify_url.as_str())
            .finish()
    }
}

/// Dodo Payments billing (cloud only). All four keys are needed; with any missing, billing is
/// off: the billing pages say so and checkout is disabled.
#[derive(Clone)]
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

/// Written by hand so a stray `{:?}` of the config can't put the API key or the webhook
/// secret in a log.
impl std::fmt::Debug for DodoConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DodoConfig")
            .field("api_key", &"<redacted>")
            .field("webhook_secret", &"<redacted>")
            .field("product_pro", &self.product_pro)
            .field("product_agency", &self.product_agency)
            .field("api_url", &self.api_url.as_str())
            .finish()
    }
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

#[derive(Clone)]
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
    /// The no-key MCP tier (cloud only; self-hosted refuses it).
    pub mcp: McpAnonConfig,
}

/// The `User-Agent`s of the hosted connectors that call from shared servers.
pub const DEFAULT_SHARED_CLIENTS: &str = "claude-user,chatgpt,openai-mcp";

/// The no-key MCP tier's limits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpAnonConfig {
    /// Fresh audits agents may start in any 24 hours, over all of them (`MCP_ANON_DAILY_AUDITS`,
    /// default 200). Cached and joined audits don't count.
    pub daily_audits: i64,
    /// Start-monitoring emails the tool may send in any 24 hours (`MCP_ANON_DAILY_EMAILS`,
    /// default 200).
    pub daily_emails: i64,
    /// Lowercase fragments of the `User-Agent` of clients that connect from shared servers
    /// (`MCP_SHARED_CLIENTS`, comma separated, matched case-insensitively). Per-IP limits don't
    /// apply to them, since everyone behind the connector shares an address.
    pub shared_clients: Vec<String>,
}

impl McpAnonConfig {
    /// Whether a request with this `User-Agent` comes from a shared connector.
    pub fn is_shared_client(&self, user_agent: Option<&str>) -> bool {
        let Some(ua) = user_agent else { return false };
        let ua = ua.to_ascii_lowercase();
        self.shared_clients.iter().any(|name| ua.contains(name))
    }
}

/// Written by hand so a stray `{:?}` of the config can't put `SECRET_KEY` or the SMTP password
/// in a log.
impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("mode", &self.mode)
            .field("base_url", &self.base_url.as_str())
            .field("bind", &self.bind)
            .field("secret_key", &"<redacted>")
            .field("smtp_url", &self.smtp_url.as_deref().map(redact_url))
            .field("mail_from", &self.mail_from)
            .field("github", &self.github)
            .field("bot_ip", &self.bot_ip)
            .field("turnstile", &self.turnstile)
            .field("client_ip_header", &self.client_ip_header)
            .field("admin_emails", &self.admin_emails)
            .field("rankorg_url", &self.rankorg_url.as_str())
            .field("billing", &self.billing)
            .field("mcp", &self.mcp)
            .finish()
    }
}

/// `url` with its password hidden; text that isn't a URL is hidden whole.
fn redact_url(url: &str) -> String {
    match Url::parse(url) {
        Ok(mut u) => {
            if u.password().is_some() {
                let _ = u.set_password(Some("REDACTED"));
            }
            u.to_string()
        }
        Err(_) => "<redacted>".to_owned(),
    }
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

        // The cloud's login mail, alert mail and digests all go through this; without it they
        // would only be logged and nobody would ever get one.
        let smtp_url = get("SMTP_URL");
        if mode == Mode::Cloud && smtp_url.is_none() {
            return Err(ConfigError::Missing("SMTP_URL"));
        }

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
        let mcp = McpAnonConfig {
            daily_audits: count_from(&get, "MCP_ANON_DAILY_AUDITS", 200)?,
            daily_emails: count_from(&get, "MCP_ANON_DAILY_EMAILS", 200)?,
            shared_clients: get("MCP_SHARED_CLIENTS")
                .unwrap_or_else(|| DEFAULT_SHARED_CLIENTS.to_owned())
                .split(',')
                .map(|name| name.trim().to_ascii_lowercase())
                .filter(|name| !name.is_empty())
                .collect(),
        };

        Ok(Config {
            mode,
            base_url,
            bind,
            secret_key,
            smtp_url,
            mail_from: get("MAIL_FROM").unwrap_or_else(|| DEFAULT_MAIL_FROM.to_owned()),
            github,
            bot_ip: get("CODOSEO_BOT_IP"),
            turnstile,
            client_ip_header: get("CLIENT_IP_HEADER")
                .unwrap_or_else(|| "CF-Connecting-IP".to_owned()),
            admin_emails,
            rankorg_url,
            billing,
            mcp,
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

/// A whole number of at least 0 from `name`, or `default` when it isn't set.
fn count_from(
    get: &impl Fn(&str) -> Option<String>,
    name: &'static str,
    default: i64,
) -> Result<i64, ConfigError> {
    let Some(raw) = get(name) else {
        return Ok(default);
    };
    raw.trim()
        .parse::<i64>()
        .ok()
        .filter(|n| *n >= 0)
        .ok_or_else(|| ConfigError::Invalid {
            name,
            reason: format!("expected a whole number of 0 or more, got {raw:?}"),
        })
}

const DODO_KEYS: [&str; 4] = [
    "DODO_API_KEY",
    "DODO_WEBHOOK_SECRET",
    "DODO_PRODUCT_PRO",
    "DODO_PRODUCT_AGENCY",
];

/// The Dodo keys that are missing when some, but not all, of the four are set: a half-filled
/// config silently turns billing off, which is worth a startup warning.
fn dodo_missing_keys(get: &impl Fn(&str) -> Option<String>) -> Option<Vec<&'static str>> {
    let missing: Vec<&'static str> = DODO_KEYS.into_iter().filter(|k| get(k).is_none()).collect();
    (!missing.is_empty() && missing.len() < DODO_KEYS.len()).then_some(missing)
}

/// Dodo's API carries the bearer key, so it is https only; a local address is allowed for
/// tests and local fakes.
fn dodo_url_allowed(url: &Url) -> bool {
    url.scheme() == "https"
        || (url.scheme() == "http"
            && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")))
}

fn dodo_from(
    get: &impl Fn(&str) -> Option<String>,
    mode: Mode,
) -> Result<Option<DodoConfig>, ConfigError> {
    if mode != Mode::Cloud {
        return Ok(None);
    }
    if let Some(missing) = dodo_missing_keys(get) {
        tracing::warn!(
            missing = %missing.join(", "),
            "Dodo billing is off: some of the four DODO_* keys are set but not all"
        );
    }
    // Values pasted from a dashboard often carry a trailing newline or space.
    let get = |k: &str| get(k).map(|v| v.trim().to_owned());
    let (Some(api_key), Some(webhook_secret), Some(product_pro), Some(product_agency)) = (
        get("DODO_API_KEY"),
        get("DODO_WEBHOOK_SECRET"),
        get("DODO_PRODUCT_PRO"),
        get("DODO_PRODUCT_AGENCY"),
    ) else {
        return Ok(None);
    };
    let invalid = |name, reason: &str| ConfigError::Invalid {
        name,
        reason: reason.to_owned(),
    };
    // The same check the webhook verifier makes, so a bad key fails at startup, not on the
    // first delivery.
    crate::billing::dodo::key(&webhook_secret).map_err(|_| {
        invalid(
            "DODO_WEBHOOK_SECRET",
            "expected the whsec_ secret from Dodo (base64, at least 16 bytes)",
        )
    })?;
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
    let api_url = Url::parse(&api_url).map_err(|e| ConfigError::Invalid {
        name: "DODO_API_URL",
        reason: e.to_string(),
    })?;
    if !dodo_url_allowed(&api_url) {
        return Err(invalid(
            "DODO_API_URL",
            "must be https (http only for localhost or 127.0.0.1)",
        ));
    }
    Ok(Some(DodoConfig {
        api_key,
        webhook_secret,
        product_pro,
        product_agency,
        api_url,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const CLOUD: [(&str, &str); 4] = [
        ("CODOSEO_MODE", "cloud"),
        ("BASE_URL", "https://codoseo.com"),
        ("SECRET_KEY", "k"),
        ("SMTP_URL", "smtp://127.0.0.1:2525"),
    ];
    const DODO: [(&str, &str); 4] = [
        ("DODO_API_KEY", "key_1"),
        ("DODO_WEBHOOK_SECRET", "whsec_c2VjcmV0LTAxMjM0NTY3ODlhYg=="),
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
        let c = cloud_with(&[]).unwrap();
        assert!(c.secure_cookies());
        assert_eq!(c.origin(), "https://codoseo.com");
    }

    #[test]
    fn cloud_requires_smtp_but_self_hosted_may_log_mail() {
        let no_smtp = [
            ("CODOSEO_MODE", "cloud"),
            ("BASE_URL", "https://codoseo.com"),
            ("SECRET_KEY", "k"),
        ];
        assert!(matches!(
            cfg(&no_smtp),
            Err(ConfigError::Missing("SMTP_URL"))
        ));
        let blank: Vec<_> = no_smtp
            .iter()
            .chain([("SMTP_URL", "  ")].iter())
            .copied()
            .collect();
        assert!(matches!(cfg(&blank), Err(ConfigError::Missing("SMTP_URL"))));
        assert!(cfg(&[]).unwrap().smtp_url.is_none());
    }

    #[test]
    fn debug_hides_the_secret_key_and_the_smtp_password() {
        let c = cloud_with(&[
            ("SECRET_KEY", "hunter2-the-key"),
            ("SMTP_URL", "smtps://mailer:p4ssw0rd-x@mail.example.com:465"),
            ("GITHUB_CLIENT_ID", "gh-id"),
            ("GITHUB_CLIENT_SECRET", "gh-secret-value"),
        ])
        .unwrap();
        let shown = format!("{c:?}");
        for secret in ["hunter2-the-key", "p4ssw0rd-x", "gh-secret-value"] {
            assert!(!shown.contains(secret), "{secret} leaked: {shown}");
        }
        assert!(shown.contains("mail.example.com"), "{shown}");
        assert!(shown.contains("mailer"), "{shown}");
        // A value that isn't a URL is hidden whole.
        let odd = cloud_with(&[("SMTP_URL", "not a url p4ssw0rd-y")]).unwrap();
        assert!(!format!("{odd:?}").contains("p4ssw0rd-y"));
    }

    #[test]
    fn the_no_key_tier_has_defaults_and_reads_its_limits() {
        let d = cfg(&[]).unwrap().mcp;
        assert_eq!((d.daily_audits, d.daily_emails), (200, 200));
        assert_eq!(d.shared_clients, ["claude-user", "chatgpt", "openai-mcp"]);

        let c = cfg(&[
            ("MCP_ANON_DAILY_AUDITS", " 50 "),
            ("MCP_ANON_DAILY_EMAILS", "0"),
            ("MCP_SHARED_CLIENTS", " Claude-User , ,Cursor "),
        ])
        .unwrap()
        .mcp;
        assert_eq!((c.daily_audits, c.daily_emails), (50, 0));
        assert_eq!(c.shared_clients, ["claude-user", "cursor"]);

        for bad in ["many", "-1", "1.5"] {
            assert!(cfg(&[("MCP_ANON_DAILY_AUDITS", bad)]).is_err(), "{bad}");
            assert!(cfg(&[("MCP_ANON_DAILY_EMAILS", bad)]).is_err(), "{bad}");
        }
        // A blank value is "not set".
        assert_eq!(
            cfg(&[("MCP_ANON_DAILY_AUDITS", " ")])
                .unwrap()
                .mcp
                .daily_audits,
            200
        );
    }

    #[test]
    fn shared_clients_are_matched_by_user_agent_fragment_ignoring_case() {
        let mcp = cfg(&[]).unwrap().mcp;
        assert!(mcp.is_shared_client(Some("Claude-User/1.0 (+https://anthropic.com)")));
        assert!(mcp.is_shared_client(Some("Mozilla/5.0 ChatGPT-User/1.0")));
        assert!(mcp.is_shared_client(Some("openai-mcp/1.2")));
        assert!(!mcp.is_shared_client(Some("claude-code/2.0 (cli)")));
        assert!(!mcp.is_shared_client(Some("node")));
        assert!(!mcp.is_shared_client(None));
        let none = cfg(&[("MCP_SHARED_CLIENTS", ",")]).unwrap().mcp;
        assert!(!none.is_shared_client(Some("claude-user")));
    }

    #[test]
    fn unknown_mode_is_rejected() {
        assert!(cfg(&[("CODOSEO_MODE", "nope")]).is_err());
    }

    #[test]
    fn turnstile_needs_both_keys_and_the_cloud() {
        let keys = [("TURNSTILE_SITE_KEY", "a"), ("TURNSTILE_SECRET", "b")];
        let cloud = CLOUD;
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
    fn dodo_values_are_trimmed() {
        let spaced = [
            ("DODO_API_KEY", " key_1 \n"),
            (
                "DODO_WEBHOOK_SECRET",
                " whsec_c2VjcmV0LTAxMjM0NTY3ODlhYg== ",
            ),
            ("DODO_PRODUCT_PRO", " pdt_pro "),
            ("DODO_PRODUCT_AGENCY", "\tpdt_agency"),
        ];
        let d = cloud_with(&spaced).unwrap().billing.expect("configured");
        assert_eq!(d.api_key, "key_1");
        assert_eq!(d.product_pro, "pdt_pro");
        assert_eq!(d.product_agency, "pdt_agency");
        assert_eq!(d.webhook_secret, "whsec_c2VjcmV0LTAxMjM0NTY3ODlhYg==");
    }

    #[test]
    fn a_partly_set_dodo_config_names_the_missing_keys() {
        let get = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        assert_eq!(
            dodo_missing_keys(&get(&[("DODO_API_KEY", "k"), ("DODO_PRODUCT_PRO", "p")])),
            Some(vec!["DODO_WEBHOOK_SECRET", "DODO_PRODUCT_AGENCY"])
        );
        // None set, or all set: nothing to warn about.
        assert_eq!(dodo_missing_keys(&get(&[])), None);
        assert_eq!(
            dodo_missing_keys(&get(&[
                ("DODO_API_KEY", "k"),
                ("DODO_WEBHOOK_SECRET", "s"),
                ("DODO_PRODUCT_PRO", "p"),
                ("DODO_PRODUCT_AGENCY", "a"),
            ])),
            None
        );
    }

    #[test]
    fn the_dodo_api_url_must_be_https_unless_it_is_local() {
        let url = |u: &str| {
            let all: Vec<_> = DODO
                .iter()
                .chain([("DODO_API_URL", u)].iter())
                .copied()
                .collect();
            cloud_with(&all)
        };
        assert!(url("https://dodo.example.com").is_ok());
        assert!(url("http://127.0.0.1:9/").is_ok());
        assert!(url("http://localhost:9/").is_ok());
        assert!(url("http://test.dodopayments.com").is_err());
        assert!(url("http://10.0.0.5/").is_err());
        assert!(url("ftp://127.0.0.1/").is_err());
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
        // An empty or tiny key would sign (and verify) anything cheaply.
        for short in ["whsec_", "whsec_YQ==", "whsec_c2VjcmV0LTAxMjM0NQ=="] {
            keys[1] = ("DODO_WEBHOOK_SECRET", short);
            assert!(cloud_with(&keys).is_err(), "{short}");
        }
        keys[1] = ("DODO_WEBHOOK_SECRET", "whsec_MDEyMzQ1Njc4OWFiY2RlZg==");
        assert!(cloud_with(&keys).unwrap().billing.is_some());
    }
}
