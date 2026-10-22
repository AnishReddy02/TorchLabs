import {useState} from 'react';
import {useEngine, useTorch, useWatch} from '../lib/store';
import {Link} from '../lib/router';
import {ago, clock, duration, label, tone} from '../lib/format';
import type {Incident} from '../lib/types';
import {Crumbs, Empty, StatusIcon} from '../ui/primitives';
import {useToast} from '../ui/toast';

const STAGES: {key: string; label: string; match: RegExp}[] = [
  {key: 'detect', label: 'Detect', match: /DETECT/},
  {key: 'diagnose', label: 'Diagnose', match: /DIAGNOS|ASSESS/},
  {key: 'authorize', label: 'Authorize', match: /AUTHORI|POLICY/},
  {key: 'repair', label: 'Repair', match: /REMEDIA|EXECUT|RESTART/},
  {key: 'verify', label: 'Verify', match: /VERIFIED|RESOLVED/},
];

export function IncidentsPage({id}: {id?: string}) {
  const s = useTorch();
  const sorted = [...s.incidents].sort((a, b) => Date.parse(b.detected_at) - Date.parse(a.detected_at));
  const selectedId = id ?? sorted[0]?.id;
  const incident = selectedId ? s.incidentDetails[selectedId] ?? s.incidents.find(i => i.id === selectedId) : undefined;
  const open = sorted.filter(i => !['RESOLVED', 'CANCELLED'].includes(i.status));
  return <>
    <header className="page-head"><Crumbs items={[{label: 'Recovery', href: '/incidents'}, ...(incident ? [{label: `${incident.service_id} · ${incident.asset_name}`}] : [])]}/></header>
    <div className="page split-page">
      <aside className="split-list">
        <div className="recovery-summary"><b>{open.length ? `${open.length} active` : 'All services healthy'}</b><small>{s.incidents.filter(i => i.status === 'RESOLVED').length} recovered automatically</small></div>
        {sorted.map(i => <Link key={i.id} href={`/incidents/${i.id}`} className={`split-item ${i.id === selectedId ? 'on' : ''}`}>
          <StatusIcon status={i.status} size={14}/>
          <span><b>{i.service_id} <span className="on">on</span> {i.asset_name}</b><small>{label(i.status)} · {ago(i.detected_at, s.now)}{i.resolved_at ? ` · ${duration(Date.parse(i.resolved_at) - Date.parse(i.detected_at))} to recover` : ''}</small></span>
        </Link>)}
      </aside>
      <section className="split-main">
        {incident ? <IncidentView incident={incident}/> : <Empty icon="shield" title="No incidents">Torch watches opted-in services on each robot. When one fails, it collects evidence, proposes a bounded repair, checks it against policy, and verifies recovery.</Empty>}
      </section>
    </div>
  </>;
}

