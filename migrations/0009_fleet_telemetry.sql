-- Durable, bounded service observations for fleet health and incident evidence.
-- The recovery orchestrator still owns incident state; this table preserves the
-- observation stream needed for fleet views and later correlation.
CREATE TABLE service_observations (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  asset_id UUID NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
  service_id TEXT NOT NULL,
  healthy BOOLEAN NOT NULL,
  summary TEXT NOT NULL DEFAULT '',
  evidence JSONB NOT NULL DEFAULT '{}'::jsonb,
  observed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  correlation_id UUID NOT NULL DEFAULT gen_random_uuid()
);

CREATE INDEX service_observations_asset_service_idx
  ON service_observations(asset_id, service_id, observed_at DESC);
CREATE INDEX service_observations_observed_idx
  ON service_observations(observed_at DESC);
