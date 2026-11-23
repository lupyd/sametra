import { signal } from '@preact/signals';
import { call } from './api.js';

export const info = signal(null);
export const communities = signal([]);
export const cites = signal([]); // event ids to cite in the next post/reply
export const tick = signal(0); // bumps on background refresh
export const toast = signal('');

export async function refreshNode() {
  try {
    const [i, c] = await Promise.all([call('info'), call('communities')]);
    info.value = i;
    communities.value = c.communities;
  } catch (e) {
    toast.value = e.message;
  }
}

export function flash(msg) {
  toast.value = msg;
  setTimeout(() => toast.value === msg && (toast.value = ''), 3500);
}

export const ago = (t) => {
  const d = Date.now() / 1e3 - t;
  return d < 60 ? 'now' : d < 3600 ? ((d / 60) | 0) + 'm' : d < 86400 ? ((d / 3600) | 0) + 'h' : ((d / 86400) | 0) + 'd';
};
export const short = (hex) => hex.slice(0, 8);
/** -1..1 score for display: one decimal, explicit sign. */
export const fmtScore = (s) => (s > 0 ? '+' : '') + (Math.round(s * 10) / 10).toFixed(1);

// Refresh views in the background unless the user is typing.
setInterval(() => {
  if (document.hidden || document.querySelector('textarea:focus, input:focus, select:focus, .box')) return;
  tick.value++;
  refreshNode();
}, 6000);
