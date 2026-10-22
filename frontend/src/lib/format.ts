import type {Run, Step, Tone} from './types';

const OK = ['PASSED', 'SUCCEEDED', 'COMPLETED', 'RESOLVED', 'AVAILABLE', 'HEALTHY', 'APPROVED'];
const FAIL = ['FAILED', 'TIMED_OUT', 'ESCALATED', 'UNHEALTHY', 'ERROR', 'DENIED'];
const LIVE = ['RUNNING', 'PREPARING', 'CLEANING_UP', 'DEPLOYING', 'INVESTIGATING', 'DIAGNOSING', 'REMEDIATING', 'VERIFYING', 'RESERVED', 'BUSY', 'PLANNING', 'ACTIVE'];
const WAIT = ['QUEUED', 'PENDING', 'WAITING_FOR_APPROVAL', 'DEGRADED', 'DETECTED', 'OPEN', 'PAUSED'];

/** One status vocabulary for the whole product: every badge, icon, and cell maps through here. */
export function tone(status: string | undefined): Tone {
  const s = (status ?? '').toUpperCase();
  if (OK.includes(s)) return 'ok';
  if (FAIL.includes(s)) return 'fail';
  if (LIVE.includes(s)) return 'live';
  if (WAIT.includes(s)) return 'wait';
  return 'idle';
}
export const isActive = (status: string) => ['QUEUED', 'PREPARING', 'RUNNING', 'CLEANING_UP'].includes(status);
export const isTerminal = (status: string) => ['PASSED', 'FAILED', 'TIMED_OUT', 'CANCELLED', 'COMPLETED', 'RESOLVED', 'ESCALATED'].includes(status);

export function label(status: string) {
  const s = status.replaceAll('_', ' ').toLowerCase();
  return s.charAt(0).toUpperCase() + s.slice(1);
}

export function ago(value?: string | null, now = Date.now()) {
  if (!value) return 'never';
  const s = Math.max(0, Math.floor((now - Date.parse(value)) / 1000));
  if (s < 10) return 'just now';
  if (s < 60) return `${s}s ago`;
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`;
  return `${Math.floor(s / 86400)}d ago`;
}

export function duration(ms: number | undefined | null) {
  if (ms == null || !Number.isFinite(ms)) return '—';
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${String(s % 60).padStart(2, '0')}s`;
  return `${Math.floor(m / 60)}h ${String(m % 60).padStart(2, '0')}m`;
}

export function runDuration(run: Run, now = Date.now()) {
  const start = run.started_at ?? run.created_at;
  const end = run.completed_at ? Date.parse(run.completed_at) : isActive(run.status) ? now : undefined;
  return end === undefined ? undefined : end - Date.parse(start);
}
export function stepDuration(step: Step, now = Date.now()) {
  if (!step.started_at) return undefined;
  return (step.completed_at ? Date.parse(step.completed_at) : now) - Date.parse(step.started_at);
}

export const shortId = (id: string) => id.slice(0, 8);
export const clock = (value: string) => new Date(value).toLocaleTimeString([], {hour: '2-digit', minute: '2-digit', second: '2-digit'});

export function bytes(n: number) {
  if (n < 1024) return `${n} B`;
  if (n < 1024 ** 2) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 ** 2).toFixed(1)} MB`;
}

export const PHASES = ['prepare', 'deploy', 'configure', 'test', 'collect', 'cleanup'] as const;

/** Collapse a run's steps into one status per workflow phase, for the phase bar. */
export function phaseStates(steps: Step[] = []) {
  return PHASES.map(phase => {
    const inPhase = steps.filter(s => s.phase.toLowerCase() === phase || (phase === 'prepare' && s.phase.toLowerCase() === 'setup'));
    if (!inPhase.length) return {phase, tone: 'none' as const, steps: inPhase};
    const tones = inPhase.map(s => tone(s.status));
    const t: Tone = tones.includes('fail') ? 'fail' : tones.includes('live') ? 'live' : tones.every(x => x === 'ok') ? 'ok' : tones.includes('ok') ? 'live' : 'idle';
    return {phase, tone: t, steps: inPhase};
  });
}

export function median(values: number[]) {
  if (!values.length) return undefined;
  const sorted = [...values].sort((a, b) => a - b);
  const mid = Math.floor(sorted.length / 2);
  return sorted.length % 2 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
}
