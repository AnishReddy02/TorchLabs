# Autonomous Recovery V1 — implementation work log

## Repository assessment and baseline

The existing five Rust crates provide an Axum/PostgreSQL control plane, a tonic
bidirectional outbound node session, a shell workflow executor, a shared workflow
domain model, and a CLI. React is built with Vite. Migrations are embedded with
SQLx. Existing Torch Agent modules provide model abstraction, policy, session
history, exact workflow approvals, and cleanup. No incident monitoring or typed
service recovery existed before this change.

Baseline: 21 Rust tests passed, one live model test was intentionally ignored;
frontend production build passed. This workspace has no `.git` directory, so
changes cannot be reviewed through git status/diff here.

Existing security constraints: shared development tokens, plaintext local gRPC,
no per-device identity credentials or TLS, in-memory connection routing, and a
trusted shell workflow executor. Recovery is restricted to a dedicated local demo
node; it is not suitable for exposed networks or safety-critical hardware.

## Vertical slices

1. `0007_autonomous_recovery.sql`, `recovery.rs`, additive protobuf messages:
   incidents, typed restart, policy, durable operations, verification and audit.
2. Node recovery monitor/executor and Docker Linux fixture: observe HTTP health,
   collect bounded evidence, restart one configured container, verify health.
3. Authenticated incident REST endpoints and React Incidents view.
4. Provider-independent structured diagnosis, then failure/adversarial tests and
   measured end-to-end demo. Stub mode must always remain visibly labeled.

## Local demonstration

Keep the existing demo on ports 8080/50051. Start the recovery control plane on
separate ports and a separate database to avoid two control planes sharing state.

```sh
docker compose up -d
docker compose exec -T postgres createdb -U bench torch_recovery
docker compose -f compose.recovery.yml up -d
```

The database creation command is needed only once. Terminal 1:

```sh
DATABASE_URL=postgres://bench:bench@localhost:5432/torch_recovery \
BENCH_HTTP_ADDR=127.0.0.1:8081 BENCH_GRPC_ADDR=127.0.0.1:50052 \
TORCH_RECOVERY_ENABLED=true TORCH_RECOVERY_ALLOWED_ASSET=recovery-demo \
TORCH_RECOVERY_ALLOWED_SERVICE=torch-test-service \
TORCH_RECOVERY_DIAGNOSIS=openai \
bash scripts/with-env.sh cargo run -p control-plane
```

Terminal 2:

```sh
cargo run -p bench-node -- --asset-id recovery-demo \
  --identity-file .bench-recovery-id --control-plane http://localhost:50052 \
  --recovery-service-id torch-test-service \
  --recovery-container-name torch-recovery-demo \
  --recovery-health-url http://127.0.0.1:8099/health \
  --recovery-ledger-file .bench-recovery-ledger.json
```

Terminal 3:

```sh
cd frontend
VITE_API_URL=http://localhost:8081 npm run dev -- --port 5174
```

Open http://localhost:5174/?page=incidents. Trigger a real unhealthy state:

```sh
curl -fsS http://127.0.0.1:8099/health
curl -fsS -X POST http://127.0.0.1:8099/fail
curl -fsS http://localhost:8081/api/incidents -H 'Authorization: Bearer dev-admin-token'
```

Inspect `/api/incidents/INCIDENT_UUID` for evidence, policy decisions, actual
execution and verification events. Poll `/health` and the incident until RESOLVED
or ESCALATED. An exit-zero restart alone never resolves the incident.

Prohibited action evaluation (does not execute anything):

```sh
curl -fsS http://localhost:8081/api/recovery/policy/evaluate \
  -H 'Authorization: Bearer dev-admin-token' -H 'Content-Type: application/json' \
  -d '{"asset":"recovery-demo","service_id":"flight-controller","action_id":"RUN_COMMAND"}'
```

The configured node account must already have Docker access. The executor maps
the fixed service ID to a configured container and calls Docker with argument
arrays. Docker access is privileged; use only a controlled local machine. A
recovery-enabled node is dedicated to service monitoring and rejects workflows.
Create `.torch-recovery-disabled` in the processes' working directory to block
new repairs. Already accepted Docker operations may still finish. Restarting the
control plane escalates incomplete incidents instead of replaying repairs.

An unresolved failure stays latched to prevent endless repairs. New incidents
require a healthy observation after terminal state; cooldown also applies.
The first slice permits one repair per incident and does not automatically retry
uncertain executions. Do not delete the node operation ledger to bypass this.

## Verified implementation and results (2026-10-17)

