// Attach a file: it goes onto this node's IPFS node and the post links to it as
// ipfs://CID, so every node that carries the post can serve it.
import { useState } from 'preact/hooks';
import { call } from '../lib/api.js';
import { flash } from '../lib/state.js';

const MAX = 32 << 20;

export function Attach({ text, setText }) {
  const [busy, setBusy] = useState(false);
  const pick = async (ev) => {
    const f = ev.currentTarget.files?.[0];
    ev.currentTarget.value = '';
    if (!f) return;
    if (f.size > MAX) return flash('Files can be up to 32 MB.');
    setBusy(true);
    try {
      const r = await call('ipfs_add', { data: new Uint8Array(await f.arrayBuffer()), name: f.name });
      setText((text && !text.endsWith('\n') ? text + '\n' : text || '') + r.link + '\n');
    } catch (e) {
      flash(e.message);
    }
    setBusy(false);
  };
  return (
    <label class={'attach' + (busy ? ' busy' : '')} title="Attach a file (stored on IPFS)">
      <input type="file" onChange={pick} disabled={busy} />
      {busy ? 'Adding…' : 'Attach'}
    </label>
  );
}
