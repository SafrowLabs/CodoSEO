# Operations

How to watch, back up and upgrade a CodoSEO deployment. Settings named here are documented in [configuration.md](configuration.md).

## Logs

The `web`, `worker`, `all` and `migrate` roles log through `tracing` to **stderr**. The format is JSON lines in cloud mode and plain text otherwise; set `CODOSEO_LOG_FORMAT=json` or `text` to choose. The default filter is `info,sqlx=warn`; set `RUST_LOG` to change it (for example `RUST_LOG=debug`). Crawl log lines carry the crawl and site ids. Emails, keys and tokens are not logged. The one exception is an install without `SMTP_URL`, which prints each email (so you can copy the sign-in link) to stdout.

Set `SENTRY_DSN` to report panics and errors to Sentry. Nothing leaves the process without it. Use `SENTRY_ENVIRONMENT` to name the environment.

## Health endpoints

On the web port (8080):

| Path | Answers |
|---|---|
| `/healthz` | `200 ok` while the process is up. Does not touch the database. |
| `/readyz` | `200 ready` when Postgres answers `SELECT 1`, otherwise `503 database unavailable`. |

`codoseo healthcheck` does a GET of `/readyz` on `127.0.0.1` (port taken from `CODOSEO_BIND`) and exits 0 on a 2xx answer, 1 otherwise. Use `--url` to probe something else and `--timeout` (default 3 seconds). The image's Docker `HEALTHCHECK` runs it, since the distroless image has no curl. The worker serves no HTTP, so it has no health check; watch it through the metrics below.

## Metrics

Set `CODOSEO_METRICS_BIND` (for example `0.0.0.0:9090`) on a `web`, `worker` or `all` process to serve Prometheus text at `/metrics` on that internal address. It is a separate listener: the public router has no `/metrics`. Keep the port inside your network. Each process exposes its own numbers, so scrape the web and every worker and sum across them.

| Metric | Type | Labels | Meaning |
|---|---|---|---|
| `codoseo_crawl_queue_wait_seconds` | histogram | `lane` (0 to 5) | Seconds a crawl waited in the queue before a worker claimed it. Lane is the crawl's priority; 0 is most urgent. |
| `codoseo_crawls_running` | gauge | none | Crawls this process is running now. |
| `codoseo_crawls_finished_total` | counter | `outcome` = `completed`, `failed` | Crawl runs that ended. A failed first attempt is retried. |
| `codoseo_pages_crawled_total` | counter | none | Pages fetched by crawls. |
| `codoseo_worker_memory_budget_bytes` | gauge | none | The worker's crawl memory budget (70% of its cgroup limit). Appears once a worker has set it. |
| `codoseo_worker_memory_reserved_bytes` | gauge | none | Memory reserved by running crawls. |
| `codoseo_db_pool_connections` | gauge | `state` = `idle`, `active` | Database pool connections, sampled. |
| `codoseo_alert_deliveries_total` | counter | `channel` = `email`, `slack`, `discord`, `webhook`; `result` = `ok`, `error` | Alert delivery attempts. |
| `codoseo_api_requests_total` | counter | `surface` = `rest`, `mcp`; `tier` = `key`, `anon`; `result` | Agent API calls. `result` is `ok` or an error code from [api.md](api.md). |
| `codoseo_scheduler_last_tick_timestamp_seconds` | gauge | none | Unix time of the last scheduler tick. Appears after the first tick. |
| `codoseo_jobs_failed_total` | counter | `kind` | Background job runs that failed, by job kind. |

Labels never carry URLs, domains, emails or account ids.

Useful alerts:

- Scheduler stalled: `time() - codoseo_scheduler_last_tick_timestamp_seconds > 300` (it ticks every 60 seconds).
- Queue backing up: a high percentile of `codoseo_crawl_queue_wait_seconds` per lane, or `codoseo_worker_memory_reserved_bytes` close to `codoseo_worker_memory_budget_bytes`.
- Failures: a rising `codoseo_crawls_finished_total{outcome="failed"}` or `codoseo_jobs_failed_total`.

## Scheduler heartbeat

