import {useRef, useState, type DragEvent} from 'react';
import {useEngine, useTorch} from '../lib/store';
import {Link, navigate, useLocation} from '../lib/router';
import {ago, bytes, PHASES} from '../lib/format';
import type {Workflow} from '../lib/types';
import {useActions} from '../app/actions';
import {Icon} from '../ui/icons';
import {Crumbs, Empty, Kbd, RunStrip} from '../ui/primitives';
import {useToast} from '../ui/toast';

export function Library({tab, id}: {tab: string; id?: string}) {
  return <>
    <header className="page-head">
      <Crumbs items={[{label: 'Library'}, {label: tab === 'bundles' ? 'Bundles' : 'Workflows', href: `/${tab}`}]}/>
      <div className="segmented sm" role="tablist" aria-label="Library">
        <Link role="tab" aria-selected={tab === 'workflows'} className={tab === 'workflows' ? 'on' : ''} href="/workflows">Workflows</Link>
        <Link role="tab" aria-selected={tab === 'bundles'} className={tab === 'bundles' ? 'on' : ''} href="/bundles">Bundles</Link>
      </div>
    </header>
    {tab === 'bundles' ? <Bundles/> : <Workflows id={id}/>}
  </>;
}

const TEMPLATE = `name: my-smoke-test
description: What this proves on hardware
environment:
  variables:
    MODE: test
prepare:
  - name: verify
    run: df -h
deploy:
  - name: load-build
    run: ./load_bundle.sh
test:
  - name: health-check
    run: ./run_test.sh
    timeout_seconds: 120
cleanup:
  - name: safe-state
    run: ./cleanup.sh
    continue_on_error: true
artifacts:
  - logs/**
`;

function Workflows({id}: {id?: string}) {
  const s = useTorch();
  const {query} = useLocation();
  const creating = query.has('new');
  const selected = s.workflows.find(w => w.id === id) ?? (creating ? undefined : s.workflows[0]);
  return <div className="page split-page">
    <aside className="split-list">
      <Link href="/workflows?new=1" className="btn btn-sm full"><Icon name="plus" size={13}/>New workflow</Link>
      {s.workflows.map(w => {
        const runs = s.runs.filter(r => r.workflow_name === w.name);
        return <Link key={w.id} href={`/workflows/${w.id}`} className={`split-item ${w.id === selected?.id && !creating ? 'on' : ''}`}>
          <Icon name="workflow" size={14}/>
          <span><b>{w.name}</b><small>v{w.version} · {runs.length} runs</small></span>
        </Link>;
      })}
    </aside>
    <section className="split-main">
      {creating ? <WorkflowEditor/> : selected ? <WorkflowView workflow={selected}/> : <Empty icon="workflow" title="No workflows yet" action={<Link className="btn btn-primary" href="/workflows?new=1">Create a workflow</Link>}>A workflow wraps the scripts you already have into prepare → deploy → configure → test → collect → cleanup.</Empty>}
    </section>
  </div>;
}

function outline(yaml: string) {
  const result: Record<string, string[]> = {};
  let phase = '';
  for (const line of yaml.split('\n')) {
    const top = /^([a-z_]+):\s*$/.exec(line);
    if (top) { phase = top[1] === 'setup' ? 'prepare' : top[1]; continue; }
    const step = /^\s*-\s*name:\s*(.+)$/.exec(line);
    if (step && (PHASES as readonly string[]).includes(phase)) (result[phase] ??= []).push(step[1].trim());
  }
  return result;
}

function Yaml({text}: {text: string}) {
  return <pre className="yaml">{text.split('\n').map((line, i) => {
    const m = /^(\s*-?\s*)([A-Za-z_]+)(:)(.*)$/.exec(line);
    return <div key={i}><span className="ln">{i + 1}</span>{m ? <>{m[1]}<span className="y-key">{m[2]}</span>{m[3]}<span className={/^\s*\.\/|^\s*[a-z]+ -/.test(m[4]) ? 'y-cmd' : 'y-val'}>{m[4]}</span></> : line.startsWith('#') ? <span className="muted">{line}</span> : line}</div>;
  })}</pre>;
}

function WorkflowView({workflow}: {workflow: Workflow}) {
  const s = useTorch();
  const actions = useActions();
  const runs = s.runs.filter(r => r.workflow_name === workflow.name);
  const finished = runs.filter(r => ['PASSED', 'FAILED', 'TIMED_OUT'].includes(r.status));
  const rate = finished.length ? Math.round((finished.filter(r => r.status === 'PASSED').length / finished.length) * 100) : undefined;
  const phases = outline(workflow.definition_yaml ?? '');
  return <div className="doc">
    <div className="doc-head">
      <div><span className="eyebrow">Workflow · v{workflow.version}</span><h1>{workflow.name}</h1><p className="muted">{workflow.description || 'No description'}</p></div>
      <button className="btn btn-primary btn-sm" onClick={() => actions.newRun({workflowId: workflow.id})}><Icon name="play" size={12}/>Run on a robot</button>
    </div>
    <div className="doc-stats">
      <div><span className="muted">Pass rate</span><b>{rate === undefined ? '—' : `${rate}%`}</b></div>
      <div><span className="muted">Runs</span><b>{runs.length}</b></div>
      <div><span className="muted">Updated</span><b>{ago(workflow.updated_at, s.now)}</b></div>
      <div className="grow"><span className="muted">Recent results</span><RunStrip runs={runs} slots={24} now={s.now}/></div>
    </div>
    <ol className="outline">
      {PHASES.map(p => <li key={p} className={phases[p] ? '' : 'empty'}><b>{p}</b>{phases[p]?.map(n => <span key={n} className="chip static">{n}</span>) ?? <span className="muted">—</span>}</li>)}
    </ol>
    {workflow.definition_yaml ? <Yaml text={workflow.definition_yaml}/> : <p className="muted">Definition not loaded.</p>}
  </div>;
}

