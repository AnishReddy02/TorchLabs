import {useState, type FormEvent} from 'react';
import {useEngine, useTorch, useWatch} from '../lib/store';
import {Link, navigate, useLocation} from '../lib/router';
import {needsYou} from '../lib/derive';
import {ago, clock, isTerminal, label} from '../lib/format';
import type {AgentEvent, AgentSession} from '../lib/types';
import {Icon} from '../ui/icons';
import {Badge, Crumbs, Kbd, StatusIcon} from '../ui/primitives';
import {useToast} from '../ui/toast';

export function AgentPage({id}: {id?: string}) {
  const s = useTorch();
  const {query} = useLocation();
  const composing = query.has('new') || (!id && !s.sessions.length);
  const session = id ? s.sessionDetails[id] ?? s.sessions.find(x => x.id === id) : undefined;
  const groups: [string, AgentSession[]][] = [
    ['Needs approval', s.sessions.filter(x => x.status === 'WAITING_FOR_APPROVAL')],
    ['In progress', s.sessions.filter(x => !isTerminal(x.status) && x.status !== 'WAITING_FOR_APPROVAL')],
    ['Finished', s.sessions.filter(x => isTerminal(x.status))],
  ];
  return <>
    <header className="page-head"><Crumbs items={[{label: 'Torch Agent', href: '/agent'}, ...(session ? [{label: truncate(session.prompt, 48)}] : composing ? [{label: 'New mission'}] : [])]}/></header>
    <div className="page split-page">
      <aside className="split-list">
        <Link href="/agent?new=1" className="btn btn-primary btn-sm full"><Icon name="plus" size={13}/>New mission</Link>
        {groups.map(([name, list]) => list.length > 0 && <div key={name} className="list-group">
          <span className="nav-label">{name}<span>{list.length}</span></span>
          {list.map(x => <Link key={x.id} href={`/agent/${x.id}`} className={`split-item ${x.id === id ? 'on' : ''}`}>
            <StatusIcon status={x.paused ? 'PAUSED' : x.status} size={14}/>
            <span><b>{truncate(x.prompt, 70)}</b><small>{x.constraints.target_asset ?? 'Any robot'} · {ago(x.created_at ?? x.events[0]?.created_at, s.now)}</small></span>
          </Link>)}
        </div>)}
      </aside>
      <section className="split-main">
        {composing ? <Composer/> : session ? <SessionView session={session}/> : <div className="agent-intro"><Icon name="agent" size={22}/><h2>Pick a mission or start a new one</h2><p className="muted">The agent tests and investigates on real hardware inside the boundaries you set.</p></div>}
      </section>
    </div>
  </>;
}

const truncate = (t: string, n: number) => t.length > n ? `${t.slice(0, n - 1)}…` : t;

