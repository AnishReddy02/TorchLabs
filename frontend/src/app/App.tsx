import {useEffect, useMemo, useRef, useState, type FormEvent, type ReactNode} from 'react';
import {useEngine, useEngineCtx, useTorch} from '../lib/store';
import {Link, navigate, useLocation} from '../lib/router';
import {needsYou} from '../lib/derive';
import {label, shortId} from '../lib/format';
import {Icon, TorchMark, type IconName} from '../ui/icons';
import {Kbd, StatusIcon} from '../ui/primitives';
import {useToast} from '../ui/toast';
import {ActionsContext, useActions, type RunPrefill} from './actions';
import {Home} from '../pages/Home';
import {Runs} from '../pages/Runs';
import {RunView} from '../pages/RunView';
import {Fleet} from '../pages/Fleet';
import {AgentPage} from '../pages/Agent';
import {IncidentsPage} from '../pages/Incidents';
import {Library} from '../pages/Library';

export function App() {
  const {path} = useLocation();
  const [palette, setPalette] = useState(false);
  const [help, setHelp] = useState(false);
  const [runDialog, setRunDialog] = useState<RunPrefill | null>(null);
  const actions = useMemo(() => ({openPalette: () => setPalette(true), newRun: (p: RunPrefill = {}) => setRunDialog(p), openHelp: () => setHelp(true)}), []);
  useShortcuts(actions);

  const [, section, id] = path.split('/');
  let page: ReactNode;
  if (section === 'runs' && id) page = <RunView id={id}/>;
  else if (section === 'runs') page = <Runs/>;
  else if (section === 'fleet') page = <Fleet selected={id ? decodeURIComponent(id) : undefined}/>;
  else if (section === 'agent') page = <AgentPage id={id}/>;
  else if (section === 'incidents') page = <IncidentsPage id={id}/>;
  else if (section === 'workflows' || section === 'bundles') page = <Library tab={section} id={id}/>;
  else page = <Home/>;

  return <ActionsContext.Provider value={actions}>
    <a className="skip" href="#main">Skip to content</a>
    <div className="shell">
      <Sidebar section={section || 'home'}/>
      <main id="main" className="main" tabIndex={-1}>{page}</main>
    </div>
    {palette && <CommandPalette onClose={() => setPalette(false)}/>}
    {runDialog && <NewRunDialog prefill={runDialog} onClose={() => setRunDialog(null)}/>}
    {help && <ShortcutHelp onClose={() => setHelp(false)}/>}
  </ActionsContext.Provider>;
}

const NAV: {group: string; items: {key: string; href: string; label: string; icon: IconName; hint: string}[]}[] = [
  {group: 'Operate', items: [
    {key: 'home', href: '/', label: 'Home', icon: 'home', hint: 'G H'},
    {key: 'runs', href: '/runs', label: 'Runs', icon: 'runs', hint: 'G R'},
    {key: 'fleet', href: '/fleet', label: 'Fleet', icon: 'fleet', hint: 'G F'},
  ]},
  {group: 'Autonomy', items: [
    {key: 'agent', href: '/agent', label: 'Torch Agent', icon: 'agent', hint: 'G A'},
    {key: 'incidents', href: '/incidents', label: 'Recovery', icon: 'siren', hint: 'G I'},
  ]},
  {group: 'Library', items: [
    {key: 'workflows', href: '/workflows', label: 'Workflows', icon: 'workflow', hint: 'G W'},
    {key: 'bundles', href: '/bundles', label: 'Bundles', icon: 'package', hint: 'G B'},
  ]},
];

