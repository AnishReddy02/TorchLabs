import {useState, type ReactNode} from 'react';
import {Icon, type IconName} from './icons';
import {Link, navigate} from '../lib/router';
import {ago, duration, label, phaseStates, runDuration, tone as toneOf} from '../lib/format';
import type {Run, Step, Tone} from '../lib/types';

/** Status glyph: shape and color both carry state, so it reads without color too. */
export function StatusIcon({status, tone, size = 16, title}: {status?: string; tone?: Tone; size?: number; title?: string}) {
  const t = tone ?? toneOf(status);
  const name = title ?? (status ? label(status) : t);
  return <svg className={`status-icon tone-${t}`} width={size} height={size} viewBox="0 0 16 16" role="img" aria-label={name}>
    <title>{name}</title>
    {t === 'ok' && <><circle cx="8" cy="8" r="7" fill="currentColor"/><path d="M4.8 8.2 7 10.3l4.2-4.4" fill="none" stroke="var(--bg)" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round"/></>}
    {t === 'fail' && <><circle cx="8" cy="8" r="7" fill="currentColor"/><path d="M5.6 5.6l4.8 4.8M10.4 5.6l-4.8 4.8" stroke="var(--bg)" strokeWidth="1.8" strokeLinecap="round"/></>}
    {t === 'live' && <><circle cx="8" cy="8" r="6" fill="none" stroke="currentColor" strokeOpacity=".25" strokeWidth="2"/><path className="spin" d="M8 2a6 6 0 0 1 6 6" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round"/></>}
    {t === 'wait' && <circle cx="8" cy="8" r="6" fill="none" stroke="currentColor" strokeWidth="2" strokeDasharray="2.6 2.1"/>}
    {t === 'idle' && <><circle cx="8" cy="8" r="6" fill="none" stroke="currentColor" strokeWidth="1.6"/><path d="M5.5 8h5" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round"/></>}
  </svg>;
}

export function Badge({status, children}: {status: string; children?: ReactNode}) {
  return <span className={`badge tone-${toneOf(status)}`}><StatusIcon status={status} size={12}/>{children ?? label(status)}</span>;
}

export function Kbd({children}: {children: ReactNode}) { return <kbd className="kbd">{children}</kbd>; }

export function Empty({icon, title, children, action}: {icon: IconName; title: string; children?: ReactNode; action?: ReactNode}) {
  return <div className="empty"><span className="empty-icon"><Icon name={icon} size={20}/></span><h3>{title}</h3>{children && <p>{children}</p>}{action}</div>;
}

export function SectionHead({title, meta, children}: {title: string; meta?: ReactNode; children?: ReactNode}) {
  return <div className="section-head"><h2>{title}{meta != null && <span className="count">{meta}</span>}</h2><div className="section-actions">{children}</div></div>;
}

/** Phase bar: the six workflow phases as segments, so the failing phase is visible from a list row. */
export function PhaseBar({steps, compact}: {steps?: Step[]; compact?: boolean}) {
  if (!steps?.length) return null;
  return <span className={`phasebar ${compact ? 'compact' : ''}`} aria-label="Workflow phases">
    {phaseStates(steps).map(p => <span key={p.phase} className={`seg tone-${p.tone}`} title={`${p.phase}: ${p.tone === 'none' ? 'no steps' : p.tone}`}/>)}
  </span>;
}

/** Single-series trend line with a hover readout; the latest point is drawn in the accent. */
export function Sparkline({values, format, labels, height = 36}: {values: number[]; format: (v: number) => string; labels?: string[]; height?: number}) {
  const [hover, setHover] = useState<number>();
  const width = 160;
  const finite = values.filter(Number.isFinite);
  if (finite.length < 2) return <div className="spark-empty" style={{height}}>Not enough data yet</div>;
  const min = Math.min(...finite), max = Math.max(...finite);
  const x = (i: number) => (i / (values.length - 1)) * (width - 4) + 2;
  const y = (v: number) => height - 4 - ((v - min) / (max - min || 1)) * (height - 8);
  const pts = values.map((v, i) => Number.isFinite(v) ? `${x(i)},${y(v)}` : '').filter(Boolean);
  const last = values.length - 1;
  return <div className="spark" onPointerLeave={() => setHover(undefined)} onPointerMove={e => {
    const box = e.currentTarget.getBoundingClientRect();
    const i = Math.round(((e.clientX - box.left) / box.width) * last);
    setHover(Math.max(0, Math.min(last, i)));
  }}>
    <svg viewBox={`0 0 ${width} ${height}`} preserveAspectRatio="none" width="100%" height={height} aria-hidden="true">
      <polyline points={`${x(0)},${height} ${pts.join(' ')} ${x(last)},${height}`} className="spark-area"/>
      <polyline points={pts.join(' ')} className="spark-line"/>
      {hover !== undefined && Number.isFinite(values[hover]) && <line x1={x(hover)} x2={x(hover)} y1={0} y2={height} className="spark-cross"/>}
      {Number.isFinite(values[last]) && <circle cx={x(last)} cy={y(values[last])} r="2.5" className="spark-dot"/>}
    </svg>
    {hover !== undefined && Number.isFinite(values[hover]) && <span className="spark-tip" style={{left: `${(x(hover) / width) * 100}%`}}>{labels?.[hover] && <small>{labels[hover]}</small>}{format(values[hover])}</span>}
  </div>;
}

export function Stat({label: name, value, delta, good, children, hint}: {label: string; value: string; delta?: string; good?: boolean; hint?: string; children?: ReactNode}) {
  return <div className="stat">
    <div className="stat-label" title={hint}>{name}</div>
    <div className="stat-value">{value}{delta && <span className={`delta ${good === undefined ? '' : good ? 'good' : 'bad'}`}>{delta}</span>}</div>
    {children}
  </div>;
}

/**
 * Run history as bars: pass is short, fail is tall, live pulses. Height is a second encoding so
 * a red-green colorblind reader still sees where failures cluster.
 */
export function RunStrip({runs, slots = 24, now = Date.now()}: {runs: Run[]; slots?: number; now?: number}) {
  const recent = runs.slice(0, slots).reverse();
  const pad = Array.from({length: Math.max(0, slots - recent.length)});
  return <span className="runstrip" role="list" aria-label={`Last ${recent.length} runs`}>
    {pad.map((_, i) => <span key={`p${i}`} className="cell pad" aria-hidden="true"/>)}
    {recent.map(run => {
      const t = toneOf(run.status);
      return <a key={run.id} role="listitem" href={`/runs/${run.id}`} className={`cell tone-${t}`}
        onClick={e => { if (!e.metaKey && !e.ctrlKey) { e.preventDefault(); navigate(`/runs/${run.id}`); } }}
        data-tip={`${run.workflow_name} · ${label(run.status)} · ${duration(runDuration(run, now))} · ${ago(run.created_at, now)}`}
        aria-label={`${run.workflow_name} ${label(run.status)} ${ago(run.created_at, now)}`}/>;
    })}
  </span>;
}

export function Avatar({name}: {name?: string}) {
  const n = name ?? '?';
  if (n === 'torch-agent') return <span className="avatar agent" title="Torch Agent"><Icon name="agent" size={11}/></span>;
  const hue = [...n].reduce((h, c) => h + c.charCodeAt(0), 0) % 360;
  return <span className="avatar" title={n} style={{background: `oklch(0.55 0.09 ${hue})`}}>{n.charAt(0).toUpperCase()}</span>;
}

export function Crumbs({items}: {items: {label: string; href?: string}[]}) {
  return <nav className="crumbs" aria-label="Breadcrumb">
    {items.map((c, i) => <span key={i}>{i > 0 && <Icon name="chevron" size={12}/>}{c.href ? <Link href={c.href}>{c.label}</Link> : <span aria-current="page">{c.label}</span>}</span>)}
  </nav>;
}

export function Mono({children, copy}: {children: ReactNode; copy?: string}) {
  return <code className="mono" title={copy ? 'Click to copy' : undefined} onClick={copy ? () => void navigator.clipboard?.writeText(copy) : undefined}>{children}</code>;
}
