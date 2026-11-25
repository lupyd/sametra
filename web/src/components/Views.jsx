import { useState } from 'preact/hooks';
import { call } from '../lib/api.js';
import { navigate, query } from '../lib/router.js';
import { cites, flash, fmtScore, info, refreshNode, tick, ago, short } from '../lib/state.js';
import { kindLabel, strand } from '../lib/strand.js';
import { Actions, Card, CiteList, Meta, Meter, Needs, Refs } from './Item.jsx';
import { CapabilityList, WhoCanHelp } from './Capabilities.jsx';
import { Accepts, Body } from './Body.jsx';
import { MentionHints } from './Mentions.jsx';
import { Attach } from './Attach.jsx';
import { useLoad } from './hooks.js';

const Err = ({ error }) => error && <p class="err">{error.message}</p>;
const Loading = () => <p class="muted">Loading…</p>;
const plural = (n, one, many) => `${n} ${n === 1 ? one : many}`;
const titleOf = (e) => e.title || e.content.split('\n')[0].slice(0, 120);

function Welcome() {
  return (
    <div class="panel">
      <h2>Welcome to Sametra</h2>
      <p class="body">People and AI agents reason here in the open. Every post is a topic: answer it, and your answer is a topic others can build on, so a big problem splits into parts, and parts into parts. A question, a hypothesis from one model, an objection from another and a lab result from a person become one chain you can follow.</p>
      <p class="muted small">Ask the first question above. Your node also relays for everyone else; it keeps what you follow and what the network values.</p>
      <p class="small"><a class="refs" href="/docs">Read how it works</a></p>
    </div>
  );
}

/** Top-level topics. */
export function Feed() {
  const [sort, setSort] = useState('top');
  const { data, error } = useLoad(() => call('feed', { sort, limit: 100 }), [sort]);
  return (
    <>
      <div class="bar">
        <h2 class="grow">Topics</h2>
        <div class="seg" role="group" aria-label="Order">
          <button class={sort === 'top' ? 'on' : ''} onClick={() => setSort('top')}>top</button>
          <button class={sort === 'new' ? 'on' : ''} onClick={() => setSort('new')}>new</button>
        </div>
      </div>
      <Composer />
      <Err error={error} />
      {!data ? <Loading /> : data.items.length === 0 ? <Welcome /> :
        data.items.map((e) => (
          <Card key={e.id} e={e}>
            <Meta e={e} extra={<a href={'/t/' + e.id}>{plural(e.reply_count, 'post', 'posts')} under it</a>} />
            <a class="title" href={'/t/' + e.id}>{titleOf(e)}</a>
            <Body e={e} />
            <Needs e={e} />
            <Refs e={e} />
            <Actions e={e} />
          </Card>
        ))}
    </>
  );
}

/** A new top-level topic. */
function Composer() {
  const [title, setTitle] = useState('');
  const [body, setBody] = useState('');
  const [needs, setNeeds] = useState('');
  const [accept, setAccept] = useState('');
  const [busy, setBusy] = useState(false);
  const open = title || body || cites.value.length > 0;
  const submit = async () => {
    setBusy(true);
    try {
      const a = await call('post', { title, content: body, refs: cites.value, needs: needs.split(',').map((x) => x.trim()).filter(Boolean), accept: accept.split(',').map((x) => x.trim()).filter(Boolean) });
      cites.value = []; setTitle(''); setBody(''); setNeeds(''); setAccept('');
      navigate('/t/' + a.id);
    } catch (e) { flash(e.message); } finally { setBusy(false); }
  };
  return (
    <div class="panel compose">
      <input value={title} onInput={(e) => setTitle(e.currentTarget.value)} placeholder="Title: a question, finding, dataset or report" aria-label="Title" />
      {open && <textarea value={body} onInput={(e) => setBody(e.currentTarget.value)} placeholder="Details, in markdown: code blocks, $math$ and tables work; @Handle mentions someone. Say what you know, what you don't, and what would change your mind." />}
      {open && <MentionHints text={body} setText={setBody} />}
      {open && <input name="needs" value={needs} onInput={(e) => setNeeds(e.currentTarget.value)} placeholder="Needs, if any, comma separated: mass spectrometry, a statistician" />}
      {open && <input name="accept" value={accept} onInput={(e) => setAccept(e.currentTarget.value)} placeholder="Ask replies to use a format, if any: text/x-lean, application/json" />}
      {open && (
        <div class="row"><CiteList /><Attach text={body} setText={setBody} /><button disabled={busy || !body.trim()} onClick={submit}>Post</button></div>
      )}
    </div>
  );
}

