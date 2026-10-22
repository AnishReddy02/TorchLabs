import {useCallback, useState} from 'react';
import {useEngine, useTorch} from '../lib/store';
import {Link, navigate} from '../lib/router';
import {currentRun, metrics, needsYou, runsByAsset, type Attention} from '../lib/derive';
import {ago, duration, label} from '../lib/format';
import {Icon} from '../ui/icons';
import {Badge, Crumbs, Empty, PhaseBar, RunStrip, SectionHead, Sparkline, Stat, StatusIcon} from '../ui/primitives';
import {useToast} from '../ui/toast';
import {useListNav} from '../ui/useListNav';

export function Home() {
  const s = useTorch();
  const inbox = needsYou(s);
  const m = metrics(s);
  const byAsset = runsByAsset(s.runs);
  const open = useCallback((i: number) => { const item = inbox[i]; if (item) navigate(item.href); }, [inbox]);
  const [active] = useListNav(inbox.length, open);

  const pct = (v?: number) => v === undefined ? '—' : `${(v * 100).toFixed(1)}%`;
  const loopDelta = m.loop.value && m.loop.prior ? (m.loop.value - m.loop.prior) / m.loop.prior : undefined;
  const passDelta = m.pass.value !== undefined && m.pass.prior !== undefined ? (m.pass.value - m.pass.prior) * 100 : undefined;
  const headline = inbox.length === 0 ? 'All clear. Nothing needs you.' : `${inbox.length} ${inbox.length === 1 ? 'thing needs' : 'things need'} you`;

  return <>
    <header className="page-head"><Crumbs items={[{label: 'Home'}]}/><div className="head-actions"><span className="live-pill"><span className="pulse"/>{s.mode === 'connecting' ? 'Connecting' : 'Live'}</span></div></header>
    <div className="page">
      <section className="hero">
        <h1>{headline}</h1>
        <p className="hero-sub">
          <span><b>{m.online}</b>/{m.total} robots online</span>
          <span><b>{m.active}</b> {m.active === 1 ? 'run' : 'runs'} in flight</span>
          <span><b>{m.recovered}</b> incidents auto-recovered</span>
        </p>
      </section>

      <section className="stats" aria-label="Last 12 hours">
        <Stat label="Loop time · p50" hint="Median time from requesting a run to having a result from real hardware, last 12 h" value={duration(m.loop.value)} delta={loopDelta === undefined ? undefined : `${loopDelta < 0 ? '↓' : '↑'} ${Math.abs(loopDelta * 100).toFixed(0)}%`} good={loopDelta === undefined ? undefined : loopDelta < 0}>
          <Sparkline values={m.loop.series} labels={m.loop.labels} format={v => duration(v)}/>
        </Stat>
        <Stat label="Pass rate" hint="Passed / (passed + failed), last 12 h" value={pct(m.pass.value)} delta={passDelta === undefined ? undefined : `${passDelta >= 0 ? '+' : '−'}${Math.abs(passDelta).toFixed(1)} pts`} good={passDelta === undefined ? undefined : passDelta >= 0}>
          <Sparkline values={m.pass.series} labels={m.pass.labels} format={v => pct(v)}/>
        </Stat>
        <Stat label="Hardware runs · 12 h" value={String(m.runs.value)}>
          <Sparkline values={m.runs.series} labels={m.runs.labels} format={v => `${v} runs`}/>
        </Stat>
        <Stat label="Time to recover · p50" hint="Detection to verified recovery, autonomous incidents" value={duration(m.mttr)}>
          <p className="stat-note"><Icon name="shield" size={12}/> {m.recovered} verified {m.recovered === 1 ? 'recovery' : 'recoveries'}, {s.incidents.filter(i => i.status === 'ESCALATED').length} escalated</p>
        </Stat>
      </section>

      <div className="home-grid">
        <section className="panel">
          <SectionHead title="Needs you" meta={inbox.length || undefined}><span className="muted hint-keys">J/K to move · ↵ to open</span></SectionHead>
          {inbox.length ? <ul className="inbox">{inbox.map((item, i) => <InboxRow key={item.id} item={item} active={i === active}/>)}</ul>
            : <Empty icon="check" title="Nothing needs you">Failures, approvals, and incidents land here. The fleet is green.</Empty>}
        </section>
        <section className="panel">
          <SectionHead title="Live activity"/>
          <ol className="feed">
            {s.activity.slice(0, 14).map(a => <li key={a.id} className="feed-item">
              <StatusIcon tone={a.tone} size={14}/>
              {a.href ? <Link href={a.href}>{a.text}</Link> : <span>{a.text}</span>}
              <time dateTime={a.at}>{ago(a.at, s.now)}</time>
            </li>)}
            {!s.activity.length && <li className="muted">Waiting for the first event…</li>}
          </ol>
        </section>
      </div>

      <section className="panel">
        <SectionHead title="Fleet pulse" meta={`${s.assets.length} robots`}>
          <span className="legend"><span className="cell tone-ok"/>passed <span className="cell tone-fail"/>failed <span className="cell tone-live"/>running</span>
          <Link className="btn btn-ghost btn-sm" href="/fleet">Open fleet <Icon name="chevron" size={12}/></Link>
        </SectionHead>
        <div className="pulse-table" role="table" aria-label="Recent runs per robot">
          {s.assets.map(asset => {
            const runs = byAsset.get(asset.name) ?? [];
            const live = currentRun(s.runs, asset.name);
            return <div key={asset.id} className="pulse-row" role="row">
              <Link href={`/fleet/${asset.name}`} className="pulse-name" role="cell"><StatusIcon status={asset.status} size={12}/><b>{asset.name}</b><small>{asset.hardware_type}</small></Link>
              <span className="pulse-now" role="cell">{live ? <Link href={`/runs/${live.id}`} className="pulse-live"><PhaseBar steps={live.steps} compact/><span className="truncate">{live.workflow_name}</span></Link> : asset.status === 'OFFLINE' ? <span className="muted">offline · seen {ago(asset.last_seen_at, s.now)}</span> : <span className="muted">{label(asset.status)}</span>}</span>
              <span role="cell"><RunStrip runs={runs} now={s.now}/></span>
            </div>;
          })}
        </div>
      </section>
    </div>
  </>;
}

