-- Roles and database for running CodoSEO's cloud layout on a Postgres shared with other apps.
-- Run once as a superuser (or a role with CREATEROLE and CREATEDB), connected to the
-- maintenance database, and again whenever you rotate a password. Safe to re-run.
--
--   psql "postgres://postgres:...@db.internal:5432/postgres" \
--        -v ON_ERROR_STOP=1 \
--        -v password="$CODOSEO_DB_PASSWORD" \
--        -v web_password="$CODOSEO_WEB_DB_PASSWORD" \
--        -f deploy/postgres-role.sql
--
-- `web_password` is optional and defaults to `password`. Use letters and digits, since both end
-- up inside postgres:// URLs.
--
-- What it sets up:
--
--   codoseo      owns the `codoseo` database. Used by the worker and by `codoseo migrate`.
--                CONNECTION LIMIT 10: a worker pool of about 5, a migration run and some room
--                for a psql session.
--   codoseo_web  used by the web container. CONNECTION LIMIT 5 (its pool is about 5) and a
--                5 second statement_timeout, so a slow query in a request cannot hold a
--                connection (or a lock) for long and starve the other apps on the server.
--
-- The limits add up to 15, which is the most CodoSEO may take from a shared server whatever
-- the deployment does.
--
-- codoseo_web is a member of codoseo and logs in with `role = codoseo` set for this database,
-- so it sees and uses every table the migrations create, now and later, without per-table
-- grants. Its own connection limit and statement_timeout still apply, because both belong to
-- the login role. It can do whatever codoseo can inside this one database and nothing outside it.

\if :{?password}
\else
  \echo 'Pass the password for the codoseo role: -v password=...'
  -- Fails on purpose so the script exits non-zero under -v ON_ERROR_STOP=1.
  SELECT 'the password variable is not set' AS error, 1 / 0;
\endif
\if :{?web_password}
\else
  \set web_password :password
\endif

-- Roles. CREATE only when missing; then always (re)apply the settings and passwords.
SELECT 'CREATE ROLE codoseo LOGIN'
 WHERE NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'codoseo') \gexec
SELECT 'CREATE ROLE codoseo_web LOGIN'
 WHERE NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'codoseo_web') \gexec

ALTER ROLE codoseo LOGIN CONNECTION LIMIT 10 PASSWORD :'password';
ALTER ROLE codoseo_web LOGIN CONNECTION LIMIT 5 PASSWORD :'web_password' NOSUPERUSER NOCREATEDB NOCREATEROLE;

-- The web role acts as the owner once connected (needs the SET option, the default).
GRANT codoseo TO codoseo_web WITH INHERIT TRUE, SET TRUE;

-- The database. CREATE DATABASE cannot run in a transaction or inside IF, hence \gexec.
SELECT 'CREATE DATABASE codoseo OWNER codoseo'
 WHERE NOT EXISTS (SELECT 1 FROM pg_database WHERE datname = 'codoseo') \gexec

-- Only these two roles (and superusers) may connect to it.
REVOKE CONNECT ON DATABASE codoseo FROM PUBLIC;
GRANT CONNECT ON DATABASE codoseo TO codoseo, codoseo_web;

-- Per-database role settings: they apply only to sessions in `codoseo`.
ALTER ROLE codoseo_web IN DATABASE codoseo SET statement_timeout = '5s';
ALTER ROLE codoseo_web IN DATABASE codoseo SET role = 'codoseo';
