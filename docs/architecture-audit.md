# Torch Labs architecture and security audit

Date: 2026-10-17  
Scope: Phase 0 repository audit only. No product behavior was changed.

## Executive summary

Bench is a compact single-control-plane system with a useful working foundation: a Rust/Axum REST API, a tonic bidirectional node stream, PostgreSQL state, a Rust node executor and CLI, a React dashboard, a bounded AI agent workflow, and a Docker-only autonomous-recovery vertical slice. The recovery prototype is disabled by default, restricts the model to proposals, applies deterministic control-plane and node-side checks, persists an idempotency ledger, and requires observed health before resolution.

The repository is suitable for continued incremental development, but it is not ready for an Internet-exposed customer pilot. The most important blockers are global bearer credentials, unauthenticated read and download APIs, no organization ownership model, permissive CORS, plaintext HTTP/gRPC defaults, browser-stored/embed-able admin credentials, an installer that downloads unverified binaries, and a privileged general workflow executor that runs authenticated YAML through `/bin/sh -c` without isolation.

Phase 1 should establish a request identity and tenant boundary before adding fleet, monitoring, or recovery breadth. Tenant columns must be added at the database layer and all access paths converted to scoped repository/service methods. Per-device enrollment and mTLS can then replace the shared node token without rewriting the existing bidirectional gRPC data path.

## Audit method and limitations

The audit inspected the workspace manifests, all Rust source under `crates/`, protobuf contracts, eight migrations, frontend source, Compose files, CI, systemd unit, installation and demo scripts, and existing product documentation. Baseline formatting, linting, Rust tests, and the frontend production build were run.

The working directory does not contain `.git`, so commit history, ignored/untracked state, provenance, and historical dead-code analysis could not be verified. No live OpenAI call was made. No production deployment configuration, reverse proxy, external object store, release registry, or cloud environment exists in this repository to inspect.

## Current architecture

### Components

| Component | Location | Responsibility | Current persistence |
|---|---|---|---|
| Control plane | `crates/control-plane` | REST, scheduling, reservations, workflow dispatch, node stream, agent orchestration, recovery | PostgreSQL plus local bundle/artifact files |
| Node agent | `crates/node` | Inventory, heartbeats, workflow execution, artifacts, Docker health/restart | Node UUID file, workspaces, JSON recovery ledger |
| Shared domain | `crates/common` | Workflow schema, validation, capability matching, token hashing | None |
| Wire protocol | `crates/protocol`, `proto/bench.proto` | Bidirectional tonic stream messages | Generated at build time |
| CLI | `crates/bench-cli` | Assets, reservations, workflows, runs, logs | Plaintext user config file |
| Web UI | `frontend` | Assets, runs, supervised agent, incidents | Admin token in browser local storage or Vite bundle |
| Database | `migrations` | Bench, agent, and recovery state | PostgreSQL 16 in local Compose |
| Demo service | `compose.recovery.yml`, `examples/recovery` | Constrained failing HTTP service | Docker container |

The control plane is a single process and the node routing hub is an in-memory `HashMap<asset_key, sender>`. PostgreSQL is authoritative for domain records, but live dispatch is not durable. Bundles and artifacts are stored on the control-plane filesystem; database rows store paths and metadata.

### Primary data flows

1. **Node connection:** the node opens an outbound bidirectional gRPC stream, sends `Register` first with a shared bearer-like token in the protobuf body, and then emits heartbeats, inventory, run events, logs, artifacts, service observations, and remediation results. The control plane hashes the submitted token and compares it to one configured shared hash, upserts the node/asset, and installs the stream sender in `NodeHub`.
2. **Bench workflow:** an admin creates a workflow, uploads an optional bundle, reserves an asset, and starts a run over REST. The control plane commits the run and then pushes `RunWorkflow` through the in-memory hub. The node downloads the bundle, checks SHA-256, extracts it into a per-run directory, and executes workflow strings with `/bin/sh -c`. Output and artifacts stream back to PostgreSQL/local storage.
3. **Supervised AI agent:** an authenticated operator creates an agent session. A background worker calls an OpenAI-compatible model for a schema-constrained typed action. Read actions are bounded; deployment/configuration actions require a matching human approval snapshot and approved workflow hash. Tool execution calls the local REST API using the global admin token.
4. **Autonomous recovery:** a configured node polls one HTTP health URL for one Docker container and reports bounded evidence. An unhealthy observation creates and advances an incident. A structured model or deterministic stub proposes `RESTART_SERVICE`; deterministic policy checks exact asset/service/action, connection state, attempts, cooldown, and concurrency. The node independently checks the action, target, deadline, and idempotency key before `docker restart`. Multiple later healthy observations are required before resolution.

