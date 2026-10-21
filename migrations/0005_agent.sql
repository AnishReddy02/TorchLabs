ALTER TABLE workflows ADD COLUMN agent_approved_hash TEXT;
CREATE TABLE agent_sessions (
 id UUID PRIMARY KEY DEFAULT gen_random_uuid(), prompt TEXT NOT NULL,
 status TEXT NOT NULL DEFAULT 'CREATED' CHECK(status IN ('CREATED','RUNNING','WAITING_FOR_APPROVAL','CLEANING_UP','COMPLETED','FAILED','CANCELLED')),
 context JSONB NOT NULL DEFAULT '[]', pending JSONB, decision BOOLEAN,
 report JSONB, error TEXT, outcome TEXT NOT NULL DEFAULT 'COMPLETED',
 cancel_requested BOOLEAN NOT NULL DEFAULT false,
 created_at TIMESTAMPTZ NOT NULL DEFAULT now(), updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 deadline TIMESTAMPTZ NOT NULL DEFAULT now() + interval '20 minutes'
);
CREATE TABLE agent_events (
 id BIGSERIAL PRIMARY KEY, session_id UUID NOT NULL REFERENCES agent_sessions(id),
 kind TEXT NOT NULL, data JSONB NOT NULL, created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX agent_events_session ON agent_events(session_id,id);
ALTER TABLE workflow_runs ADD COLUMN agent_session_id UUID REFERENCES agent_sessions(id);
CREATE INDEX workflow_runs_agent ON workflow_runs(agent_session_id);