function Sidebar({section}: {section: string}) {
  const s = useTorch();
  const {connect, switchToDemo} = useEngineCtx();
  const actions = useActions();
  const [theme, setTheme] = useState(() => document.documentElement.dataset.theme ?? 'dark');
  const inbox = needsYou(s).length;
  const counts: Record<string, number> = {
    home: inbox,
    runs: s.runs.filter(r => ['QUEUED', 'PREPARING', 'RUNNING', 'CLEANING_UP'].includes(r.status)).length,
    agent: s.sessions.filter(x => x.status === 'WAITING_FOR_APPROVAL').length,
    incidents: s.incidents.filter(i => !['RESOLVED', 'CANCELLED'].includes(i.status)).length,
  };
  const toggleTheme = () => {
    const next = theme === 'dark' ? 'light' : 'dark';
    document.documentElement.dataset.theme = next;
    try { localStorage.setItem('torch_theme', next); } catch { /* storage unavailable */ }
    setTheme(next);
  };
  return <aside className="sidebar">
    <div className="brand"><TorchMark/><div><b>Torch</b><small>Labs · Fleet</small></div></div>
    <button className="search-btn" onClick={actions.openPalette}><Icon name="search" size={14}/><span>Search or jump to…</span><Kbd>⌘K</Kbd></button>
    <button className="btn btn-primary new-run" onClick={() => actions.newRun()}><Icon name="play" size={13}/>New run<Kbd>N</Kbd></button>
    <nav aria-label="Primary">
      {NAV.map(g => <div key={g.group} className="nav-group">
        <span className="nav-label">{g.group}</span>
        {g.items.map(item => <Link key={item.key} href={item.href} className={`nav-item ${section === item.key ? 'active' : ''}`} aria-current={section === item.key ? 'page' : undefined} title={`${item.label} (${item.hint})`}>
          <Icon name={item.icon} size={16}/><span>{item.label}</span>
          {!!counts[item.key] && <span className={`nav-count ${item.key === 'runs' ? 'live' : item.key === 'home' ? '' : 'warn'}`}>{counts[item.key]}</span>}
        </Link>)}
      </div>)}
    </nav>
    <div className="sidebar-foot">
      <div className={`conn conn-${s.mode}`}>
        <span className="conn-dot"/>
        <div>
          <b>{s.mode === 'live' ? 'Live' : s.mode === 'demo' ? 'Demo fleet' : 'Connecting…'}</b>
          <small>{s.mode === 'live' ? s.endpoint.replace(/^https?:\/\//, '') : s.mode === 'demo' ? 'Control plane not reachable' : s.endpoint.replace(/^https?:\/\//, '')}</small>
        </div>
        {s.mode === 'demo' && <button className="btn btn-ghost btn-sm" onClick={connect}>Retry</button>}
        {s.mode === 'live' && <button className="btn btn-ghost btn-sm" onClick={switchToDemo} title="Explore with simulated data">Demo</button>}
      </div>
      <div className="foot-row">
        <button className="icon-btn" onClick={toggleTheme} aria-label={`Switch to ${theme === 'dark' ? 'light' : 'dark'} theme`}><Icon name={theme === 'dark' ? 'sun' : 'moon'} size={15}/></button>
        <button className="icon-btn" onClick={actions.openHelp} aria-label="Keyboard shortcuts"><Icon name="keyboard" size={15}/></button>
        <span className="me" title="Signed in with the local admin token"><span className="avatar" style={{background: 'oklch(0.55 0.09 40)'}}>Y</span>You</span>
      </div>
    </div>
  </aside>;
}
function typing(e: KeyboardEvent) {
  const el = e.target as HTMLElement | null;
  return !!el && (el.isContentEditable || ['INPUT', 'TEXTAREA', 'SELECT'].includes(el.tagName));
}

function useShortcuts(actions: {openPalette: () => void; newRun: () => void; openHelp: () => void}) {
  useEffect(() => {
    let leader = 0;
    const go: Record<string, string> = {h: '/', r: '/runs', f: '/fleet', a: '/agent', i: '/incidents', w: '/workflows', b: '/bundles'};
    const on = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k') { e.preventDefault(); actions.openPalette(); return; }
      if (typing(e) || e.metaKey || e.ctrlKey || e.altKey || document.querySelector('.overlay')) return;
      if (Date.now() - leader < 900 && go[e.key]) { e.preventDefault(); leader = 0; navigate(go[e.key]); return; }
      if (e.key === 'g') { leader = Date.now(); return; }
      if (e.key === 'n') { e.preventDefault(); actions.newRun(); }
      if (e.key === '?') { e.preventDefault(); actions.openHelp(); }
      if (e.key === '/') { e.preventDefault(); actions.openPalette(); }
    };
    window.addEventListener('keydown', on);
    return () => window.removeEventListener('keydown', on);
  }, [actions]);
}

/** Modal shell: traps focus, closes on Escape or backdrop, and returns focus to whatever opened it. */
function Overlay({onClose, children, label: name, className = ''}: {onClose: () => void; children: ReactNode; label: string; className?: string}) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const opener = document.activeElement as HTMLElement | null;
    const focusables = () => [...(ref.current?.querySelectorAll<HTMLElement>('input,select,textarea,button,[href],[tabindex]:not([tabindex="-1"])') ?? [])].filter(el => !el.hasAttribute('disabled'));
    if (!ref.current?.contains(document.activeElement)) focusables()[0]?.focus();
    const on = (e: KeyboardEvent) => {
      if (e.key === 'Escape') { e.preventDefault(); onClose(); }
      if (e.key === 'Tab') {
        const els = focusables(); if (!els.length) return;
        const first = els[0], last = els[els.length - 1];
        if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last.focus(); }
        else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
      }
    };
    window.addEventListener('keydown', on);
    return () => { window.removeEventListener('keydown', on); opener?.focus?.(); };
  }, [onClose]);
  return <div className="overlay" onMouseDown={e => { if (e.target === e.currentTarget) onClose(); }}>
    <div ref={ref} className={`dialog ${className}`} role="dialog" aria-modal="true" aria-label={name}>{children}</div>
  </div>;
}