### Existing features confirmed

- Asset/node registration, inventory, connectivity status, heartbeat timeout, reservations, capability filters, workflows, run dispatch/cancellation, logs, bundles, artifacts, and audit rows.
- Workflow validation for size, timeouts, relative working directories, and artifact paths; per-run workspaces; bundle digest validation; artifact size limits; process-group cancellation.
- A schema-constrained supervised agent with typed tools, bounded session lifetime, pause/resume/cancel, exact workflow approval hashes, and cleanup behavior.
- Recovery disabled by default, an emergency-disable file, exact single asset/service allowlists, one maximum restart attempt, cooldown/concurrency checks, durable database execution records, a local node ledger, stale request rejection, bounded Docker diagnostics, and multi-observation recovery verification.
- CI checks Rust formatting, strict Clippy, workspace tests with PostgreSQL, and the frontend production build.

## Database assessment

The eight migrations define nodes, assets, reservations, workflows, runs, steps, logs, bundles, artifacts, generic audit events, agent sessions/events, incidents, remediation executions, and incident events.

Strengths:

- Foreign keys cover most parent/child relations.
- Partial uniqueness prevents two active reservations per asset and two active incidents per asset/service.
- Remediation idempotency keys are globally unique.
- Incident transitions are applied with compare-and-set updates and timeline writes in a transaction.
- Relevant run, event, incident, reservation-expiry, and artifact indexes exist.

Weaknesses:

- There is no organization, user, membership, role, fleet, enrollment token, or device credential model.
- Every current resource is globally addressable. No table has `organization_id`, so tenant isolation cannot be expressed or enforced.
- `assets.name` is globally unique; customer-scoped uniqueness will require a controlled constraint migration.
- Nodes are historical identities but have no credential relationship, revocation state, expiration, or ownership constraint.
- Audit actors are free text (usually `admin`) and are not linked to a user, device, organization, request, or source address.
- Retention/partitioning is absent for logs, events, audit events, and telemetry-like data.
- Several state changes span database commit and in-memory dispatch without an outbox. Dispatch failure is compensated in some paths but process death between commit and send remains ambiguous.
- Migration execution occurs automatically on every control-plane start; production rollout/locking/rollback policy is undocumented.

## Authentication and authorization assessment

### REST

Mutations generally call a local `authorize` helper that accepts one global admin bearer token. Agent and recovery modules duplicate this logic. There are no users, sessions, service accounts, roles, permissions, organization context, credential expiry, or revocation.

Many sensitive reads do not authenticate: assets, nodes, reservations, workflows, runs, logs, bundles, artifacts, and bundle/artifact downloads. Incident and agent reads do authenticate, but still use the global admin identity. UUID entropy is the only barrier for direct downloads, and list APIs disclose IDs.

The router is duplicated at both root and `/api` for the main API, increasing surface area and policy drift risk. CORS is fully permissive. Only multipart bundles have an explicit 100 MiB Axum body limit; global JSON/body/time/concurrency limits are absent. Error handling sometimes logs database errors appropriately, while tonic's `internal` helper returns raw SQLx error text to nodes.

### gRPC/device identity

All nodes share `BENCH_NODE_TOKEN`, defaulting to `dev-node-token`. The registration payload carries this token inside plaintext gRPC unless an external TLS terminator is supplied. There is no one-time enrollment, approval, per-device secret/certificate, rotation, revocation, expiry, nonce, or replay defense. A valid shared token can claim an arbitrary asset key and replace its current node association.

After registration, envelope node/asset identity is checked against the established stream, which is a good session-level invariant. However, control messages themselves are not signed and node trust is entirely the transport/session trust.

### Credential handling

