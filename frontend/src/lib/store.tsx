import {createContext, useContext, useEffect, useMemo, useState, useSyncExternalStore, type ReactNode} from 'react';
import {emptySnapshot, LiveEngine, probe, type Engine, type Snapshot} from './engine';
import {DemoEngine} from './demo';

type Ctx = {engine: Engine; unreachable: boolean; connect: () => void; switchToDemo: () => void};
const EngineContext = createContext<Ctx | null>(null);

const connecting: Engine = new Proxy({} as Engine, {
  get: (_, key) => key === 'get' ? () => idle : key === 'subscribe' ? () => () => {} : key.toString().startsWith('watch') ? () => () => {} : () => Promise.reject(new Error('Still connecting'))
});
const idle = emptySnapshot('connecting');
const DEMO_KEY = 'torch_demo';
const RETRY_MS = 5000;

/** Demo data is only ever shown on request: ?demo in the URL, or the button on the unreachable screen (this tab only). */
function demoRequested() {
  try { return new URLSearchParams(location.search).has('demo') || sessionStorage.getItem(DEMO_KEY) === '1'; }
  catch { return false; }
}
function setDemo(on: boolean) {
  try { if (on) sessionStorage.setItem(DEMO_KEY, '1'); else sessionStorage.removeItem(DEMO_KEY); } catch { /* storage unavailable */ }
}

/** Connects to the control plane; if it is down, reports that and keeps retrying instead of substituting fake data. */
export function TorchProvider({children}: {children: ReactNode}) {
  const [engine, setEngine] = useState<Engine>(connecting);
  const [unreachable, setUnreachable] = useState(false);
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    let cancelled = false;
    let created: (LiveEngine | DemoEngine) | undefined;
    let retry: number | undefined;
    const start = (next: LiveEngine | DemoEngine) => { created = next; setUnreachable(false); setEngine(next); };
    const tryLive = async () => {
      const ok = await probe();
      if (cancelled) return;
      if (ok) start(new LiveEngine());
      else { setUnreachable(true); retry = window.setTimeout(() => void tryLive(), RETRY_MS); }
    };
    setEngine(connecting);
    if (demoRequested()) start(new DemoEngine());
    else void tryLive();
    return () => { cancelled = true; clearTimeout(retry); created?.dispose(); };
  }, [attempt]);
  const value = useMemo<Ctx>(() => ({
    engine, unreachable,
    connect: () => { setDemo(false); setAttempt(n => n + 1); },
    switchToDemo: () => { setDemo(true); setAttempt(n => n + 1); },
  }), [engine, unreachable]);
  return <EngineContext.Provider value={value}>{children}</EngineContext.Provider>;
}

export function useEngineCtx() {
  const ctx = useContext(EngineContext);
  if (!ctx) throw new Error('TorchProvider missing');
  return ctx;
}
export const useEngine = () => useEngineCtx().engine;
export function useTorch(): Snapshot {
  const engine = useEngine();
  const [subscribe, get] = useMemo(() => [engine.subscribe.bind(engine), engine.get.bind(engine)], [engine]);
  return useSyncExternalStore(subscribe, get);
}
export function useWatch(kind: 'run' | 'incident' | 'session' | 'telemetry', id: string | undefined) {
  const engine = useEngine();
  useEffect(() => {
    if (!id) return;
    const fn = kind === 'run' ? engine.watchRun : kind === 'incident' ? engine.watchIncident : kind === 'session' ? engine.watchSession : engine.watchTelemetry;
    return fn.call(engine, id);
  }, [engine, kind, id]);
}

/** A clock for relative times on screens that would otherwise only re-render on data changes. */
export function useNow(every = 1000) {
  const [now, setNow] = useState(Date.now());
  useEffect(() => { const t = setInterval(() => setNow(Date.now()), every); return () => clearInterval(t); }, [every]);
  return now;
}
