# Configuration

CodoSEO is configured through environment variables. This page lists every one the code reads. Empty values count as unset. The compose files and `deploy/.env.example` pass the common ones through.

Roles: **web** (`codoseo web`), **worker** (`codoseo worker`), **all** (`codoseo all`, which is web + worker + jobs + scheduler in one process) and **migrate** (`codoseo migrate`). The CLI commands (`crawl`, `check`, `robots`, `redirects`, `diff`, `mcp`) read none of these. "Cloud" means `CODOSEO_MODE=cloud`.

## Core

| Variable | Default | Roles | Cloud | Meaning |
|---|---|---|---|---|
| `DATABASE_URL` | none | web, worker, all, migrate | required | Postgres connection string. Every role exits at startup without it. In the two-container cloud layout the web container may use a separate login role (see [Postgres roles](operations.md#postgres-roles-for-a-shared-server)); the compose file maps `DATABASE_URL_WEB` onto the web container's `DATABASE_URL`. The app itself never reads `DATABASE_URL_WEB`. |
| `CODOSEO_MODE` | `selfhost` | web, worker, all, migrate | set to `cloud` | `selfhost` (also accepts `self-host`, `self_hosted`) or `cloud`. Cloud turns on plan limits, Turnstile, billing, the no-key MCP tier and the refusal to crawl private or internal addresses, and makes `BASE_URL`, `SECRET_KEY` and `SMTP_URL` mandatory. It also selects JSON logs. Any other value is an error. |
| `CODOSEO_BIND` | `0.0.0.0:8080` | web, all (worker and `healthcheck` also read it) | optional | Listen address of the web app (`host:port`). `codoseo web --bind` and `codoseo all --bind` override it. `codoseo healthcheck` takes its default port from it. |
| `BASE_URL` | `http://localhost:<port of CODOSEO_BIND>` | web, worker, all | required | Public address of the app. Used in magic links, OAuth callbacks, alert emails, and for the `Origin` check on every form post. `https` turns on the `Secure` cookie flag. Set it when running behind a reverse proxy. |
| `SECRET_KEY` | built-in development key (a warning is logged) | web, worker, all | required | Derives the key that encrypts stored channel secrets (webhook URLs, SMTP details). Use a long random value (`openssl rand -hex 32`) and keep it: changing it makes stored channel secrets unreadable. |
| `HOSTNAME` | `codoseo-worker` (worker) or `codoseo` (all) | worker, all | optional | Only used to name the worker in logs and the queue (`<hostname>-<pid>`). Docker sets it. |

## Email and sign-in

| Variable | Default | Roles | Cloud | Meaning |
|---|---|---|---|---|
| `SMTP_URL` | none | web, worker, all | required | Outgoing mail, for example `smtps://user:pass@smtp.example.com:465`. Unset in self-host mode, sign-in links and alerts are written to the log instead of sent. A value that does not parse stops the process at startup. |
| `MAIL_FROM` | `CodoSEO <hello@codoseo.com>` | web, worker, all | optional | The sender of every email. Change it when self-hosting. |
| `GITHUB_CLIENT_ID`, `GITHUB_CLIENT_SECRET` | none | web, all | optional | Enable "Continue with GitHub". Both are needed. The OAuth callback is `<BASE_URL>/auth/github/callback`. |
| `ADMIN_EMAILS` | none | web, worker, all | optional | Comma-separated emails that may open `/admin` in cloud mode. |

## Cloud only

These are read in every long-running role but only take effect in cloud mode (the web role for most of them).

| Variable | Default | Meaning |
|---|---|---|
| `TURNSTILE_SITE_KEY`, `TURNSTILE_SECRET` | none | Cloudflare Turnstile on the public audit form. Both are needed; without them the form has no challenge. |
| `TURNSTILE_VERIFY_URL` | `https://challenges.cloudflare.com/turnstile/v0/siteverify` | Verification endpoint. Meant for tests; leave unset. |
| `CLIENT_IP_HEADER` | `CF-Connecting-IP` | Request header that carries the visitor's address behind your proxy. Used for per-IP abuse limits. |
| `CODOSEO_BOT_IP` | none | Public IP that crawls come from, shown on the bot page so site owners can allow-list it. |
| `RANKORG_URL` | `https://rankorg.com` | Where RankOrg links point. |
| `MCP_ANON_DAILY_AUDITS` | `200` | Fresh audits that no-key MCP agents may start in any 24 hours, over all of them. Cached and joined audits do not count. A whole number, 0 or more. |
| `MCP_ANON_DAILY_EMAILS` | `200` | Start-monitoring emails the no-key MCP tool may send in any 24 hours. |
| `MCP_SHARED_CLIENTS` | `claude-user,chatgpt,openai-mcp` | Comma-separated, case-insensitive fragments of the `User-Agent` of hosted connectors that call from shared servers. Per-IP limits are not applied to them, since everyone behind the connector shares an address. |
| `DODO_API_KEY`, `DODO_WEBHOOK_SECRET`, `DODO_PRODUCT_PRO`, `DODO_PRODUCT_AGENCY` | none | Billing through Dodo Payments. All four are needed; with some but not all set, billing stays off and a warning is logged. `DODO_WEBHOOK_SECRET` is the `whsec_...` value from Dodo. |
| `DODO_ENV` | `test` | `test` or `live`; picks the Dodo API host. |
| `DODO_API_URL` | derived from `DODO_ENV` | Overrides the API URL. Must be `https` (`http` only for localhost). Meant for tests. |

In self-host mode the plan limits do not apply (no caps on sites, no API quota; crawls are bounded at 100,000 pages and 24 hours), there is no billing, and `/mcp` accepts keyed requests only.

## Jobs and scheduler

| Variable | Default | Roles | Meaning |
|---|---|---|---|
| `CODOSEO_SCHEDULER` | on | web, all | `off` stops this process from running the scheduler. The scheduler ticks once a minute: it enqueues due crawls, cleanup and inactivity emails. Several schedulers can run at once (due work is claimed with row locks), so this is only for turning it off on an extra web container. In the cloud layout the web container runs it. |
| `SCHEDULER_HEARTBEAT_URL` | none | web, all | A URL that gets a GET after every successful scheduler tick, for a dead-man's-switch monitor. An invalid URL is logged and ignored. |
| `RETENTION_DAYS_SELFHOST` | `365` | worker, all | Days of crawl history a self-hosted install keeps; the daily cleanup deletes older crawls. A whole number. |

## Logging, metrics and error reporting

| Variable | Default | Roles | Meaning |
|---|---|---|---|
| `CODOSEO_LOG_FORMAT` | `json` in cloud, `text` otherwise | web, worker, all, migrate | `json` or `text`. Logs go to stderr. |
| `RUST_LOG` | `info,sqlx=warn` | web, worker, all, migrate | Standard `tracing` filter, for example `debug` or `codoseo_web=debug,info`. |
| `CODOSEO_METRICS_BIND` | none (no metrics endpoint) | web, worker, all | Address for the internal Prometheus listener, for example `0.0.0.0:9090`. Serves `/metrics` only and never on the public port. Do not expose it publicly. |
| `SENTRY_DSN` | none | web, worker, all, migrate | Report panics and errors to Sentry. Nothing is sent without it. An invalid DSN is logged and ignored. |
| `SENTRY_ENVIRONMENT` | the value of `CODOSEO_MODE`, or `selfhost` | same | Environment name attached to Sentry events. |

## Not read by the app

`POSTGRES_PASSWORD`, `CODOSEO_VERSION`, `CODOSEO_PORT` and `CODOSEO_IMAGE` appear in the compose files and `.env.example` only. Compose uses them to build the image name, the port mapping and the database password.
