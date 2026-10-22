import {useEffect, useMemo, useRef, useState} from 'react';
import {useEngine, useTorch, useWatch} from '../lib/store';
import {Link, navigate} from '../lib/router';
import {lastPassing, newerBundle} from '../lib/derive';
import {ago, bytes, clock, duration, isActive, phaseStates, runDuration, shortId, stepDuration, tone} from '../lib/format';
import type {Diagnosis, Log, Run} from '../lib/types';
import {Icon} from '../ui/icons';
import {Avatar, Badge, Crumbs, Empty, Kbd, Mono, StatusIcon} from '../ui/primitives';
import {useToast} from '../ui/toast';

export function RunView({id}: {id: string}) {
  useWatch('run', id);
  const s = useTorch();
  const engine = useEngine();
  const toast = useToast();
  const detail = s.runDetails[id];
  const run = detail?.run ?? s.runs.find(r => r.id === id);
  const logs = detail?.logs ?? [];
  const [stepId, setStepId] = useState<string>();
  const [focusLine, setFocusLine] = useState<number>();
  const [busy, setBusy] = useState(false);
  const fix = run ? newerBundle(run, s.bundles) : undefined;
  const baseline = run ? lastPassing(run, s.runs) : undefined;

  const rerun = async (bundleId?: string) => {
    if (!run) return;
    setBusy(true);
    try {
      const next = await engine.rerun(run.id, bundleId);
      toast({tone: 'live', title: 'Rerun queued', body: `${next.workflow_name} on ${next.asset_name}${bundleId && fix ? ` with ${fix.name}` : ''}`, action: {label: 'Back to original', run: () => navigate(`/runs/${run.id}`)}});
      navigate(`/runs/${next.id}`);
    } catch (e) { toast({tone: 'fail', title: 'Could not rerun', body: (e as Error).message}); }
    finally { setBusy(false); }
  };
  const cancel = async () => {
    if (!run) return;
    try { await engine.cancelRun(run.id); toast({tone: 'idle', title: 'Run cancelled', body: 'Cleanup steps still run to leave the robot in a safe state.'}); }
    catch (e) { toast({tone: 'fail', title: 'Could not cancel', body: (e as Error).message}); }
  };
  const investigate = async () => {
    if (!run) return;
    try {
      const sid = await engine.startMission({prompt: `Investigate why run ${run.id} (${run.workflow_name} on ${run.asset_name}) failed. Use its logs and recent runs, and propose the smallest next test.`, target_asset: run.asset_name, max_runtime_minutes: 20, stop_after_failure: true});
      navigate(`/agent/${sid}`);
    } catch (e) { toast({tone: 'fail', title: 'Could not start the agent', body: (e as Error).message}); }
  };

  useEffect(() => {
    const on = (e: KeyboardEvent) => {
      const el = e.target as HTMLElement;
      if (['INPUT', 'TEXTAREA', 'SELECT'].includes(el.tagName) || e.metaKey || e.ctrlKey || document.querySelector('.overlay')) return;
      if (e.key === 'r' && run && !isActive(run.status)) { e.preventDefault(); void rerun(); }
      if (e.key === 'e') {
        e.preventDefault();
        const errors = logs.map((l, i) => l.stream === 'stderr' ? i + 1 : 0).filter(Boolean);
        const next = errors.find(n => n > (focusLine ?? 0)) ?? errors[0];
        if (next) { setStepId(undefined); setFocusLine(next); }
      }
    };
    window.addEventListener('keydown', on);
    return () => window.removeEventListener('keydown', on);
  });

  if (!run) return <>
    <header className="page-head"><Crumbs items={[{label: 'Runs', href: '/runs'}, {label: shortId(id)}]}/></header>
    <div className="page">{s.mode === 'connecting' || s.mode === 'live' ? <div className="skeleton-block"/> : <Empty icon="runs" title="Run not found">It may belong to a different control plane.</Empty>}</div>
  </>;

  const failed = tone(run.status) === 'fail';
  const active = isActive(run.status);
  return <>
    <header className="page-head">
      <Crumbs items={[{label: 'Runs', href: '/runs'}, {label: `${run.workflow_name} · ${shortId(run.id)}`}]}/>
      <div className="head-actions">
        {active && <button className="btn btn-sm btn-danger" onClick={() => void cancel()}>Cancel run</button>}
        {!active && <button className="btn btn-sm" disabled={busy} onClick={() => void rerun()} title="Rerun with identical inputs"><Icon name="rerun" size={13}/>Rerun <Kbd>R</Kbd></button>}
        {!active && failed && fix && <button className="btn btn-sm btn-primary" disabled={busy} onClick={() => void rerun(fix.id)}><Icon name="bolt" size={13}/>Try {fix.name}</button>}
      </div>
    </header>
    <div className="page run-page">
      <section className="run-hero">
        <StatusIcon status={run.status} size={28}/>
        <div>
          <h1>{run.workflow_name} <span className="on">on</span> <Link href={`/fleet/${run.asset_name}`}>{run.asset_name}</Link></h1>
          <p className="run-facts">
            <Badge status={run.status}/>
            <Mono copy={run.id}>{shortId(run.id)}</Mono>
            {run.bundle_name && <span><Icon name="package" size={12}/>{run.bundle_name}</span>}
            {run.commit && <span><Icon name="commit" size={12}/>{run.commit}</span>}
            {run.actor && <span><Avatar name={run.actor}/>{run.actor === 'torch-agent' ? 'Torch Agent' : run.actor}</span>}
            <span><Icon name="clock" size={12}/>{duration(runDuration(run, s.now))}</span>
            <span className="muted">started {ago(run.started_at ?? run.created_at, s.now)}</span>
          </p>
        </div>
      </section>

      <Pipeline run={run} now={s.now} onPick={setStepId}/>

      {failed && (detail?.diagnosis
        ? <DiagnosisCard diagnosis={detail.diagnosis} onLine={n => { setStepId(undefined); setFocusLine(n); }} fixName={fix?.name} onFix={fix ? () => void rerun(fix.id) : undefined} onRerun={() => void rerun()} busy={busy}/>
        : <section className="diagnosis pending"><div><b>No diagnosis yet</b><p>Torch Agent can read this run’s logs, compare recent runs on this robot, and propose the smallest next test.</p></div><button className="btn btn-primary btn-sm" onClick={() => void investigate()}><Icon name="agent" size={13}/>Investigate with Torch Agent</button></section>)}

      {failed && baseline && <ChangedSince run={run} baseline={baseline} bundles={s.bundles}/>}

      <section className="run-split">
        <StepList run={run} now={s.now} selected={stepId} onSelect={id => { setStepId(id); setFocusLine(undefined); }}/>
        <LogViewer logs={logs} run={run} stepId={stepId} focusLine={focusLine} onClearStep={() => setStepId(undefined)}/>
      </section>

      <section className="panel">
        <h2 className="panel-title">Artifacts</h2>
        {detail?.artifacts?.length ? <ul className="artifacts">{detail.artifacts.map(a => <li key={a.id}>
          <Icon name="download" size={14}/>
          <a href={s.mode === 'live' ? `${s.endpoint}/api/artifacts/${a.id}/download` : undefined} aria-disabled={s.mode !== 'live'} onClick={e => { if (s.mode !== 'live') { e.preventDefault(); toast({tone: 'idle', title: 'Demo data', body: 'Artifacts download from a live control plane.'}); } }}>{a.path}</a>
          <span className="muted">{bytes(a.size_bytes)} · {a.content_type}</span>
        </li>)}</ul> : <p className="muted">{active ? 'Artifacts are collected in the collect phase.' : 'No artifacts were collected.'}</p>}
      </section>
    </div>
  </>;
}