- The control plane, node, CLI fallback, examples, and frontend all know development defaults.
- The CLI saves the admin token as plaintext JSON and does not explicitly set restrictive file permissions.
- The UI reads the admin token from `localStorage` or `VITE_API_TOKEN`; a Vite value is compiled into public JavaScript.
- Token hashes use unsalted SHA-256. This is acceptable only for high-entropy generated tokens, but the current defaults are low entropy and there is no token metadata/rotation.
- Secrets are not intentionally logged, but there is no shared redaction layer for REST, tracing, workflow output, artifacts, or model context.

## Remote execution and privilege review

There are two distinct execution boundaries:

1. **General Bench workflows** execute authenticated workflow commands through `/bin/sh -c` as the node service account. This is intentionally arbitrary code execution and must remain separate from autonomous recovery authorization. Environment values can also be supplied by workflows. There is no container, seccomp profile, cgroup quota, filesystem namespace, network policy, command signature, or local workflow allowlist.
2. **Recovery** constructs fixed Docker CLI argument arrays and validates a literal configured container identifier; model text never becomes a command. This is materially safer, but access to the Docker socket is effectively root-equivalent on typical Linux hosts. The supplied systemd unit does not currently grant Docker socket access, and it has no systemd recovery adapter.

The systemd unit includes several useful hardening directives and runs as `bench`, but its intended privilege model conflicts with Docker restart unless the user is added to the Docker group or a privileged helper is introduced. General workflow execution also means the `bench` account's filesystem/device permissions define the blast radius.

The installer is root-only, downloads a binary and unit over configurable HTTP using `curl`, performs no checksum or signature verification, defaults to development identity/token values, and writes the node token through a heredoc. It provides no version pin, upgrade transaction, rollback, uninstall, enrollment approval, or installation validation. The README currently presents curl-to-root-shell as the primary path.

## AI and recovery assessment

The supervised agent and recovery diagnosis both use structured schemas and restrict selectable actions. Untrusted logs are identified as data in the recovery system prompt, response size is bounded, and malformed or incomplete responses fail closed. The recovery model provider is behind a trait, though only OpenAI and a test stub exist. The general agent also has typed tools and tests rejecting unknown tool fields.

Gaps:

- Provider configuration, timeout, retries, rate/cost accounting, and error classes are incomplete; recovery has a timeout but no rate limit or cost ledger.
- There is no shared secret-redaction pipeline before model context. Line-based recovery log filtering is easy to evade and does not cover arbitrary evidence fields.
- Recovery diagnosis is one-shot rather than a bounded investigation loop and does not reference durable evidence objects.
- Recovery supports one configured Docker service per node. There is no service inventory table, adapter abstraction, systemd support, recovery-mode hierarchy, safety classification, or human-approval mode.
- Policy is environment-based, global, and loaded only at startup. Authorization context is not backed by a real user/customer grant.
- The local JSON ledger is useful but has no file lock, fsync/directory-sync guarantee, ownership validation, bounded retention, or tamper protection. An entry left incomplete produces an uncertain result and is not retried, which is the correct safety direction.
- Control-plane startup escalates every nonterminal incident rather than reconstructing safe observation-only states. This is conservative but limits availability.
- A node with recovery configured rejects all normal workflows because `recovery.is_some()` is treated as a concurrency conflict. Monitoring and Bench execution therefore cannot currently coexist on the same installed node.

## Frontend assessment

The React application provides asset/run, supervised-agent, and incident views. It clearly labels assessment, policy/execution, and verification in the incident screen. It polls REST and has no frontend-specific server/BFF.

Security and pilot gaps:

- Global admin credentials are stored in `localStorage` or compiled through `VITE_API_TOKEN`.
- There is no login, logout, user identity, organization/fleet selector, role-aware UI, CSRF strategy, CSP configuration, or session expiry.
- The backend is the true missing enforcement layer; hiding controls in React would not provide authorization.
- Artifact links cannot attach authorization headers and currently depend on unauthenticated downloads.
- There are no enrollment, service inventory, policy configuration, credential management, or approval-role flows.

## Reliability and operational weaknesses

