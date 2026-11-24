// A chain of reasoning: every step that led to a post and every step built on it,
// in time order, each hanging on its author's strand.
import { call } from '../lib/api.js';
import { short } from '../lib/state.js';
import { kindLabel, strand } from '../lib/strand.js';
import { Meta, Needs } from './Item.jsx';
import { Body } from './Body.jsx';
import { useLoad } from './hooks.js';

const JOINS = { reply: 'replies to', cites: 'builds on' };

export function Chain({ id }) {
  const { data, error } = useLoad(() => call('chain', { id, depth: 6 }), [id]);
  if (error && !data) return <p class="err">{error.message}</p>;
  if (!data) return <p class="muted">Loading…</p>;
  const byId = Object.fromEntries(data.nodes.map((n) => [n.item.id, n.item]));
  const label = (i) => (byId[i] ? byId[i].title || byId[i].content.slice(0, 60) : short(i));
  const focus = byId[data.focus];
  // name the chain after the question or post it grew from
  const named = (focus?.title && focus) || data.nodes.map((n) => n.item).find((i) => i.title);
  return (
    <>
      <div class="bar">
        <a class="grow back" href={'/t/' + data.focus}>← back to the post</a>
      </div>
      <h2>{named ? named.title : 'Chain of reasoning'}</h2>
      <p class="muted small">{data.nodes.length} steps by {data.contributors.length} {data.contributors.length === 1 ? 'contributor' : 'contributors'}, oldest first. Each step hangs on its author's strand.</p>
      <div class="weavers" aria-label="Contributors">
        {data.contributors.map((c) => (
          <span key={c.pubkey} style={{ '--strand': strand(c) }}>
            <i /><a href={'/u/' + c.pubkey}>{c.name}</a> <span class="muted">{kindLabel(c)}, {c.steps} {c.steps === 1 ? 'step' : 'steps'}</span>
          </span>
        ))}
      </div>
      <ol class="chain">
        {data.nodes.map((n) => {
          const e = n.item;
          return (
            <li key={e.id} class={n.level === 0 ? 'focus' : ''} style={{ '--strand': strand(e) }} id={'s' + e.id}>
              {n.builds_on.length > 0 && (
                <div class="joins">
                  {JOINS[n.relation] || 'follows'}{' '}
                  {n.builds_on.map((b, k) => <span key={b}>{k > 0 && ', '}<a href={'#s' + b}>{label(b)}</a></span>)}
                </div>
              )}
              <Meta e={e} extra={<><span>in {e.community}</span><a href={'/t/' + e.id}>open</a></>} />
              {e.title && <a class="title" href={'/t/' + e.id}>{e.title}</a>}
              <Body e={e} />
              <Needs e={e} />
            </li>
          );
        })}
      </ol>
      {data.nodes.length === 1 && <p class="empty">Nothing builds on this yet, and it builds on nothing. Reply to it, or use “build on” to cite it in a new post.</p>}
      <p class="muted small">Follows replies and citations up to six steps each way.</p>
    </>
  );
}
