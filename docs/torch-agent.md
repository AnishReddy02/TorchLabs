# Torch Agent V0

The repository retains its Bench crate and command names. Torch Agent is a new feature in the existing control plane and React application.

## Run the investor demo

Docker Desktop must be running. From the repository root:

```bash
make setup
make infra
```

Terminal 1 (keep it running):

`make dev-control-plane` automatically loads the repository's local, git-ignored `.env` file. If it already contains `OPENAI_API_KEY`, run just `make dev-control-plane`; no manual export is needed. Otherwise configure the environment as below.

```bash
export OPENAI_API_KEY='your-key'
export TORCH_AGENT_MODEL='gpt-4.1'
make dev-control-plane
```

Terminal 2:

```bash
cargo run -p bench-node -- --asset-id Rover-03 \
  --identity-file .bench-agent-rover-id \
  --control-plane http://localhost:50051
```

Terminal 3:

```bash
make agent-demo-setup
make frontend
```

Open http://localhost:5173, choose **Torch Agent**, set the target asset and time limit, and enter:

> Test the latest rover-demo build on an available Rover-03 and investigate any failures.

The agent inspects real context, reserves the named simulated rover, selects the fixture build and approved workflow, and pauses. Review the displayed asset, bundle hash, workflow and reason; click **Approve**. The node downloads the archive and runs the actual test. Its configured and expected localization frames differ, so the test exits 42. Logs and result artifacts contain the observed values. The model reads those records and generates its own structured diagnosis. Cleanup is a real workflow step. The agent runtime releases the lease and retains the timeline/report. A workflow FAILED result and an agent COMPLETED investigation are different states.

The control surface supports **Pause / Resume**, authenticated mid-session instructions, **Deny**, and **Cancel & release**. Pause stops new agent decisions between actions; it does not freeze a process already executing on the node. Cancel requests workflow cancellation, cleanup, and lease release. The selected asset is enforced by the control plane, not merely included in the prompt. Refreshing the browser preserves sessions, controls, decisions, and the audit timeline.

No diagnostic text is hard-coded. The only scripted model responses exist inside Rust tests and are not selectable from the application. The demo node runs on your local machine; it does not move a physical robot.

## Configuration

| Variable | Purpose / default |
|---|---|
| `OPENAI_API_KEY` | Required for live reasoning. Server environment only; never sent to browser or stored in DB. |
| `TORCH_AGENT_MODEL` | OpenAI model with structured outputs, demo setting `gpt-4.1`. Account must have access. |
| `DATABASE_URL` | Existing default `postgres://bench:bench@localhost:5432/bench` |
| `BENCH_ADMIN_TOKEN` | Existing API token; local default `dev-admin-token` |
| `BENCH_NODE_TOKEN` | Existing node token; local default `dev-node-token` |
| `BENCH_API_URL` | Setup script API address; default `http://localhost:8080` |
| `VITE_API_URL` / `VITE_API_TOKEN` | Existing browser API address/token settings |