- Single control-plane instance and in-memory connection routing; no multi-instance dispatch ownership.
- No durable command outbox or acknowledgment reconciliation for workflows. Recovery is more conservative but still dispatched from memory.
- Node reconnect uses exponential backoff capped at 30 seconds but has no jitter and does not reset the delay after a healthy connection.
- No server keepalive/TLS policy, graceful drain protocol, bounded global request concurrency, or database statement timeout.
- Heartbeat updates and offline sweeps are simple and useful, but health states are incomplete and monitoring lacks debounce beyond incident latching.
- Local filesystem bundles/artifacts prevent horizontal control-plane scaling and have no quota/retention/encryption policy.
- Logs and artifacts can contain secrets; ingestion is bounded per chunk/artifact but total run/log growth is unbounded.
- Artifact gRPC messages carry the entire artifact in memory and have no configured tonic message-size policy documented.
- No Prometheus/OpenTelemetry metrics, readiness check that verifies PostgreSQL, backup/restore procedure, or SLOs.
- Runtime database queries are handwritten and mostly runtime-checked; no offline SQLx metadata validation is present.

## Dead code, duplication, and incomplete implementation

- REST bearer authorization is duplicated across `api.rs`, `agent/mod.rs`, and `recovery.rs`.
- Recovery action/service identifiers and policy knowledge are duplicated between control plane and node without a versioned shared domain type.
- Root and `/api` copies of the main API duplicate the externally reachable surface.
- Node and asset inventory columns duplicate portions of `assets.metadata`; source-of-truth rules are unclear.
- README claims installer/release behavior that the repository cannot publish or verify itself.
- `torch doctor`, organizations/fleets/users, per-device enrollment, systemd service monitoring, multi-service inventory, and pilot documentation do not exist.
- No obvious wholly unreachable Rust module was found, but absence of Git metadata prevents reliable historical/dead-feature analysis.

## Security findings by priority

### Critical before customer exposure

1. Shared default admin/node credentials and no production-mode startup validation.
2. No tenant ownership model; all data and identifiers are global.
3. Sensitive read/download endpoints are unauthenticated.
4. Plaintext device transport and a shared node credential allow impersonation and asset takeover.
5. General workflows provide unsandboxed shell execution on the device under a global admin authority.
6. Root installer downloads unverified code and defaults to insecure HTTP/development credentials.

### High

1. Permissive CORS combined with browser-held admin tokens.
2. No RBAC separation between view, workflow execution, credential management, and recovery approval.
3. No centralized redaction before logs or AI context.
4. Docker socket privilege model is undocumented and conflicts with least privilege.
5. Filesystem downloads lack authorization and retention/quota controls.
6. Commit-then-dispatch gaps can leave uncertain workflow state.

### Medium

1. No global request timeouts/body limits/rate limits.
2. Tokens have no expiry/rotation/revocation metadata and CLI file permissions are not hardened.
3. Raw SQLx details may cross the gRPC error boundary.
4. No metrics, readiness, backup/restore, or formal retention policy.
5. Recovery configuration is global and static rather than resource-scoped and authorized.

## Missing customer-pilot requirements

- Organization/user/membership/RBAC model and enforced tenant scoping.
- Customer login/session or trusted identity-provider integration.
- One-time device enrollment, approval, unique identity, renewal, rotation, and revocation.
- TLS for REST and gRPC; preferably mTLS for nodes.
- Signed/checksummed versioned releases and safe install/upgrade/rollback/uninstall.
- `torch doctor` and machine-readable installation diagnostics.
- Fleet/device groups and privacy-conscious inventory controls.
- Multi-service Docker/systemd adapters and service-level recovery policies.
- Observe/recommend/approve/automatic recovery modes with restrictive inheritance.
- Central redaction, retention, quotas, audit export, metrics, backups, and operational runbooks.
- Tenant/security/integration/end-to-end tests and a customer onboarding UI.

## Recommended implementation order

1. Introduce production/development configuration validation, restrictive CORS, global limits/timeouts, a shared authentication extractor, and protect every non-public endpoint.
2. Add organizations, users, memberships/roles, fleets, organization IDs, and migration/backfill constraints. Convert all database access to require an authenticated organization context.
3. Add a permission catalog and enforce it server-side, keeping workflow execution separate from recovery approval and credential management.
4. Add enrollment tokens and per-device credentials, then TLS/mTLS, rotation, and revocation while preserving the existing stream protocol.
5. Add centralized redaction, upload/log quotas, retention metadata/jobs, and credential/policy audit events.
6. Only after the security foundation, implement onboarding/doctor, service inventory/adapters, and broader policy-controlled recovery.

