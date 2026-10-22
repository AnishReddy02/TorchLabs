import type {Activity, AgentEvent, AgentSession, Asset, Bundle, Diagnosis, Incident, Log, Run, Step, Telemetry, Tone, Workflow} from './types';
import {emptySnapshot, type Engine, type MissionInput, type Snapshot} from './engine';
import {PHASES, tone} from './format';

/**
 * A self-running fleet used when no control plane is reachable. It tells one coherent story:
 * rover-demo 2.4.1 broke localization, 2.4.2 fixes it, an agent is waiting to deploy the fix,
 * and a telemetry service on Rover-05 fails and recovers on its own a few seconds after load.
 */

let seed = 7;
const rand = () => { seed = (seed * 16807) % 2147483647; return (seed - 1) / 2147483646; };
const pick = <T>(xs: T[]) => xs[Math.floor(rand() * xs.length)];
const uid = () => {
  const hex = () => Math.floor(rand() * 0x10000).toString(16).padStart(4, '0');
  return `${hex()}${hex()}-${hex()}-4${hex().slice(1)}-a${hex().slice(1)}-${hex()}${hex()}${hex()}`;
};
const iso = (t: number) => new Date(t).toISOString();
const sha = () => Array.from({length: 64}, () => Math.floor(rand() * 16).toString(16)).join('');

const MIN = 60_000;

type Plan = {durations: number[]; failAt: number; failReason?: string};
type StepTemplate = {name: string; phase: string; run: string};

const WORKFLOW_TESTS: Record<string, StepTemplate[]> = {
  'rover-nav-smoke': [{name: 'nav-goal', phase: 'test', run: './nav_goal.sh --goal dock'}, {name: 'obstacle-stop', phase: 'test', run: './obstacle_stop.sh'}],
  'localization-regression': [{name: 'imu-calibration', phase: 'test', run: './imu_check.sh'}, {name: 'localization', phase: 'test', run: './localization.sh --map warehouse-b'}],
  'perception-latency': [{name: 'camera-pipeline', phase: 'test', run: './camera_latency.py --p99 45'}],
  'arm-pick-place': [{name: 'home-joints', phase: 'test', run: './home.sh'}, {name: 'pick-place-x20', phase: 'test', run: './pick_place.py --cycles 20'}],
  'battery-thermal-soak': [{name: 'thermal-soak', phase: 'test', run: './soak.sh --minutes 4'}],
  'drone-hover-check': [{name: 'hover-hold', phase: 'test', run: './hover_hold.sh --alt 1.5'}],
};
const stepsFor = (workflow: string): StepTemplate[] => [
  {name: 'verify-disk', phase: 'prepare', run: 'df -h /opt'},
  {name: 'load-bundle', phase: 'deploy', run: './load_bundle.sh'},
  {name: 'configure', phase: 'configure', run: './configure.sh'},
  ...(WORKFLOW_TESTS[workflow] ?? [{name: 'health-check', phase: 'test', run: './run_test.sh'}]),
  {name: 'export-results', phase: 'collect', run: './export_results.sh'},
  {name: 'safe-state', phase: 'cleanup', run: './cleanup.sh'},
];
const yamlFor = (name: string, description: string) => {
  const steps = stepsFor(name);
  const phase = (p: string) => steps.filter(s => s.phase === p).map(s => `  - name: ${s.name}\n    run: ${s.run}${s.phase === 'test' ? '\n    timeout_seconds: 300' : ''}${s.phase === 'cleanup' ? '\n    continue_on_error: true' : ''}`).join('\n');
  return `name: ${name}\ndescription: ${description}\nenvironment:\n  variables:\n    MODE: test\n${PHASES.map(p => `${p}:\n${phase(p)}`).join('\n')}\nartifacts:\n  - logs/**\n  - results/**\n`;
};

const LOC_FAIL_LOGS = (asset: string) => [
  '[localization] loading map warehouse-b (412 nodes)',
  '[localization] subscribing /imu/data, /odom, /scan',
  '[localization] imu calibration: reading /etc/torch/imu/calib.yaml',
  `[localization] WARN calibration file not found, falling back to identity bias (asset=${asset})`,
  '[localization] particle filter initialised: 2000 particles',
  '[localization] t+04.0s pose drift 0.08 m',
  '[localization] t+08.0s pose drift 0.41 m',
  '[localization] t+12.0s pose drift 1.27 m',
  'ERROR localization drift 1.27 m exceeds threshold 0.25 m',
  'AssertionError: localization did not converge within 12 s',
  'exit status 2',
];

export class DemoEngine implements Engine {
  private state: Snapshot = emptySnapshot('demo');
  private listeners = new Set<() => void>();
  private plans = new Map<string, Plan>();
  private timers: number[] = [];
  private ticker: number;
  private logSeq = 1;
  private evSeq = 1;

  constructor() {
    this.seed();
    this.ticker = window.setInterval(() => this.tick(), 700);
    this.later(9_000, () => this.startIncident());
    this.later(16_000, () => this.autoRun());
  }
  dispose() { clearInterval(this.ticker); this.timers.forEach(clearTimeout); }
  subscribe(fn: () => void) { this.listeners.add(fn); return () => { this.listeners.delete(fn); }; }
  get() { return this.state; }
  private emit() { this.state = {...this.state, now: Date.now()}; this.listeners.forEach(fn => fn()); }
  private later(ms: number, fn: () => void) { this.timers.push(window.setTimeout(() => { fn(); this.emit(); }, ms)); }
  private activity(tone: Tone, text: string, href?: string) {
    const item: Activity = {id: `a${this.evSeq++}`, at: iso(Date.now()), tone, text, href};
    this.state.activity = [item, ...this.state.activity].slice(0, 60);
  }

