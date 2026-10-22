import type {Snapshot} from './engine';
import type {Bundle, Run, Tone} from './types';
import {isActive, median, runDuration, tone} from './format';

export type Attention = {
  id: string; tone: Tone; kind: 'approval' | 'failure' | 'incident' | 'offline';
  title: string; detail: string; href: string; at: string;
  runId?: string; sessionId?: string; fixBundle?: Bundle;
};

const HOUR = 3600_000;

/** The inbox: things a person must decide or fix, newest first. Resolved failures drop out on their own. */
export function needsYou(s: Snapshot): Attention[] {
  const items: Attention[] = [];
  for (const session of s.sessions) {
    if (session.status === 'WAITING_FOR_APPROVAL' && session.pending) {
      items.push({id: `s:${session.id}`, tone: 'wait', kind: 'approval', title: `Approve ${session.pending.bundle.name} → ${session.pending.asset}`, detail: `Torch Agent wants to run ${session.pending.workflow}`, href: `/agent/${session.id}`, at: session.events.at(-1)?.created_at ?? session.created_at ?? new Date(s.now).toISOString(), sessionId: session.id});
    }
  }
  for (const incident of s.incidents) {
    if (!['RESOLVED', 'CANCELLED'].includes(incident.status)) {
      items.push({id: `i:${incident.id}`, tone: tone(incident.status) === 'fail' ? 'fail' : 'live', kind: 'incident', title: `${incident.service_id} unhealthy on ${incident.asset_name}`, detail: incident.status === 'ESCALATED' ? incident.escalation_reason ?? 'Escalated to a human' : `Auto-recovery: ${incident.status.toLowerCase()}`, href: `/incidents/${incident.id}`, at: incident.detected_at});
    }
  }
  // A failure stays open until a later run of the same workflow passes on the same robot.
  // Open failures that share a workflow and failing step are one problem, so they become one row.
  const settled = new Set<string>();
  const groups = new Map<string, Run[]>();
  const sorted = [...s.runs].sort((a, b) => Date.parse(b.created_at) - Date.parse(a.created_at));
  for (const run of sorted) {
    const key = `${run.workflow_name}@${run.asset_name}`;
    if (settled.has(key)) continue;
    if (run.status === 'PASSED' || isActive(run.status)) { settled.add(key); continue; }
    if (['FAILED', 'TIMED_OUT'].includes(run.status) && s.now - Date.parse(run.created_at) < 24 * HOUR) {
      settled.add(key);
      const step = run.steps?.find(st => ['FAILED', 'TIMED_OUT'].includes(st.status));
      const signature = `${run.workflow_name}:${step?.name ?? run.error ?? ''}`;
      groups.set(signature, [...(groups.get(signature) ?? []), run]);
    }
  }
  for (const [signature, runs] of groups) {
    const run = runs[0];
    const failed = run.steps?.find(st => ['FAILED', 'TIMED_OUT'].includes(st.status));
    const others = runs.length - 1;
    items.push({
      id: `r:${signature}`, tone: 'fail', kind: 'failure',
      title: `${run.workflow_name} failing on ${run.asset_name}${others ? ` +${others} more` : ''}`,
      detail: `${failed ? `${failed.name}: ${failed.error ?? `exit ${failed.exit_code}`}` : run.error ?? 'Run failed'}${others ? ` · also ${runs.slice(1).map(r => r.asset_name).join(', ')}` : ''}`,
      href: `/runs/${run.id}`, at: run.completed_at ?? run.created_at, runId: run.id, fixBundle: newerBundle(run, s.bundles),
    });
  }
  for (const asset of s.assets) {
    if (asset.status === 'OFFLINE') items.push({id: `a:${asset.id}`, tone: 'idle', kind: 'offline', title: `${asset.name} is offline`, detail: `${asset.hardware_type} · ${asset.hostname ?? 'no host'}`, href: `/fleet/${asset.name}`, at: asset.last_seen_at ?? new Date(s.now).toISOString()});
  }
  const rank = {approval: 0, incident: 1, failure: 2, offline: 3};
  return items.sort((a, b) => rank[a.kind] - rank[b.kind] || Date.parse(b.at) - Date.parse(a.at));
}

/** Same bundle family, uploaded after the run's bundle: the obvious thing to retry with. */
export function newerBundle(run: Run, bundles: Bundle[]): Bundle | undefined {
  const current = bundles.find(b => b.id === run.bundle_id);
  if (!current) return undefined;
  const family = current.name.replace(/[-_.]?v?\d+(\.\d+)*$/, '');
  return bundles
    .filter(b => b.id !== current.id && b.name.replace(/[-_.]?v?\d+(\.\d+)*$/, '') === family && Date.parse(b.created_at ?? '') > Date.parse(current.created_at ?? ''))
    .sort((a, b) => Date.parse(b.created_at ?? '') - Date.parse(a.created_at ?? ''))[0];
}

export function lastPassing(run: Run, runs: Run[]) {
  return runs
    .filter(r => r.id !== run.id && r.workflow_name === run.workflow_name && r.status === 'PASSED' && Date.parse(r.created_at) < Date.parse(run.created_at))
    .sort((a, b) => (b.asset_id === run.asset_id ? 1 : 0) - (a.asset_id === run.asset_id ? 1 : 0) || Date.parse(b.created_at) - Date.parse(a.created_at))[0];
}

/** Headline numbers for Home, each with an hourly series and a change versus the prior 12 hours. */
export function metrics(s: Snapshot) {
  const done = s.runs.filter(r => r.completed_at && ['PASSED', 'FAILED', 'TIMED_OUT'].includes(r.status));
  const within = (from: number, to: number) => done.filter(r => { const t = Date.parse(r.completed_at!); return t > s.now - from && t <= s.now - to; });
  const loop = (rs: Run[]) => median(rs.map(r => Date.parse(r.completed_at!) - Date.parse(r.created_at)));
  const pass = (rs: Run[]) => rs.length ? rs.filter(r => r.status === 'PASSED').length / rs.length : undefined;
  const hours = Array.from({length: 24}, (_, i) => 23 - i);
  const bucket = (h: number) => within((h + 1) * HOUR, h * HOUR);
  const recent = within(12 * HOUR, 0), prior = within(24 * HOUR, 12 * HOUR);
  const resolved = s.incidents.filter(i => i.resolved_at);
  const mttr = median(resolved.map(i => Date.parse(i.resolved_at!) - Date.parse(i.detected_at)));
  const online = s.assets.filter(a => a.status !== 'OFFLINE').length;
  const labels = hours.map(h => h === 0 ? 'last hour' : `${h}h ago`);
  return {
    loop: {value: loop(recent) ?? loop(done), prior: loop(prior), series: hours.map(h => loop(bucket(h)) ?? NaN), labels},
    pass: {value: pass(recent) ?? pass(done), prior: pass(prior), series: hours.map(h => pass(bucket(h)) ?? NaN), labels},
    runs: {value: recent.length, series: hours.map(h => bucket(h).length), labels},
    online, total: s.assets.length, active: s.runs.filter(r => isActive(r.status)).length,
    mttr, recovered: resolved.length,
  };
}

export function runsByAsset(runs: Run[]) {
  const map = new Map<string, Run[]>();
  for (const r of runs) map.set(r.asset_name, [...(map.get(r.asset_name) ?? []), r]);
  for (const list of map.values()) list.sort((a, b) => Date.parse(b.created_at) - Date.parse(a.created_at));
  return map;
}

export const currentRun = (runs: Run[], assetName: string) => runs.find(r => r.asset_name === assetName && isActive(r.status));
export {runDuration};