## Concrete Phase 1 plan

### Slice 1: secure configuration and HTTP perimeter

- Add a typed environment/profile configuration module used by `main.rs` and node startup.
- Require an explicit environment (`development`, `test`, or `production`). Development defaults are available only when an opt-in flag is set; production rejects missing, short, or known development credentials and non-TLS public endpoints.
- Replace duplicated authorization helpers with Axum extractors/middleware that return an `ActorContext` and correlation ID.
- Classify routes as public (`/health` initially), authenticated reads, or permissioned mutations. Protect bundle/artifact downloads and remove the duplicate root API surface after a compatibility deprecation window.
- Configure exact CORS origins, body limits by route, request/response timeouts, concurrency limits, and safe error mapping/redaction.
- Add tests for production startup rejection, CORS, body limits, and protected reads.

### Slice 2: tenant and identity schema

- Add organizations, users, organization memberships, roles, fleets, API credentials, and richer audit events.
- Add nullable `organization_id` to existing top-level resources first, create one legacy organization, backfill, then make columns non-null and add composite foreign keys/unique constraints.
- Add a bootstrap owner workflow that is explicit and one-time; do not create a silent production admin.
- Introduce scoped data-access functions requiring `OrganizationId`; prohibit raw unscoped resource lookup in HTTP handlers and AI tools.
- Add cross-tenant REST and direct repository tests for every identifier class.

### Slice 3: RBAC

- Define permissions in Rust (`device:view`, `logs:read`, `fleet:manage`, `agent:deploy`, `workflow:create`, `workflow:dispatch`, `incident:view`, `remediation:approve`, `recovery_policy:manage`, `credential:manage`).
- Map Owner/Admin/Engineer/Viewer to an explicit permission table or stable code mapping.
- Enforce permissions through middleware plus scoped queries; never rely on frontend visibility.
- Record user ID, organization ID, correlation ID, permission, decision, and target in audit events.

### Slice 4: device enrollment and transport identity

- Add hashed, short-lived, one-time enrollment tokens with fleet/org scope and server-side approval state.
- Exchange an approved enrollment token for a unique device credential/certificate; store only hashes/fingerprints and lifecycle metadata server-side.
- Bind node ID, asset/device ID, and organization in the database and reject reassignment without permission.
- Add credential rotation, overlap window, revocation, expiration, and replay-resistant enrollment consumption in transactions.
- Configure tonic TLS. Prefer mTLS certificates issued by an established internal/customer CA rather than custom cryptography; retain a time-limited token credential mode only for constrained pilots if explicitly approved.
- Add enrollment, expiry, replay, revocation, wrong-tenant, reconnect, and renewal tests.

### Slice 5: sensitive-data controls

- Implement shared structured redaction for logs, provider context, audit metadata, and errors.
- Add organization-configurable retention metadata with conservative defaults and deletion jobs for logs/artifacts/events.
- Add total upload/log quotas in addition to per-item limits.
- Document TLS termination and database/object-storage encryption expectations; audit every credential and recovery-policy change.

## Expected migrations

The exact numbering should follow the repository at implementation time. A safe sequence is:

1. **Identity foundation:** `organizations`, `users`, `organization_memberships`, role constraint/type, `fleets`, `api_credentials`; expand `audit_events` with organization/actor/correlation/source fields.
2. **Tenant columns (nullable):** add `organization_id` to nodes, assets, reservations, workflows, workflow runs (or derive with consistency constraints), bundles, artifacts, agent sessions, incidents, remediation executions, and event/audit records; add `fleet_id` to assets/devices.
3. **Legacy backfill:** create a deterministic legacy organization and fleet, associate all existing rows through parent relationships, and emit a migration audit record. An operator must assign the bootstrap owner before production mode starts.
4. **Tenant constraints:** make ownership columns non-null, add composite uniqueness such as `(organization_id, name)`, composite foreign keys where needed, and organization-leading indexes. Replace global `assets.name` uniqueness in a two-step migration.
5. **Device identity:** add `device_credentials` and `enrollment_tokens` with hashed token, organization/fleet scope, expiry, one-time consumption, approval, revocation, certificate fingerprint/public-key metadata, and rotation lineage.
6. **Policy/audit support:** add immutable credential/policy audit attributes and optional organization security settings. Database triggers or restricted DB roles should protect append-only audit semantics in production.

