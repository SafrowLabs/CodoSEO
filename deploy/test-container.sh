#!/usr/bin/env bash
# Smoke test for the container image: brings up the self-host compose file with the image under
# test, checks the web app answers, then crawls the fixture site in deploy/test-site through the
# image's CLI and asserts it found more than one page. Cleans up everything it created.
#
#   IMAGE=codoseo:m9-local deploy/test-container.sh
#
# Settings (environment): IMAGE (default codoseo:local), PROJECT (compose project name, default
# codoseo-smoke), PORT (host port, default 18080).
set -euo pipefail

IMAGE="${IMAGE:-codoseo:local}"
PROJECT="${PROJECT:-codoseo-smoke}"
PORT="${PORT:-18080}"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
site="${PROJECT}-site"

# A repo/tag split so compose's ${CODOSEO_IMAGE}:${CODOSEO_VERSION} resolves to IMAGE.
export CODOSEO_IMAGE="${IMAGE%:*}"
export CODOSEO_VERSION="${IMAGE##*:}"
export CODOSEO_PORT="$PORT"
export BASE_URL="http://127.0.0.1:${PORT}"
export SECRET_KEY="smoke-test-secret-key-0123456789abcdef"
export POSTGRES_PASSWORD="smoke0123456789"

compose() { docker compose -p "$PROJECT" -f "$here/compose.selfhost.yml" "$@"; }

cleanup() {
  status=$?
  if [ "$status" -ne 0 ]; then
    echo "--- failed; compose logs ---" >&2
    compose logs --no-color --tail 60 >&2 || true
  fi
  docker rm -f "$site" >/dev/null 2>&1 || true
  compose down -v --remove-orphans >/dev/null 2>&1 || true
  exit "$status"
}
trap cleanup EXIT

echo "== image: $IMAGE"
docker image inspect -f 'size: {{.Size}} bytes' "$IMAGE"
docker run --rm "$IMAGE" --version

echo "== compose up (project $PROJECT, port $PORT)"
compose up -d --quiet-pull --wait --wait-timeout 120

echo "== web app"
for path in /readyz /healthz /; do
  # Self-host mode redirects / to the signed-in area, so follow redirects to the final page.
  code="$(curl -sL -o /dev/null -w '%{http_code}' "http://127.0.0.1:${PORT}${path}")"
  echo "GET $path -> $code"
  [ "$code" = 200 ] || { echo "expected 200 for $path" >&2; exit 1; }
done

echo "== crawl the fixture site through the image"
network="${PROJECT}_default"
docker run -d --name "$site" --network "$network" --network-alias testsite \
  -v "$here/test-site:/usr/share/nginx/html:ro" nginx:alpine >/dev/null
sleep 2
report="$(docker run --rm --network "$network" "$IMAGE" crawl http://testsite/ --format json)"
pages="$(printf '%s' "$report" | python3 -c 'import json,sys; print(json.load(sys.stdin)["report"]["summary"]["pages"])')"
echo "pages crawled: $pages"
[ "$pages" -gt 1 ] || { echo "expected more than one page" >&2; exit 1; }

echo "== ok"
