CREATE EXTENSION IF NOT EXISTS pgcrypto;

CREATE TABLE nodes (
  id UUID PRIMARY KEY,
  asset_key TEXT NOT NULL UNIQUE,
  connected BOOLEAN NOT NULL DEFAULT false,
  state TEXT NOT NULL DEFAULT 'OFFLINE',
  last_seen_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE assets (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  name TEXT NOT NULL UNIQUE,
  node_id UUID UNIQUE REFERENCES nodes(id),
  status TEXT NOT NULL DEFAULT 'OFFLINE' CHECK (status IN ('OFFLINE','AVAILABLE','RESERVED','RUNNING','ERROR')),
  hostname TEXT, os TEXT, architecture TEXT,
  metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
  last_seen_at TIMESTAMPTZ,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE reservations (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  asset_id UUID NOT NULL REFERENCES assets(id),
  owner TEXT NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  expires_at TIMESTAMPTZ NOT NULL,
  status TEXT NOT NULL DEFAULT 'ACTIVE' CHECK (status IN ('ACTIVE','RELEASED','EXPIRED'))
);
CREATE UNIQUE INDEX one_active_reservation_per_asset ON reservations(asset_id) WHERE status = 'ACTIVE';
CREATE INDEX reservations_expiry ON reservations(expires_at) WHERE status = 'ACTIVE';

CREATE TABLE workflows (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  name TEXT NOT NULL,
  definition_yaml TEXT NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE workflow_runs (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  workflow_id UUID NOT NULL REFERENCES workflows(id),
  asset_id UUID NOT NULL REFERENCES assets(id),
  reservation_id UUID REFERENCES reservations(id),
  status TEXT NOT NULL DEFAULT 'QUEUED' CHECK (status IN ('QUEUED','PREPARING','RUNNING','CLEANING_UP','PASSED','FAILED','CANCELLED')),
  error TEXT,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  started_at TIMESTAMPTZ,
  completed_at TIMESTAMPTZ,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX workflow_runs_created ON workflow_runs(created_at DESC);

CREATE TABLE workflow_steps (
  id UUID PRIMARY KEY,
  run_id UUID NOT NULL REFERENCES workflow_runs(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  phase TEXT NOT NULL CHECK (phase IN ('setup','test','cleanup')),
  position INTEGER NOT NULL,
  status TEXT NOT NULL DEFAULT 'PENDING' CHECK (status IN ('PENDING','RUNNING','PASSED','FAILED','CANCELLED')),
  exit_code INTEGER,
  error TEXT,
  started_at TIMESTAMPTZ,
  completed_at TIMESTAMPTZ
);
CREATE INDEX workflow_steps_run ON workflow_steps(run_id, position);

CREATE TABLE logs (
  id BIGSERIAL PRIMARY KEY,
  run_id UUID NOT NULL REFERENCES workflow_runs(id) ON DELETE CASCADE,
  step_id UUID REFERENCES workflow_steps(id) ON DELETE CASCADE,
  stream TEXT NOT NULL CHECK (stream IN ('stdout','stderr')),
  sequence BIGINT NOT NULL,
  content BYTEA NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX logs_run_sequence ON logs(run_id, id);
