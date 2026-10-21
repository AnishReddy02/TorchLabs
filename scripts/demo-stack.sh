#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "$0")/.." && pwd)
CONTROL_LOG=$(mktemp -t bench-control-plane.log)
NODE_LOG=$(mktemp -t bench-node.log)
CONTROL_PID=""
NODE_PID=""
DEMO_HTTP_PORT="${BENCH_DEMO_HTTP_PORT:-18080}"
DEMO_GRPC_PORT="${BENCH_DEMO_GRPC_PORT:-55051}"
DEMO_API="http://localhost:$DEMO_HTTP_PORT"

cleanup() {
  if [[ -n "$NODE_PID" ]]; then kill "$NODE_PID" 2>/dev/null || true; fi
  if [[ -n "$CONTROL_PID" ]]; then kill "$CONTROL_PID" 2>/dev/null || true; fi
  wait "$NODE_PID" 2>/dev/null || true
  wait "$CONTROL_PID" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

cd "$ROOT_DIR"

echo "Starting PostgreSQL..."
docker compose up -d

echo "Building Bench..."
cargo build -p control-plane -p bench-node

echo "Starting control plane..."
target/debug/control-plane --http-addr "127.0.0.1:$DEMO_HTTP_PORT" --grpc-addr "127.0.0.1:$DEMO_GRPC_PORT" --bundle-base-url "$DEMO_API" >"$CONTROL_LOG" 2>&1 &
CONTROL_PID=$!

for _ in $(seq 1 60); do
  if curl -fsS "$DEMO_API/health" >/dev/null 2>&1; then break; fi
  if ! kill -0 "$CONTROL_PID" 2>/dev/null; then
    echo "Control plane exited unexpectedly:" >&2
    sed -n '1,200p' "$CONTROL_LOG" >&2
    exit 1
  fi
  sleep 1
done

if ! curl -fsS "$DEMO_API/health" >/dev/null 2>&1; then
  echo "Control plane did not become ready:" >&2
  sed -n '1,200p' "$CONTROL_LOG" >&2
  exit 1
fi

echo "Starting robot-01..."
target/debug/bench-node \
  --asset-id robot-01 \
  --control-plane "http://localhost:$DEMO_GRPC_PORT" \
  --identity-file /tmp/bench-demo-node-id \
  --workspace-dir /tmp/bench-demo-workspaces \
  >"$NODE_LOG" 2>&1 &
NODE_PID=$!

for _ in $(seq 1 60); do
  if curl -fsS "$DEMO_API/nodes/robot-01/health" >/dev/null 2>&1; then break; fi
  if ! kill -0 "$NODE_PID" 2>/dev/null; then
    echo "Bench Node exited unexpectedly:" >&2
    sed -n '1,200p' "$NODE_LOG" >&2
    exit 1
  fi
  sleep 1
done

if ! curl -fsS "$DEMO_API/nodes/robot-01/health" >/dev/null 2>&1; then
  echo "robot-01 did not connect." >&2
  echo "Control-plane log:" >&2
  sed -n '1,200p' "$CONTROL_LOG" >&2
  echo "Node log:" >&2
  sed -n '1,200p' "$NODE_LOG" >&2
  exit 1
fi

ASSET_JSON=$(curl -fsS "$DEMO_API/assets" | jq -c '.[] | select(.name=="robot-01")')
ASSET_ID=$(jq -r .id <<<"$ASSET_JSON")
ASSET_STATUS=$(jq -r .status <<<"$ASSET_JSON")
if [[ "$ASSET_STATUS" == "RESERVED" ]]; then
  ACTIVE_OWNER=$(curl -fsS "$DEMO_API/reservations" | jq -r --arg asset "$ASSET_ID" '.[] | select(.asset_id==$asset and .status=="ACTIVE") | .owner')
  if [[ "$ACTIVE_OWNER" == "demo" ]]; then
    echo "Releasing reservation left by an interrupted demo..."
    curl -fsS -X POST "$DEMO_API/assets/$ASSET_ID/release" -H 'authorization: Bearer dev-admin-token' -H 'content-type: application/json' -d '{}' >/dev/null
  else
    echo "robot-01 is reserved by '$ACTIVE_OWNER'; release it before running the demo." >&2
    exit 1
  fi
fi

echo "Running end-to-end acceptance flow..."
BENCH_API_URL="$DEMO_API" bash scripts/demo.sh

echo "Demo complete. PostgreSQL remains running; temporary Bench processes were stopped."