The live-model path is implemented and exercised against the actual Docker test
service. OpenAI uses the configured `TORCH_AGENT_MODEL` (tested with `gpt-4.1`),
strict JSON schema and independent Rust validation. A `DiagnosisProvider` trait
isolates provider logic. `TORCH_RECOVERY_DIAGNOSIS=stub` explicitly selects a
deterministic fixture and labels its assessment. There is no fallback to the stub
after provider failure: provider failures escalate. Responses have bounded size
and a 45-second request timeout. Logs are labeled untrusted; the model can only
recommend `RESTART_SERVICE` or `ESCALATE`. It cannot choose a different target.
See the [official structured-output contract](https://developers.openai.com/api/docs/guides/structured-outputs?api-mode=chat).

Verified live incident: `d6190640-0432-49ad-bcf2-e3434f82b028`.

- Detection: `2026-10-17T16:54:30.245381Z`.
- Authorized execution created: `2026-10-17T16:54:32.648885Z`.
- Node acknowledged: `2026-10-17T16:54:32.689519Z`.
- Actual restart completed: `2026-10-17T16:54:34.945289Z`.
- Verified resolution: `2026-10-17T16:54:40.242315Z`.
- Detection to verified recovery: **9.997 seconds**, one restart attempt.

This is one local test result, not a fleet benchmark. A separate live-model run
took 13.955 seconds. The database retains both histories. No engineering-time
savings or fleet reliability statistics have been estimated.

To repeat after allowing at least 60 seconds since the last repair completed:

```sh
bash scripts/recovery-check.sh
```

This checks initial health, confirms a prohibited action is denied, injects a
failure, waits up to 90 seconds, requires a new RESOLVED incident, checks actual
health again, and prints the diagnosis and execution evidence. It exits nonzero
on escalation or timeout. It does not start services; use the terminal commands
above first. `BENCH_API_URL`, `BENCH_ADMIN_TOKEN`, and `TORCH_DEMO_URL` override its
local defaults.

Validation: `make test` passes **32 tests**, with one existing live-provider unit
test intentionally ignored; live recovery was exercised separately. Strict
workspace Clippy, Rust formatting, and the frontend production build pass.
Tests cover state-transition skips, allowlist/disable/retry/cooldown/concurrency
decisions, stale requests, duplicate execution replay protection, latching repeated
failures, failed and inconsistent results, verification, and restart/deadline
escalation. Existing workflow/agent tests continue passing. Authenticated incident
reads work; unauthenticated requests return 401. Prohibited `RUN_COMMAND` targeting
`flight-controller` returns denied and creates an audit entry without dispatch.

The node and control plane use isolated recovery ports/database, leaving the
original Bench demo on port 8080. The recovery UI is on port 5174. Browser visual
QA could not run because no browser connection was available; UI compilation and
HTTP delivery were verified.

## Fleet telemetry and overview APIs

Service observations are durably stored in `service_observations` as they arrive
from a connected node. Each record contains the asset, service, health state,
bounded summary/evidence, observation time, and correlation ID. This preserves
the telemetry stream independently of incident state and gives the fleet UI a
queryable history.

The control plane exposes:

```text
GET /fleet/overview
GET /telemetry/{asset}?limit=100
```

`/fleet/overview` reports registered assets, recently online assets, degraded
assets, active incidents, and observations received in the last five minutes.
The dashboard overview uses these values and displays the latest observations
for the first registered asset. The existing authenticated incident APIs remain
the source for diagnosis, policy, execution, and verification detail.

## Scope and remaining limitations

- This is the first local recovery slice: one operator-configured Docker service,
  one restart per incident, one control plane, a dedicated recovery node. It is not
  a general systemd/fleet recovery release. Additional diagnostic tool requests
  and multi-step model planning are not implemented.
- Verification requires a running container, successful HTTP health responses,
  at least three healthy samples by default, and at least four seconds of
  continuous observed health. Stale telemetry is rejected. Missing observations
  or expired operation/verification deadlines escalate.
- Immediately before executing a repair, the node checks health again and
  refuses an unnecessary restart if the service has already recovered.
- Node disconnect and control-plane restart escalate uncertain work; there is no
  automatic replay. A pending node ledger entry blocks later repairs until an
  operator inspects the service. Exactly-once physical execution through power
  loss is not claimed.
- `POST /api/incidents/ID/cancel` stops further incident progression; an already
  accepted Docker restart may finish. The UI explicitly states this. The disable
  file blocks new repairs on both sides, but is not a hardware emergency stop.
- Shared development authentication and plaintext local gRPC are retained.
  A configured node trusts its control-plane connection; there are no signed
  repair grants or per-device TLS identities. Use only the controlled local demo.
- Diagnostic redaction is conservative keyword-based line removal, not a general
  secrets detector. Only use non-sensitive fixture logs. Docker access remains a
  privileged host capability; the service container itself is read-only and
  drops Linux capabilities.
- Unit/integration tests cover the listed failure paths, but a full network/power
  fault-injection matrix and live-model prompt-injection evaluation are still
  future work. Model-schema rejection is tested independently of execution policy.

Next priorities: device-specific authentication and signed authorization, an
unprivileged systemd service adapter, durable agent/control-plane reconciliation,
and broader fault-injection/evaluation coverage before a real hardware pilot.

## Files changed

- `proto/bench.proto`: additive observations, repair request/ack/result messages.
- `migrations/0007_autonomous_recovery.sql`, `0008_recovery_verification.sql`:
  incident/execution/audit persistence and verification timing.
- `crates/control-plane/src/recovery.rs`, `recovery_model.rs`: orchestration,
  policy, authenticated incident/cancel APIs, provider abstraction and tests.
- Control-plane `main.rs`, `api.rs`, `grpc.rs`, and `agent/tests.rs`: integration,
  session identity binding, bounded dispatch, and existing test fixture updates.
- `crates/node/src/recovery.rs`, node `main.rs` and `Cargo.toml`: monitor,
  structured restart, durable ledger, bounded diagnostics and connection cleanup.
- `frontend/src/Incidents.tsx`, `frontend/src/main.tsx`: incident view and navigation.
- `examples/recovery/service.py`, `compose.recovery.yml`,
  `scripts/recovery-check.sh`: reproducible service, failure injection and exercise.
- `.gitignore` and this document: local-state exclusions, operating instructions,
  measurements and limitations.