  // ---------- seed ----------
  private seed() {
    const now = Date.now();
    const asset = (name: string, hardware_type: string, os: string, architecture: string, caps: string[], status = 'AVAILABLE', team = 'autonomy'): Asset => ({
      id: uid(), name, status, hostname: `${name.toLowerCase()}.lab.torch`, os, architecture, hardware_type,
      capabilities: Object.fromEntries(caps.map(c => [c, true])), labels: {team, site: 'sf-lab-2'}, last_seen_at: iso(now - Math.floor(rand() * 8000)),
      description: `${hardware_type} on bench ${name.split('-')[1]}`,
    });
    const assets = [
      asset('Rover-01', 'Clearpath Jackal', 'Ubuntu 22.04', 'aarch64', ['jetson-orin', 'lidar', 'can', 'nix']),
      asset('Rover-02', 'Clearpath Jackal', 'Ubuntu 22.04', 'aarch64', ['jetson-orin', 'lidar', 'can']),
      asset('Rover-03', 'Clearpath Jackal', 'Ubuntu 22.04', 'aarch64', ['jetson-orin', 'lidar', 'can', 'nix']),
      asset('Rover-04', 'Husky A200', 'Ubuntu 20.04', 'x86_64', ['lidar', 'gps', 'can']),
      asset('Rover-05', 'Husky A200', 'Ubuntu 22.04', 'x86_64', ['lidar', 'gps', 'can', 'nix']),
      asset('Arm-01', 'UR5e', 'Ubuntu 22.04', 'x86_64', ['realsense', 'gripper'], 'AVAILABLE', 'manipulation'),
      asset('Arm-02', 'UR5e', 'Ubuntu 22.04', 'x86_64', ['realsense', 'gripper'], 'AVAILABLE', 'manipulation'),
      asset('Arm-03', 'Franka FR3', 'Ubuntu 22.04', 'x86_64', ['realsense', 'gripper', 'nix'], 'AVAILABLE', 'manipulation'),
      asset('Drone-01', 'Holybro X500', 'Ubuntu 22.04', 'aarch64', ['px4', 'gps'], 'AVAILABLE', 'aerial'),
      asset('Drone-02', 'Holybro X500', 'Ubuntu 22.04', 'aarch64', ['px4', 'gps'], 'OFFLINE', 'aerial'),
      asset('Dock-01', 'Charging dock', 'Debian 12', 'aarch64', ['can'], 'AVAILABLE', 'platform'),
      asset('Bench-HIL', 'Jetson Orin HIL rig', 'Ubuntu 22.04', 'aarch64', ['jetson-orin', 'can', 'nix'], 'AVAILABLE', 'platform'),
    ];
    assets[9].last_seen_at = iso(now - 23 * MIN);

    const wf = (name: string, description: string, version: number): Workflow => ({id: uid(), name, description, version, definition_yaml: yamlFor(name, description), updated_at: iso(now - Math.floor(rand() * 5 * 86400_000))});
    const workflows = [
      wf('localization-regression', 'IMU calibration and localization convergence on the warehouse-b map', 7),
      wf('rover-nav-smoke', 'Drive to dock and verify obstacle stop', 12),
      wf('perception-latency', 'Camera-to-detection p99 latency budget', 4),
      wf('arm-pick-place', '20-cycle pick and place with grasp verification', 9),
      wf('battery-thermal-soak', 'Four-minute load soak with thermal limits', 3),
      wf('drone-hover-check', 'Tethered hover hold at 1.5 m', 2),
    ];

    const bundle = (name: string, version: string, hoursAgo: number, kb: number): Bundle => ({id: uid(), name: `${name}-${version}`, filename: `${name}-${version}.tar.gz`, size_bytes: kb * 1024, sha256: sha(), created_at: iso(now - hoursAgo * 3600_000)});
    const bundles = [
      bundle('rover-demo', '2.4.2', 0.6, 18_420), bundle('rover-demo', '2.4.1', 9, 18_377), bundle('rover-demo', '2.4.0', 30, 18_102),
      bundle('arm-control', '0.9.3', 20, 6_220), bundle('perception', '1.12.0', 14, 42_870), bundle('px4-mission', '0.3.1', 40, 2_310),
    ];
    this.state = {...this.state, assets, workflows, bundles};

    // 24h of history: loop time trends down as the team adopts Torch, localization breaks with 2.4.1.
    const pairs: [string, string[]][] = [
      ['localization-regression', ['Rover-01', 'Rover-02', 'Rover-03', 'Rover-05']],
      ['rover-nav-smoke', ['Rover-01', 'Rover-02', 'Rover-03', 'Rover-04', 'Rover-05']],
      ['perception-latency', ['Rover-01', 'Bench-HIL', 'Rover-03']],
      ['arm-pick-place', ['Arm-01', 'Arm-02', 'Arm-03']],
      ['battery-thermal-soak', ['Dock-01', 'Rover-04']],
      ['drone-hover-check', ['Drone-01', 'Drone-02']],
    ];
    const actors = ['maya', 'dev', 'jonas', 'priya', 'torch-agent'];
    const runs: Run[] = [];
    const bundle241At = now - 9 * 3600_000;
    for (let i = 0; i < 150; i++) {
      const age = (i / 150) * 24 * 3600_000 + rand() * 6 * MIN + 4 * MIN;
      const created = now - age;
      const [workflowName, robots] = pick(pairs);
      const robot = pick(robots);
      if (robot === 'Drone-02' && created > now - 3 * 3600_000) continue;
      const progress = 1 - age / (24 * 3600_000);
      const loop = (9 - progress * 5.2) * MIN * (0.7 + rand() * 0.6);
      let b: Bundle | undefined;
      if (workflowName.startsWith('loc') || workflowName.startsWith('rover')) b = created > bundle241At ? bundles[1] : bundles[2];
      if (workflowName.startsWith('arm')) b = bundles[3];
      if (workflowName.startsWith('perc')) b = bundles[4];
      if (workflowName.startsWith('drone')) b = bundles[5];
      let failAt = -1; let reason: string | undefined;
      const steps = stepsFor(workflowName);
      if (workflowName === 'localization-regression' && b === bundles[1]) { failAt = steps.findIndex(s => s.name === 'localization'); reason = 'localization drift 1.27 m exceeds threshold 0.25 m'; }
      else if (robot === 'Arm-02' && rand() < 0.35) { failAt = steps.findIndex(s => s.phase === 'test'); reason = 'gripper did not report closed within 2.0 s'; }
      else if (rand() < 0.04) { failAt = steps.findIndex(s => s.phase === 'test'); reason = 'test timed out after 300 s'; }
      runs.push(this.buildRun(workflowName, robot, b, created, loop, failAt, reason, pick(actors)));
    }
    // The featured failure, freshly minted so it tops the inbox.
    const featured = this.buildRun('localization-regression', 'Rover-03', bundles[1], now - 7 * MIN, 4.6 * MIN, stepsFor('localization-regression').findIndex(s => s.name === 'localization'), 'localization drift 1.27 m exceeds threshold 0.25 m', 'maya');
    runs.push(featured);
    runs.sort((a, b) => Date.parse(b.created_at) - Date.parse(a.created_at));
    this.state.runs = runs;

    // Two runs in flight right now.
    this.startRun('perception-latency', 'Rover-01', bundles[4], 'priya', {offset: 3});
    this.startRun('arm-pick-place', 'Arm-01', bundles[3], 'jonas', {offset: 1});

    this.state.incidents = [this.pastIncident('Arm-02', 'gripper-driver', now - 5 * 3600_000, 38), this.pastIncident('Rover-02', 'telemetry-bridge', now - 15 * 3600_000, 52)];
    this.state.sessions = this.seedSessions(featured, bundles[0]);
    this.state.activity = runs.slice(0, 14).map(r => ({id: `seed-${r.id}`, at: r.completed_at ?? r.created_at, tone: tone(r.status), text: `${r.workflow_name} on ${r.asset_name} · ${r.status.toLowerCase().replaceAll('_', ' ')}`, href: `/runs/${r.id}`}));
    for (const a of assets) this.state.telemetry[a.name] = this.telemetryFor(a);
  }