Large-table `NOT NULL` and constraint validation should be staged (`NOT VALID`/validate where supported) rather than performed in one locking migration. Existing API behavior should remain available only inside the legacy organization during the transition.

## Architectural decisions requiring approval before Phase 1

1. **Identity provider:** use local email/password sessions for the first pilot, or integrate an external OIDC provider. Recommendation: OIDC plus scoped service/API tokens; avoid building password security unless offline deployments require it.
2. **Device authentication:** require mTLS for the pilot, or permit a transitional per-device high-entropy token over TLS. Recommendation: mTLS as the target, with an explicitly time-bounded token mode only if pilot infrastructure cannot issue certificates.
3. **TLS ownership:** terminate TLS inside the Rust services or at a supported reverse proxy. Recommendation: support tonic TLS directly for device identity; allow a documented reverse proxy for browser REST.
4. **Tenant migration:** map all existing data to a generated `legacy` organization/fleet and require explicit owner bootstrap. Recommendation: approve this strategy to preserve current data.
5. **API compatibility:** temporarily preserve both root and `/api` routes or deprecate root routes immediately. Recommendation: preserve for one release with authentication applied equally, emit deprecation headers, then remove.
6. **Workflow trust boundary:** continue allowing arbitrary authenticated workflows for engineers, or restrict them for pilot devices. Recommendation: retain the feature but require a distinct `workflow:dispatch` permission, signed/versioned workflow approval, and a local node mode that disables shell workflows on recovery-only devices.
7. **Artifact storage:** keep local filesystem storage for the first single-instance pilot or introduce object storage now. Recommendation: keep local storage for Phase 1, add organization-scoped authorization/quotas and document single-instance limitations.
8. **Recovery approval semantics:** automatic recovery must remain disabled globally until Phase 5; decide whether Phase 1 should model permission/consent records now. Recommendation: add the permission and auditable organization consent entity now, but do not expand automatic actions.

## Breaking-change risks

- Replacing global asset-name uniqueness with tenant-scoped names affects node registration, hub routing, URLs, CLI lookup, and recovery allowlists. Internal routing should move to immutable device UUIDs before duplicate names are allowed.
- Protecting current read/download endpoints will require CLI/UI header updates and may break existing scripts.
- Removing default tokens will break `make demo` unless development defaults are explicitly enabled by demo scripts.
- mTLS changes node provisioning and reconnect behavior; a compatibility window is needed for already-installed nodes.
- Organization foreign keys touch nearly every query. Partial conversion would create exploitable holes, so each resource family must migrate as a tested vertical slice.
- Securing artifact links requires authenticated fetch/download behavior in the React app rather than plain anchors.
- Tightening workflow execution may affect existing customer scripts and should be separated from recovery restrictions.

## Baseline verification

Executed on 2026-10-17:

| Check | Result |
|---|---|
| `cargo fmt --all -- --check` | Passed |
| `cargo clippy --workspace --all-targets -- -D warnings` | Passed |
| `DATABASE_URL=postgres://bench:bench@localhost:5432/bench cargo test --workspace` | Passed: 32 tests total across crates (22 control-plane, 6 node, 4 common); 0 failed; 1 live-model test ignored; crates with no tests reported 0 |
| `npm run build` in `frontend` | Passed (`tsc -b` and Vite production build) |

The first unconfigured test invocation correctly failed 13 SQLx tests because `DATABASE_URL` was absent; a sandboxed retry then failed local socket access. PostgreSQL was confirmed running via the repository Compose service and the authoritative rerun with database access passed. The live provider test remains intentionally ignored because it requires an explicit real API call and credential. The end-to-end demo scripts, installer on a real Linux/systemd host, TLS, and production deployment were not verified in this audit.

## Phase 0 conclusion

The existing product should be extended, not rewritten. Its strongest reusable pieces are the typed Rust domain boundary, outbound bidirectional node connection, PostgreSQL lifecycle records, safe-by-construction recovery command shape, node-side idempotency checks, verified recovery observations, and existing CI. The immediate engineering priority is a coherent identity/tenant perimeter. Phase 1 should not add more autonomous capability until every resource access and device connection has an authenticated, organization-scoped actor and recovery enablement can be tied to explicit customer authorization.