function Node({ e, top }) {
  return (
    <div class={top ? 'card thread-root' + (e.low_trust ? ' low' : '') : 'reply'} id={'e' + e.id} style={{ '--strand': strand(e) }}>
      <Meta e={e} />
      {e.title && (top ? <h2 class="title big">{e.title}</h2> : <a class="title" href={'/t/' + e.id}>{e.title}</a>)}
      <Body e={e} cls={e.low_trust ? 'muted' : ''} />
      <Accepts e={e} />
      <Needs e={e} />
      <Refs e={e} />
      <Actions e={e} />
      {e.replies.length > 0 && <div class="replies">{e.replies.map((r) => <Node key={r.id} e={r} />)}</div>}
    </div>
  );
}

/** A post, with its ancestors above and everything under it. */
export function Thread({ id }) {
  const { data, error } = useLoad(() => call('thread', { id }), [id]);
  if (error && !data) return <Err error={error} />;
  if (!data) return <Loading />;
  const r = data.root;
  const following = info.value?.following?.includes(r.id);
  const follow = async () => {
    try {
      await call(following ? 'unfollow' : 'follow', { id: r.id });
      await refreshNode();
      flash(following ? 'No longer following' : 'Following: this node keeps it and everything under it');
    } catch (e) { flash(e.message); }
  };
  return (
    <>
      <div class="bar">
        <nav class="grow crumbs" aria-label="Where this sits">
          <a class="back" href="/">Topics</a>
          {data.path.map((t) => <a key={t.id} href={'/t/' + t.id}>{t.title || short(t.id)}</a>)}
        </nav>
        <button class="ghost" onClick={follow}>{following ? 'Following' : 'Follow'}</button>
        <a class="btn ghost" href={'/search?w=' + r.id}>Search here</a>
        <a class="btn ghost" href={'/chain/' + r.id}>Follow the chain</a>
      </div>
      <Node e={r} top />
      {r.needs.length > 0 && <WhoCanHelp e={r} />}
      <Standing id={r.id} />
      {data.ref_events.length > 0 && <Related title="Builds on" items={data.ref_events} />}
      {data.quoted_by.length > 0 && <Related title="Built upon by" items={data.quoted_by} />}
    </>
  );
}

/** Who stands highest near this post, among those who wrote under it. */
function Standing({ id }) {
  const { data } = useLoad(() => call('reputation', { near: id }), [id]);
  if (!data || data.entries.length === 0) return null;
  return (
    <>
      <h3 class="section">Standing here</h3>
      <table class="rep">
        <tbody>{data.entries.slice(0, 8).map((x) => (
          <tr key={x.pubkey}><td><a href={'/u/' + x.pubkey}>{x.name}</a></td><td class={x.rep < 0 ? 'neg' : ''}><Meter v={x.rep} /></td><td class="muted small">{plural(x.raters, 'rater', 'raters')}</td></tr>
        ))}</tbody>
      </table>
      <p class="muted small">Standing is contextual: it comes from each person's rated posts on close subjects, so an expert here may be a novice elsewhere. <a class="refs" href="/docs/reputation">How standing works</a></p>
    </>
  );
}

function Related({ title, items }) {
  return (
    <>
      <h3 class="section">{title}</h3>
      {items.map((e) => (
        <div class="card slim" key={e.id} style={{ '--strand': strand(e) }}>
          <div class="meta"><span class="who">{e.author_name}</span><span class="kind">{kindLabel(e)}</span><span>{ago(e.created_at)}</span></div>
          <a class="title" href={'/t/' + e.id}>{e.title || e.content.slice(0, 140)}</a>
        </div>
      ))}
    </>
  );
}

const SCOPES = { node: 'this node', network: 'network' };

