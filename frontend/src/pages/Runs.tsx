import {useCallback} from 'react';
import {useTorch} from '../lib/store';
import {Link, navigate, setQuery, useLocation} from '../lib/router';
import {ago, duration, isActive, runDuration, shortId, tone} from '../lib/format';
import type {Run} from '../lib/types';
import {useActions} from '../app/actions';
import {Icon} from '../ui/icons';
import {Avatar, Crumbs, Empty, Kbd, PhaseBar, StatusIcon} from '../ui/primitives';
import {useListNav} from '../ui/useListNav';

const FILTERS = [
  {key: '', label: 'All'},
  {key: 'active', label: 'In flight'},
  {key: 'failed', label: 'Failed'},
  {key: 'passed', label: 'Passed'},
];

export function Runs() {
  const s = useTorch();
  const {query} = useLocation();
  const actions = useActions();
  const status = query.get('status') ?? '';
  const robot = query.get('robot') ?? '';
  const workflow = query.get('workflow') ?? '';
  const q = (query.get('q') ?? '').toLowerCase();

  const match = (r: Run) =>
    (!status || (status === 'active' ? isActive(r.status) : status === 'failed' ? tone(r.status) === 'fail' : r.status === 'PASSED'))
    && (!robot || r.asset_name === robot) && (!workflow || r.workflow_name === workflow)
    && (!q || `${r.id} ${r.workflow_name} ${r.asset_name} ${r.bundle_name ?? ''} ${r.commit ?? ''} ${r.actor ?? ''}`.toLowerCase().includes(q));
  const runs = s.runs.filter(match);
  const counts = Object.fromEntries(FILTERS.map(f => [f.key, s.runs.filter(r => !f.key || (f.key === 'active' ? isActive(r.status) : f.key === 'failed' ? tone(r.status) === 'fail' : r.status === 'PASSED')).length]));

  const open = useCallback((i: number) => { if (runs[i]) navigate(`/runs/${runs[i].id}`); }, [runs]);
  const [active] = useListNav(runs.length, open);

  const groups: {day: string; runs: Run[]; start: number}[] = [];
  runs.forEach((r, i) => {
    const day = dayLabel(r.created_at, s.now);
    if (groups.at(-1)?.day !== day) groups.push({day, runs: [], start: i});
    groups.at(-1)!.runs.push(r);
  });

  const workflows = [...new Set(s.runs.map(r => r.workflow_name))].sort();
  return <>
    <header className="page-head">
      <Crumbs items={[{label: 'Runs'}]}/>
      <div className="head-actions"><button className="btn btn-primary btn-sm" onClick={() => actions.newRun()}><Icon name="play" size={12}/>New run <Kbd>N</Kbd></button></div>
    </header>
    <div className="page">
      <div className="toolbar">
        <div className="segmented" role="tablist" aria-label="Status">
          {FILTERS.map(f => <button key={f.key} role="tab" aria-selected={status === f.key} className={status === f.key ? 'on' : ''} onClick={() => setQuery({status: f.key || null})}>{f.label}<span>{counts[f.key]}</span></button>)}
        </div>
        <select aria-label="Robot" value={robot} onChange={e => setQuery({robot: e.target.value || null})}><option value="">All robots</option>{s.assets.map(a => <option key={a.id}>{a.name}</option>)}</select>
        <select aria-label="Workflow" value={workflow} onChange={e => setQuery({workflow: e.target.value || null})}><option value="">All workflows</option>{workflows.map(w => <option key={w}>{w}</option>)}</select>
        <label className="search-field"><Icon name="search" size={14}/><input type="search" placeholder="Run id, bundle, commit, person…" defaultValue={query.get('q') ?? ''} onChange={e => setQuery({q: e.target.value || null})} spellCheck={false}/></label>
      </div>

      {groups.map(g => <section key={g.day} className="run-group">
        <h3 className="group-label">{g.day}<span>{g.runs.length}</span></h3>
        <ul className="list">
          {g.runs.map((r, j) => <RunRow key={r.id} run={r} now={s.now} active={g.start + j === active}/>)}
        </ul>
      </section>)}
      {!runs.length && <Empty icon="runs" title={s.runs.length ? 'No runs match these filters' : 'No runs yet'} action={s.runs.length ? <button className="btn" onClick={() => setQuery({status: null, robot: null, workflow: null, q: null})}>Clear filters</button> : <button className="btn btn-primary" onClick={() => actions.newRun()}>Start your first run</button>}>
        {s.runs.length ? 'Try a different status, robot, or search.' : 'Pick a robot and a workflow. Torch handles reservation, deploy, test, and cleanup.'}
      </Empty>}
    </div>
  </>;
}

function RunRow({run, now, active}: {run: Run; now: number; active: boolean}) {
  const failed = run.steps?.find(s => tone(s.status) === 'fail');
  return <li data-nav-active={active}>
    <Link href={`/runs/${run.id}`} className={`run-row ${active ? 'is-active' : ''}`}>
      <StatusIcon status={run.status} size={16}/>
      <span className="run-title">
        <b>{run.workflow_name}<span className="on"> on </span>{run.asset_name}</b>
        <span className="run-meta">
          <code>{shortId(run.id)}</code>
          {run.bundle_name && <span><Icon name="package" size={11}/>{run.bundle_name}</span>}
          {run.commit && <span><Icon name="commit" size={11}/>{run.commit}</span>}
          {failed && <span className="meta-fail">{failed.name}: {failed.error ?? `exit ${failed.exit_code}`}</span>}
        </span>
      </span>
      <PhaseBar steps={run.steps}/>
      <span className="run-actor">{run.actor && <Avatar name={run.actor}/>}</span>
      <span className="run-num"><Icon name="clock" size={11}/>{duration(runDuration(run, now))}</span>
      <time className="run-num" dateTime={run.created_at}>{ago(run.created_at, now)}</time>
    </Link>
  </li>;
}

function dayLabel(value: string, now: number) {
  const d = new Date(value), today = new Date(now);
  const diff = Math.round((new Date(today.toDateString()).getTime() - new Date(d.toDateString()).getTime()) / 86400_000);
  return diff === 0 ? 'Today' : diff === 1 ? 'Yesterday' : d.toLocaleDateString([], {weekday: 'long', month: 'short', day: 'numeric'});
}
