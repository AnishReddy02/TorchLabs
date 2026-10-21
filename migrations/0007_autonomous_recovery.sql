CREATE TABLE incidents (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  asset_id UUID NOT NULL REFERENCES assets(id),
  service_id TEXT NOT NULL,
  incident_type TEXT NOT NULL,
  severity TEXT NOT NULL CHECK (severity IN ('LOW','MEDIUM','HIGH','CRITICAL')),
  status TEXT NOT NULL CHECK (status IN ('DETECTED','INVESTIGATING','DIAGNOSED','AUTHORIZATION_PENDING','REMEDIATING','VERIFYING','RESOLVED','ESCALATED','FAILED','CANCELLED')),
  detected_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  resolved_at TIMESTAMPTZ,
  diagnostic_evidence JSONB NOT NULL DEFAULT '{}'::jsonb,
  diagnosis JSONB,
  proposed_remediation JSONB,
  remediation_attempts INTEGER NOT NULL DEFAULT 0,
  verification_result JSONB,
  verification_started_at TIMESTAMPTZ,
  healthy_observations INTEGER NOT NULL DEFAULT 0,
  escalation_reason TEXT,
  failure_latched BOOLEAN NOT NULL DEFAULT true,
  correlation_id UUID NOT NULL DEFAULT gen_random_uuid(),
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX one_active_incident_per_service
  ON incidents(asset_id, service_id)
  WHERE status NOT IN ('RESOLVED','ESCALATED','FAILED','CANCELLED');
CREATE INDEX incidents_created_idx ON incidents(created_at DESC);

CREATE TABLE remediation_executions (
  id UUID PRIMARY KEY,
  incident_id UUID NOT NULL REFERENCES incidents(id) ON DELETE CASCADE,
  action_id TEXT NOT NULL,
  service_id TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('AUTHORIZED','DISPATCHED','ACKNOWLEDGED','EXECUTED','FAILED','REJECTED','EXPIRED')),
  idempotency_key TEXT NOT NULL UNIQUE,
  policy_decision JSONB NOT NULL,
  authorization_context JSONB NOT NULL,
  deadline TIMESTAMPTZ NOT NULL,
  acknowledged_at TIMESTAMPTZ,
  completed_at TIMESTAMPTZ,
  output TEXT,
  error TEXT,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX remediation_incident_idx ON remediation_executions(incident_id, created_at);

CREATE TABLE incident_events (
  id BIGSERIAL PRIMARY KEY,
  incident_id UUID NOT NULL REFERENCES incidents(id) ON DELETE CASCADE,
  kind TEXT NOT NULL,
  actor TEXT NOT NULL,
  data JSONB NOT NULL DEFAULT '{}'::jsonb,
  correlation_id UUID NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX incident_events_incident_idx ON incident_events(incident_id, id);