type Command = {id: string; group: string; label: string; hint?: string; icon?: IconName; status?: string; run: () => void};

function CommandPalette({onClose}: {onClose: () => void}) {
  const s = useTorch();
  const engine = useEngine();
  const toast = useToast();
  const actions = useActions();
  const [q, setQ] = useState('');
  const [index, setIndex] = useState(0);
  const listRef = useRef<HTMLDivElement>(null);
  const go = (href: string) => () => { navigate(href); onClose(); };

  const commands = useMemo<Command[]>(() => {
    const lastFailed = s.runs.find(r => r.status === 'FAILED');
    const cmds: Command[] = [
      {id: 'new', group: 'Actions', label: 'Start a new run…', hint: 'N', icon: 'play', run: () => { onClose(); actions.newRun(); }},
      ...(lastFailed ? [{id: 'rerun', group: 'Actions', label: `Rerun last failure · ${lastFailed.workflow_name} on ${lastFailed.asset_name}`, icon: 'rerun' as IconName, run: () => { onClose(); void engine.rerun(lastFailed.id).then(r => { toast({tone: 'live', title: 'Rerun queued', body: `${r.workflow_name} on ${r.asset_name}`}); navigate(`/runs/${r.id}`); }).catch(e => toast({tone: 'fail', title: 'Could not rerun', body: String(e.message ?? e)})); }}] : []),
      {id: 'mission', group: 'Actions', label: 'Start an agent mission…', icon: 'agent', run: go('/agent?new=1')},
      {id: 'workflow', group: 'Actions', label: 'Create a workflow…', icon: 'workflow', run: go('/workflows?new=1')},
      {id: 'theme', group: 'Actions', label: 'Toggle light / dark theme', icon: 'sun', run: () => { const next = document.documentElement.dataset.theme === 'light' ? 'dark' : 'light'; document.documentElement.dataset.theme = next; try { localStorage.setItem('torch_theme', next); } catch { /* storage unavailable */ } onClose(); }},
      ...NAV.flatMap(g => g.items.map(item => ({id: `nav-${item.key}`, group: 'Go to', label: item.label, hint: item.hint, icon: item.icon, run: go(item.href)}))),
      ...s.assets.map(a => ({id: `a-${a.id}`, group: 'Robots', label: `${a.name} · ${a.hardware_type}`, status: a.status, run: go(`/fleet/${a.name}`)})),
      ...s.runs.slice(0, 60).map(r => ({id: `r-${r.id}`, group: 'Runs', label: `${r.workflow_name} on ${r.asset_name}`, hint: shortId(r.id), status: r.status, run: go(`/runs/${r.id}`)})),
      ...s.incidents.map(i => ({id: `i-${i.id}`, group: 'Incidents', label: `${i.service_id} on ${i.asset_name}`, hint: label(i.status), status: i.status, run: go(`/incidents/${i.id}`)})),
      ...s.workflows.map(w => ({id: `w-${w.id}`, group: 'Workflows', label: w.name, hint: `v${w.version}`, icon: 'workflow' as IconName, run: go(`/workflows/${w.id}`)})),
    ];
    return cmds;
  }, [s.runs, s.assets, s.incidents, s.workflows]); // eslint-disable-line react-hooks/exhaustive-deps

  const terms = q.toLowerCase().split(/\s+/).filter(Boolean);
  const results = terms.length ? commands.filter(c => terms.every(t => `${c.label} ${c.hint ?? ''} ${c.group} ${c.status ?? ''}`.toLowerCase().includes(t))).slice(0, 40) : commands.filter(c => c.group === 'Actions' || c.group === 'Go to');
  const active = Math.min(index, Math.max(0, results.length - 1));
  useEffect(() => { listRef.current?.querySelector('[aria-selected="true"]')?.scrollIntoView({block: 'nearest'}); }, [active]);

  let lastGroup = '';
  return <Overlay onClose={onClose} label="Command palette" className="palette">
    <div className="palette-input">
      <Icon name="search" size={16}/>
      <input autoFocus placeholder="Search runs, robots, workflows, or type a command…" value={q} role="combobox" aria-expanded="true" aria-controls="palette-list" aria-activedescendant={results[active] ? `cmd-${results[active].id}` : undefined}
        onChange={e => { setQ(e.target.value); setIndex(0); }}
        onKeyDown={e => {
          if (e.key === 'ArrowDown') { e.preventDefault(); setIndex(i => Math.min(i + 1, results.length - 1)); }
          if (e.key === 'ArrowUp') { e.preventDefault(); setIndex(i => Math.max(i - 1, 0)); }
          if (e.key === 'Enter') { e.preventDefault(); results[active]?.run(); }
        }}/>
      <Kbd>esc</Kbd>
    </div>
    <div className="palette-list" id="palette-list" role="listbox" ref={listRef}>
      {!results.length && <div className="palette-empty">No matches for “{q}”</div>}
      {results.map((c, i) => {
        const head = c.group !== lastGroup ? c.group : null; lastGroup = c.group;
        return <div key={c.id}>
          {head && <div className="palette-group">{head}</div>}
          <div id={`cmd-${c.id}`} role="option" aria-selected={i === active} className="palette-item" onMouseMove={() => setIndex(i)} onClick={c.run}>
            {c.status ? <StatusIcon status={c.status} size={14}/> : <Icon name={c.icon ?? 'chevron'} size={14}/>}
            <span>{c.label}</span>
            {c.hint && <span className="palette-hint">{c.hint}</span>}
          </div>
        </div>;
      })}
    </div>
    <div className="palette-foot"><span><Kbd>↑</Kbd><Kbd>↓</Kbd> move</span><span><Kbd>↵</Kbd> open</span><span><Kbd>?</Kbd> all shortcuts</span></div>
  </Overlay>;
}