The model adapter follows the [OpenAI JSON output documentation](https://developers.openai.com/api/docs/guides/structured-outputs). It uses HTTPS, a 60-second request deadline and server-side Rust deserialization. Model errors fail the session and trigger cleanup. Asset metadata and bounded logs are sent to OpenAI; only use non-sensitive demo data in V0.

## Architecture and boundaries

```text
React Agent page → persisted session worker → AgentModel
                            ↓ typed intent
                  policy + exact approval checks
                            ↓
                 existing authenticated Axum router
                            ↓
                 reservations / runs / node gRPC
                            ↓
                    existing node executor
```

`agent/model.rs` defines the provider-independent `AgentModel`, closed `Action` enum and structured diagnosis. `agent/tools.rs` invokes the same Axum router as human clients using a server-owned token. It cannot accept URLs, shell strings or arbitrary request bodies from the model. `agent/runtime.rs` records actions and observations, pauses for approval and enters a cleanup phase on completion or failure. `agent/mod.rs` implements authenticated session, decision and workflow-approval endpoints. Migration 0005 adds sessions/events and associates runs with their agent session; reservation owners also encode session identity, closing the crash window between reservation creation and the next model turn.

All existing workflows are denied for agent execution until an operator reviews and approves their exact YAML:

```bash
curl -fsS -X POST http://localhost:8080/api/agent/workflows/WORKFLOW_UUID/approve \
  -H "Authorization: Bearer ${BENCH_ADMIN_TOKEN:-dev-admin-token}" \
  -H 'Content-Type: application/json' -d '{}'
```

Only approve non-actuating demo tests/diagnostics whose commands and cleanup you have reviewed. Approval is not a sandbox or a static proof that shell scripts are safe. Workflow edits invalidate the hash. Bundle transfer and deploy/configure phases require a separate human decision. The start-run handler checks session ownership and the live approval inside its transaction. Reads and reservations are automatic. The model cannot create or approve workflows, upload bundles, flash firmware, or issue shell commands. Run mutation is limited to that session's own runs.

Sessions have a 20-minute deadline, 30-minute reservation leases, bounded model output/context and an action limit. The single worker retries cleanup without asking the model. It requests active-run cancellation, waits for a terminal workflow state, then releases the exact owned reservation. On restart, interrupted sessions enter cleanup rather than replaying a possibly executed action. Ctrl+C requests cleanup before shutdown, with a bounded grace period. Disconnected hardware cannot provide a physical cleanup guarantee; inspect recorded cleanup step results. Lease expiry prevents permanent reservation stranding if the service is down.

## HTTP interface

- `GET/POST /api/agent/sessions` — history / create with `{ "prompt": "...", "target_asset": "Rover-03", "max_runtime_minutes": 20, "stop_after_failure": true }`
- `GET /api/agent/sessions/:id` — status, report, pending approval, ordered events
- `POST /api/agent/sessions/:id/approval` — `{ "approved": true, "request_id": "UUID" }`
- `POST /api/agent/sessions/:id/pause` and `/resume` — control decisions between tool actions
- `POST /api/agent/sessions/:id/instructions` — `{ "message": "Focus the report on localization configuration" }`
- `POST /api/agent/sessions/:id/cancel` — cancel, cleanup and release
- `POST /api/agent/workflows/:id/approve` — operator approves current workflow hash

Every agent endpoint requires the existing Bearer API token. Events show operational actions, actual results, and the final diagnosis—not private reasoning. Unknown tools and extra arguments are rejected before execution. Logs are bounded to the last 40 chunks, 4096 bytes each; artifacts are inspected as metadata, not arbitrary file contents. The complete underlying run remains available through existing APIs.

## Verification

```bash
make infra
make test
cargo fmt --all -- --check
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
cd frontend && npm run build
# Back at the repo root:
make demo
```

SQLx tests create isolated databases and test allowed/denied tools, ownership, approval pause/resume/denial/replay, changed workflow rejection, model failure, cancellation, deadlines, restart recovery, persisted events, and log evidence. `make demo` tests the existing node stack plus the real seeded localization failure. These tests do **not** verify model intelligence. A live API key and a manual approval in the Agent page are required to verify the complete live-model demo.

## Limits and next steps

- Single control-plane process and sequential worker. A slow model call can delay another session's cancellation; the node still enforces step deadlines. No distributed worker coordination.
- Transient provider rate limits receive bounded retries; there is no token-budget accounting, calibrated confidence or evaluation dataset yet.
- No persistent installation: bundles are extracted into per-run workspaces and removed afterward.
- Existing localhost development authentication is shared-admin. Human approval is not per-user identity or RBAC; existing non-agent read/download routes keep their prior authentication behavior.
- Workflow hashes protect YAML changes but cannot attest external programs or a bundle's safety. Existing shell execution remains trusted and unsandboxed.
- A node disconnect or service crash can leave physical cleanup unconfirmed. V0 must not be used for actuator control. Durable node job recovery and hardware-specific safe-stop procedures are required before such use.
- No customer traction or live-model success is implied by the test fixtures.

Production priorities: durable dispatch/cancel acknowledgement and node recovery; scoped user and agent identities; signed workflows/bundles with sandboxed execution; secret redaction and artifact access controls; measured model/tool evaluations and reliability budgets.