  private buildRun(workflowName: string, robot: string, b: Bundle | undefined, created: number, loop: number, failAt: number, reason: string | undefined, actor: string): Run {
    const templates = stepsFor(workflowName);
    const weights = templates.map(s => s.phase === 'test' ? 5 : s.phase === 'deploy' ? 2.5 : 1);
    const total = weights.reduce((a, b) => a + b, 0);
    const queue = 8_000 + rand() * 20_000;
    let t = created + queue;
    const runId = uid();
    const steps: Step[] = [];
    const logs: Log[] = [];
    let status = 'PASSED';
    for (let i = 0; i < templates.length; i++) {
      const tpl = templates[i];
      const d = ((loop - queue) * weights[i]) / total;
      const failed = i === failAt;
      const skipped = failAt >= 0 && i > failAt && tpl.phase !== 'cleanup';
      const step: Step = {id: uid(), name: tpl.name, phase: tpl.phase, position: i, status: skipped ? 'SKIPPED' : failed ? 'FAILED' : 'PASSED', exit_code: skipped ? null : failed ? 2 : 0, error: failed ? reason : null, started_at: skipped ? null : iso(t), completed_at: skipped ? null : iso(t + d)};
      if (!skipped) {
        logs.push(...this.logsFor(step, tpl, robot, b, t, d, failed && workflowName === 'localization-regression', reason));
        t += d;
      }
      steps.push(step);
      if (failed) status = 'FAILED';
    }
    const workflow = this.state.workflows.find(w => w.name === workflowName)!;
    const asset = this.state.assets.find(a => a.name === robot)!;
    const run: Run = {
      id: runId, workflow_id: workflow.id, asset_id: asset.id, asset_name: robot, workflow_name: workflowName, status,
      error: status === 'FAILED' ? reason : null, bundle_id: b?.id ?? null, bundle_name: b?.name, commit: sha().slice(0, 7), actor, trigger: actor === 'torch-agent' ? 'agent' : 'human',
      created_at: iso(created), started_at: iso(created + queue), completed_at: iso(t), steps,
    };
    const artifacts = status === 'PASSED' || workflowName === 'localization-regression' ? [
      {id: uid(), name: 'results.json', path: 'results/results.json', size_bytes: 2_400 + Math.floor(rand() * 4000), content_type: 'application/json'},
      {id: uid(), name: 'run.log', path: 'logs/run.log', size_bytes: 40_000 + Math.floor(rand() * 90_000), content_type: 'text/plain'},
      ...(workflowName === 'localization-regression' ? [{id: uid(), name: 'pose_trace.mcap', path: 'results/pose_trace.mcap', size_bytes: 3_800_000, content_type: 'application/octet-stream'}] : []),
    ] : [];
    this.state.runDetails[runId] = {run, logs, artifacts, diagnosis: this.diagnose(run, logs)};
    return run;
  }

