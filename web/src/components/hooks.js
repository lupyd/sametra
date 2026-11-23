import { useEffect, useState } from 'preact/hooks';
import { tick } from '../lib/state.js';

/** Load data for a view; re-runs when deps change and on background ticks. */
export function useLoad(fn, deps) {
  const [s, set] = useState({ data: null, error: null });
  const t = tick.value;
  const key = JSON.stringify(deps);
  useEffect(() => {
    let live = true;
    fn().then((data) => live && set({ data, error: null }), (error) => live && set((p) => ({ data: p.data, error })));
    return () => (live = false);
  }, [key, t]);
  // drop stale data when navigating to a different resource
  useEffect(() => set({ data: null, error: null }), [key]);
  return s;
}
