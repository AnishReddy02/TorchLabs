# Torch Fleet

Torch Fleet is Torch Labs' reliability platform for Linux based robotics fleets.
It connects robot agents, records fleet telemetry, investigates service failures,
proposes bounded repairs, and verifies recovery. The original Bench workflow and
reservation capabilities remain available as the test and deployment layer.

See the [customer pilot guide](docs/customer-pilot.md), [architecture audit](docs/architecture-audit.md),
[autonomous recovery runbook](docs/autonomous-recovery.md), and [Torch Agent guide](docs/torch-agent.md).

**Connect an asset once. Test it from anywhere.**

Torch Fleet is developer-first infrastructure for registering, reserving, deploying to, testing, and observing physical robotics and hardware assets. It runs existing scripts rather than requiring teams to rewrite their deployment stack.

```text
 React UI / bench CLI
          │ HTTP
          ▼
 ┌─────────────────────┐       PostgreSQL
 │ Torch Control Plane │◀──── durable state
 │ REST + scheduler    │       local artifacts
 └──────────┬──────────┘
            │ persistent outbound gRPC
            ▼
 ┌─────────────────────┐
 │     Torch Node      │
 │ inventory/executor  │
 └──────────┬──────────┘
            ▼
       physical asset
```

## Quick start

Prerequisites: Rust stable, `protoc`, Docker Desktop, Node.js 22+, npm, `curl`, `jq`, and `tar`.

The shortest verified path is:

```bash
make setup
make demo
```

`make demo` uses isolated ports, starts PostgreSQL and temporary control-plane/node processes, runs passing, fake-robot, timeout, cancellation, and failing workflows, verifies artifacts and reservation locking, then stops the Rust processes. PostgreSQL remains running.

## Autonomous recovery demo

The recovery demo runs on an isolated database and ports so it can run beside the
regular Bench demo. It includes a deliberately unhealthy noncritical HTTP service,
bounded evidence collection, structured OpenAI diagnosis, deterministic action
authorization, one allowlisted Docker restart, and independent health verification.

Start the stack:

```bash
docker compose up -d
docker compose -f compose.recovery.yml up -d
```

Then start the recovery control plane, node, and frontend using the commands in
the [recovery runbook](docs/autonomous-recovery.md). Open
<http://localhost:5174/?page=overview> for fleet health or
<http://localhost:5174/?page=incidents> for the incident timeline.

Once the stack is running, exercise the complete flow:

```bash
bash scripts/recovery-check.sh
```

The script verifies initial health, rejects a prohibited action, injects a
failure with `POST /fail`, waits for the incident, and requires a `RESOLVED`
incident backed by actual post-restart health observations. The fixture is a
controlled local service; it is not a simulator for arbitrary robot hardware.

## Run the product manually

Terminal 1:

```bash
make infra
make dev-control-plane
```

Terminal 2:

```bash
make dev-node
```

Terminal 3:

```bash
make frontend
```

Open <http://localhost:5173>. The local development admin token is `dev-admin-token`; the node token is `dev-node-token`. Override both through environment variables before any non-local deployment.

The fleet overview APIs are available at `/api/fleet/overview` and
`/api/telemetry/{asset}`. They expose aggregate fleet state and bounded service
observations for the connected agents.

## First workflow

```yaml
name: autonomy-smoke-test
environment:
  variables:
    MODE: test
prepare:
  - name: verify
    run: df -h
deploy:
  - name: load-build
    run: ./load_bundle.sh
configure:
  - name: configure
    run: ./configure.sh
test:
  - name: health-check
    run: ./run_test.sh
    timeout_seconds: 120
collect:
  - name: export
    run: ./export_results.sh
cleanup:
  - name: safe-state
    run: ./cleanup.sh
    continue_on_error: true
artifacts:
  - logs/**
  - results/**
```

Legacy `setup:` is accepted as an alias for `prepare:`. Steps may also define a relative `working_directory` and an `environment` map. Workflow and step environment values merge, with step values taking precedence.

## Software bundles

```bash
tar -C ./my-software -czf my-software.tar.gz .
curl -fsS http://localhost:8080/api/bundles \
  -H 'authorization: Bearer dev-admin-token' \
  -F 'name=my-software' \
  -F 'file=@my-software.tar.gz;type=application/gzip'
```

Select the returned bundle in the UI or pass its UUID as `bundle_id` when creating a run. The node enforces a 100 MiB download limit, verifies SHA-256, safely extracts into a per-run workspace, executes there, uploads matching artifacts up to 10 MiB each, and removes the workspace.