The scheduler ticks once a minute: it enqueues due crawls, runs the daily cleanup and sends inactivity emails. After each successful tick it records a heartbeat in the database and, if `SCHEDULER_HEARTBEAT_URL` is set, sends a GET to that URL. Point that at a dead-man's-switch monitor (a heartbeat check in your uptime tool) that alerts when pings stop. The `web` and `all` roles run the scheduler; set `CODOSEO_SCHEDULER=off` to stop a given process from running it.

## Backups

Everything lives in Postgres: `pg_dump` the database. For a self-hosted compose install:

```sh
docker compose exec db pg_dump -U codoseo -Fc codoseo > codoseo-$(date +%F).dump
```

Keep `SECRET_KEY` with the backup. It encrypts stored channel secrets (webhook URLs, SMTP details), and a restored database cannot decrypt them without it. Crawl history older than the plan's window is deleted by the daily cleanup (`RETENTION_DAYS_SELFHOST`, default 365 days, for self-hosted installs), so the database does not grow without bound.

## Upgrades

1. Back up the database.
2. Run the migrations with the new version: `codoseo migrate` (the `migrate` service in the cloud compose file; the `all` role does this itself when it starts).
3. Restart `web` and `worker` on the new version.

`docker compose up -d` against `compose.cloud.yml` does all three in order, because `web` and `worker` depend on `migrate` completing. The `web` role does not apply migrations itself, so never start it on a new version before `migrate` has run.

On shutdown (SIGTERM) the worker stops claiming work and lets the running crawl finish for up to 60 seconds. The cloud compose file sets `stop_grace_period: 75s` to leave room for that. A crawl that does not finish in time is picked up again by the stale-crawl sweep once its heartbeat is 45 seconds old.

### Large databases: build the pages index first

Migration `0010_agent_access.sql` creates the index `pages_crawl_id_id_idx` on `pages (crawl_id, id)` and drops the older `pages_crawl_id_idx`. sqlx runs a migration in one transaction, so building that index inside it blocks writes to `pages` while it builds. On a database with many pages, build it first without a lock, then run the upgrade:

```sql
CREATE INDEX CONCURRENTLY IF NOT EXISTS pages_crawl_id_id_idx ON pages (crawl_id, id);

-- A failed CONCURRENTLY build leaves an invalid index behind, and IF NOT EXISTS would skip it.
-- This must return one row with indisvalid = true:
SELECT indexrelid::regclass, indisvalid FROM pg_index
 WHERE indexrelid = 'pages_crawl_id_id_idx'::regclass;

ANALYZE pages;
```

If `indisvalid` is false, `DROP INDEX pages_crawl_id_id_idx;` and run the build again. Run `CREATE INDEX CONCURRENTLY` outside a transaction block (plain `psql`, one statement at a time). The migration then finds the index in place and only drops the old one. Small databases can skip this.

## Postgres roles for a shared server

If CodoSEO shares a Postgres server with other apps, `deploy/postgres-role.sql` sets up the database and two login roles. Run it once as a superuser (or a role with `CREATEROLE` and `CREATEDB`); it is safe to re-run, for example after a password rotation.

```sh
psql "postgres://postgres:...@db.internal:5432/postgres" \
     -v ON_ERROR_STOP=1 \
     -v password="$CODOSEO_DB_PASSWORD" \
     -v web_password="$CODOSEO_WEB_DB_PASSWORD" \
     -f deploy/postgres-role.sql
```

- `codoseo` owns the `codoseo` database and is used by the worker and `codoseo migrate`. `CONNECTION LIMIT 10`.
- `codoseo_web` is used by the web container: `CONNECTION LIMIT 5`, a 5 second `statement_timeout`, and data access only (no schema changes). Set it as `DATABASE_URL_WEB` in the cloud compose file.
- Together they cap CodoSEO at 15 connections on the shared server. `web_password` is optional and defaults to `password`. Use letters and digits, since both go inside `postgres://` URLs.

The CSV export and the admin funnel pages are the deliberate exceptions to the 5 second limit: they lift it for their own transaction (`SET LOCAL statement_timeout = '120s'`), so a big export is not cancelled.