function WorkflowEditor() {
  const engine = useEngine();
  const toast = useToast();
  const [yaml, setYaml] = useState(TEMPLATE);
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const save = async () => {
    setBusy(true); setError('');
    try { const w = await engine.createWorkflow(yaml); toast({tone: 'ok', title: `Workflow ${w.name} created`}); navigate(`/workflows/${w.id}`); }
    catch (e) { setError((e as Error).message); setBusy(false); }
  };
  return <div className="doc">
    <div className="doc-head"><div><span className="eyebrow">New workflow</span><h1>Describe a hardware test</h1><p className="muted">Steps run your existing scripts. Torch validates the YAML before saving.</p></div>
      <button className="btn btn-primary btn-sm" disabled={busy} onClick={() => void save()}>{busy ? 'Saving…' : 'Create workflow'} <Kbd>⌘↵</Kbd></button></div>
    <textarea className="code-editor" spellCheck={false} value={yaml} onChange={e => setYaml(e.target.value)} aria-label="Workflow YAML"
      onKeyDown={e => {
        if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) { e.preventDefault(); void save(); }
        if (e.key === 'Tab') { e.preventDefault(); const t = e.currentTarget, a = t.selectionStart; setYaml(v => v.slice(0, a) + '  ' + v.slice(t.selectionEnd)); requestAnimationFrame(() => { t.selectionStart = t.selectionEnd = a + 2; }); }
      }}/>
    {error && <p className="form-error" role="alert">{error}</p>}
  </div>;
}

function Bundles() {
  const s = useTorch();
  const engine = useEngine();
  const actions = useActions();
  const toast = useToast();
  const input = useRef<HTMLInputElement>(null);
  const [drag, setDrag] = useState(false);
  const upload = async (file?: File) => {
    if (!file) return;
    if (!/\.(tar\.gz|tgz)$/i.test(file.name)) { toast({tone: 'fail', title: 'Bundles must be .tar.gz or .tgz'}); return; }
    try { const b = await engine.uploadBundle(file); toast({tone: 'ok', title: `${b.name} uploaded`, body: 'SHA-256 recorded. Deploy it to any robot.', action: {label: 'Deploy & test', run: () => actions.newRun({bundleId: b.id})}}); }
    catch (e) { toast({tone: 'fail', title: 'Upload failed', body: (e as Error).message}); }
  };
  const onDrop = (e: DragEvent) => { e.preventDefault(); setDrag(false); void upload(e.dataTransfer.files[0]); };
  const bundles = [...s.bundles].sort((a, b) => Date.parse(b.created_at ?? '') - Date.parse(a.created_at ?? ''));
  return <div className="page">
    <div className={`dropzone ${drag ? 'drag' : ''}`} onDragOver={e => { e.preventDefault(); setDrag(true); }} onDragLeave={() => setDrag(false)} onDrop={onDrop}>
      <Icon name="upload" size={18}/>
      <div><b>Drop a .tar.gz build here</b><small className="muted">or <button className="link" onClick={() => input.current?.click()}>choose a file</button> · nodes verify SHA-256 and extract into a per-run workspace (100 MiB max)</small></div>
      <code className="muted">tar -C ./build -czf build.tar.gz .</code>
      <input ref={input} type="file" hidden accept=".tgz,.tar.gz,application/gzip" onChange={e => void upload(e.target.files?.[0])}/>
    </div>
    {bundles.length ? <div className="table" role="table" aria-label="Bundles">
      <div className="tr th bundles-row" role="row"><span>Bundle</span><span>SHA-256</span><span className="num">Size</span><span className="num">Used in</span><span className="num">Uploaded</span><span/></div>
      {bundles.map(b => {
        const used = s.runs.filter(r => r.bundle_id === b.id);
        return <div key={b.id} className="tr bundles-row" role="row">
          <span className="cell-robot"><b>{b.name}</b><small>{b.filename}</small></span>
          <span className="mono-sm" title={b.sha256}>{b.sha256.slice(0, 12)}…</span>
          <span className="num">{bytes(b.size_bytes)}</span>
          <span className="num">{used.length ? <Link href={`/runs?q=${encodeURIComponent(b.name)}`}>{used.length} runs</Link> : <span className="muted">—</span>}</span>
          <span className="num muted">{ago(b.created_at, s.now)}</span>
          <span className="num"><button className="btn btn-sm" onClick={() => actions.newRun({bundleId: b.id})}>Deploy & test</button></span>
        </div>;
      })}
    </div> : <Empty icon="package" title="No bundles yet">Upload a build to deploy it to robots.</Empty>}
  </div>;
}