  private logsFor(step: Step, tpl: StepTemplate, robot: string, b: Bundle | undefined, start: number, d: number, locFail: boolean, reason?: string): Log[] {
    let lines: string[];
    if (locFail) lines = LOC_FAIL_LOGS(robot);
    else if (tpl.phase === 'prepare') lines = [`$ ${tpl.run}`, 'Filesystem      Size  Used Avail Use% Mounted on', '/dev/nvme0n1p2  234G   61G  161G  28% /opt', 'chrony: offset +0.4 ms'];
    else if (tpl.phase === 'deploy') lines = [`$ ${tpl.run}`, `fetching bundle ${b?.filename ?? 'none'}`, `sha256 ${b?.sha256.slice(0, 16) ?? '—'}… verified`, 'extracting into /var/lib/torch/workspaces/run', b?.name === 'rover-demo-2.4.1' ? 'note: config layout v3 (imu calib moved to /etc/torch/rover/imu.yaml)' : 'layout ok', 'bundle loaded'];
    else if (tpl.phase === 'configure') lines = [`$ ${tpl.run}`, 'MODE=test', `ROBOT_ID=${robot}`, 'ros2 daemon started', 'configuration applied'];
    else if (tpl.phase === 'collect') lines = [`$ ${tpl.run}`, 'collected logs/** (3 files)', 'collected results/** (2 files)'];
    else if (tpl.phase === 'cleanup') lines = [`$ ${tpl.run}`, 'motors disabled', 'asset returned to safe state'];
    else if (step.status === 'FAILED') lines = [`$ ${tpl.run}`, 'starting test', `ERROR ${reason}`, 'exit status 2'];
    else lines = [`$ ${tpl.run}`, 'starting test', ...(tpl.name === 'localization' ? ['imu calibration: /etc/torch/rover/imu.yaml', 'pose drift 0.06 m', 'converged in 5.8 s'] : tpl.name === 'camera-pipeline' ? ['p50 21 ms', 'p99 38 ms (budget 45 ms)'] : ['ok']), 'PASS'];
    return lines.map((content, i) => ({id: this.logSeq++, stream: /ERROR|Assertion|exit status [1-9]|WARN/.test(content) ? 'stderr' : 'stdout', content, step_id: step.id, created_at: iso(start + (d * i) / lines.length)}));
  }

  private diagnose(run: Run, logs: Log[]): Diagnosis | undefined {
    if (run.status !== 'FAILED') return undefined;
    const lineOf = (needle: string) => { const i = logs.findIndex(l => l.content.includes(needle)); return i >= 0 ? i + 1 : undefined; };
    if (run.workflow_name === 'localization-regression') return {
      summary: 'Localization diverged because the IMU calibration file was not found after deploying rover-demo-2.4.1.',
      likely_cause: 'rover-demo-2.4.1 moved IMU calibration to /etc/torch/rover/imu.yaml, but the localization node still reads /etc/torch/imu/calib.yaml and falls back to an identity bias.',
      evidence: [
        {text: 'Bundle 2.4.1 announces config layout v3 with the calibration moved', line: lineOf('config layout v3')},
        {text: 'Localization falls back to identity IMU bias', line: lineOf('calibration file not found')},
        {text: 'Drift grows 0.08 → 1.27 m in 12 s, consistent with uncorrected gyro bias', line: lineOf('ERROR localization drift')},
        {text: 'Same workflow passed on Rover-03 with rover-demo-2.4.0 (last 9 h of runs)'},
      ],
      next_action: 'Rerun with rover-demo-2.4.2, which restores the calibration path. No hardware change needed.',
      confidence: 0.86,
    };
    if (run.asset_name === 'Arm-02') return {
      summary: 'Arm-02 gripper intermittently misses its closed-state report.',
      likely_cause: 'Flaky on this robot only: the same bundle passes on Arm-01 and Arm-03, which points at Arm-02 hardware (gripper sensor or cable) rather than software.',
      evidence: [{text: 'Gripper did not report closed within 2.0 s', line: lineOf('ERROR')}, {text: 'Arm-01 and Arm-03 passed with arm-control-0.9.3 in the same window'}],
      next_action: 'Mark Arm-02 for inspection and route arm-pick-place to Arm-01 or Arm-03.',
      confidence: 0.71,
    };
    return {summary: run.error ?? 'Test failed.', likely_cause: 'Not enough signal yet. This failure has not been seen on other robots.', evidence: [{text: run.error ?? 'Non-zero exit', line: lineOf('ERROR')}], next_action: 'Rerun once to separate flake from regression.', confidence: 0.4};
  }

  // ---------- live simulation ----------
  private startRun(workflowName: string, robot: string, b: Bundle | undefined, actor: string, opts: {offset?: number; trigger?: Run['trigger']} = {}): Run {
    const workflow = this.state.workflows.find(w => w.name === workflowName)!;
    const asset = this.state.assets.find(a => a.name === robot)!;
    const templates = stepsFor(workflowName);
    const now = Date.now();
    const steps: Step[] = templates.map((t, i) => ({id: uid(), name: t.name, phase: t.phase, position: i, status: 'PENDING', exit_code: null, started_at: null, completed_at: null}));
    const willFail = workflowName === 'localization-regression' && b?.name === 'rover-demo-2.4.1';
    const plan: Plan = {durations: templates.map(t => (t.phase === 'test' ? 7000 : t.phase === 'deploy' ? 4000 : 2200) * (0.8 + rand() * 0.4)), failAt: willFail ? templates.findIndex(t => t.name === 'localization') : -1, failReason: willFail ? 'localization drift 1.27 m exceeds threshold 0.25 m' : undefined};
    const run: Run = {id: uid(), workflow_id: workflow.id, asset_id: asset.id, asset_name: robot, workflow_name: workflowName, status: 'QUEUED', bundle_id: b?.id ?? null, bundle_name: b?.name, commit: sha().slice(0, 7), actor, trigger: opts.trigger ?? (actor === 'torch-agent' ? 'agent' : 'human'), created_at: iso(now), started_at: null, completed_at: null, steps};
    // Seeded in-flight runs start partway through so the dashboard opens mid-motion.
    const logs: Log[] = [];
    if (opts.offset) {
      run.status = 'RUNNING'; run.started_at = iso(now - 20_000);
      let t = now - 20_000;
      for (let i = 0; i < opts.offset; i++) {
        steps[i] = {...steps[i], status: 'PASSED', exit_code: 0, started_at: iso(t), completed_at: iso(t + plan.durations[i])};
        logs.push(...this.logsFor(steps[i], templates[i], robot, b, t, plan.durations[i], false));
        t += plan.durations[i];
      }
      steps[opts.offset] = {...steps[opts.offset], status: 'RUNNING', started_at: iso(now - 1500)};
    }
    asset.status = 'RESERVED';
    this.plans.set(run.id, plan);
    this.state.runs = [run, ...this.state.runs];
    this.state.runDetails[run.id] = {run, logs, artifacts: []};
    this.state.assets = [...this.state.assets];
    if (!opts.offset) this.activity('wait', `${workflowName} queued on ${robot}`, `/runs/${run.id}`);
    return run;
  }