function IncidentView({incident}: {incident: Incident}) {
  useWatch('incident', incident.id);
  const s = useTorch();
  const engine = useEngine();
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  const events = incident.events ?? [];
  const t0 = Date.parse(incident.detected_at);
  const resolvedMs = incident.resolved_at ? Date.parse(incident.resolved_at) - t0 : undefined;
  const reached = STAGES.map(st => events.find(e => st.match.test(e.kind)));
  const stopped = ['ESCALATED', 'FAILED', 'CANCELLED'].includes(incident.status);
  const current = reached.findIndex(e => !e);
  const active = !['RESOLVED', 'ESCALATED', 'FAILED', 'CANCELLED'].includes(incident.status);
  const diag = incident.diagnosis ?? undefined;

  return <div className="incident">
    <div className="incident-head">
      <div>
        <span className="eyebrow">{incident.severity} severity · {incident.asset_name}</span>
        <h1>{incident.service_id} {incident.status === 'RESOLVED' ? 'recovered' : stopped ? 'needs a human' : 'is recovering'}</h1>
      </div>
      <div className="ttr">
        <span className="muted">{resolvedMs !== undefined ? 'Detected → verified' : 'Elapsed'}</span>
        <b>{duration(resolvedMs ?? s.now - t0)}</b>
      </div>
    </div>

    <ol className="stages" aria-label="Recovery stages">
      {STAGES.map((st, i) => {
        const ev = reached[i];
        const state = ev ? 'ok' : stopped && i === current ? 'fail' : active && i === current ? 'live' : 'idle';
        return <li key={st.key} className={`stage tone-${state}`}>
          <StatusIcon tone={state} size={16}/>
          <b>{st.label}</b>
          <small>{ev ? `+${duration(Date.parse(ev.created_at) - t0)}` : state === 'live' ? 'in progress' : '—'}</small>
        </li>;
      })}
    </ol>

    {incident.escalation_reason && <p className="notice fail"><b>Escalated:</b> {incident.escalation_reason}</p>}
    {active && <div className="notice"><span>Torch is handling this. You can stop further automatic steps; a restart already accepted by the robot may still finish.</span><button className="btn btn-sm btn-danger" disabled={busy} onClick={async () => { setBusy(true); try { await engine.cancelIncident(incident.id); toast({tone: 'idle', title: 'Investigation stopped'}); } catch (e) { toast({tone: 'fail', title: 'Could not cancel', body: (e as Error).message}); } finally { setBusy(false); } }}>Stop automation</button></div>}

    <div className="incident-grid">
      <section className="panel">
        <h2 className="panel-title">Assessment {diag?.source ? <span className="chip static">{String(diag.source)}</span> : null}</h2>
        {diag ? <>
          {diag.summary != null && <p className="lead">{String(diag.summary)}</p>}
          {diag.likely_cause != null && <p>{String(diag.likely_cause)}</p>}
          <Fields data={diag} skip={['summary', 'likely_cause', 'source']}/>
        </> : <p className="muted">{active ? 'Collecting bounded evidence…' : 'No assessment recorded.'}</p>}
      </section>
      <section className="panel">
        <h2 className="panel-title">Repair</h2>
        {incident.proposed_remediation ? <Fields data={incident.proposed_remediation}/> : <p className="muted">{active ? 'Waiting for diagnosis.' : 'No repair proposed.'}</p>}
        {(incident.executions?.length ?? 0) > 0 && <><h3 className="sub">Executed</h3>{incident.executions!.map((x, i) => <Fields key={i} data={x as Record<string, unknown>}/>)}</>}
        <h3 className="sub">Verification</h3>
        {incident.verification_result ? <Fields data={incident.verification_result}/> : <p className="muted">{active ? 'Pending.' : 'Not verified.'}</p>}
      </section>
    </div>

    <section className="panel">
      <h2 className="panel-title">Timeline</h2>
      <ol className="timeline">
        {events.map(e => {
          const data = (e.data ?? {}) as Record<string, unknown>;
          return <li key={e.id}>
            <StatusIcon tone={/VERIFIED|RESOLVED|AUTHORI/.test(e.kind) ? 'ok' : /FAIL|ESCALAT|DENIED/.test(e.kind) ? 'fail' : /DETECT/.test(e.kind) ? 'wait' : tone(e.kind) === 'idle' ? 'idle' : 'live'} size={13}/>
            <div><div className="t-head"><b>{label(e.kind)}</b><span className="muted">{e.actor}</span><time>{clock(e.created_at)} · +{duration(Date.parse(e.created_at) - t0)}</time></div>
              {data.message ? <p>{String(data.message)}</p> : null}
              {Object.keys(data).some(k => k !== 'message') && <details><summary>Evidence</summary><pre>{JSON.stringify(data, null, 2)}</pre></details>}
            </div>
          </li>;
        })}
        {!events.length && <li className="muted">No events yet.</li>}
      </ol>
    </section>
  </div>;
}

/** Renders control-plane JSON as labeled fields instead of a raw dump; nested objects fall back to code. */
function Fields({data, skip = []}: {data: Record<string, unknown>; skip?: string[]}) {
  const entries = Object.entries(data).filter(([k, v]) => !skip.includes(k) && v != null && v !== '');
  if (!entries.length) return null;
  return <dl className="facts">
    {entries.map(([k, v]) => <div key={k} className="fact-row">
      <dt>{label(k)}</dt>
      <dd>{k === 'confidence' && typeof v === 'number' ? `${Math.round(v * 100)}%`
        : typeof v === 'boolean' ? <span className={`badge tone-${v ? 'ok' : 'fail'}`}>{v ? 'Yes' : 'No'}</span>
        : Array.isArray(v) ? <ul className="plain-list">{v.map((x, i) => <li key={i}>{typeof x === 'object' ? JSON.stringify(x) : String(x)}</li>)}</ul>
        : typeof v === 'object' ? <pre>{JSON.stringify(v, null, 2)}</pre>
        : k.includes('ms') && typeof v === 'number' ? `${v} ms` : String(v)}</dd>
    </div>)}
  </dl>;
}