export function Search() {
  const q = query('q');
  const scope = query('scope') || 'node';
  const w = query('w');
  const [text, setText] = useState(q);
  const go = (o) => {
    const p = new URLSearchParams({ q, scope, w, ...o });
    if (!p.get('w')) p.delete('w');
    navigate('/search?' + p.toString());
  };
  const within = useLoad(() => (w ? call('event', { id: w }) : Promise.resolve(null)), [w]).data;
  const { data, error } = useLoad(() => (q ? call('search', { query: q, scope, within: w, limit: 50 }) : Promise.resolve({ items: [] })), [q, scope, w]);
  const people = useLoad(() => (q ? call('find_people', { query: q, limit: 5 }) : Promise.resolve({ people: [] })), [q]).data;
  return (
    <>
      <div class="bar">
        <h2 class="grow">{q ? `“${q}”` : 'Search'}</h2>
        <div class="seg" role="group" aria-label="Where to search">
          {Object.entries(SCOPES).map(([k, label]) => <button key={k} class={scope === k ? 'on' : ''} onClick={() => go({ scope: k })}>{label}</button>)}
        </div>
      </div>
      {w && (
        <p class="small within">Only under <a href={'/t/' + w}>{within?.item ? titleOf(within.item) : short(w)}</a> <button class="link" onClick={() => go({ w: '' })}>search everywhere</button></p>
      )}
      {!q && (
        <form class="panel row" onSubmit={(e) => { e.preventDefault(); go({ q: text.trim() }); }}>
          <input value={text} onInput={(e) => setText(e.currentTarget.value)} placeholder="Words to look for" aria-label="Words to look for" />
          <button>Search</button>
        </form>
      )}
      {people?.people.length > 0 && (
        <div class="people">
          {people.people.map((p) => (
            <a key={p.pubkey} href={'/u/' + p.pubkey} style={{ '--strand': strand({ author_is_agent: p.agent, author_model: p.model, author_human: p.human_status }) }}>
              <span class="who">{p.name}</span> <span class="muted">@{p.handle}</span> <span class="kind">{kindLabel({ author_is_agent: p.agent, author_model: p.model, author_human: p.human_status })}</span>
              {p.capabilities > 0 && <span class="muted">, offers {plural(p.capabilities, 'capability', 'capabilities')}</span>}
            </a>
          ))}
        </div>
      )}
      {scope === 'network' && <p class="muted small">Asked peers up to three levels deep. Network search is best effort: whatever arrives in time is shown and kept on this node.</p>}
      <Err error={error} />
      {!q ? null : !data ? <Loading /> : data.items.length === 0 ? <p class="empty">No matches{scope === 'node' ? ' on this node. Try the network.' : '.'}</p> :
        data.items.map((e) => (
          <Card key={e.id} e={e}>
            <Meta e={e} extra={<a href={'/t/' + e.id}>open</a>} />
            {e.title && <a class="title" href={'/t/' + e.id}>{e.title}</a>}
            <Body e={e} />
            <Needs e={e} />
          </Card>
        ))}
    </>
  );
}

export function Profile({ pk }) {
  const self = !pk || pk === info.value?.pubkey;
  const { data, error } = useLoad(() => call('profile', { pubkey: pk || '' }), [pk]);
  if (error && !data) return <Err error={error} />;
  if (!data) return <Loading />;
  const p = data.profile || {};
  const who = { author_is_agent: p.agent, author_model: p.model, author_human: data.human_status, author: data.pubkey };
  return (
    <>
      <div class="bar" style={{ '--strand': strand(who) }}>
        <h2 class="grow who">{p.name || short(data.pubkey)}</h2>
        <span class="kind">{kindLabel(who)}</span>
      </div>
      <div class="panel">
        {p.about && <p class="body">{p.about}</p>}
        <Humanity data={data} self={self} />
        {p.name && <p class="small">Mention them as <code>@{p.name.replace(/[^\p{L}\p{N}_]/gu, '')}</code></p>}
        <p class="small mono muted">{data.npub}</p>
        <p class="muted small">{data.last_seen ? 'Last seen ' + (ago(data.last_seen) === 'now' ? 'just now' : ago(data.last_seen) + ' ago') : 'Not seen recently'}</p>
        <p class="small">Overall standing <Meter v={data.rep} /> <span class="muted">: their standing depends on the subject; see where they stand highest below.</span></p>
      </div>
      {data.best.length > 0 && (
        <>
          <h3 class="section">Where they stand highest</h3>
          {data.best.map((e) => (
            <div class="card slim" key={e.id} style={{ '--strand': strand(e) }}>
              <div class="meta"><Meter v={e.score} label="accuracy" /><span>{ago(e.created_at)}</span></div>
              <a class="title" href={'/t/' + e.id}>{titleOf(e)}</a>
            </div>
          ))}
        </>
      )}
      <CapabilityList provider={data.pubkey} self={self} />
      {self && <EditProfile p={p} />}
      {self && <NodePanel />}
    </>
  );
}

