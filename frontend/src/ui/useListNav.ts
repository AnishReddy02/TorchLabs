import {useEffect, useState} from 'react';

/** j/k to move, Enter to open — the list keyboard model shared by every list view. */
export function useListNav(count: number, open: (index: number) => void) {
  const [index, setIndex] = useState(-1);
  useEffect(() => {
    const on = (e: KeyboardEvent) => {
      const el = e.target as HTMLElement | null;
      if (el && (['INPUT', 'TEXTAREA', 'SELECT'].includes(el.tagName) || el.isContentEditable)) return;
      if (e.metaKey || e.ctrlKey || e.altKey || document.querySelector('.overlay')) return;
      if (e.key === 'j' || e.key === 'ArrowDown') { e.preventDefault(); setIndex(i => Math.min(count - 1, i + 1)); }
      else if (e.key === 'k' || e.key === 'ArrowUp') { e.preventDefault(); setIndex(i => Math.max(0, i - 1)); }
      else if (e.key === 'Enter' && index >= 0 && index < count) { e.preventDefault(); open(index); }
    };
    window.addEventListener('keydown', on);
    return () => window.removeEventListener('keydown', on);
  }, [count, index, open]);
  useEffect(() => { document.querySelector('[data-nav-active="true"]')?.scrollIntoView({block: 'nearest'}); }, [index]);
  return [index, setIndex] as const;
}
