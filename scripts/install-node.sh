#!/bin/sh
set -eu

if [ "$(id -u)" -ne 0 ]; then echo "Run this installer as root (for example: curl ... | sudo sh)." >&2; exit 1; fi
ARCH=$(uname -m)
case "$ARCH" in x86_64) RELEASE_ARCH=x86_64-unknown-linux-gnu;; aarch64|arm64) RELEASE_ARCH=aarch64-unknown-linux-gnu;; *) echo "Unsupported architecture: $ARCH" >&2; exit 1;; esac
CONTROL_PLANE=${BENCH_CONTROL_PLANE:-http://localhost:50051}
NODE_TOKEN=${BENCH_NODE_TOKEN:-dev-node-token}
NODE_NAME=${BENCH_NODE_NAME:-$(hostname)}
ASSET_ID=${BENCH_ASSET_ID:-robot-01}
RELEASE_BASE=${BENCH_RELEASE_BASE:-http://localhost:8080/releases}

if [ -t 0 ]; then
  printf 'Control plane [%s]: ' "$CONTROL_PLANE"; read -r value; CONTROL_PLANE=${value:-$CONTROL_PLANE}
  printf 'Node token: '; stty -echo; read -r value; stty echo; echo; NODE_TOKEN=${value:-$NODE_TOKEN}
  printf 'Node name [%s]: ' "$NODE_NAME"; read -r value; NODE_NAME=${value:-$NODE_NAME}
  printf 'Asset ID [%s]: ' "$ASSET_ID"; read -r value; ASSET_ID=${value:-$ASSET_ID}
fi

id bench >/dev/null 2>&1 || useradd --system --home /var/lib/bench --shell /usr/sbin/nologin bench
install -d -m 0750 -o bench -g bench /etc/bench /var/lib/bench
curl -fsSL "$RELEASE_BASE/bench-node-$RELEASE_ARCH" -o /usr/local/bin/bench-node
chmod 0755 /usr/local/bin/bench-node
cat >/etc/bench/node.env <<EOF
BENCH_NODE_NAME=$NODE_NAME
BENCH_NODE_TOKEN=$NODE_TOKEN
BENCH_NODE_IDENTITY_FILE=/var/lib/bench/node-id
BENCH_NODE_WORKSPACE_DIR=/var/lib/bench/workspaces
BENCH_ASSET_ID=$ASSET_ID
BENCH_CONTROL_PLANE=$CONTROL_PLANE
EOF
chmod 0600 /etc/bench/node.env
curl -fsSL "${BENCH_HTTP_BASE:-http://localhost:8080}/install/bench-node.service" -o /etc/systemd/system/bench-node.service
systemctl daemon-reload
systemctl enable --now bench-node
echo "Bench Node installed. Asset '$ASSET_ID' should appear after its first heartbeat."