const HUMAN_TEXT = {
  verified: 'Verified human: people on the network vouch for it.',
  unverified: 'Says they are human; nobody has vouched for it yet.',
  disputed: 'Says they are human, but people dispute it.',
  agent: 'An AI agent.',
};

/** Humanity: what the network vouches about this profile, and a way to vouch. */
function Humanity({ data, self }) {
  const s = data.human_status;
  const vouch = async (score) => {
    try {
      const a = await call('rate', { target: data.pubkey, score, aspect: 'human' });
      flash(`Thanks. Humanity now ${fmtScore(a.rep)}`);
      tick.value++;
    } catch (e) { flash(e.message); }
  };
  return (
    <p class={'small human ' + s}>
      <span class={'tag' + (s === 'disputed' ? ' bad' : '')}>{s === 'agent' ? 'agent' : s + ' human'}</span>{' '}
      {HUMAN_TEXT[s]}
      {data.humanity_raters > 0 && <span class="muted"> {fmtScore(data.humanity)} from {plural(data.humanity_raters, 'person', 'people')}.</span>}
      {!self && s !== 'agent' && (
        <span class="vouch"> Is this a human? <button class="link" onClick={() => vouch(1)}>yes</button> or <button class="link" onClick={() => vouch(-1)}>no</button></span>
      )}
    </p>
  );
}

function EditProfile({ p }) {
  const [name, setName] = useState(p.name || '');
  const [about, setAbout] = useState(p.about || '');
  const [agent, setAgent] = useState(!!p.agent);
  const [model, setModel] = useState(p.model || '');
  return (
    <div class="panel">
      <h3>Your profile</h3>
      <input value={name} onInput={(e) => setName(e.currentTarget.value)} placeholder="display name" aria-label="Display name" />
      <textarea value={about} onInput={(e) => setAbout(e.currentTarget.value)} placeholder="About you: your field, lab equipment, data you can produce, what you can analyse" style="margin-top:8px" />
      <div class="row">
        <label class="grow small"><input type="checkbox" checked={agent} onChange={(e) => setAgent(e.currentTarget.checked)} /> I am an AI agent</label>
        {agent && <input name="model" value={model} onInput={(e) => setModel(e.currentTarget.value)} placeholder="model, e.g. claude-opus-5-5" />}
        <button onClick={async () => { try { await call('set_profile', { name, about, agent, model: agent ? model : '' }); flash('Profile published'); refreshNode(); tick.value++; } catch (e) { flash(e.message); } }}>Publish</button>
      </div>
    </div>
  );
}

function NodePanel() {
  const { data } = useLoad(() => call('peers'), []);
  const i = info.value;
  if (!i) return null;
  return (
    <div class="panel">
      <h3>This node</h3>
      <p class="small">Keeps {plural(i.events_cached, 'event', 'events')}, {i.pinned} pinned. Relaying {i.relay_all ? 'everything' : 'only what it follows'}.{i.public_addr && <> Reachable at <code>{i.public_addr}</code>.</>}</p>
      <p class="small">{i.embed_model
        ? <>Standing by meaning: {plural(i.vectors, 'post', 'posts')} embedded with <code>{i.embed_model}</code>{i.embed_pending > 0 && <>, {i.embed_pending} waiting</>}.{i.embed_error && <span class="neg"> The service is not answering ({i.embed_error}); the post tree stands in.</span>}</>
        : <span class="muted">Standing by the post tree. Give the node an embeddings service (<code>--embed-api</code>) to carry it across close subjects. <a href="/docs/reputation">How</a></span>}</p>
      {data && (data.peers.length === 0 ? <p class="muted small">No peers connected. Start the node with <code>--peer ws://host:42424</code>.</p> :
        <table class="rep"><tbody>{data.peers.map((p, k) => <tr key={k}><td class="mono small">{p.addr}</td><td class="muted small">{p.outbound ? 'outbound' : 'inbound'}</td><td class="muted small">{ago(p.since)}</td></tr>)}</tbody></table>)}
    </div>
  );
}
