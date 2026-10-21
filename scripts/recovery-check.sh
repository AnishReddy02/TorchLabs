#!/usr/bin/env bash
# Exercise an already-running isolated recovery stack. See docs/autonomous-recovery.md.
set -euo pipefail
api="${BENCH_API_URL:-http://localhost:8081}"
token="${BENCH_ADMIN_TOKEN:-dev-admin-token}"
service="${TORCH_DEMO_URL:-http://127.0.0.1:8099}"
request() { curl --max-time 10 -fsS -H "Authorization: Bearer $token" "$@"; }
curl --max-time 5 -fsS "$service/health" | jq -e '.healthy == true' >/dev/null
previous=$(request "$api/api/incidents" | jq -r '.[0].id // ""')
request "$api/api/recovery/policy/evaluate" -H 'Content-Type: application/json' \
  -d '{"asset":"recovery-demo","service_id":"flight-controller","action_id":"RUN_COMMAND"}' \
  | jq -e '.executed == false and .decision.authorized == false' >/dev/null
curl --max-time 5 -fsS -X POST "$service/fail"
for attempt in $(seq 1 90); do
  latest=$(request "$api/api/incidents" | jq '.[0]')
  id=$(jq -r '.id // ""' <<<"$latest")
  status=$(jq -r '.status // ""' <<<"$latest")
  if [[ "$id" != "$previous" && "$status" == RESOLVED ]]; then
    curl --max-time 5 -fsS "$service/health" | jq -e '.healthy == true' >/dev/null
    request "$api/api/incidents/$id" | jq '{id,status,diagnosis,detected_at,resolved_at,remediation_attempts,verification_result,executions}'
    exit 0
  fi
  if [[ "$id" != "$previous" && "$status" =~ ^(ESCALATED|FAILED|CANCELLED)$ ]]; then
    request "$api/api/incidents/$id" | jq '{id,status,escalation_reason,diagnosis,executions}'
    exit 1
  fi
  sleep 1
done
echo 'Recovery did not complete within 90 seconds.' >&2
exit 1
