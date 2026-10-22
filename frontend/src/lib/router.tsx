import {useEffect, useState, type AnchorHTMLAttributes, type MouseEvent} from 'react';

const EVENT = 'torch:navigate';
const LEGACY: Record<string, string> = {overview: '/', assets: '/fleet', workflows: '/workflows', runs: '/runs', agent: '/agent', incidents: '/incidents'};

/** Old links (README, pitch captures) used ?page=…; translate them to real paths once on load. */
export function upgradeLegacyUrl() {
  const params = new URLSearchParams(location.search);
  const page = params.get('page');
  if (!page) return;
  let path = LEGACY[page] ?? '/';
  if (params.get('run')) path = `/runs/${params.get('run')}`;
  else if (params.get('session')) path = `/agent/${params.get('session')}`;
  else if (params.get('asset')) path = `/fleet/${params.get('asset')}`;
  history.replaceState(null, '', path);
}

export function navigate(to: string, opts: {replace?: boolean} = {}) {
  if (to === location.pathname + location.search) return;
  if (opts.replace) history.replaceState(null, '', to); else history.pushState(null, '', to);
  window.dispatchEvent(new Event(EVENT));
}

export function useLocation() {
  const read = () => ({path: location.pathname, query: new URLSearchParams(location.search)});
  const [loc, setLoc] = useState(read);
  useEffect(() => {
    const on = () => setLoc(read());
    window.addEventListener('popstate', on);
    window.addEventListener(EVENT, on);
    return () => { window.removeEventListener('popstate', on); window.removeEventListener(EVENT, on); };
  }, []);
  return loc;
}

/** Filters live in the URL so a view can be shared, refreshed, or navigated back to. */
export function setQuery(patch: Record<string, string | null>) {
  const params = new URLSearchParams(location.search);
  for (const [k, v] of Object.entries(patch)) { if (v === null || v === '') params.delete(k); else params.set(k, v); }
  const qs = params.toString();
  navigate(location.pathname + (qs ? `?${qs}` : ''), {replace: true});
}

/** A real anchor, so cmd/middle-click and "copy link" behave like the web. */
export function Link({href, onClick, ...rest}: AnchorHTMLAttributes<HTMLAnchorElement> & {href: string}) {
  return <a href={href} {...rest} onClick={(e: MouseEvent<HTMLAnchorElement>) => {
    onClick?.(e);
    if (e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
    e.preventDefault();
    navigate(href);
  }}/>;
}