function Pipeline({run, now, onPick}: {run: Run; now: number; onPick: (stepId: string) => void}) {
  const phases = phaseStates(run.steps);
  return <ol className="pipeline" aria-label="Workflow phases">
    {phases.map(p => {
      const ms = p.steps.reduce((sum, st) => sum + (stepDuration(st, now) ?? 0), 0);
      return <li key={p.phase} className={`pipe tone-${p.tone}`}>
        <button disabled={!p.steps.length} onClick={() => p.steps[0] && onPick(p.steps[0].id)}>
          <span className="pipe-head">{p.tone === 'none' ? <span className="pipe-none"/> : <StatusIcon tone={p.tone} size={14}/>}{p.phase}</span>
          <span className="pipe-time">{p.steps.length ? (ms ? duration(ms) : '—') : 'no steps'}</span>
        </button>
      </li>;
    })}
  </ol>;
}

function DiagnosisCard({diagnosis, onLine, fixName, onFix, onRerun, busy}: {diagnosis: Diagnosis; onLine: (n: number) => void; fixName?: string; onFix?: () => void; onRerun: () => void; busy: boolean}) {
  const pct = Math.round(diagnosis.confidence * 100);
  return <section className="diagnosis">
    <div className="diag-head">
      <span className="diag-mark"><Icon name="agent" size={14}/></span>
      <span className="eyebrow">Torch diagnosis</span>
      <span className="confidence" title="Model estimate, not a calibrated probability"><span className="meter"><span style={{width: `${pct}%`}}/></span>{pct}% confident</span>
    </div>
    <h2>{diagnosis.summary}</h2>
    <p className="diag-cause">{diagnosis.likely_cause}</p>
    <ul className="evidence">
      {diagnosis.evidence.map((e, i) => <li key={i}>
        {e.line ? <button className="evidence-link" onClick={() => onLine(e.line!)}><span className="line-ref">L{e.line}</span>{e.text}</button> : <span><span className="line-ref muted">ctx</span>{e.text}</span>}
      </li>)}
    </ul>
    <div className="diag-next">
      <div><span className="eyebrow">Next test</span><p>{diagnosis.next_action}</p></div>
      <div className="diag-actions">
        <button className="btn btn-sm" disabled={busy} onClick={onRerun}>Rerun as is</button>
        {onFix && <button className="btn btn-sm btn-primary" disabled={busy} onClick={onFix}><Icon name="bolt" size={13}/>Rerun with {fixName}</button>}
      </div>
    </div>
  </section>;
}

