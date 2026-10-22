import {createContext, useContext, useEffect, useMemo, useState, useSyncExternalStore, type ReactNode} from 'react';
import {emptySnapshot, LiveEngine, probe, type Engine, type Snapshot} from './engine';
import {DemoEngine} from './demo';

type Ctx = {engine: Engine; connect: () => void; switchToDemo: () => void};
const EngineContext = createContext<Ctx | null>(null);

const connecting: Engine = new Proxy({} as Engine, {
  get: (_, key) => key === 'get' ? () => idle : key === 'subscribe' ? () => () => {} : key.toString().startsWith('watch') ? () => () => {} : () => Promise.reject(new Error('Still connecting'))
});
const idle = emptySnapshot('connecting');

function forcedDemo() {
  try { return new URLSearchParams(location.search).has('demo') || localStorage.getItem('torch_mode') === 'demo'; }
  catch { return false; }
}

/** Uses the control plane when it answers; otherwise runs the bundled demo fleet so the UI is never empty. */
export function TorchProvider({children}: {children: ReactNode}) {
  const [engine, setEngine] = useState<Engine>(connecting);
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    let cancelled = false;
    let created: (LiveEngine | DemoEngine) | undefined;
    void (async () => {
      const live = !forcedDemo() && await probe();
      if (cancelled) return;
      created = live ? new LiveEngine() : new DemoEngine();
      setEngine(created);
    })();
    return () => { cancelled = true; created?.dispose(); };
  }, [attempt]);
  const value = useMemo<Ctx>(() => ({
    engine,
    connect: () => { try { localStorage.removeItem('torch_mode'); } catch { /* storage unavailable */ } setAttempt(n => n + 1); },
    switchToDemo: () => { try { localStorage.setItem('torch_mode', 'demo'); } catch { /* storage unavailable */ } setAttempt(n => n + 1); },
  }), [engine]);
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