## CLI

```bash
cargo install --path crates/bench-cli
bench login --token dev-admin-token
bench assets
bench assets describe robot-01
bench reserve robot-01
bench workflows create examples/workflows/fake-robot.yaml
bench run examples/workflows/smoke-test.yaml --asset robot-01
bench runs
bench runs show RUN_UUID
bench logs RUN_UUID --follow
bench cancel RUN_UUID
bench release RESERVATION_UUID
```

All commands support `--json`. Configuration is stored in `~/.config/bench/config.json`.

## API and authentication

The V1 surface is under `/api`: nodes, filtered assets, reservations, workflow CRUD, capability scheduling, runs/reruns/cancellation, logs, bundles, and artifacts. Mutations require:

```text
Authorization: Bearer $BENCH_ADMIN_TOKEN
```

Node registration uses the separate `BENCH_NODE_TOKEN`; only SHA-256 hashes of issued reservation tokens are stored. This is deliberately simple pilot authentication, not an internet-grade identity system. Read endpoints and random-UUID bundle/artifact downloads remain available on the localhost-bound development server.

Asset filters use `?status=AVAILABLE`, `?capability=nix=true`, or `?label=team=autonomy`. Capability scheduling atomically selects an available matching asset with PostgreSQL row locks:

```bash
curl -fsS http://localhost:8080/api/scheduler/reserve \
  -H 'authorization: Bearer dev-admin-token' \
  -H 'content-type: application/json' \
  -d '{"owner":"alice","requires":{"compute":"jetson-orin","can":true}}'
```

## Linux node installation

For a local pilot release server:

```bash
curl -fsSL http://CONTROL_PLANE:8080/install.sh | sudo bash
```

The installer detects `x86_64`/`aarch64`, downloads a prebuilt binary from the configured release path, creates a locked environment file, installs [the systemd unit](systemd/bench-node.service), enables it, and starts it. Set `BENCH_RELEASE_BASE`, `BENCH_CONTROL_PLANE`, `BENCH_HTTP_BASE`, `BENCH_NODE_TOKEN`, `BENCH_NODE_NAME`, and `BENCH_ASSET_ID` for noninteractive installation. The repository does not publish binaries itself yet.

For a customer pilot, use `BENCH_ENV=production`, high-entropy admin and node
credentials, explicit `BENCH_CORS_ORIGINS`, and a TLS reverse proxy. Production
mode rejects the development credentials. Follow [the pilot guide](docs/customer-pilot.md)
before connecting customer hardware.

## Development

```bash
make setup
make infra
make migrate
make dev-control-plane
make dev-node
make frontend
make cli
make test
make lint
make demo
```

Migrations run automatically at control-plane startup. CI runs formatting, strict Clippy, Rust tests, and the frontend production build.

## Security boundary

Implemented now: localhost binding, separate admin/node tokens, production credential checks, configured CORS, durable fleet telemetry, structured diagnosis, deterministic recovery policy, hashed token comparisons, hashed reservation tokens, command length and timeout limits, validated workflow paths, workspace-contained artifact collection, bounded uploads, process-group termination, durable audit events, and no token logging.

Still required before public production exposure: TLS and mTLS, per-user identities, RBAC/SSO, token rotation/revocation, signed workflows, a secrets manager, sandboxed execution, encrypted object storage, network policies, customer isolation, artifact authorization, and audit export. `/bin/sh -c` executes authenticated trusted workflow input on physical machines and must be treated accordingly.

## Nix direction

Nodes currently detect Nix and publish `capabilities.nix`. A future `NixExecutor`/`NixEnvironmentManager` should make `git SHA + flake.lock + Nix closure + hardware identity` a reproducible physical test record. Bench does not yet manage NixOS generations or closures.

## Current limitations

- One control-plane process; connected-node routing is in memory.
- Immediate dispatch rather than a durable outbox/queue.
- One configured asset per node CLI invocation; the protocol/schema permit future expansion.
- Node YAML configuration and published installer binaries are not yet complete.
- Local filesystem bundle/artifact storage only.
- Polling rather than WebSockets/SSE.
- No sandbox or resource quotas beyond timeout and process termination.
- A control-plane restart reconnects nodes, but an in-flight run is currently failed rather than resumed.
- Organization isolation, RBAC, per-device enrollment, mTLS, signed releases, and systemd recovery adapters are not implemented yet.