function ChangedSince({run, baseline, bundles}: {run: Run; baseline: Run; bundles: {id: string; name: string}[]}) {
  const name = (id?: string | null) => bundles.find(b => b.id === id)?.name ?? (id ? shortId(id) : 'none');
  // Only inputs that can change the outcome: which hardware, and which software.
  const rows: {k: string; was: string; now: string}[] = [
    {k: 'Robot', was: baseline.asset_name, now: run.asset_name},
    {k: 'Bundle', was: baseline.bundle_name ?? name(baseline.bundle_id), now: run.bundle_name ?? name(run.bundle_id)},
  ];
  const changed = rows.filter(r => r.was !== r.now);
  return <section className="changed">
    <div className="changed-head">
      <Icon name="diff" size={14}/>
      <b>What changed since it last passed</b>
      <Link href={`/runs/${baseline.id}`} className="muted">{shortId(baseline.id)} · {ago(baseline.created_at)} <Icon name="arrow" size={11}/></Link>
    </div>
    <div className="changed-grid">
      {rows.map(r => <div key={r.k} className={r.was !== r.now ? 'diff' : ''}>
        <span className="muted">{r.k}</span>
        {r.was === r.now ? <code>{r.now}</code> : <><code className="was">{r.was}</code><Icon name="chevron" size={11}/><code className="now">{r.now}</code></>}
      </div>)}
    </div>
    <p className="muted changed-note">{changed.length === 1 ? `Only the ${changed[0].k.toLowerCase()} changed. Start there.` : changed.length === 0 ? 'Same robot and bundle. Suspect flakiness or the environment.' : `${changed.length} inputs changed.`}</p>
  </section>;
}

