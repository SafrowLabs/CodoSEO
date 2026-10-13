//! The deploy webhook: a deploy pipeline asks for an immediate crawl of a site, which rechecks its
//! SEO checks and AI access against what just shipped. With an API key it is
//! `POST /api/v1/sites/{site}/deploy`; platforms that can't send headers (Vercel, Netlify) call
//! the site's secret URL `POST /api/v1/hooks/deploy/{secret}` instead, made and replaced with the
//! key. See [`codoseo_store::deploy_hooks`] for the idempotency window and the plan limits.

use codoseo_core::plan::PlanLimits;
use codoseo_store::accounts::Account;
use codoseo_store::crawls::{CrawlStatus, ManualWindow};
use codoseo_store::deploy_hooks::{self, DeployOutcome};
use serde::Serialize;
use time::OffsetDateTime;
use uuid::Uuid;

use super::auth::ApiCaller;
use super::error::AgentError;
use super::service::{AgentService, Reply};
use crate::auth::session::{hash, random_token};
use crate::crawl_policy::{limit_message, manual_priority};

/// Every deploy hook secret starts with this, so a leaked one is recognisable.
pub const SECRET_PREFIX: &str = "cdh_";
/// `cdh_` and the 43 characters of 32 random bytes.
const SECRET_LEN: usize = SECRET_PREFIX.len() + 43;
/// The path the secret URL lives under, after the API prefix.
pub const HOOK_PATH: &str = "/hooks/deploy/";

/// What a deploy call answers (`202`).
#[derive(Debug, Clone, Serialize)]
pub struct DeployQueued {
    pub site_id: Uuid,
    pub crawl_id: Uuid,
    pub number: i64,
    /// `queued`, `running`, or `done` for a recent crawl that already finished.
    pub status: &'static str,
    /// True when an existing crawl answers the call and nothing new was queued.
    pub deduplicated: bool,
}

/// A new secret URL, shown once.
#[derive(Debug, Clone, Serialize)]
pub struct DeployHookCreated {
    pub site_id: Uuid,
    pub url: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// Whether a site has a secret URL; never the URL itself, which is not kept.
#[derive(Debug, Clone, Serialize)]
pub struct DeployHookStatus {
    pub site_id: Uuid,
    pub active: bool,
    #[serde(with = "time::serde::rfc3339::option")]
    pub created_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_used_at: Option<OffsetDateTime>,
}

fn status_word(s: CrawlStatus) -> &'static str {
    match s {
        CrawlStatus::Queued => "queued",
        CrawlStatus::Running => "running",
        CrawlStatus::Done => "done",
        CrawlStatus::Failed => "failed",
    }
}

