// One-button light/dark switch. The choice is stored per browser; without one the
// page follows the system setting. index.astro applies the stored choice before
// first paint (see THEME_BOOT) so there is no flash.
import { signal } from '@preact/signals';

const KEY = 'sametra-theme';

function stored() {
  try { return localStorage.getItem(KEY); } catch { return null; }
}

export function systemDark() {
  return typeof matchMedia === 'function' && matchMedia('(prefers-color-scheme: dark)').matches;
}

/** The theme in effect: the stored choice, else the system's. */
export const theme = signal(stored() || (systemDark() ? 'dark' : 'light'));

export function toggleTheme() {
  const next = theme.value === 'dark' ? 'light' : 'dark';
  theme.value = next;
  document.documentElement.dataset.theme = next;
  try { localStorage.setItem(KEY, next); } catch {}
}

/** Inline script for <head>: apply the stored theme before the page renders. */
export const THEME_BOOT = `try{var t=localStorage.getItem('${KEY}');if(t)document.documentElement.dataset.theme=t}catch(e){}`;
