# Deploying on Coolify

Two ways to run CodoSEO on [Coolify](https://coolify.io): the one-click self-host template (web app, worker and scheduler in one container, with its own Postgres), and the two-container layout (separate web and worker against a Postgres you run yourself). Nothing here needs more than a domain and a Coolify server.

## Self-host template

`deploy/coolify/template.yml` is a Coolify service template: a `codoseo` container running `all`, and a `postgres:18-alpine` container with a persistent volume.

1. In Coolify, create a **Docker Compose** service (or add the template to your Coolify templates) and paste the contents of `deploy/coolify/template.yml`.
2. Coolify generates the secrets from the template's magic variables: the database password (`SERVICE_PASSWORD_POSTGRES`) and `SECRET_KEY` (`SERVICE_BASE64_64_SECRETKEY`). It also assigns a domain (`SERVICE_FQDN_CODOSEO_8080`) and routes it to container port 8080; `BASE_URL` is set from that domain. If `BASE_URL` comes out empty after the import, set it to the service's public URL by hand.
3. Optional settings appear as environment variables on the service: `SMTP_URL`, `MAIL_FROM`, `ADMIN_EMAILS`, `GITHUB_CLIENT_ID`, `GITHUB_CLIENT_SECRET`, `RETENTION_DAYS_SELFHOST`. See [configuration.md](configuration.md).
4. Deploy, open the domain and sign in. The first account becomes the owner.

The image is `ghcr.io/safrowlabs/codoseo:latest`. To pin a version, edit the tag in the compose file. The container applies migrations when it starts, so a redeploy with a new tag upgrades the database. Back up the Postgres volume (or run `pg_dump`) before upgrading; see [operations.md](operations.md#backups).

## Two containers against your own Postgres

`deploy/compose.cloud.yml` is the layout the hosted codoseo.com uses, and it works for any larger deployment:

| Service | Command | Purpose | Limits |
|---|---|---|---|
| `migrate` | `migrate` | One-shot: applies pending migrations, then exits. | none |
| `web` | `web` | The web app, REST API and MCP endpoint. Runs the scheduler. | 384 MB, 0.5 CPU |
| `worker` | `worker` | Claims crawls and background jobs from the Postgres queue. | 1.5 GB, 1.5 CPU |

Both `web` and `worker` depend on `migrate` completing successfully, so one deploy does the right thing: migrate, then (re)start both. There is no Postgres in this file; point `DATABASE_URL` at a database you run elsewhere.

### Steps

1. Create the database and roles. `deploy/postgres-role.sql` makes a `codoseo` role (worker and migrations) and a `codoseo_web` role (web, with a 5 s `statement_timeout`), and caps their connections at 15 together. Run it as a superuser; see [operations.md](operations.md#postgres-roles-for-a-shared-server) for how to run it.
2. Create a Docker Compose resource in Coolify from `deploy/compose.cloud.yml`.
3. Set the environment variables:
   - Required: `DATABASE_URL` (as the `codoseo` role), `BASE_URL` (the public `https://` address), `SECRET_KEY` (`openssl rand -hex 32`), `SMTP_URL`.
   - Recommended: `DATABASE_URL_WEB` (as `codoseo_web`; the web container falls back to `DATABASE_URL` when it is unset), `MAIL_FROM`, `ADMIN_EMAILS`, `CLIENT_IP_HEADER` if your proxy uses something other than `CF-Connecting-IP`, `CODOSEO_BOT_IP`.
   - Optional: the Turnstile, MCP limit, Dodo billing, Sentry and logging variables. [configuration.md](configuration.md) lists them all.
4. Attach your domain to the `web` service on port 8080. The compose file declares `expose` for 8080 and 9090 and publishes no host ports; Coolify's proxy reaches the container over the Docker network.
5. Deploy. `docker compose up -d` and a Coolify redeploy both run `migrate` first.

### Behaviour worth knowing

- **Worker memory.** The worker budgets crawl memory as 70% of its cgroup limit (it falls back to 1 GB when no limit is readable), so keep `mem_limit` accurate. A crawl whose page cap does not fit the budget is failed rather than risking an out-of-memory kill.
- **Shutdown.** `worker` has `stop_grace_period: 75s`; on SIGTERM it stops claiming work and lets running crawls drain for up to 60 seconds, so redeploys do not cut crawls off mid-batch.
- **Health.** `web` has a health check (`codoseo healthcheck`, which probes `/readyz`); `worker` has none, because it serves no HTTP. Watch the worker through metrics ([operations.md](operations.md)).
- **Metrics.** `CODOSEO_METRICS_BIND` defaults to `0.0.0.0:9090` in this file. Scrape `web:9090` and `worker:9090` from inside the Docker network; do not publish 9090 or route the domain to it.
- **Logs.** Cloud mode writes JSON lines to stderr; Coolify shows them in the service log.
- **Scaling.** Run more workers by raising the `worker` replica count (or adding another worker service) against the same database. Run extra web containers with `CODOSEO_SCHEDULER=off`.
