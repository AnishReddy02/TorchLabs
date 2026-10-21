#!/usr/bin/env bash
set -euo pipefail

API="${BENCH_API_URL:-http://localhost:8080}"
ASSET_NAME="${BENCH_DEMO_ASSET:-robot-01}"
API_TOKEN="${BENCH_ADMIN_TOKEN:-dev-admin-token}"

need() { command -v "$1" >/dev/null || { echo "missing required command: $1" >&2; exit 1; }; }
need curl
need jq
need tar

wait_for_asset() {
  for _ in $(seq 1 30); do
    ASSET_ID=$(curl -fsS "$API/assets" | jq -r --arg name "$ASSET_NAME" '.[] | select(.name==$name) | .id')
    STATUS=$(curl -fsS "$API/assets" | jq -r --arg name "$ASSET_NAME" '.[] | select(.name==$name) | .status')
    if [[ -n "$ASSET_ID" && "$STATUS" == "AVAILABLE" ]]; then return; fi
    sleep 1
  done
  echo "asset $ASSET_NAME did not become AVAILABLE" >&2; exit 1
}

upload() {
  jq -Rs '{definition_yaml:.}' < "$1" | curl -fsS -X POST "$API/workflows" -H "authorization: Bearer $API_TOKEN" -H 'content-type: application/json' -d @- | jq -r .id
}
reserve() {
  curl -fsS -X POST "$API/assets/$ASSET_ID/reserve" -H "authorization: Bearer $API_TOKEN" -H 'content-type: application/json' -d '{"owner":"demo","duration_minutes":10}' | jq -r .id
}
run_and_wait() {
  local workflow_id="$1" expected="$2" bundle_id="${3:-}" run_id status
  run_id=$(jq -n --arg asset "$ASSET_ID" --arg workflow "$workflow_id" --arg bundle "$bundle_id" '{asset_id:$asset,workflow_id:$workflow,bundle_id:(if $bundle=="" then null else $bundle end)}' | curl -fsS -X POST "$API/runs" -H "authorization: Bearer $API_TOKEN" -H 'content-type: application/json' -d @- | jq -r .id)
  for _ in $(seq 1 60); do
    status=$(curl -fsS "$API/runs/$run_id" | jq -r .status)
    if [[ "$status" =~ ^(PASSED|FAILED|CANCELLED|TIMED_OUT)$ ]]; then break; fi
    sleep 1
  done
  [[ "$status" == "$expected" ]] || { echo "run $run_id: expected $expected, got $status" >&2; exit 1; }
  curl -fsS "$API/runs/$run_id/logs" | jq -r '.[].content'
  echo "verified run $run_id -> $status"
  RUN_ID="$run_id"
}
run_and_cancel() {
  local workflow_id="$1" run_id status
  run_id=$(jq -n --arg asset "$ASSET_ID" --arg workflow "$workflow_id" '{asset_id:$asset,workflow_id:$workflow}' | curl -fsS -X POST "$API/runs" -H "authorization: Bearer $API_TOKEN" -H 'content-type: application/json' -d @- | jq -r .id)
  for _ in $(seq 1 20); do
    status=$(curl -fsS "$API/runs/$run_id" | jq -r .status)
    [[ "$status" =~ ^(PREPARING|RUNNING)$ ]] && break
    sleep 1
  done
  curl -fsS -X POST "$API/runs/$run_id/cancel" -H "authorization: Bearer $API_TOKEN" -H 'content-type: application/json' -d '{}' >/dev/null
  for _ in $(seq 1 30); do
    status=$(curl -fsS "$API/runs/$run_id" | jq -r .status)
    [[ "$status" == "CANCELLED" ]] && break
    sleep 1
  done
  [[ "$status" == "CANCELLED" ]] || { echo "expected CANCELLED, got $status" >&2; exit 1; }
  curl -fsS "$API/runs/$run_id/logs" | jq -e 'map(.content) | any(contains("cleanup ran after cancellation"))' >/dev/null
  echo "verified cancellation kills the command and still runs cleanup"
}