function Composer() {
  const s = useTorch();
  const engine = useEngine();
  const toast = useToast();
  const suggestions = needsYou(s).filter(i => i.kind === 'failure').slice(0, 3).map(i => `Find out why ${i.title.replace(' failing on ', ' fails on ')} and verify a fix on hardware.`);
  const [prompt, setPrompt] = useState('');
  const [asset, setAsset] = useState('');
  const [minutes, setMinutes] = useState(20);
  const [stop, setStop] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  async function submit(e?: FormEvent) {
    e?.preventDefault();
    if (!prompt.trim()) { setError('Describe what the agent should test or investigate.'); return; }
    setBusy(true); setError('');
    try {
      const id = await engine.startMission({prompt: prompt.trim(), target_asset: asset || null, max_runtime_minutes: minutes, stop_after_failure: stop});
      toast({tone: 'live', title: 'Mission started'});
      navigate(`/agent/${id}`);
    } catch (err) { setError((err as Error).message); setBusy(false); }
  }
  return <form className="composer" onSubmit={submit}>
    <span className="eyebrow">New mission · human-supervised</span>
    <h1>What should Torch test or investigate?</h1>
    <div className="prompt-box">
      <textarea autoFocus aria-label="Mission objective" maxLength={4000} value={prompt} onChange={e => setPrompt(e.target.value)} placeholder="e.g. Run perception-latency on every Jetson robot and flag anything over budget."
        onKeyDown={e => { if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) void submit(); }}/>
      <div className="prompt-opts">
        <select aria-label="Target robot" value={asset} onChange={e => setAsset(e.target.value)}><option value="">Any compatible robot</option>{s.assets.filter(a => a.status !== 'OFFLINE').map(a => <option key={a.id}>{a.name}</option>)}</select>
        <div className="segmented sm" role="radiogroup" aria-label="Time limit">{[10, 20, 30, 60].map(m => <button type="button" key={m} role="radio" aria-checked={minutes === m} className={minutes === m ? 'on' : ''} onClick={() => setMinutes(m)}>{m}m</button>)}</div>
        <label className="toggle"><input type="checkbox" checked={stop} onChange={e => setStop(e.target.checked)}/><span/>Stop after first failure</label>
        <button className="btn btn-primary" disabled={busy}>{busy ? 'Starting…' : 'Start mission'} <Kbd>⌘↵</Kbd></button>
      </div>
    </div>
    {error && <p className="form-error" role="alert">{error}</p>}
    {suggestions.length > 0 && <div className="suggestions"><span className="muted">From what’s failing now</span>{suggestions.map(t => <button type="button" key={t} className="chip" onClick={() => setPrompt(t)}>{t}</button>)}</div>}
    <div className="guardrails">
      <div><span className="g-head"><StatusIcon tone="ok" size={13}/>Runs on its own</span><p>Inspect fleet · reserve · run approved workflows · read logs · release</p></div>
      <div><span className="g-head"><StatusIcon tone="wait" size={13}/>Asks you first</span><p>Deploying or switching a software bundle</p></div>
      <div><span className="g-head"><StatusIcon tone="fail" size={13}/>Never</span><p>Arbitrary shell · firmware · safety bypass</p></div>
    </div>
  </form>;
}

