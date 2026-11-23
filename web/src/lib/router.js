// ~30 lines of history-API routing on top of a signal.
import { signal } from '@preact/signals';

export const loc = signal(location.pathname + location.search);

export function navigate(to, replace = false) {
  if (to === loc.value) return;
  history[replace ? 'replaceState' : 'pushState'](null, '', to);
  loc.value = to;
  window.scrollTo(0, 0);
}

addEventListener('popstate', () => (loc.value = location.pathname + location.search));

// Static pages outside the app (the docs) load normally.
const NATIVE = /^\/docs(\/|$|#)/;

// Intercept same-origin <a href="/..."> clicks.
addEventListener('click', (e) => {
  const a = e.target.closest?.('a[href^="/"]');
  if (!a || a.target || e.defaultPrevented || e.button || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
  if (NATIVE.test(a.getAttribute('href'))) return;
  e.preventDefault();
  navigate(a.getAttribute('href'));
});

/** match('/c/:slug', '/c/math') -> { slug: 'math' } | null */
export function match(pattern, path) {
  const p = pattern.split('/'), s = path.split('?')[0].split('/');
  if (p.length !== s.length) return null;
  const out = {};
  for (let i = 0; i < p.length; i++) {
    if (p[i].startsWith(':')) out[p[i].slice(1)] = decodeURIComponent(s[i]);
    else if (p[i] !== s[i]) return null;
  }
  return out;
}

export const query = (k) => new URLSearchParams(loc.value.split('?')[1] || '').get(k) || '';
