#!/usr/bin/env bash
# Runs `codoseo crawl` for the action: GitHub annotations on stdout, the SARIF file, the audit (the
# next run's baseline) and a Markdown report for the job summary, then hands back the exit code.
#
# Inputs (environment): CODOSEO_URL, CODOSEO_FAIL_ON, CODOSEO_BASELINE, CODOSEO_SARIF,
# CODOSEO_AUDIT, CODOSEO_POLICY, CODOSEO_MAX_PAGES, CODOSEO_MAX_TIME, CODOSEO_RPS, CODOSEO_ARGS (extra flags,
# split on spaces), GITHUB_OUTPUT, GITHUB_STEP_SUMMARY, RUNNER_TEMP.
#
# Exit codes are codoseo's: 0 passed, 1 a --fail-on gate failed, 2 the crawl could not run.
set -uo pipefail

if [ -z "${CODOSEO_URL:-}" ]; then
  echo "::error::The url input is required."
  exit 2
fi

sarif="${CODOSEO_SARIF:-codoseo.sarif}"
audit="${CODOSEO_AUDIT:-codoseo-audit.json}"
summary="${RUNNER_TEMP:-/tmp}/codoseo-summary.md"
args=(crawl "$CODOSEO_URL" --format github --save "$sarif" --save "$audit" --save "$summary")
[ -n "${CODOSEO_MAX_PAGES:-}" ] && args+=(--max-pages "$CODOSEO_MAX_PAGES")
[ -n "${CODOSEO_MAX_TIME:-}" ] && args+=(--max-time "$CODOSEO_MAX_TIME")
[ -n "${CODOSEO_RPS:-}" ] && args+=(--rps "$CODOSEO_RPS")
[ -n "${CODOSEO_FAIL_ON:-}" ] && args+=(--fail-on "$CODOSEO_FAIL_ON")
[ -n "${CODOSEO_POLICY:-}" ] && args+=(--policy "$CODOSEO_POLICY")
if [ -n "${CODOSEO_BASELINE:-}" ]; then
  if [ -f "$CODOSEO_BASELINE" ]; then
    args+=(--baseline "$CODOSEO_BASELINE")
  else
    # The first run has nothing to compare with: every result counts.
    echo "::notice::No baseline at ${CODOSEO_BASELINE}; every result counts on this run."
  fi
fi
if [ -n "${CODOSEO_ARGS:-}" ]; then
  read -r -a extra <<< "$CODOSEO_ARGS"
  args+=("${extra[@]}")
fi

codoseo "${args[@]}"
code=$?

{
  echo "exit-code=$code"
  echo "sarif=$sarif"
  echo "audit=$audit"
} >> "${GITHUB_OUTPUT:-/dev/null}"
if [ -f "$summary" ] && [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  cat "$summary" >> "$GITHUB_STEP_SUMMARY"
fi
case "$code" in
  0) ;;
  1) echo "::error::CodoSEO: a --fail-on gate (${CODOSEO_FAIL_ON:-}) failed; see the annotations." ;;
  *) echo "::error::CodoSEO could not audit ${CODOSEO_URL} (exit ${code})." ;;
esac
exit "$code"