function StepList({run, now, selected, onSelect}: {run: Run; now: number; selected?: string; onSelect: (id?: string) => void}) {
  return <nav className="steps" aria-label="Steps">
    <button className={`step ${!selected ? 'on' : ''}`} onClick={() => onSelect(undefined)}><Icon name="terminal" size={14}/><span>All output</span></button>
    {(run.steps ?? []).map(st => <button key={st.id} className={`step ${selected === st.id ? 'on' : ''} tone-${tone(st.status)}`} onClick={() => onSelect(st.id)}>
      <StatusIcon status={st.status} size={14}/>
      <span><b>{st.name}</b><small>{st.phase}{st.exit_code != null && st.exit_code !== 0 ? ` · exit ${st.exit_code}` : ''}</small></span>
      <span className="step-time">{st.status === 'SKIPPED' ? 'skipped' : duration(stepDuration(st, now))}</span>
    </button>)}
  </nav>;
}

function LogViewer({logs, run, stepId, focusLine, onClearStep}: {logs: Log[]; run: Run; stepId?: string; focusLine?: number; onClearStep: () => void}) {
  const [filter, setFilter] = useState('');
  const [errorsOnly, setErrorsOnly] = useState(false);
  const [follow, setFollow] = useState(true);
  const box = useRef<HTMLDivElement>(null);
  const step = run.steps?.find(s => s.id === stepId);
  const rows = useMemo(() => logs.map((l, i) => ({...l, n: i + 1})).filter(l =>
    (!stepId || l.step_id === stepId) && (!errorsOnly || l.stream === 'stderr') && (!filter || l.content.toLowerCase().includes(filter.toLowerCase()))
  ), [logs, stepId, errorsOnly, filter]);
  const active = isActive(run.status);

  // Scroll the log pane itself (not the page) so the cited line lands in the middle of the viewer.
  useEffect(() => {
    const el = focusLine ? box.current?.querySelector<HTMLElement>(`[data-line="${focusLine}"]`) : null;
    if (!el || !box.current) return;
    box.current.closest('.run-split')?.scrollIntoView({block: 'nearest', behavior: 'smooth'});
    box.current.scrollTo({top: el.offsetTop - box.current.clientHeight / 2, behavior: 'smooth'});
  }, [focusLine]);
  useEffect(() => {
    if (active && follow && !focusLine && box.current) box.current.scrollTop = box.current.scrollHeight;
  }, [rows.length, active, follow, focusLine]);

  const highlight = (text: string) => {
    if (!filter) return text;
    const i = text.toLowerCase().indexOf(filter.toLowerCase());
    return i < 0 ? text : <>{text.slice(0, i)}<mark>{text.slice(i, i + filter.length)}</mark>{text.slice(i + filter.length)}</>;
  };

  return <section className="logs">
    <div className="logs-bar">
      <span className="logs-title">{step ? <><b>{step.name}</b><button className="chip" onClick={onClearStep}>{step.phase} <Icon name="x" size={10}/></button></> : <b>Output</b>}</span>
      <label className="search-field sm"><Icon name="search" size={12}/><input type="search" placeholder="Filter lines" value={filter} onChange={e => setFilter(e.target.value)} spellCheck={false}/></label>
      <button className={`chip ${errorsOnly ? 'on' : ''}`} aria-pressed={errorsOnly} onClick={() => setErrorsOnly(v => !v)}>stderr only</button>
      {active && <button className={`chip ${follow ? 'on' : ''}`} aria-pressed={follow} onClick={() => setFollow(v => !v)}><span className="pulse"/>Follow</button>}
      <span className="muted logs-hint"><Kbd>E</Kbd> next error</span>
    </div>
    <div className="logs-body" ref={box} role="log" aria-live={active ? 'polite' : 'off'}>
      {rows.map(l => <div key={l.id} data-line={l.n} className={`line ${l.stream} ${l.n === focusLine ? 'focus' : ''}`}>
        <span className="ln">{l.n}</span>
        <span className="lt">{l.created_at ? clock(l.created_at) : ''}</span>
        <span className="lc">{highlight(l.content)}</span>
      </div>)}
      {!rows.length && <div className="logs-empty">{logs.length ? 'No lines match.' : active ? <><span className="pulse"/> Waiting for output from {run.asset_name}…</> : 'No output was captured.'}</div>}
      {active && rows.length > 0 && <div className="line cursor"><span className="ln"/><span className="lt"/><span className="lc"><span className="caret"/></span></div>}
    </div>
  </section>;
}