  private tick() {
    const now = Date.now();
    let changed = false;
    this.state.runs = this.state.runs.map(run => {
      const plan = this.plans.get(run.id);
      if (!plan || !['QUEUED', 'RUNNING'].includes(run.status)) return run;
      changed = true;
      const steps = [...(run.steps ?? [])];
      const detail = this.state.runDetails[run.id];
      if (run.status === 'QUEUED') {
        if (now - Date.parse(run.created_at) < 1800) return run;
        steps[0] = {...steps[0], status: 'RUNNING', started_at: iso(now)};
        this.activity('live', `${run.workflow_name} started on ${run.asset_name}`, `/runs/${run.id}`);
        const next = {...run, status: 'RUNNING', started_at: iso(now), steps};
        this.state.runDetails[run.id] = {...detail, run: next};
        return next;
      }
      const i = steps.findIndex(s => s.status === 'RUNNING');
      if (i < 0) return run;
      const step = steps[i];
      const tpl = stepsFor(run.workflow_name)[i];
      const elapsed = now - Date.parse(step.started_at!);
      const failing = i === plan.failAt;
      // Stream log lines across the step's duration.
      const bundle = this.state.bundles.find(b => b.id === run.bundle_id);
      const all = this.logsFor(step, tpl, run.asset_name, bundle, Date.parse(step.started_at!), plan.durations[i], failing && run.workflow_name === 'localization-regression', plan.failReason);
      const have = detail.logs.filter(l => l.step_id === step.id).length;
      const due = Math.min(all.length, Math.ceil((elapsed / plan.durations[i]) * all.length));
      const logs = due > have ? [...detail.logs, ...all.slice(have, due).map(l => ({...l, created_at: iso(now)}))] : detail.logs;
      let next: Run = {...run, steps};
      if (elapsed >= plan.durations[i]) {
        steps[i] = {...step, status: failing ? 'FAILED' : 'PASSED', exit_code: failing ? 2 : 0, error: failing ? plan.failReason : null, completed_at: iso(now)};
        let j = i + 1;
        if (failing) { for (; j < steps.length && steps[j].phase !== 'cleanup'; j++) steps[j] = {...steps[j], status: 'SKIPPED'}; }
        if (j < steps.length) steps[j] = {...steps[j], status: 'RUNNING', started_at: iso(now)};
        else next = this.finishRun(next, steps.some(s => s.status === 'FAILED') ? 'FAILED' : 'PASSED', plan.failReason);
      }
      const finished = {...next, steps};
      this.state.runDetails[run.id] = {...this.state.runDetails[run.id], run: finished, logs};
      if (isDone(finished.status)) this.state.runDetails[run.id].diagnosis = this.diagnose(finished, logs);
      return finished;
    });
    if (changed) this.emit();
  }

  private finishRun(run: Run, status: string, reason?: string): Run {
    const asset = this.state.assets.find(a => a.id === run.asset_id);
    if (asset) asset.status = 'AVAILABLE';
    this.state.assets = [...this.state.assets];
    this.plans.delete(run.id);
    this.state.runDetails[run.id] = {...this.state.runDetails[run.id], artifacts: [
      {id: uid(), name: 'results.json', path: 'results/results.json', size_bytes: 3_120, content_type: 'application/json'},
      {id: uid(), name: 'run.log', path: 'logs/run.log', size_bytes: 61_540, content_type: 'text/plain'},
    ]};
    this.activity(status === 'PASSED' ? 'ok' : 'fail', `${run.workflow_name} ${status === 'PASSED' ? 'passed' : 'failed'} on ${run.asset_name}`, `/runs/${run.id}`);
    this.onFinish.get(run.id)?.(status);
    this.onFinish.delete(run.id);
    return {...run, status, error: status === 'FAILED' ? reason : null, completed_at: iso(Date.now())};
  }
  private onFinish = new Map<string, (status: string) => void>();

  private autoRun() {
    const active = this.state.runs.filter(r => ['QUEUED', 'RUNNING'].includes(r.status)).length;
    const free = this.state.assets.filter(a => a.status === 'AVAILABLE' && !['Rover-03', 'Rover-05'].includes(a.name));
    if (active < 3 && free.length) {
      const asset = pick(free);
      const workflow = asset.name.startsWith('Arm') ? 'arm-pick-place' : asset.name.startsWith('Drone') ? 'drone-hover-check' : asset.name.startsWith('Dock') ? 'battery-thermal-soak' : pick(['rover-nav-smoke', 'perception-latency']);
      const b = this.state.bundles.find(x => x.name.startsWith(workflow.startsWith('arm') ? 'arm' : workflow.startsWith('perc') ? 'perception' : workflow.startsWith('drone') ? 'px4' : 'rover-demo-2.4.2'));
      this.startRun(workflow, asset.name, b, pick(['maya', 'dev', 'jonas', 'priya']));
    }
    this.later(22_000 + rand() * 10_000, () => this.autoRun());
  }

  // ---------- incidents ----------
  private pastIncident(asset: string, service: string, detected: number, seconds: number): Incident {
    const at = (s: number) => iso(detected + s * 1000);
    return {
      id: uid(), asset_name: asset, service_id: service, status: 'RESOLVED', severity: 'MEDIUM', detected_at: at(0), resolved_at: at(seconds), remediation_attempts: 1,
      diagnosis: {source: 'model', summary: `${service} stopped answering health checks after a socket error.`, likely_cause: 'Process wedged on a stale CAN socket after bus reset.', confidence: 0.82},
      proposed_remediation: {action: 'restart_service', target: service, reason: 'Allowlisted, noncritical service; restart clears stale socket.'},
      verification_result: {healthy: true, consecutive_healthy_checks: 3, observed_for_seconds: 15},
      events: [
        {id: 1, kind: 'DETECTED', actor: 'monitor', created_at: at(0), data: {message: 'Health check failed 3× in a row'}},
        {id: 2, kind: 'DIAGNOSED', actor: 'torch-agent', created_at: at(9), data: {message: 'Stale CAN socket after bus reset'}},
        {id: 3, kind: 'AUTHORIZED', actor: 'policy', created_at: at(10), data: {message: 'restart_service allowed for noncritical service'}},
        {id: 4, kind: 'REMEDIATED', actor: 'node', created_at: at(14), data: {message: `docker restart ${service}`}},
        {id: 5, kind: 'VERIFIED', actor: 'monitor', created_at: at(seconds), data: {message: '3 consecutive healthy checks'}},
      ],
    };
  }

