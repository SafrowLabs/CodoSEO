# Self-hosting

CodoSEO needs Postgres and one container. There is no Redis, no queue service and no outside requests (fonts and scripts are bundled). The `all` role runs the web app, a crawl worker, the job runner and the scheduler in one process, and applies pending migrations when it starts.

## Docker Compose

`deploy/compose.selfhost.yml` runs the `codoseo` container (image `ghcr.io/safrowlabs/codoseo`) and a `postgres:18` container with its data in a named volume.

```sh
curl -fsSL https://raw.githubusercontent.com/SafrowLabs/CodoSEO/main/deploy/compose.selfhost.yml -o compose.yml
printf 'SECRET_KEY=%s\nPOSTGRES_PASSWORD=%s\n' "$(openssl rand -hex 32)" "$(openssl rand -hex 24)" > .env
docker compose up -d
```

Open <http://localhost:8080>. The first account to sign in becomes the owner; the owner can close signups from **Account**. `SECRET_KEY` and `POSTGRES_PASSWORD` are required, and compose refuses to start without them. Use letters and digits for the password, since it goes inside a `postgres://` URL.

Keep `SECRET_KEY` safe and unchanged. It encrypts stored channel secrets (webhook URLs, SMTP details); a new key makes the old ones unreadable.

Settings you may want in `.env` (all optional; [configuration.md](configuration.md) has the full list):

| Variable | Default | Meaning |
|---|---|---|
| `CODOSEO_VERSION` | `latest` | Image tag. Pin a release such as `0.1.0` for repeatable upgrades. |
| `CODOSEO_PORT` | `8080` | Host port. |
| `BASE_URL` | `http://localhost:8080` | The address people open the app at. Set it when you use a domain or a proxy. |
| `SMTP_URL`, `MAIL_FROM` | none | Outgoing mail. |
| `GITHUB_CLIENT_ID`, `GITHUB_CLIENT_SECRET` | none | GitHub sign-in. |
| `ADMIN_EMAILS` | none | Admin pages. |
| `RETENTION_DAYS_SELFHOST` | `365` | Days of crawl history to keep. |

A template with all of these is in `deploy/.env.example`.

### Build from source

From a checkout of this repository:

```sh
cd deploy
docker compose -f compose.selfhost.yml -f compose.build.yml up -d --build
```

## Sign-in and email

Sign-in uses magic links. Without `SMTP_URL`, every email (the sign-in link included) is printed to the container's standard output instead of sent:

```sh
docker compose logs codoseo | grep -A4 'email to'
```

To send real mail, set `SMTP_URL` (for example `smtps://user:pass@smtp.example.com:465`) and `MAIL_FROM`. Alert emails and digests use the same settings. To add "Continue with GitHub", create an OAuth app on GitHub with the callback `<BASE_URL>/auth/github/callback` and set `GITHUB_CLIENT_ID` and `GITHUB_CLIENT_SECRET`.

Alerts can also go to Slack, Discord and webhooks; configure them in the app.

## Putting it behind HTTPS

Run a reverse proxy (Caddy, nginx, Traefik) in front of port 8080 and set `BASE_URL` to the public `https://` address. CodoSEO checks the `Origin` header of every form post against `BASE_URL`, so a mismatch shows up as rejected sign-ins and forms. With an `https` `BASE_URL` cookies are marked `Secure`. Forward the usual headers; a Caddy example:

```
codoseo.example.com {
    reverse_proxy 127.0.0.1:8080
}
```

## Crawling from a self-hosted install

Self-hosted crawls may reach private and internal addresses (an intranet, a staging server on your network), which the hosted cloud refuses. Crawls identify as `CodoSEObot`. There are no plan limits, but each crawl stops at 100,000 pages or 24 hours.

## Without Docker

You need a Postgres (18 is what CodoSEO is tested against in production) and the binary:

```sh
createdb codoseo
export DATABASE_URL=postgres://localhost/codoseo
export SECRET_KEY=$(openssl rand -hex 32)
export BASE_URL=http://localhost:8080
codoseo all
```

For a bigger setup run `codoseo migrate` once, then `codoseo web` and one or more `codoseo worker` processes against the same database. Only one worker process is needed for a small team.

## Upgrades

```sh
docker compose pull
docker compose up -d
```

The container applies pending migrations on start and drains running crawls for up to 60 seconds when it stops. Back up the database first. See [operations.md](operations.md#upgrades) for the larger-database note.

## Backups

Everything lives in Postgres.

```sh
docker compose exec db pg_dump -U codoseo -Fc codoseo > codoseo-$(date +%F).dump
# restore into an empty database
docker compose exec -T db pg_restore -U codoseo -d codoseo --no-owner < codoseo-2026-01-01.dump
```

Keep `SECRET_KEY` with the backup: without it the dump's stored channel secrets cannot be decrypted. The Postgres volume is mounted at `/var/lib/postgresql` (the layout the `postgres:18` image expects).

## Health checks

`GET /healthz` answers `ok` while the process is up. `GET /readyz` answers 200 when Postgres is reachable and 503 otherwise. The container's own health check runs `codoseo healthcheck`, which probes `/readyz`. More in [operations.md](operations.md).
