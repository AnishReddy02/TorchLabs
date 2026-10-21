#!/usr/bin/env bash
set -euo pipefail
API="${BENCH_API_URL:-http://localhost:8080}"
TOKEN="${BENCH_ADMIN_TOKEN:-dev-admin-token}"
curl -fsS "$API/health" >/dev/null || { echo 'Start make dev-control-plane first.' >&2; exit 1; }
archive_dir=$(mktemp -d -t torch-agent-bundle)
tar -C examples/agent-rover -czf "$archive_dir/rover.tar.gz" .
curl -fsS "$API/bundles" -H "authorization: Bearer $TOKEN" -F 'name=rover-demo-frame-check' -F "file=@$archive_dir/rover.tar.gz;type=application/gzip" | jq '{id,name,sha256}'
workflow_id=$(jq -Rs '{definition_yaml:.}' < examples/workflows/agent-rover.yaml | curl -fsS "$API/workflows" -H "authorization: Bearer $TOKEN" -H 'content-type: application/json' -d @- | jq -er .id)
# Operator-reviewed fixture: approval is for this exact definition, not all workflows.
curl -fsS -X POST "$API/api/agent/workflows/$workflow_id/approve" -H "authorization: Bearer $TOKEN" -H 'content-type: application/json' -d '{}'
echo
echo 'Fixture ready. Open Torch Agent and enter:'
echo 'Test the latest rover-demo build on an available Rover-03 and investigate any failures.'
echo 'Review the bundle and workflow in the approval panel, then Approve.'
