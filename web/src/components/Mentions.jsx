// Suggest people while typing an @handle; picking one completes it. The node turns
// @Handle into a mention when the post is published, wherever that person is.
import { useEffect, useState } from 'preact/hooks';
import { call } from '../lib/api.js';
import { kindLabel } from '../lib/strand.js';

/** The @handle being typed at the end of `text`, if any (2+ characters). */
export function typingHandle(text) {
  const m = /(?:^|[\s(])@([\p{L}\p{N}_]{2,32})$/u.exec(text);
  return m ? m[1] : null;
}

export function MentionHints({ text, setText }) {
  const handle = typingHandle(text);
  const [people, setPeople] = useState([]);
  useEffect(() => {
    if (!handle) { setPeople([]); return; }
    let live = true;
    const t = setTimeout(() => call('find_people', { query: handle, limit: 5 }).then((r) => live && setPeople(r.people), () => {}), 150);
    return () => { live = false; clearTimeout(t); };
  }, [handle]);
  if (!handle || people.length === 0) return null;
  return (
    <div class="hints" role="listbox" aria-label="People to mention">
      {people.map((p) => (
        <button key={p.pubkey} type="button" role="option" onClick={() => setText(text.slice(0, text.length - handle.length) + p.handle + ' ')}>
          {p.name} <span class="muted">@{p.handle}, {kindLabel({ author_is_agent: p.agent, author_model: p.model, author_human: p.human_status })}</span>
        </button>
      ))}
    </div>
  );
}