function SessionView({session}: {session: AgentSession}) {
  useWatch('session', session.id);
  const s = useTorch();
  const engine = useEngine();
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState('');
  const done = isTerminal(session.status);
  const act = async (fn: () => Promise<void>, ok?: string) => {
    setBusy(true);
    try { await fn(); if (ok) toast({tone: 'ok', title: ok}); } catch (e) { toast({tone: 'fail', title: 'Action failed', body: (e as Error).message}); } finally { setBusy(false); }
  };
  const steer = (e: FormEvent) => { e.preventDefault(); if (!message.trim()) return; void act(() => engine.instruct(session.id, message.trim())).then(() => setMessage('')); };

  return <div className="session">
    <div className="session-head">
      <div>
        <Badge status={session.paused ? 'PAUSED' : session.status}/>
        <h1>{session.prompt}</h1>
      </div>
      {!done && <div className="head-actions">
        {session.paused ? <button className="btn btn-sm" disabled={busy} onClick={() => void act(() => engine.controlSession(session.id, 'resume'))}><Icon name="play" size={12}/>Resume</button>
          : <button className="btn btn-sm" disabled={busy} onClick={() => void act(() => engine.controlSession(session.id, 'pause'))}><Icon name="pause" size={12}/>Pause</button>}
        <button className="btn btn-sm btn-danger" disabled={busy} onClick={() => void act(() => engine.controlSession(session.id, 'cancel'), 'Mission cancelled')}>Cancel & release</button>
      </div>}
    </div>
    <div className="bounds">
      <span><Icon name="fleet" size={12}/>{session.constraints.target_asset ?? 'Any compatible robot'}</span>
      <span><Icon name="clock" size={12}/>{session.constraints.max_runtime_minutes} min limit</span>
      <span><Icon name="lock" size={12}/>Deploys need approval</span>
      <span><Icon name="shield" size={12}/>Shell denied</span>
    </div>
    {session.paused && <p className="notice"><b>Agent decisions are paused.</b> A workflow already running on a robot continues; cancel to stop it safely.</p>}
    {session.error && <p className="notice fail">{session.error}</p>}

    <ol className="thread">
      {session.events.map(e => <ThreadEvent key={e.id} event={e}/>)}
      {!done && session.status !== 'WAITING_FOR_APPROVAL' && !session.paused && <li className="thread-item thinking"><span className="t-icon"><span className="pulse"/></span><span className="muted">Working…</span></li>}
    </ol>

    {session.status === 'WAITING_FOR_APPROVAL' && session.pending && <section className="approval">
      <div className="approval-head"><Icon name="lock" size={16}/><div><span className="eyebrow">Your decision</span><h2>Deploy {session.pending.bundle.name} to {session.pending.asset}?</h2></div></div>
      <p>{session.pending.reason}</p>
      <dl className="facts compact">
        <dt>Workflow</dt><dd>{session.pending.workflow}</dd>
        <dt>Bundle</dt><dd className="mono-sm">{session.pending.bundle.filename}</dd>
        <dt>SHA-256</dt><dd className="mono-sm truncate" title={session.pending.bundle.sha256}>{session.pending.bundle.sha256}</dd>
      </dl>
      <p className="muted">{session.pending.notice}</p>
      {session.decision === null ? <div className="approval-actions">
        <button className="btn" disabled={busy} onClick={() => void act(() => engine.decide(session.id, false), 'Deployment denied')}>Deny</button>
        <button className="btn btn-primary" disabled={busy} onClick={() => void act(() => engine.decide(session.id, true), 'Deployment approved')}><Icon name="check" size={13}/>Approve & run</button>
      </div> : <p className="muted">Decision recorded. Resuming…</p>}
    </section>}

    {session.report && <section className="report">
      <span className="eyebrow">Report · grounded in run evidence</span>
      <h2>{session.report.summary}</h2>
      {session.report.likely_cause !== 'n/a' && <><h3>Likely cause</h3><p>{session.report.likely_cause}</p></>}
      {session.report.evidence.length > 0 && <ul className="evidence plain">{session.report.evidence.map((e, i) => <li key={i}>{e}</li>)}</ul>}
      <h3>Recommended next step</h3><p>{session.report.recommended_next_action}</p>
      <small className="muted">Confidence {Math.round(session.report.confidence * 100)}% · a model estimate, not a calibrated probability</small>
    </section>}

    {!done && session.status !== 'WAITING_FOR_APPROVAL' && <form className="steer" onSubmit={steer}>
      <input value={message} maxLength={1000} onChange={e => setMessage(e.target.value)} placeholder="Steer the agent — applied before its next action" aria-label="Instruction for the agent"/>
      <button className="btn btn-sm" disabled={busy || !message.trim()}><Icon name="send" size={12}/>Send</button>
    </form>}
    <p className="muted thread-foot">Every action is audited by Torch · {s.mode === 'demo' ? 'demo session' : `session ${session.id.slice(0, 8)}`}</p>
  </div>;
}

function ThreadEvent({event}: {event: AgentEvent}) {
  const kind = event.kind;
  const t = kind.includes('FAIL') || kind.includes('DENIED') ? 'fail' : kind.includes('APPROVAL_REQ') ? 'wait' : kind.includes('SUCCEEDED') || kind.includes('GRANTED') || kind === 'COMPLETED' ? 'ok' : 'idle';
  const tool = kind === 'TOOL_CALL';
  const operator = kind === 'OPERATOR_INSTRUCTION';
  const text = String(event.data.message ?? (event.data.action as {tool?: string} | undefined)?.tool ?? event.data.summary ?? '');
  return <li className={`thread-item ${tool ? 'tool' : ''} ${operator ? 'operator' : ''}`}>
    <span className="t-icon">{operator ? <Icon name="user" size={12}/> : tool ? <Icon name="terminal" size={12}/> : <StatusIcon tone={t} size={12}/>}</span>
    <div>
      <div className="t-head"><b>{operator ? 'You' : label(kind)}</b><time>{clock(event.created_at)}</time></div>
      {tool ? <code className="t-code">{text}</code> : <p>{text}</p>}
    </div>
  </li>;
}