function InboxRow({item, active}: {item: Attention; active: boolean}) {
  const engine = useEngine();
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  const act = (fn: () => Promise<unknown>, ok: string) => async () => {
    setBusy(true);
    try { await fn(); toast({tone: 'ok', title: ok}); }
    catch (e) { toast({tone: 'fail', title: 'Action failed', body: (e as Error).message}); }
    finally { setBusy(false); }
  };
  const rerun = (bundleId?: string) => async () => {
    setBusy(true);
    try {
      const run = await engine.rerun(item.runId!, bundleId);
      toast({tone: 'live', title: 'Rerun queued', body: `${run.workflow_name} on ${run.asset_name}`, action: {label: 'Watch', run: () => navigate(`/runs/${run.id}`)}});
    } catch (e) { toast({tone: 'fail', title: 'Could not rerun', body: (e as Error).message}); }
    finally { setBusy(false); }
  };
  return <li className={`inbox-row ${active ? 'is-active' : ''}`} data-nav-active={active}>
    <StatusIcon tone={item.tone} size={16}/>
    <Link href={item.href} className="inbox-main">
      <b>{item.title}</b>
      <span>{item.detail}</span>
    </Link>
    <span className="inbox-actions">
      {item.kind === 'approval' && <><button className="btn btn-sm" disabled={busy} onClick={() => navigate(item.href)}>Review</button><button className="btn btn-sm btn-primary" disabled={busy} onClick={act(() => engine.decide(item.sessionId!, true), 'Deployment approved')}>Approve</button></>}
      {item.kind === 'failure' && <>
        <button className="btn btn-sm" disabled={busy} onClick={rerun()} title="Rerun with the same inputs"><Icon name="rerun" size={13}/>Rerun</button>
        {item.fixBundle && <button className="btn btn-sm btn-primary" disabled={busy} onClick={rerun(item.fixBundle.id)} title={`Rerun with the newest ${item.fixBundle.name}`}>Try {item.fixBundle.name}</button>}
      </>}
      {item.kind === 'incident' && <Badge status={item.tone === 'fail' ? 'ESCALATED' : 'INVESTIGATING'}>{item.tone === 'fail' ? 'Escalated' : 'Self-healing'}</Badge>}
    </span>
    <time className="inbox-time" dateTime={item.at}>{ago(item.at)}</time>
  </li>;
}
