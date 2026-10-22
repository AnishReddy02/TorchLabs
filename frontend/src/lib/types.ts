export type Asset = {
  id: string; name: string; status: string; hostname?: string; os?: string; architecture?: string;
  hardware_type: string; description?: string; capabilities: Record<string, unknown>;
  labels?: Record<string, unknown>; last_seen_at?: string;
};
export type Step = {
  id: string; name: string; phase: string; position?: number; status: string;
  exit_code?: number | null; error?: string | null; started_at?: string | null; completed_at?: string | null;
};
export type Run = {
  id: string; workflow_id?: string; asset_id: string; asset_name: string; workflow_name: string;
  status: string; error?: string | null; bundle_id?: string | null;
  created_at: string; started_at?: string | null; completed_at?: string | null; steps?: Step[];
  // Present in demo data; the control plane may not send these yet.
  bundle_name?: string; commit?: string; actor?: string; trigger?: 'human' | 'agent' | 'recovery';
};
export type Log = { id: number; stream: string; content: string; step_id?: string | null; created_at?: string };
export type Artifact = { id: string; name: string; path: string; size_bytes: number; content_type: string };
export type Workflow = { id: string; name: string; description: string; definition_yaml?: string; version: number; updated_at?: string };
export type Bundle = { id: string; name: string; filename: string; size_bytes: number; sha256: string; created_at?: string };
export type FleetOverview = { total_assets: number; online_assets: number; degraded_assets: number; active_incidents: number; recent_observations: number };
export type Telemetry = { id: string; service_id: string; healthy: boolean; summary: string; observed_at: string };

export type IncidentEvent = { id: number; kind: string; actor: string; created_at: string; data: unknown };
export type Incident = {
  id: string; asset_name: string; service_id: string; status: string; severity: string;
  detected_at: string; resolved_at?: string | null; escalation_reason?: string | null;
  diagnosis?: Record<string, unknown> | null; proposed_remediation?: Record<string, unknown> | null;
  verification_result?: Record<string, unknown> | null; remediation_attempts: number;
  events?: IncidentEvent[]; executions?: unknown[];
};

export type AgentEvent = { id: number; kind: string; data: Record<string, unknown>; created_at: string };
export type AgentSession = {
  id: string; prompt: string; status: string; paused: boolean; decision: boolean | null; error?: string | null;
  created_at?: string;
  constraints: { target_asset?: string | null; max_runtime_minutes: number; stop_after_failure: boolean; software_changes_require_approval?: boolean; arbitrary_shell?: string };
  report?: { summary: string; likely_cause: string; evidence: string[]; recommended_next_action: string; confidence: number } | null;
  pending?: { request_id: string; asset: string; workflow: string; reason: string; bundle: { name: string; sha256: string; filename: string }; notice: string } | null;
  events: AgentEvent[];
};

/** Torch's read of a failed run. Demo data supplies these; live runs ask the agent instead. */
export type Diagnosis = {
  summary: string; likely_cause: string; next_action: string; confidence: number;
  evidence: { text: string; line?: number }[];
};

export type RunDetail = { run?: Run; logs: Log[]; artifacts: Artifact[]; diagnosis?: Diagnosis };

export type Activity = { id: string; at: string; tone: Tone; text: string; href?: string };
export type Tone = 'ok' | 'fail' | 'live' | 'wait' | 'idle';
