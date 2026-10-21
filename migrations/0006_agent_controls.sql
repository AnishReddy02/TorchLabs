ALTER TABLE agent_sessions
    ADD COLUMN paused BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN control_version BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN constraints JSONB NOT NULL DEFAULT '{}'::jsonb;

