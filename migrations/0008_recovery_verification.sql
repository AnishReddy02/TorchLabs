ALTER TABLE incidents ADD COLUMN healthy_since TIMESTAMPTZ;
ALTER TABLE incidents ADD COLUMN verification_deadline TIMESTAMPTZ;