function NewRunDialog({prefill, onClose}: {prefill: RunPrefill; onClose: () => void}) {
  const s = useTorch();
  const engine = useEngine();
  const toast = useToast();
  const usable = s.assets.filter(a => a.status !== 'OFFLINE');
  const [assetId, setAssetId] = useState(prefill.assetId ?? usable.find(a => a.status === 'AVAILABLE')?.id ?? usable[0]?.id ?? '');
  const [workflowId, setWorkflowId] = useState(prefill.workflowId ?? s.workflows[0]?.id ?? '');
  const [bundleId, setBundleId] = useState<string>(prefill.bundleId ?? '');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const asset = s.assets.find(a => a.id === assetId);
  const bundles = [...s.bundles].sort((a, b) => Date.parse(b.created_at ?? '') - Date.parse(a.created_at ?? ''));

  async function submit(e?: FormEvent) {
    e?.preventDefault();
    if (!assetId || !workflowId) { setError('Pick a robot and a workflow.'); return; }
    setBusy(true); setError('');
    try {
      const run = await engine.createRun({asset_id: assetId, workflow_id: workflowId, bundle_id: bundleId || null});
      toast({tone: 'live', title: 'Run queued', body: `${run.workflow_name} on ${run.asset_name}`});
      onClose();
      navigate(`/runs/${run.id}`);
    } catch (err) { setError((err as Error).message); setBusy(false); }
  }

  return <Overlay onClose={onClose} label="Start a run">
    <form onSubmit={submit} onKeyDown={e => { if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) void submit(); }}>
      <div className="dialog-head"><h2>Start a run</h2><p>Deploy a bundle to a robot and run a workflow against it.</p></div>
      <div className="dialog-body">
        <label className="field">Robot
          <select value={assetId} onChange={e => setAssetId(e.target.value)}>
            {s.assets.map(a => <option key={a.id} value={a.id} disabled={a.status === 'OFFLINE'}>{a.name} · {a.hardware_type} · {label(a.status)}</option>)}
          </select>
          {asset && asset.status !== 'AVAILABLE' && <small className="field-note">{asset.name} is {label(asset.status).toLowerCase()}. Torch will queue the run.</small>}
        </label>
        <label className="field">Workflow
          <select value={workflowId} onChange={e => setWorkflowId(e.target.value)}>
            {s.workflows.map(w => <option key={w.id} value={w.id}>{w.name} · v{w.version}</option>)}
          </select>
        </label>
        <label className="field">Software bundle <span className="optional">optional</span>
          <select value={bundleId} onChange={e => setBundleId(e.target.value)}>
            <option value="">Use what is already on the robot</option>
            {bundles.map(b => <option key={b.id} value={b.id}>{b.name} · {b.sha256.slice(0, 8)}</option>)}
          </select>
        </label>
        {error && <p className="form-error" role="alert">{error}</p>}
      </div>
      <div className="dialog-foot">
        <span className="muted"><Kbd>⌘</Kbd><Kbd>↵</Kbd> to start</span>
        <button type="button" className="btn btn-ghost" onClick={onClose}>Cancel</button>
        <button className="btn btn-primary" disabled={busy}>{busy ? 'Starting…' : bundleId ? 'Deploy & test' : 'Start run'}</button>
      </div>
    </form>
  </Overlay>;
}

function ShortcutHelp({onClose}: {onClose: () => void}) {
  const rows: [string, string[]][] = [
    ['Command palette', ['⌘', 'K']], ['Start a new run', ['N']], ['Search', ['/']], ['This help', ['?']],
    ['Go to Home', ['G', 'H']], ['Go to Runs', ['G', 'R']], ['Go to Fleet', ['G', 'F']], ['Go to Torch Agent', ['G', 'A']], ['Go to Recovery', ['G', 'I']], ['Go to Workflows', ['G', 'W']],
    ['Move through lists', ['J', 'K']], ['Open selected', ['↵']], ['Rerun (on a run)', ['R']], ['Next failing line (on a run)', ['E']],
  ];
  return <Overlay onClose={onClose} label="Keyboard shortcuts">
    <div className="dialog-head"><h2>Keyboard shortcuts</h2><p>Torch is built to be driven from the keyboard.</p></div>
    <div className="dialog-body shortcuts">{rows.map(([name, keys]) => <div key={name}><span>{name}</span><span>{keys.map(k => <Kbd key={k}>{k}</Kbd>)}</span></div>)}</div>
  </Overlay>;
}
