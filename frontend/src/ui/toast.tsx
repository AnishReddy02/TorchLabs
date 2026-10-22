import {createContext, useCallback, useContext, useState, type ReactNode} from 'react';
import {Icon} from './icons';
import type {Tone} from '../lib/types';

type Toast = {id: number; tone: Tone; title: string; body?: string; action?: {label: string; run: () => void}};
const ToastContext = createContext<(t: Omit<Toast, 'id'>) => void>(() => {});
let next = 1;

export function ToastProvider({children}: {children: ReactNode}) {
  const [toasts, setToasts] = useState<Toast[]>([]);
  const dismiss = (id: number) => setToasts(ts => ts.filter(t => t.id !== id));
  const push = useCallback((t: Omit<Toast, 'id'>) => {
    const id = next++;
    setToasts(ts => [...ts.slice(-3), {...t, id}]);
    setTimeout(() => dismiss(id), t.tone === 'fail' ? 8000 : 5000);
  }, []);
  return <ToastContext.Provider value={push}>
    {children}
    <div className="toasts" role="status" aria-live="polite">
      {toasts.map(t => <div key={t.id} className={`toast tone-${t.tone}`}>
        <Icon name={t.tone === 'fail' ? 'alert' : t.tone === 'ok' ? 'check' : 'bolt'} size={16}/>
        <div><b>{t.title}</b>{t.body && <span>{t.body}</span>}</div>
        {t.action && <button className="btn btn-ghost btn-sm" onClick={() => { t.action!.run(); dismiss(t.id); }}>{t.action.label}</button>}
        <button className="icon-btn" aria-label="Dismiss" onClick={() => dismiss(t.id)}><Icon name="x" size={14}/></button>
      </div>)}
    </div>
  </ToastContext.Provider>;
}
export const useToast = () => useContext(ToastContext);
