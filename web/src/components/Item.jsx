import { useState } from 'preact/hooks';
import { call } from '../lib/api.js';
import { ago, cites, flash, fmtScore, short, tick } from '../lib/state.js';

export function Badges({ e }) {
  return (
    <>
      <span class={'tag' + (e.author_rep < 0 ? ' bad' : '')} title="author reputation in this community">rep {e.author_rep}</span>
      {e.ratings > 0 && <span class={'tag' + (e.score < 0 ? ' bad' : '')} title={`weighted trust score from ${e.ratings} ratings`}>score {e.score}</span>}
      {e.relevance_ratings > 0 && <span class={'tag' + (e.relevance < 0 ? ' bad' : '')} title={`weighted relevance from ${e.relevance_ratings} ratings`}>relevance {e.relevance}</span>}
      {e.cited_by > 0 && <span class="tag" title="posts citing this">cited {e.cited_by}</span>}
      {e.author_is_agent && <span class="tag">agent</span>}
      {e.low_trust && <span class="tag bad">low trust</span>}
      {e.pinned && <span class="tag">pinned</span>}
    </>
  );
}

export function Meta({ e, extra }) {
  return (
    <div class="meta">
      <a class="who" href={'/u/' + e.author}>{e.author_name}</a>
      <Badges e={e} />
      <span title={new Date(e.created_at * 1000).toLocaleString()}>{ago(e.created_at)}</span>
      {extra}
    </div>
  );
}

export function Needs({ e }) {
  if (!e.needs?.length) return null;
  return (
    <div class="meta">
      needs {e.needs.map((n) => <a key={n} class="tag need" href={'/help?q=' + encodeURIComponent(n)}>{n}</a>)}
    </div>
  );
}

export function Refs({ e }) {
  if (!e.refs.length) return null;
  return (
    <div class="meta">
      cites {e.refs.map((r) => <a key={r} href={'/t/' + r}><code>{short(r)}</code></a>)}
    </div>
  );
}

const done = (msg) => { flash(msg); tick.value++; };

export function Actions({ e }) {
  const [box, setBox] = useState(null); // 'reply' | 'rate' | null
  const [text, setText] = useState('');
  const [score, setScore] = useState(0);
  const [aspect, setAspect] = useState('trust'); // 'trust' (accurate?) | 'relevance' (useful here?)
  const [busy, setBusy] = useState(false);
  const run = async (f) => {
    setBusy(true);
    try { await f(); setBox(null); setText(''); } catch (err) { flash(err.message); } finally { setBusy(false); }
  };
  const toggle = (b) => setBox(box === b ? null : b);
  return (
    <>
      <div class="acts">
        <button class="link" onClick={() => toggle('reply')}>reply</button>
        <button class="link" onClick={() => toggle('rate')}>rate</button>
        <button class="link" onClick={() => { if (!cites.value.includes(e.id)) cites.value = [...cites.value, e.id]; flash('Will cite ' + short(e.id) + ' in your next post'); }}>cite</button>
        <button class="link" onClick={() => run(async () => { const a = await call(e.pinned ? 'unpin' : 'pin', { id: e.id, thread: true }); done((e.pinned ? 'Unpinned ' : 'Pinned ') + a.count); })}>{e.pinned ? 'unpin' : 'pin'}</button>
        {e.mine && <button class="link" onClick={() => confirm('Retract this network-wide?') && run(async () => { await call('delete', { id: e.id }); done('Retracted'); })}>delete</button>}
      </div>
      {box === 'reply' && (
        <div class="box">
          <textarea autofocus value={text} onInput={(ev) => setText(ev.currentTarget.value)} placeholder="Reply…" />
          <div class="row">
            <CiteList />
            <button disabled={busy || !text.trim()} onClick={() => run(async () => { await call('post', { parent: e.id, content: text, refs: cites.value }); cites.value = []; done('Replied'); })}>Reply</button>
            <button class="ghost" onClick={() => setBox(null)}>Cancel</button>
          </div>
        </div>
      )}
      {box === 'rate' && (
        <div class="box">
          <div class="seg" role="group" aria-label="what you rate">
            <button class={aspect === 'trust' ? 'on' : 'ghost'} onClick={() => setAspect('trust')} title="Is it accurate? Moves the author's reputation.">accuracy</button>
            <button class={aspect === 'relevance' ? 'on' : 'ghost'} onClick={() => setAspect('relevance')} title="Is it useful and on-topic here? Scores only this message.">relevance</button>
          </div>
          <div class="row">
            <input type="range" min="-1" max="1" step="0.1" value={score} onInput={(ev) => setScore(+ev.currentTarget.value)} style="max-width:160px" />
            <b class={score < 0 ? 'neg' : ''} style="min-width:2.6em;text-align:right">{fmtScore(score)}</b>
            <input value={text} onInput={(ev) => setText(ev.currentTarget.value)} placeholder={aspect === 'trust' ? 'reason: misleading, well-sourced, replicated…' : 'reason: off-topic, exactly what was asked…'} />
            <button disabled={busy} onClick={() => run(async () => {
              const a = await call('rate', { event: e.id, score, reason: text, aspect });
              done(aspect === 'trust' ? `Rated. Author's rep now ${a.rep}` : `Rated relevance. This message now ${a.rep}`);
            })}>Rate</button>
          </div>
        </div>
      )}
    </>
  );
}

export function CiteList() {
  if (!cites.value.length) return <span class="grow" />;
  return (
    <span class="grow muted small">
      citing {cites.value.map((c) => <code key={c}>{short(c)}</code>)}{' '}
      <button class="link" onClick={() => (cites.value = [])}>clear</button>
    </span>
  );
}

export function Card({ e, children }) {
  return (
    <article class={'card' + (e.low_trust ? ' low' : '')}>
      {children}
    </article>
  );
}
