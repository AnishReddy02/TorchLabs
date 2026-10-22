const PATHS = {
  home: 'M3 10.5 12 3l9 7.5V20a1 1 0 0 1-1 1h-5v-6h-6v6H4a1 1 0 0 1-1-1z',
  runs: 'M5 4h14M5 12h14M5 20h14M2 4h.01M2 12h.01M2 20h.01',
  fleet: 'M4 7a2 2 0 0 1 2-2h12a2 2 0 0 1 2 2v7a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2zM8 20h8M12 16v4M9 10h.01M15 10h.01',
  agent: 'M12 3v3M12 18v3M3 12h3M18 12h3M12 8l1.4 2.6L16 12l-2.6 1.4L12 16l-1.4-2.6L8 12l2.6-1.4z',
  siren: 'M7 18v-6a5 5 0 0 1 10 0v6M5 21h14v-3H5zM12 3v1M4.2 6.2l.8.8M19.8 6.2l-.8.8',
  workflow: 'M14 3v4a1 1 0 0 0 1 1h4M17 21H7a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h7l5 5v11a2 2 0 0 1-2 2zM10 13l-2 2 2 2M14 13l2 2-2 2',
  package: 'M21 8 12 3 3 8v8l9 5 9-5zM3 8l9 5 9-5M12 13v8',
  search: 'M11 18a7 7 0 1 0 0-14 7 7 0 0 0 0 14zM21 21l-5-5',
  plus: 'M12 5v14M5 12h14',
  rerun: 'M3 12a9 9 0 0 1 15.5-6.2L21 8M21 3v5h-5M21 12a9 9 0 0 1-15.5 6.2L3 16M3 21v-5h5',
  x: 'M18 6 6 18M6 6l12 12',
  check: 'M20 6 9 17l-5-5',
  chevron: 'M9 18l6-6-6-6',
  down: 'M6 9l6 6 6-6',
  sun: 'M12 16a4 4 0 1 0 0-8 4 4 0 0 0 0 8zM12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4',
  moon: 'M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z',
  clock: 'M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18zM12 7v5l3 2',
  commit: 'M12 16a4 4 0 1 0 0-8 4 4 0 0 0 0 8zM2 12h6M16 12h6',
  terminal: 'M4 17l6-5-6-5M12 19h8',
  alert: 'M12 9v4M12 17h.01M10.3 3.9 1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z',
  pause: 'M8 5v14M16 5v14',
  play: 'M7 4l13 8-13 8z',
  arrow: 'M7 17 17 7M8 7h9v9',
  download: 'M12 3v12M7 10l5 5 5-5M5 21h14',
  bolt: 'M13 2 4 14h7l-1 8 9-12h-7z',
  user: 'M20 21a8 8 0 0 0-16 0M12 13a5 5 0 1 0 0-10 5 5 0 0 0 0 10z',
  shield: 'M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z',
  lock: 'M6 11h12v10H6zM8 11V7a4 4 0 0 1 8 0v4',
  filter: 'M3 5h18M6 12h12M10 19h4',
  send: 'M22 2 11 13M22 2l-7 20-4-9-9-4z',
  upload: 'M12 21V9M7 14l5-5 5 5M5 3h14',
  keyboard: 'M3 6h18v12H3zM7 10h.01M11 10h.01M15 10h.01M7 14h10',
  diff: 'M12 3v14M5 10h14M5 21h14',
  layers: 'M12 2 2 7l10 5 10-5zM2 17l10 5 10-5M2 12l10 5 10-5',
  copy: 'M9 9h11v11H9zM5 15H4V4h11v1',
} as const;
export type IconName = keyof typeof PATHS;

export function Icon({name, size = 16, className}: {name: IconName; size?: number; className?: string}) {
  return <svg className={className} width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={1.75} strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d={PATHS[name]}/>
  </svg>;
}

export function TorchMark({size = 22}: {size?: number}) {
  return <svg width={size} height={size} viewBox="0 0 32 32" aria-hidden="true">
    <defs>
      <linearGradient id="torch-flame" x1="0" y1="1" x2="0" y2="0">
        <stop offset="0" stopColor="#ff4d1f"/><stop offset="1" stopColor="#ffb547"/>
      </linearGradient>
    </defs>
    <rect width="32" height="32" rx="8" fill="var(--mark-bg)"/>
    <path d="M16 5c2.2 3.4 6.5 6.4 6.5 11.6A6.5 6.5 0 0 1 9.5 16.6c0-2.9 1.5-4.6 3.1-6 .1 2.3 1.2 3.7 2.6 3.9C15 11 14.6 8 16 5z" fill="url(#torch-flame)"/>
    <path d="M13 24.5h6" stroke="var(--mark-fg)" strokeWidth="2" strokeLinecap="round"/>
  </svg>;
}