  private startIncident() {
    const asset = this.state.assets.find(a => a.name === 'Rover-05')!;
    const t0 = Date.now();
    const incident: Incident = {id: uid(), asset_name: 'Rover-05', service_id: 'telemetry-bridge', status: 'DETECTED', severity: 'HIGH', detected_at: iso(t0), remediation_attempts: 0, events: [], diagnosis: null, proposed_remediation: null, verification_result: null};
    const event = (kind: string, actor: string, message: string, data: Record<string, unknown> = {}) => {
      incident.events = [...(incident.events ?? []), {id: this.evSeq++, kind, actor, created_at: iso(Date.now()), data: {message, ...data}}];
    };
    const update = (patch: Partial<Incident>, tone: Tone, text: string) => {
      Object.assign(incident, patch);
      this.state.incidents = [{...incident}, ...this.state.incidents.filter(i => i.id !== incident.id)];
      this.state.incidentDetails[incident.id] = {...incident};
      this.activity(tone, text, `/incidents/${incident.id}`);
    };
    asset.status = 'DEGRADED';
    this.state.assets = [...this.state.assets];
    event('DETECTED', 'monitor', 'GET /health returned 503 three times in 15 s');
    update({}, 'fail', 'telemetry-bridge unhealthy on Rover-05');
    this.later(4_000, () => { event('INVESTIGATING', 'torch-agent', 'Collecting bounded evidence: last 200 log lines, process table, socket state'); update({status: 'INVESTIGATING'}, 'live', 'Torch is investigating telemetry-bridge on Rover-05'); });
    this.later(10_000, () => {
      event('DIAGNOSED', 'torch-agent', 'Event loop blocked on a full outbound queue after uplink drop', {confidence: 0.88});
      update({status: 'REMEDIATING', diagnosis: {source: 'model', summary: 'telemetry-bridge is alive but its event loop is blocked on a full outbound queue after the uplink dropped for 40 s.', likely_cause: 'Unbounded retry against a dead uplink saturates the queue; the health endpoint shares the same loop.', evidence: ['503 from /health ×3', 'queue depth 65,536 (max)', 'uplink restored 12 s before detection'], confidence: 0.88}, proposed_remediation: {action: 'restart_service', target: 'telemetry-bridge', reason: 'Allowlisted noncritical service; restart drains the queue. Root fix: bound the retry queue.'}}, 'live', 'Diagnosis ready for telemetry-bridge');
    });
    this.later(12_000, () => { event('AUTHORIZED', 'policy', 'restart_service is allowlisted for telemetry-bridge (noncritical)'); update({}, 'ok', 'Policy authorized restart of telemetry-bridge'); });
    this.later(15_000, () => { event('REMEDIATED', 'node', 'docker restart telemetry-bridge (exit 0, 1.9 s)'); update({status: 'VERIFYING', remediation_attempts: 1, executions: [{action: 'restart_service', target: 'telemetry-bridge', exit_code: 0, duration_ms: 1900, authorized_by: 'policy'}]}, 'live', 'Verifying telemetry-bridge recovery'); });
    this.later(27_000, () => {
      event('VERIFIED', 'monitor', '3 consecutive healthy checks over 12 s');
      asset.status = 'AVAILABLE'; this.state.assets = [...this.state.assets];
      update({status: 'RESOLVED', resolved_at: iso(Date.now()), verification_result: {healthy: true, consecutive_healthy_checks: 3, observed_for_seconds: 12, p95_latency_ms: 41}}, 'ok', `Rover-05 recovered automatically in ${Math.round((Date.now() - t0) / 1000)}s`);
    });
    this.emit();
  }