/// Whether `secret` has the shape of one we hand out; anything else is a 404 without a lookup.
pub fn is_well_formed(secret: &str) -> bool {
    secret.len() == SECRET_LEN
        && secret.starts_with(SECRET_PREFIX)
        && secret[SECRET_PREFIX.len()..]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn not_found() -> AgentError {
    AgentError::NotFound("No such deploy hook. It may have been replaced or removed.".to_owned())
}

impl AgentService<'_> {
    /// `POST /api/v1/sites/{site}/deploy`.
    pub async fn deploy(&self, caller: &ApiCaller, site: &str) -> Reply<DeployQueued> {
        self.metered(caller, async {
            let site = self.site(caller, site).await?;
            self.deploy_work(&caller.account, site.id, &site.domain)
                .await
        })
        .await
    }

    /// `POST /api/v1/hooks/deploy/{secret}`: an unknown secret is a 404 and costs nothing, a
    /// known one counts against its account's allowance like a keyed call.
    pub async fn deploy_by_secret(&self, secret: &str) -> Reply<DeployQueued> {
        let resolved = if is_well_formed(secret) {
            deploy_hooks::resolve(&self.state.pool, &hash(secret)).await
        } else {
            Ok(None)
        };
        let (site_id, domain, account) = match resolved {
            Ok(Some(found)) => found,
            Ok(None) => {
                return Reply {
                    quota: None,
                    outcome: Err(not_found()),
                };
            }
            Err(e) => {
                return Reply {
                    quota: None,
                    outcome: Err(e.into()),
                };
            }
        };
        self.metered_account(&account, self.deploy_work(&account, site_id, &domain))
            .await
    }

    async fn deploy_work(
        &self,
        account: &Account,
        site_id: Uuid,
        domain: &str,
    ) -> Result<DeployQueued, AgentError> {
        let allowance = PlanLimits::for_plan(account.plan).manual_crawls;
        let outcome = deploy_hooks::deploy_crawl(
            &self.state.pool,
            site_id,
            domain,
            manual_priority(account.plan),
            ManualWindow::for_allowance(allowance),
        )
        .await
        .map_err(|e| match e {
            sqlx::Error::RowNotFound => AgentError::site_not_found(),
            e => e.into(),
        })?;
        match outcome {
            DeployOutcome::Queued { id, number } => Ok(DeployQueued {
                site_id,
                crawl_id: id,
                number,
                status: "queued",
                deduplicated: false,
            }),
            DeployOutcome::Existing { id, number, status } => Ok(DeployQueued {
                site_id,
                crawl_id: id,
                number,
                status: status_word(status),
                deduplicated: true,
            }),
            DeployOutcome::LimitReached { frees_at } => Err(AgentError::PlanLimit(limit_message(
                account.plan,
                allowance,
                frees_at - OffsetDateTime::now_utc(),
            ))),
        }
    }

    /// `POST /api/v1/sites/{site}/deploy-hook`: a new secret URL, replacing the site's old one.
    pub async fn create_deploy_hook(
        &self,
        caller: &ApiCaller,
        site: &str,
    ) -> Reply<DeployHookCreated> {
        self.metered(caller, async {
            let site = self.site(caller, site).await?;
            let secret = format!("{SECRET_PREFIX}{}", random_token());
            deploy_hooks::set(&self.state.pool, site.id, &hash(&secret)).await?;
            let url = self
                .state
                .config
                .base_url
                .join(&format!(
                    "{}{HOOK_PATH}{secret}",
                    &crate::routes::api::PREFIX[1..]
                ))
                .map_err(|e| AgentError::Internal(e.to_string()))?;
            Ok(DeployHookCreated {
                site_id: site.id,
                url: url.to_string(),
                created_at: OffsetDateTime::now_utc(),
            })
        })
        .await
    }

    /// `GET /api/v1/sites/{site}/deploy-hook`.
    pub async fn deploy_hook_status(
        &self,
        caller: &ApiCaller,
        site: &str,
    ) -> Reply<DeployHookStatus> {
        self.metered(caller, async {
            let site = self.site(caller, site).await?;
            let status = deploy_hooks::status(&self.state.pool, site.id).await?;
            Ok(DeployHookStatus {
                site_id: site.id,
                active: status.is_some(),
                created_at: status.map(|s| s.0),
                last_used_at: status.and_then(|s| s.1),
            })
        })
        .await
    }

    /// `DELETE /api/v1/sites/{site}/deploy-hook`: the secret URL stops working.
    pub async fn delete_deploy_hook(
        &self,
        caller: &ApiCaller,
        site: &str,
    ) -> Reply<DeployHookStatus> {
        self.metered(caller, async {
            let site = self.site(caller, site).await?;
            deploy_hooks::delete(&self.state.pool, site.id).await?;
            Ok(DeployHookStatus {
                site_id: site.id,
                active: false,
                created_at: None,
                last_used_at: None,
            })
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_secrets_of_our_shape_are_looked_up() {
        let secret = format!("{SECRET_PREFIX}{}", random_token());
        assert!(is_well_formed(&secret));
        assert!(!is_well_formed("cdh_short"));
        assert!(!is_well_formed(&secret.replacen("cdh_", "cdo_", 1)));
        assert!(!is_well_formed(&format!(
            "{}!",
            &secret[..secret.len() - 1]
        )));
    }
}
