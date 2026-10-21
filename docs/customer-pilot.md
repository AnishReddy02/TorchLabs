# Torch Fleet customer pilot

This pilot is for noncritical Linux test robots and services. It demonstrates
registration, health telemetry, incident investigation, allowlisted recovery,
and independent verification. It must not be used for propulsion, navigation,
motion control, emergency stops, or other safety-critical functions.

## Pilot shape

Start with one organization, one fleet, and up to five Linux test devices. Give
each device a stable asset name and dedicate one low-risk service to recovery.
Keep recovery in observe-only mode until the customer signs off on the exact
service, action, cooldown, and escalation procedure.

The current repository supports a single control-plane instance, PostgreSQL,
outbound node gRPC sessions, one configured Docker recovery service per node,
and local bundle/artifact storage. It does not yet provide tenant isolation,
RBAC, mTLS enrollment, or systemd recovery adapters; these are prerequisites
for a broader customer deployment.

## Required pilot configuration

Use high-entropy credentials and a private network. Production mode rejects the
development tokens and requires explicit CORS origins:

```sh
export BENCH_ENV=production
export DATABASE_URL='postgres://torch:<password>@postgres.internal:5432/torch'
export BENCH_ADMIN_TOKEN='<at-least-24-character-admin-token>'
export BENCH_NODE_TOKEN='<at-least-24-character-node-token>'
export BENCH_CORS_ORIGINS='https://torch.customer.example'
export BENCH_HTTP_ADDR='127.0.0.1:8080'
export BENCH_GRPC_ADDR='127.0.0.1:50051'
export TORCH_RECOVERY_ENABLED=false
```

Place the control plane behind a TLS reverse proxy for browser traffic. Keep
the node connection on a private network until device-specific TLS identity is
implemented. Never commit these values or place them in a frontend build.

## Acceptance run

1. Install PostgreSQL and apply migrations by starting the control plane.
2. Start one node with a unique asset ID and confirm it appears as `AVAILABLE`.
3. Confirm `/api/fleet/overview` reports the device online.
4. Confirm `/api/telemetry/<asset>` contains healthy observations.
5. Enable recovery for the designated noncritical service only.
6. Inject a controlled failure and confirm an incident is created.
7. Review the diagnosis, evidence, policy decision, and proposed action.
8. Confirm the allowlisted restart executes once and the node acknowledges it.
9. Require multiple healthy observations before declaring recovery.
10. Confirm the dashboard timeline and audit evidence are complete.

Record detection time, diagnosis time, remediation time, verified recovery time,
escalations, denied actions, and any human intervention. Treat these as pilot
measurements for this customer and date; do not generalize from one fixture.

## Rollback and escalation

Create `.torch-recovery-disabled` in the control-plane and node working
directories to stop new recovery actions. An already accepted operation may
finish, so this is not an emergency stop. If execution or verification becomes
uncertain, Torch escalates and does not automatically replay the action.

The customer operator owns the device-level emergency procedure. Torch should
be removed from the recovery allowlist before uninstalling the node. Preserve
the incident and telemetry records for review, then follow the customer’s
backup and retention policy.

## Pilot readiness boundary

Torch is credible for a supervised local pilot on noncritical Linux services.
Before expanding to multiple customers or safety-adjacent systems, implement
organization isolation, role permissions, per-device enrollment and revocation,
mTLS, signed agent releases, authenticated artifact access, durable dispatch
reconciliation, retention controls, and systemd service adapters.