  // ---------- agent ----------
  private seedSessions(featured: Run, fix: Bundle): AgentSession[] {
    const now = Date.now();
    const ev = (kind: string, secondsAgo: number, data: Record<string, unknown>): AgentEvent => ({id: this.evSeq++, kind, created_at: iso(now - secondsAgo * 1000), data});
    const waiting: AgentSession = {
      id: uid(), prompt: 'Find out why localization-regression fails on Rover-03 and verify a fix on hardware.', status: 'WAITING_FOR_APPROVAL', paused: false, decision: null, created_at: iso(now - 6 * MIN),
      constraints: {target_asset: 'Rover-03', max_runtime_minutes: 20, stop_after_failure: true, software_changes_require_approval: true, arbitrary_shell: 'denied'},
      pending: {request_id: uid(), asset: 'Rover-03', workflow: 'localization-regression', reason: `Run ${featured.id.slice(0, 8)} failed because rover-demo-2.4.1 moved the IMU calibration file. ${fix.name} restores the path. Deploying it and rerunning the same workflow will confirm the fix on the same robot.`, bundle: {name: fix.name, sha256: fix.sha256, filename: fix.filename}, notice: 'Torch will transfer the bundle, verify its SHA-256, run the workflow, and return Rover-03 to a safe state.'},
      events: [
        ev('MISSION_STARTED', 360, {message: 'Mission accepted with a 20-minute limit on Rover-03'}),
        ev('TOOL_CALL', 352, {message: 'list_runs(workflow="localization-regression", asset="Rover-03")', action: {tool: 'list_runs'}}),
        ev('OBSERVATION', 349, {message: '6 of the last 6 runs with rover-demo-2.4.1 failed; all runs with 2.4.0 passed'}),
        ev('TOOL_CALL', 340, {message: `get_logs(run="${featured.id.slice(0, 8)}", step="localization")`, action: {tool: 'get_logs'}}),
        ev('OBSERVATION', 336, {message: 'calibration file not found at /etc/torch/imu/calib.yaml → identity bias → drift 1.27 m'}),
        ev('TOOL_CALL', 330, {message: 'diff_bundles("rover-demo-2.4.0", "rover-demo-2.4.1")', action: {tool: 'diff_bundles'}}),
        ev('OBSERVATION', 322, {message: '2.4.1 moves imu calibration to /etc/torch/rover/imu.yaml; 2.4.2 adds a compatibility path'}),
        ev('APPROVAL_REQUESTED', 300, {message: `Deploy ${fix.name} to Rover-03 and run localization-regression`}),
      ],
    };
    const done: AgentSession = {
      id: uid(), prompt: 'Run the nightly rover-nav-smoke across all Jackal rovers and summarize regressions.', status: 'COMPLETED', paused: false, decision: true, created_at: iso(now - 11 * 3600_000),
      constraints: {target_asset: null, max_runtime_minutes: 60, stop_after_failure: false},
      report: {summary: 'rover-nav-smoke passed on Rover-01, Rover-02, and Rover-03. No regressions against the previous night.', likely_cause: 'n/a', evidence: ['3/3 runs passed', 'Median dock time 41.2 s (−1.8 s vs. last night)'], recommended_next_action: 'None. Keep the nightly schedule.', confidence: 0.93},
      events: [ev('MISSION_STARTED', 39600, {message: 'Mission accepted'}), ev('COMPLETED', 38400, {message: 'Report ready'})],
    };
    const failed: AgentSession = {
      id: uid(), prompt: 'Check whether Drone-02 can complete drone-hover-check after the ESC firmware update.', status: 'FAILED', paused: false, decision: null, created_at: iso(now - 2 * 3600_000), error: 'Drone-02 went offline before the run could start. Firmware changes are outside agent permissions.',
      constraints: {target_asset: 'Drone-02', max_runtime_minutes: 20, stop_after_failure: true},
      events: [ev('MISSION_STARTED', 7200, {message: 'Mission accepted'}), ev('RESERVATION_FAILED', 7150, {message: 'Drone-02 is OFFLINE'}), ev('FAILED', 7140, {message: 'Asset unavailable'})],
    };
    return [waiting, failed, done];
  }

  private sessionEvent(session: AgentSession, kind: string, message: string) {
    session.events = [...session.events, {id: this.evSeq++, kind, created_at: iso(Date.now()), data: {message}}];
    this.state.sessions = this.state.sessions.map(s => s.id === session.id ? {...session} : s);
  }

  private telemetryFor(asset: Asset): Telemetry[] {
    const services = ['telemetry-bridge', 'nav-stack', 'can-gateway', 'camera-driver'];
    return Array.from({length: 12}, (_, i) => {
      const service = services[i % services.length];
      const healthy = !(asset.name === 'Drone-02' && i < 4);
      return {id: uid(), service_id: service, healthy, summary: healthy ? `${service} ok · ${Math.round(20 + rand() * 40)} ms` : `${service} unreachable`, observed_at: iso(Date.now() - i * 20_000)};
    });
  }

  // ---------- Engine API ----------
  watchRun() { return () => {}; }
  watchIncident(id: string) {
    const incident = this.state.incidents.find(i => i.id === id);
    if (incident) this.state.incidentDetails[id] = incident;
    return () => {};
  }
  watchSession() { return () => {}; }
  watchTelemetry() { return () => {}; }

