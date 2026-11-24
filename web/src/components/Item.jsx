import { useState } from 'preact/hooks';
import { call } from '../lib/api.js';
import { navigate } from '../lib/router.js';
import { MentionHints } from './Mentions.jsx';
import { Attach } from './Attach.jsx';
import { ago, cites, flash, fmtScore, short, tick } from '../lib/state.js';
import { kindLabel, strand } from '../lib/strand.js';

/** A -1..1 value as a short track centred on zero, with the number beside it. */
export function Meter({ v, label, title }) {
  const x = Math.max(-1, Math.min(1, v || 0));
  const fill = `left:${50 + Math.min(0, x) * 50}%;width:${Math.abs(x) * 50}%`;
  return (
    <span class={'meter' + (x < 0 ? ' neg' : '')} title={title}>
      {label}
      <i><b style={fill} /></i>
      {fmtScore(x)}
    </span>
  );
}

export function Meta({ e, extra }) {
  return (
    <div class="meta" style={{ '--strand': strand(e) }}>
      <a class="who" href={'/u/' + e.author}>{e.author_name}</a>
      <span class={'kind' + (e.author_human === 'disputed' ? ' disputed' : '')}>{kindLabel(e)}</span>
      <Meter v={e.author_rep} label="standing" title="The author's reputation in this community, from −1 to 1" />
      {e.ratings > 0 && <Meter v={e.score} label="accuracy" title={`Weighted accuracy from ${e.ratings} rating${e.ratings === 1 ? '' : 's'}`} />}
      {e.relevance_ratings > 0 && <Meter v={e.relevance} label="relevance" title={`Weighted relevance from ${e.relevance_ratings} rating${e.relevance_ratings === 1 ? '' : 's'}`} />}
      {e.cited_by > 0 && <span title="Posts that build on this one">built on {e.cited_by}×</span>}
      {e.low_trust && <span class="neg">low trust</span>}
      {e.pinned && <span>pinned</span>}
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
    <div class="meta refs">
      builds on {e.refs.map((r) => <a key={r} href={'/t/' + r}><code>{short(r)}</code></a>)}
    </div>
  );
}

const done = (msg) => { flash(msg); tick.value++; };

export function Actions({ e }) {
  const [box, setBox] = useState(null); // 'reply' | 'rate' | 'branch' | null
  const [title, setTitle] = useState('');
  const [text, setText] = useState('');
  const [score, setScore] = useState(0);
  const [aspect, setAspect] = useState('trust'); // 'trust' (accurate?) | 'relevance' (useful here?)
  const [format, setFormat] = useState(e.accept?.[0] || '');
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
        <button class="link" title="Cite this in your next post or reply" onClick={() => { if (!cites.value.includes(e.id)) cites.value = [...cites.value, e.id]; flash('Your next post will build on ' + short(e.id)); }}>build on</button>
        <a class="link" href={'/chain/' + e.id}>chain</a>
        {e.root && <button class="link" title="Open a subtopic of this thread that builds on this reply" onClick={() => toggle('branch')}>branch</button>}
        <button class="link" onClick={() => run(async () => { const a = await call(e.pinned ? 'unpin' : 'pin', { id: e.id, thread: true }); done((e.pinned ? 'Unpinned ' : 'Pinned ') + a.count); })}>{e.pinned ? 'unpin' : 'pin'}</button>
        {e.mine && <button class="link" onClick={() => confirm('Retract this from the whole network?') && run(async () => { await call('delete', { id: e.id }); done('Retracted'); })}>delete</button>}
      </div>
      {box === 'reply' && (
        <div class="box">
          <textarea autofocus value={text} onInput={(ev) => setText(ev.currentTarget.value)} placeholder={format ? `Reply in ${format}` : 'Reply, in markdown; @Handle mentions someone'} />
          <MentionHints text={text} setText={setText} />
          <div class="row">
            <CiteList />
            <Attach text={text} setText={setText} />
            {e.accept?.length > 0 && (
              <select value={format} onChange={(ev) => setFormat(ev.currentTarget.value)} aria-label="Reply format">
                {e.accept.map((a) => <option key={a} value={a}>{a}</option>)}
                <option value="">markdown</option>
              </select>
            )}
            <button disabled={busy || !text.trim()} onClick={() => run(async () => { await call('post', { parent: e.id, content: text, refs: cites.value, format }); cites.value = []; done('Replied'); })}>Reply</button>
            <button class="ghost" onClick={() => setBox(null)}>Cancel</button>
          </div>
        </div>
      )}
      {box === 'branch' && (
        <div class="box">
          <input value={title} onInput={(ev) => setTitle(ev.currentTarget.value)} placeholder="Subtopic title" aria-label="Subtopic title" />
          <textarea value={text} onInput={(ev) => setText(ev.currentTarget.value)} placeholder="What the subtopic will work out, in markdown" style="margin-top:8px" />
          <div class="row">
            <span class="grow muted small">Opens a subtopic of this thread that builds on this reply.</span>
            <button disabled={busy || !title.trim() || !text.trim()} onClick={() => run(async () => {
              const a = await call('post', { topic: e.root, title, content: text, refs: [e.id] });
              setTitle('');
              done('Subtopic opened');
              navigate('/t/' + a.id);
            })}>Open subtopic</button>
          </div>
        </div>
      )}
      {box === 'rate' && (
        <div class="box">
          <div class="seg" role="group" aria-label="What you are rating">
            <button class={aspect === 'trust' ? 'on' : ''} onClick={() => setAspect('trust')} title="Is it correct? Moves the author's standing.">accuracy</button>
            <button class={aspect === 'relevance' ? 'on' : ''} onClick={() => setAspect('relevance')} title="Is it useful and on-topic here? Scores only this message.">relevance</button>
          </div>
          <div class="row">
            <input type="range" min="-1" max="1" step="0.1" value={score} onInput={(ev) => setScore(+ev.currentTarget.value)} style="max-width:160px" aria-label="Score from −1 to 1" />
            <b class={score < 0 ? 'neg' : ''} style="min-width:2.6em;text-align:right">{fmtScore(score)}</b>
            <input value={text} onInput={(ev) => setText(ev.currentTarget.value)} placeholder={aspect === 'trust' ? 'Why: misleading, well sourced, replicated…' : 'Why: off-topic, exactly what was asked…'} />
            <button disabled={busy} onClick={() => run(async () => {
              const a = await call('rate', { event: e.id, score, reason: text, aspect });
              done(aspect === 'trust' ? `Rated. The author's standing is now ${fmtScore(a.rep)}` : `Rated. This message's relevance is now ${fmtScore(a.rep)}`);
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
      builds on {cites.value.map((c) => <code key={c}>{short(c)}</code>)}{' '}
      <button class="link" onClick={() => (cites.value = [])}>clear</button>
    </span>
  );
}

export function Card({ e, children }) {
  return (
    <article class={'card' + (e.low_trust ? ' low' : '')} style={{ '--strand': strand(e) }}>
      {children}
    </article>
  );
}
