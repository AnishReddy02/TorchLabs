import type {Activity, AgentSession, Artifact, Asset, Bundle, FleetOverview, Incident, Log, Run, RunDetail, Telemetry, Workflow} from './types';
import {tone} from './format';

export type Mode = 'connecting' | 'live' | 'demo';
export type Snapshot = {
  mode: Mode; endpoint: string; now: number; error?: string;
  assets: Asset[]; runs: Run[]; workflows: Workflow[]; bundles: Bundle[];
  incidents: Incident[]; sessions: AgentSession[]; fleet?: FleetOverview;
  activity: Activity[];
  runDetails: Record<string, RunDetail>;
  incidentDetails: Record<string, Incident>;
  sessionDetails: Record<string, AgentSession>;
  telemetry: Record<string, Telemetry[]>;
};
export type MissionInput = {prompt: string; target_asset: string | null; max_runtime_minutes: number; stop_after_failure: boolean};

/** Everything the UI can read or do. Live and demo engines implement the same contract. */
export interface Engine {
  subscribe(fn: () => void): () => void;
  get(): Snapshot;
  watchRun(id: string): () => void;
  watchIncident(id: string): () => void;
  watchSession(id: string): () => void;
  watchTelemetry(asset: string): () => void;
  createRun(input: {asset_id: string; workflow_id: string; bundle_id?: string | null}): Promise<Run>;
  rerun(id: string, bundleId?: string | null): Promise<Run>;
  cancelRun(id: string): Promise<void>;
  reserve(assetId: string): Promise<void>;
  release(assetId: string): Promise<void>;
  createWorkflow(yaml: string): Promise<Workflow>;
  uploadBundle(file: File): Promise<Bundle>;
  startMission(input: MissionInput): Promise<string>;
  decide(sessionId: string, approved: boolean): Promise<void>;
  controlSession(id: string, action: 'pause' | 'resume' | 'cancel'): Promise<void>;
  instruct(id: string, message: string): Promise<void>;
  cancelIncident(id: string): Promise<void>;
}

export const API = (import.meta.env.VITE_API_URL as string | undefined) ?? 'http://localhost:8080';
const token = () => {
  try { return localStorage.getItem('bench_token') ?? (import.meta.env.VITE_API_TOKEN as string | undefined) ?? 'dev-admin-token'; }
  catch { return 'dev-admin-token'; }
};

async function call<T>(path: string, init?: RequestInit): Promise<T> {
  const headers: Record<string, string> = {authorization: `Bearer ${token()}`};
  if (init?.body && !(init.body instanceof FormData)) headers['content-type'] = 'application/json';
  const response = await fetch(`${API}/api${path}`, {...init, headers: {...headers, ...(init?.headers as Record<string, string>)}});
  if (!response.ok) {
    let message = response.statusText;
    try { message = (await response.json()).error ?? message; } catch { /* non-JSON error body */ }
    throw new Error(message || `HTTP ${response.status}`);
  }
  const text = await response.text();
  return (text ? JSON.parse(text) : undefined) as T;
}
const post = <T>(path: string, body: unknown = {}) => call<T>(path, {method: 'POST', body: JSON.stringify(body)});

export async function probe(): Promise<boolean> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 1500);
  try { return (await fetch(`${API}/api/health`, {signal: controller.signal})).ok; }
  catch { return false; }
  finally { clearTimeout(timer); }
}

export function emptySnapshot(mode: Mode): Snapshot {
  return {mode, endpoint: API, now: Date.now(), assets: [], runs: [], workflows: [], bundles: [], incidents: [], sessions: [], activity: [], runDetails: {}, incidentDetails: {}, sessionDetails: {}, telemetry: {}};
}

/** Talks to the Torch control plane, polling lists and any detail views currently on screen. */
export class LiveEngine implements Engine {
  private state = emptySnapshot('live');
  private listeners = new Set<() => void>();
  private watchers = new Map<string, number>();
  private timer?: number;

  constructor() { void this.poll(); this.timer = window.setInterval(() => void this.poll(), 2500); }
  dispose() { clearInterval(this.timer); }
  subscribe(fn: () => void) { this.listeners.add(fn); return () => { this.listeners.delete(fn); }; }
  get() { return this.state; }
  private set(patch: Partial<Snapshot>) { this.state = {...this.state, ...patch, now: Date.now()}; this.listeners.forEach(fn => fn()); }

  private async poll() {
    const [assets, runs, workflows, bundles, incidents, sessions, fleet] = await Promise.allSettled([
      call<Asset[]>('/assets'), call<Run[]>('/runs'), call<Workflow[]>('/workflows'), call<Bundle[]>('/bundles'),
      call<Incident[]>('/incidents'), call<AgentSession[]>('/agent/sessions'), call<FleetOverview>('/fleet/overview'),
    ]);
    const pick = <T>(r: PromiseSettledResult<T>, fallback: T) => r.status === 'fulfilled' ? r.value ?? fallback : fallback;
    const next = {
      assets: pick(assets, this.state.assets), runs: pick(runs, this.state.runs), workflows: pick(workflows, this.state.workflows),
      bundles: pick(bundles, this.state.bundles), incidents: pick(incidents, this.state.incidents),
      sessions: pick(sessions, this.state.sessions), fleet: pick(fleet, this.state.fleet),
    };
    this.set({...next, activity: this.diffActivity(next.runs, next.incidents), error: runs.status === 'rejected' ? String(runs.reason) : undefined});
  }

