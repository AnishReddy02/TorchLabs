CREATE TABLE software_bundles (
  id UUID PRIMARY KEY,
  name TEXT NOT NULL,
  filename TEXT NOT NULL,
  content_type TEXT NOT NULL,
  size_bytes BIGINT NOT NULL CHECK (size_bytes > 0),
  sha256 TEXT NOT NULL,
  storage_key TEXT NOT NULL UNIQUE,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX software_bundles_created ON software_bundles(created_at DESC);

ALTER TABLE workflow_runs
  ADD COLUMN bundle_id UUID REFERENCES software_bundles(id);
