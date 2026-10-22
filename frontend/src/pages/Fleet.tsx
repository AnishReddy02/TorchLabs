import {useCallback, useEffect, useState} from 'react';
import {useEngine, useTorch, useWatch} from '../lib/store';
import {Link, navigate, setQuery, useLocation} from '../lib/router';
import {currentRun, runsByAsset} from '../lib/derive';
import {ago, duration, runDuration, tone} from '../lib/format';
import type {Asset} from '../lib/types';
import {useActions} from '../app/actions';
import {Icon} from '../ui/icons';
import {Badge, Crumbs, Empty, PhaseBar, RunStrip, StatusIcon} from '../ui/primitives';
import {useToast} from '../ui/toast';
import {useListNav} from '../ui/useListNav';

const STATES = [{key: '', label: 'All'}, {key: 'AVAILABLE', label: 'Available'}, {key: 'RESERVED', label: 'Busy'}, {key: 'OFFLINE', label: 'Offline'}];

export function Fleet({selected}: {selected?: string}) {
  const s = useTorch();
  const {query} = useLocation();
  const status = query.get('status') ?? '';
  const byAsset = runsByAsset(s.runs);
  const assets = s.assets.filter(a => !status || (status === 'RESERVED' ? !['AVAILABLE', 'OFFLINE'].includes(a.status) : a.status === status));
  const open = useCallback((i: number) => { if (assets[i]) navigate(`/fleet/${assets[i].name}${location.search}`); }, [assets]);
  const [active] = useListNav(assets.length, open);
  const asset = s.assets.find(a => a.name === selected);

  return <>
    <header className="page-head"><Crumbs items={[{label: 'Fleet', href: '/fleet'}, ...(asset ? [{label: asset.name}] : [])]}/></header>
    <div className={`page fleet-page ${asset ? 'with-drawer' : ''}`}>
      <div className="fleet-main">
        <div className="toolbar">
          <div className="segmented" role="tablist" aria-label="Status">
            {STATES.map(f => <button key={f.key} role="tab" aria-selected={status === f.key} className={status === f.key ? 'on' : ''} onClick={() => setQuery({status: f.key || null})}>{f.label}<span>{s.assets.filter(a => !f.key || (f.key === 'RESERVED' ? !['AVAILABLE', 'OFFLINE'].includes(a.status) : a.status === f.key)).length}</span></button>)}
          </div>
        </div>
        {assets.length ? <div className="table" role="table" aria-label="Robots">
          <div className="tr th" role="row"><span role="columnheader">Robot</span><span role="columnheader">Status</span><span role="columnheader">Now</span><span role="columnheader">Last 16 runs</span><span role="columnheader" className="num">Pass rate</span><span role="columnheader">System</span><span role="columnheader" className="num">Seen</span></div>
          {assets.map((a, i) => {
            const runs = byAsset.get(a.name) ?? [];
            const live = currentRun(s.runs, a.name);
            const finished = runs.filter(r => ['PASSED', 'FAILED', 'TIMED_OUT'].includes(r.status));
            const rate = finished.length ? finished.filter(r => r.status === 'PASSED').length / finished.length : undefined;
            return <div key={a.id} role="row" data-nav-active={i === active} className={`tr ${a.name === selected ? 'selected' : ''} ${i === active ? 'is-active' : ''}`} onClick={e => { if (!(e.target as HTMLElement).closest('a')) navigate(`/fleet/${a.name}${location.search}`); }}>
              <span role="cell" className="cell-robot"><Link href={`/fleet/${a.name}${location.search}`}><b>{a.name}</b></Link><small>{a.hardware_type}</small></span>
              <span role="cell"><Badge status={a.status}/></span>
              <span role="cell" className="cell-now">{live ? <Link href={`/runs/${live.id}`} className="now-run"><PhaseBar steps={live.steps} compact/>{live.workflow_name}</Link> : <span className="muted">—</span>}</span>
              <span role="cell"><RunStrip runs={runs} slots={16} now={s.now}/></span>
              <span role="cell" className={`num ${rate !== undefined && rate < 0.8 ? 'warn-text' : ''}`}>{rate === undefined ? '—' : `${Math.round(rate * 100)}%`}</span>
              <span role="cell" className="mono-sm">{a.os} · {a.architecture}</span>
              <span role="cell" className="num muted">{ago(a.last_seen_at, s.now)}</span>
            </div>;
          })}
        </div> : <Empty icon="fleet" title={s.assets.length ? 'No robots in this state' : 'No robots connected'}>
          {s.assets.length ? 'Try another filter.' : <>Install the Torch node on a Linux robot: <code>curl -fsSL http://CONTROL_PLANE:8080/install.sh | sudo bash</code></>}
        </Empty>}
      </div>
      {asset && <RobotDrawer asset={asset}/>}
    </div>
  </>;
}

function RobotDrawer({asset}: {asset: Asset}) {
  useWatch('telemetry', asset.name);
  const s = useTorch();
  const engine = useEngine();
  const actions = useActions();
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  const runs = s.runs.filter(r => r.asset_name === asset.name).slice(0, 6);
  const telemetry = s.telemetry[asset.name] ?? [];
  const latest = [...new Map(telemetry.map(t => [t.service_id, t])).values()];
  const close = () => navigate(`/fleet${location.search}`);
  useEffect(() => {
    const on = (e: KeyboardEvent) => { if (e.key === 'Escape' && !document.querySelector('.overlay')) close(); };
    window.addEventListener('keydown', on);
    return () => window.removeEventListener('keydown', on);
  });
  const act = async (fn: () => Promise<void>, title: string) => {
    setBusy(true);
    try { await fn(); toast({tone: 'ok', title}); } catch (e) { toast({tone: 'fail', title: 'Action failed', body: (e as Error).message}); } finally { setBusy(false); }
  };
  const caps = Object.entries(asset.capabilities ?? {}).filter(([, v]) => v === true).map(([k]) => k);
  const labels = Object.entries(asset.labels ?? {});

  return <aside className="drawer" aria-label={`${asset.name} details`}>
    <div className="drawer-head">
      <div><h2>{asset.name}</h2><p className="muted">{asset.hardware_type}{asset.description ? ` · ${asset.description}` : ''}</p></div>
      <button className="icon-btn" aria-label="Close" onClick={close}><Icon name="x" size={16}/></button>
    </div>
    <div className="drawer-actions">
      <button className="btn btn-primary btn-sm" disabled={asset.status === 'OFFLINE'} onClick={() => actions.newRun({assetId: asset.id})}><Icon name="play" size={12}/>Run a workflow</button>
      {asset.status === 'AVAILABLE' && <button className="btn btn-sm" disabled={busy} onClick={() => void act(() => engine.reserve(asset.id), `${asset.name} reserved for 60 min`)}><Icon name="lock" size={12}/>Reserve</button>}
      {asset.status === 'RESERVED' && <button className="btn btn-sm" disabled={busy} onClick={() => void act(() => engine.release(asset.id), `${asset.name} released`)}>Release</button>}
    </div>
    <dl className="facts">
      <dt>Status</dt><dd><Badge status={asset.status}/></dd>
      <dt>Host</dt><dd className="mono-sm">{asset.hostname ?? '—'}</dd>
      <dt>System</dt><dd className="mono-sm">{asset.os} · {asset.architecture}</dd>
      <dt>Last seen</dt><dd>{ago(asset.last_seen_at, s.now)}</dd>
      {labels.length > 0 && <><dt>Labels</dt><dd className="chips">{labels.map(([k, v]) => <span key={k} className="chip static">{k}={String(v)}</span>)}</dd></>}
      <dt>Capabilities</dt><dd className="chips">{caps.length ? caps.map(c => <span key={c} className="chip static">{c}</span>) : <span className="muted">generic Linux</span>}</dd>
    </dl>
    <h3 className="drawer-sub">Services</h3>
    {latest.length ? <ul className="services">{latest.map(t => <li key={t.service_id}>
      <StatusIcon tone={t.healthy ? 'ok' : 'fail'} size={13} title={t.healthy ? 'Healthy' : 'Unhealthy'}/>
      <span><b>{t.service_id}</b><small>{t.summary}</small></span>
      <time className="muted">{ago(t.observed_at, s.now)}</time>
    </li>)}</ul> : <p className="muted">No telemetry yet. Services report once the node agent publishes observations.</p>}
    <h3 className="drawer-sub">Recent runs</h3>
    {runs.length ? <ul className="mini-runs">{runs.map(r => <li key={r.id}><Link href={`/runs/${r.id}`}>
      <StatusIcon status={r.status} size={13}/><span className="truncate">{r.workflow_name}</span><span className="muted">{duration(runDuration(r, s.now))}</span><time className="muted">{ago(r.created_at, s.now)}</time>
    </Link></li>)}</ul> : <p className="muted">No runs on this robot yet.</p>}
    {runs.some(r => tone(r.status) === 'fail') && <p className="drawer-note"><Icon name="alert" size={12}/> {runs.filter(r => tone(r.status) === 'fail').length} of the last {runs.length} runs failed here. <Link href={`/runs?robot=${asset.name}&status=failed`}>See failures</Link></p>}
  </aside>;
}