  async createRun(input: {asset_id: string; workflow_id: string; bundle_id?: string | null}) {
    const asset = this.state.assets.find(a => a.id === input.asset_id);
    const workflow = this.state.workflows.find(w => w.id === input.workflow_id);
    if (!asset || !workflow) throw new Error('Unknown robot or workflow');
    if (asset.status === 'OFFLINE') throw new Error(`${asset.name} is offline`);
    if (this.state.runs.some(r => r.asset_id === asset.id && ['QUEUED', 'RUNNING'].includes(r.status))) throw new Error(`${asset.name} is busy with another run`);
    const run = this.startRun(workflow.name, asset.name, this.state.bundles.find(b => b.id === input.bundle_id), 'you');
    this.emit(); return run;
  }
  async rerun(id: string, bundleId?: string | null) {
    const original = this.state.runs.find(r => r.id === id);
    if (!original?.workflow_id) throw new Error('Run not found');
    return this.createRun({asset_id: original.asset_id, workflow_id: original.workflow_id, bundle_id: bundleId === undefined ? original.bundle_id : bundleId});
  }
  async cancelRun(id: string) {
    const run = this.state.runs.find(r => r.id === id);
    if (!run) return;
    this.plans.delete(id);
    const steps = (run.steps ?? []).map(s => s.status === 'RUNNING' || s.status === 'PENDING' ? {...s, status: 'CANCELLED'} : s);
    const next = {...run, status: 'CANCELLED', steps, completed_at: iso(Date.now())};
    this.state.runs = this.state.runs.map(r => r.id === id ? next : r);
    this.state.runDetails[id] = {...this.state.runDetails[id], run: next};
    const asset = this.state.assets.find(a => a.id === run.asset_id);
    if (asset) asset.status = 'AVAILABLE';
    this.state.assets = [...this.state.assets];
    this.activity('idle', `${run.workflow_name} cancelled on ${run.asset_name}`, `/runs/${id}`);
    this.emit();
  }
  async reserve(assetId: string) { this.setAsset(assetId, 'RESERVED', 'reserved by you'); }
  async release(assetId: string) { this.setAsset(assetId, 'AVAILABLE', 'released'); }
  private setAsset(id: string, status: string, verb: string) {
    this.state.assets = this.state.assets.map(a => a.id === id ? {...a, status} : a);
    const a = this.state.assets.find(x => x.id === id);
    if (a) this.activity(tone(status), `${a.name} ${verb}`, `/fleet/${a.name}`);
    this.emit();
  }
  async createWorkflow(yaml: string) {
    const name = /^name:\s*(.+)$/m.exec(yaml)?.[1]?.trim();
    if (!name) throw new Error('invalid workflow YAML: missing field `name`');
    const description = /^description:\s*(.+)$/m.exec(yaml)?.[1]?.trim() ?? '';
    const w: Workflow = {id: uid(), name, description, version: 1, definition_yaml: yaml, updated_at: iso(Date.now())};
    this.state.workflows = [w, ...this.state.workflows];
    this.emit(); return w;
  }
  async uploadBundle(file: File) {
    const b: Bundle = {id: uid(), name: file.name.replace(/\.(tar\.gz|tgz)$/i, ''), filename: file.name, size_bytes: file.size, sha256: sha(), created_at: iso(Date.now())};
    this.state.bundles = [b, ...this.state.bundles];
    this.emit(); return b;
  }
  async startMission(input: MissionInput) {
    const session: AgentSession = {id: uid(), prompt: input.prompt, status: 'PLANNING', paused: false, decision: null, created_at: iso(Date.now()), constraints: {target_asset: input.target_asset, max_runtime_minutes: input.max_runtime_minutes, stop_after_failure: input.stop_after_failure}, events: []};
    this.state.sessions = [session, ...this.state.sessions];
    this.sessionEvent(session, 'MISSION_STARTED', `Mission accepted with a ${input.max_runtime_minutes}-minute limit${input.target_asset ? ` on ${input.target_asset}` : ''}`);
    this.later(1500, () => { session.status = 'RUNNING'; this.sessionEvent(session, 'TOOL_CALL', 'list_assets(status="AVAILABLE")'); });
    this.later(3500, () => this.sessionEvent(session, 'OBSERVATION', `${this.state.assets.filter(a => a.status === 'AVAILABLE').length} robots available`));
    this.later(6000, () => {
      session.status = 'COMPLETED';
      session.report = {summary: 'Inspected fleet state and recent runs. No action needed beyond what is already queued.', likely_cause: 'n/a', evidence: ['No new failures in the last 30 minutes'], recommended_next_action: 'Approve the pending rover-demo-2.4.2 deployment to close out the localization regression.', confidence: 0.7};
      this.sessionEvent(session, 'COMPLETED', 'Report ready');
    });
    this.activity('live', 'Torch Agent mission started', `/agent/${session.id}`);
    this.emit(); return session.id;
  }
  async decide(sessionId: string, approved: boolean) {
    const session = this.state.sessions.find(s => s.id === sessionId);
    if (!session?.pending) return;
    session.decision = approved;
    if (!approved) {
      this.sessionEvent(session, 'APPROVAL_DENIED', 'Deployment denied by operator. No software was transferred.');
      session.status = 'COMPLETED';
      session.report = {summary: 'Mission stopped at the approval gate.', likely_cause: session.pending.reason, evidence: [], recommended_next_action: 'Deploy the fix manually or start a new mission.', confidence: 0.86};
      session.pending = null;
      this.sessionEvent(session, 'COMPLETED', 'Report ready');
      this.emit(); return;
    }
    const bundle = this.state.bundles.find(b => b.name === session.pending!.bundle.name);
    session.status = 'RUNNING';
    this.sessionEvent(session, 'APPROVAL_GRANTED', `Operator approved ${session.pending.bundle.name} → ${session.pending.asset}`);
    session.pending = null;
    this.activity('ok', 'Deployment approved for Torch Agent', `/agent/${session.id}`);
    const run = this.startRun('localization-regression', 'Rover-03', bundle, 'torch-agent', {trigger: 'agent'});
    this.sessionEvent(session, 'RUN_STARTED', `localization-regression on Rover-03 (run ${run.id.slice(0, 8)})`);
    this.onFinish.set(run.id, status => {
      this.sessionEvent(session, status === 'PASSED' ? 'RUN_SUCCEEDED' : 'RUN_FAILED', `Run ${run.id.slice(0, 8)} ${status.toLowerCase()}`);
      session.status = 'COMPLETED';
      session.report = {summary: 'rover-demo-2.4.2 fixes the localization regression on Rover-03.', likely_cause: '2.4.1 moved the IMU calibration file without a compatibility path; the localization node fell back to identity bias.', evidence: ['Run with 2.4.2 converged; drift stayed under 0.1 m', 'Same robot, same workflow, only the bundle changed'], recommended_next_action: 'Promote rover-demo-2.4.2 to the rest of the Jackal fleet and add a calibration-path check to the deploy phase.', confidence: 0.91};
      this.sessionEvent(session, 'COMPLETED', 'Report ready');
      this.activity('ok', 'Torch Agent verified the localization fix on Rover-03', `/agent/${session.id}`);
    });
    this.emit();
  }
  async controlSession(id: string, action: 'pause' | 'resume' | 'cancel') {
    const session = this.state.sessions.find(s => s.id === id);
    if (!session) return;
    if (action === 'cancel') { session.status = 'CANCELLED'; session.pending = null; this.sessionEvent(session, 'CANCELLED', 'Mission cancelled; reservations released'); }
    else { session.paused = action === 'pause'; this.sessionEvent(session, action === 'pause' ? 'PAUSED' : 'RESUMED', action === 'pause' ? 'Agent decisions paused by operator' : 'Agent resumed'); }
    this.emit();
  }
  async instruct(id: string, message: string) {
    const session = this.state.sessions.find(s => s.id === id);
    if (!session) return;
    this.sessionEvent(session, 'OPERATOR_INSTRUCTION', message);
    this.emit();
  }
  async cancelIncident(id: string) {
    this.state.incidents = this.state.incidents.map(i => i.id === id ? {...i, status: 'CANCELLED'} : i);
    this.emit();
  }
}

const isDone = (s: string) => ['PASSED', 'FAILED', 'CANCELLED'].includes(s);