wait_for_asset
SMOKE_ID=$(upload examples/workflows/smoke-test.yaml)
FAIL_ID=$(upload examples/workflows/failing-test.yaml)
BUNDLE_WORKFLOW_ID=$(upload examples/workflows/fake-robot.yaml)
TIMEOUT_ID=$(upload examples/workflows/timeout-test.yaml)
CANCEL_ID=$(upload examples/workflows/cancellation-test.yaml)
BUNDLE_TEMP_DIR=$(mktemp -d -t bench-demo-bundle)
BUNDLE_ARCHIVE="$BUNDLE_TEMP_DIR/demo-software.tar.gz"
tar -C examples/fake-robot -czf "$BUNDLE_ARCHIVE" .
BUNDLE_ID=$(curl -fsS -X POST "$API/bundles" -H "authorization: Bearer $API_TOKEN" -F 'name=demo-software-1.0.0' -F "file=@$BUNDLE_ARCHIVE;type=application/gzip" | jq -r .id)
echo "uploaded software bundle as $BUNDLE_ID"
RESERVATION_ID=$(reserve)
echo "reserved $ASSET_NAME as $RESERVATION_ID"
SECOND_STATUS=$(curl -sS -o /dev/null -w '%{http_code}' -X POST "$API/assets/$ASSET_ID/reserve" -H "authorization: Bearer $API_TOKEN" -H 'content-type: application/json' -d '{"owner":"reservation-racer","duration_minutes":10}')
[[ "$SECOND_STATUS" == "409" ]] || { echo "expected second reservation to return 409, got $SECOND_STATUS" >&2; exit 1; }
echo "verified a second reservation is rejected atomically"
run_and_wait "$SMOKE_ID" PASSED
curl -fsS "$API/assets/$ASSET_ID/release" -X POST -H "authorization: Bearer $API_TOKEN" -H 'content-type: application/json' -d '{}' >/dev/null
[[ $(curl -fsS "$API/assets/$ASSET_ID" | jq -r .status) == "AVAILABLE" ]]
echo "released $ASSET_NAME; it is AVAILABLE"

RESERVATION_ID=$(reserve)
run_and_wait "$BUNDLE_WORKFLOW_ID" PASSED "$BUNDLE_ID"
curl -fsS "$API/runs/$RUN_ID/logs" | jq -e 'map(.content) | any(contains("PASS: fake robot health checks"))' >/dev/null
ARTIFACT_COUNT=$(curl -fsS "$API/runs/$RUN_ID/artifacts" | jq length)
[[ "$ARTIFACT_COUNT" -ge 3 ]] || { echo "expected at least 3 fake-robot artifacts, got $ARTIFACT_COUNT" >&2; exit 1; }
echo "verified fake robot bundle and $ARTIFACT_COUNT collected artifacts"
run_and_wait "$TIMEOUT_ID" TIMED_OUT
curl -fsS "$API/runs/$RUN_ID/logs" | jq -e 'map(.content) | any(contains("cleanup ran after timeout"))' >/dev/null
echo "verified timeout kills the command and still runs cleanup"
run_and_cancel "$CANCEL_ID"
curl -fsS "$API/assets/$ASSET_ID/release" -X POST -H "authorization: Bearer $API_TOKEN" -H 'content-type: application/json' -d '{}' >/dev/null

RESERVATION_ID=$(reserve)
run_and_wait "$FAIL_ID" FAILED
curl -fsS "$API/runs/$RUN_ID/logs" | jq -e 'map(.content) | any(contains("cleanup ran after failure"))' >/dev/null
echo "verified cleanup output was persisted after failure"
curl -fsS "$API/assets/$ASSET_ID/release" -X POST -H "authorization: Bearer $API_TOKEN" -H 'content-type: application/json' -d '{}' >/dev/null
# Real seeded evidence for Torch Agent; this checks the physical execution path,
# not a fabricated model response. The live model demo is started from the UI.
AGENT_WORKFLOW_ID=$(upload examples/workflows/agent-rover.yaml)
tar -C examples/agent-rover -czf "$BUNDLE_TEMP_DIR/agent-rover.tar.gz" .
AGENT_BUNDLE_ID=$(curl -fsS -X POST "$API/bundles" -H "authorization: Bearer $API_TOKEN" -F 'name=rover-demo-frame-check' -F "file=@$BUNDLE_TEMP_DIR/agent-rover.tar.gz;type=application/gzip" | jq -er .id)
curl -fsS -X POST "$API/api/agent/workflows/$AGENT_WORKFLOW_ID/approve" -H "authorization: Bearer $API_TOKEN" -H 'content-type: application/json' -d '{}' >/dev/null
RESERVATION_ID=$(reserve)
run_and_wait "$AGENT_WORKFLOW_ID" FAILED "$AGENT_BUNDLE_ID"
curl -fsS "$API/runs/$RUN_ID" | jq -e '.steps | any(.name=="localization-frame" and .exit_code==42)' >/dev/null
curl -fsS "$API/runs/$RUN_ID/logs" | jq -e 'map(.content) | any(contains("does not match required"))' >/dev/null
curl -fsS "$API/runs/$RUN_ID/logs" | jq -e 'map(.content) | any(contains("cleanup complete"))' >/dev/null
curl -fsS "$API/runs/$RUN_ID/artifacts" | jq -e 'any(.path=="results/localization.json")' >/dev/null
curl -fsS "$API/assets/$ASSET_ID/release" -X POST -H "authorization: Bearer $API_TOKEN" -H 'content-type: application/json' -d '{}' >/dev/null
echo "Verified Torch Agent fixture: real exit 42, configuration evidence, cleanup and artifacts"
echo "Bench end-to-end demo passed"
