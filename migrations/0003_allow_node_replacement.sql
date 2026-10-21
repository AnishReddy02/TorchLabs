-- A physical asset may be reattached to a reinstalled or replacement Bench Node.
-- Keep historical node identities while assets.node_id identifies the current one.
ALTER TABLE nodes DROP CONSTRAINT IF EXISTS nodes_asset_key_key;
CREATE INDEX IF NOT EXISTS nodes_asset_key_idx ON nodes(asset_key);