  /** The control plane has no event feed yet, so the activity stream is derived from status changes between polls. */
  private seen = new Map<string, string>();
  private diffActivity(runs: Run[], incidents: Incident[]): Activity[] {
    const first = this.seen.size === 0;
    const events: Activity[] = [];
    for (const run of runs) {
      const key = `run:${run.id}`;
      if (this.seen.get(key) !== run.status && !first) {
        events.push({id: `${key}:${run.status}`, at: new Date().toISOString(), tone: tone(run.status), text: `${run.workflow_name} on ${run.asset_name} · ${run.status.toLowerCase().replaceAll('_', ' ')}`, href: `/runs/${run.id}`});
      }
      this.seen.set(key, run.status);
    }
    for (const incident of incidents) {
      const key = `incident:${incident.id}`;
      if (this.seen.get(key) !== incident.status && !first) {
        events.push({id: `${key}:${incident.status}`, at: new Date().toISOString(), tone: tone(incident.status), text: `${incident.service_id} on ${incident.asset_name} · ${incident.status.toLowerCase()}`, href: `/incidents/${incident.id}`});
      }
      this.seen.set(key, incident.status);
    }
    if (first) {
      return runs.slice(0, 12).map(run => ({id: `run:${run.id}:${run.status}`, at: run.completed_at ?? run.started_at ?? run.created_at, tone: tone(run.status), text: `${run.workflow_name} on ${run.asset_name} · ${run.status.toLowerCase().replaceAll('_', ' ')}`, href: `/runs/${run.id}`}));
    }
    return [...events, ...this.state.activity].slice(0, 40);
  }

  private watch(key: string, load: () => Promise<void>, every = 1000) {
    void load();
    const id = window.setInterval(() => void load(), every);
    this.watchers.set(key, id);
    return () => { clearInterval(id); this.watchers.delete(key); };
  }
  watchRun(id: string) {
    return this.watch(`run:${id}`, async () => {
      const [run, logs, artifacts] = await Promise.all([call<Run>(`/runs/${id}`), call<Log[]>(`/runs/${id}/logs`), call<Artifact[]>(`/runs/${id}/artifacts`).catch(() => [])]);
      this.set({runDetails: {...this.state.runDetails, [id]: {run, logs, artifacts}}});
    });
  }
  watchIncident(id: string) {
    return this.watch(`incident:${id}`, async () => {
      const incident = await call<Incident>(`/incidents/${id}`);
      this.set({incidentDetails: {...this.state.incidentDetails, [id]: incident}});
    }, 2000);
  }
  watchSession(id: string) {
    return this.watch(`session:${id}`, async () => {
      const session = await call<AgentSession>(`/agent/sessions/${id}`);
      this.set({sessionDetails: {...this.state.sessionDetails, [id]: session}});
    });
  }
  watchTelemetry(asset: string) {
    return this.watch(`telemetry:${asset}`, async () => {
      const rows = await call<Telemetry[]>(`/telemetry/${encodeURIComponent(asset)}?limit=12`);
      this.set({telemetry: {...this.state.telemetry, [asset]: rows}});
    }, 4000);
  }

  async createRun(input: {asset_id: string; workflow_id: string; bundle_id?: string | null}) {
    const run = await post<Run>('/runs', {...input, bundle_id: input.bundle_id ?? null});
    void this.poll(); return run;
  }
  async rerun(id: string, bundleId?: string | null) {
    if (bundleId === undefined) { const run = await post<Run>(`/runs/${id}/rerun`); void this.poll(); return run; }
    const original = this.state.runs.find(r => r.id === id) ?? this.state.runDetails[id]?.run;
    if (!original?.workflow_id) throw new Error('Original run not loaded yet');
    return this.createRun({asset_id: original.asset_id, workflow_id: original.workflow_id, bundle_id: bundleId});
  }
  async cancelRun(id: string) { await post(`/runs/${id}/cancel`); void this.poll(); }
  async reserve(assetId: string) { await post(`/assets/${assetId}/reserve`, {owner: 'local-developer', duration_minutes: 60}); void this.poll(); }
  async release(assetId: string) { await post(`/assets/${assetId}/release`); void this.poll(); }
  async createWorkflow(definition_yaml: string) { const w = await post<Workflow>('/workflows', {definition_yaml}); void this.poll(); return w; }
  async uploadBundle(file: File) {
    const body = new FormData();
    body.append('name', file.name.replace(/\.(tar\.gz|tgz)$/i, ''));
    body.append('file', file);
    const bundle = await call<Bundle>('/bundles', {method: 'POST', body});
    void this.poll(); return bundle;
  }
  async startMission(input: MissionInput) { const created = await post<{id: string}>('/agent/sessions', input); void this.poll(); return created.id; }
  async decide(sessionId: string, approved: boolean) {
    const session = this.state.sessionDetails[sessionId] ?? this.state.sessions.find(s => s.id === sessionId);
    await post(`/agent/sessions/${sessionId}/approval`, {approved, request_id: session?.pending?.request_id});
  }
  async controlSession(id: string, action: 'pause' | 'resume' | 'cancel') { await post(`/agent/sessions/${id}/${action}`); }
  async instruct(id: string, message: string) { await post(`/agent/sessions/${id}/instructions`, {message}); }
  async cancelIncident(id: string) { await post(`/incidents/${id}/cancel`); }
}
