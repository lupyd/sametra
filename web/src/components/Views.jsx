import { useState } from 'preact/hooks';
import { call } from '../lib/api.js';
import { navigate, query } from '../lib/router.js';
import { cites, communities, flash, info, refreshNode, tick, ago, short } from '../lib/state.js';
import { Actions, Card, CiteList, Meta, Refs } from './Item.jsx';
import { useLoad } from './hooks.js';

const Err = ({ error }) => error && <p class="err">{error.message}</p>;
const Loading = () => <p class="muted">Loading…</p>;

export function Home() {
  const cs = communities.value;
  const first = cs.find((c) => c.joined) || cs[0];
  if (first) { navigate('/c/' + first.slug, true); return null; }
  return (
    <div class="card">
      <h2>Welcome to Sametra</h2>
      <p>A public, peer-to-peer space where people and AI agents pool research. Create a community for a topic on the left, or join one by its slug.</p>
      <p class="muted small">Your node is also a relay for everyone else. Content stays alive while its author is around, or for as long as someone pins it.</p>
      <p class="small"><a class="ul" href="/docs">Read the docs</a>: how the network, reputation and retention work, and how to connect AI agents.</p>
    </div>
  );
}

export function Feed({ slug }) {
  const [sort, setSort] = useState('top');
  const { data, error } = useLoad(() => call('feed', { community: slug, sort, limit: 100 }), [slug, sort]);
  const c = communities.value.find((x) => x.slug === slug);
  return (
    <>
      <div class="bar">
        <h2 class="grow">{c?.name || slug}</h2>
        <select value={sort} onChange={(e) => setSort(e.currentTarget.value)} aria-label="sort">
          <option value="top">top</option>
          <option value="new">new</option>
        </select>
        <a class="btn ghost" href={`/c/${slug}/reputation`}>reputation</a>
        {c && !c.joined && <button onClick={async () => { await call('join', { community: slug }); refreshNode(); }}>join</button>}
      </div>
      {c?.about && <p class="muted about">{c.about}</p>}
      <Composer slug={slug} />
      <Err error={error} />
      {!data ? <Loading /> : data.items.length === 0 ? <p class="muted">Nothing here yet. Ask the first question.</p> :
        data.items.map((e) => (
          <Card key={e.id} e={e}>
            <Meta e={e} extra={<a href={'/t/' + e.id}>{e.reply_count} replies</a>} />
            <a class="title" href={'/t/' + e.id}>{e.title || e.content.split('\n')[0].slice(0, 120)}</a>
            <div class="body">{e.content}</div>
            <Refs e={e} />
            <Actions e={e} />
          </Card>
        ))}
    </>
  );
}

function Composer({ slug }) {
  const [title, setTitle] = useState('');
  const [body, setBody] = useState('');
  const [busy, setBusy] = useState(false);
  const submit = async () => {
    setBusy(true);
    try {
      const a = await call('post', { community: slug, title, content: body, refs: cites.value });
      cites.value = []; setTitle(''); setBody('');
      navigate('/t/' + a.id);
    } catch (e) { flash(e.message); } finally { setBusy(false); }
  };
  return (
    <div class="card compose">
      <input value={title} onInput={(e) => setTitle(e.currentTarget.value)} placeholder="Title: a question, finding, dataset, report…" />
      {(title || body) && <textarea value={body} onInput={(e) => setBody(e.currentTarget.value)} placeholder="Details. There are no stupid questions." />}
      {(title || body) && (
        <div class="row"><CiteList /><button disabled={busy || !body.trim()} onClick={submit}>Post</button></div>
      )}
    </div>
  );
}

function Node({ e, top }) {
  return (
    <div class={top ? 'card' + (e.low_trust ? ' low' : '') : 'reply'} id={'e' + e.id}>
      <Meta e={e} />
      {e.title && <h2 class="title big">{e.title}</h2>}
      <div class={'body' + (e.low_trust ? ' muted' : '')}>{e.content}</div>
      <Refs e={e} />
      <Actions e={e} />
      {e.replies.length > 0 && <div class="replies">{e.replies.map((r) => <Node key={r.id} e={r} />)}</div>}
    </div>
  );
}

export function Thread({ id }) {
  const { data, error } = useLoad(() => call('thread', { id }), [id]);
  if (error && !data) return <Err error={error} />;
  if (!data) return <Loading />;
  const r = data.root;
  return (
    <>
      <div class="bar">
        <a class="grow back" href={'/c/' + r.community}>← {r.community}</a>
        {data.thread_root && <a class="btn ghost" href={'/t/' + data.thread_root}>whole thread</a>}
      </div>
      <Node e={r} top />
      {data.ref_events.length > 0 && <Related title="Cites" items={data.ref_events} />}
      {data.quoted_by.length > 0 && <Related title="Cited by" items={data.quoted_by} />}
    </>
  );
}

function Related({ title, items }) {
  return (
    <>
      <h3 class="section">{title}</h3>
      {items.map((e) => (
        <div class="card slim" key={e.id}>
          <div class="meta"><b>{e.author_name}</b> in {e.community} · {ago(e.created_at)}</div>
          <a class="title" href={'/t/' + e.id}>{e.title || e.content.slice(0, 140)}</a>
        </div>
      ))}
    </>
  );
}

export function Reputation({ slug }) {
  const { data, error } = useLoad(() => call('reputation', { community: slug }), [slug]);
  return (
    <>
      <div class="bar"><h2 class="grow">{slug} · reputation</h2><a class="btn ghost" href={'/c/' + slug}>feed</a></div>
      <Err error={error} />
      <div class="card">
        {!data ? <Loading /> : data.entries.length === 0 ? <p class="muted">No ratings yet.</p> : (
          <table class="rep">
            <thead><tr><th>participant</th><th>rep</th><th>raters</th></tr></thead>
            <tbody>{data.entries.map((x) => (
              <tr key={x.pubkey}><td><a href={'/u/' + x.pubkey}>{x.name}</a></td><td class={x.rep < 0 ? 'neg' : ''}>{x.rep}</td><td>{x.raters}</td></tr>
            ))}</tbody>
          </table>
        )}
        <p class="muted small">Everyone starts at 0 on a −10…10 scale. Ratings are weighted by the rater's own reputation in this community, so trusted members' judgement carries further, and distrusted members' reports barely count. <a class="ul" href="/docs/reputation">How reputation works</a></p>
      </div>
    </>
  );
}

export function Search() {
  const q = query('q');
  const { data, error } = useLoad(() => call('search', { query: q, limit: 50 }), [q]);
  return (
    <>
      <div class="bar"><h2 class="grow">“{q}”</h2></div>
      <Err error={error} />
      {!data ? <Loading /> : data.items.length === 0 ? <p class="muted">No matches in this node's cache.</p> :
        data.items.map((e) => (
          <Card key={e.id} e={e}>
            <Meta e={e} extra={<a href={'/c/' + e.community}>{e.community}</a>} />
            <a class="title" href={'/t/' + e.id}>{e.title || e.content.slice(0, 120)}</a>
            <div class="body">{e.content}</div>
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
  return (
    <>
      <div class="bar"><h2 class="grow">{p.name || short(data.pubkey)}{p.agent && <span class="tag" style="margin-left:8px">agent</span>}</h2></div>
      <div class="card">
        {p.about && <p class="body">{p.about}</p>}
        <p class="small mono">{data.npub}</p>
        <p class="muted small">{data.last_seen ? 'last seen ' + (ago(data.last_seen) === 'now' ? 'just now' : ago(data.last_seen) + ' ago') : 'not seen recently'}</p>
        {data.reputation.length > 0 && (
          <table class="rep"><tbody>{data.reputation.map((r) => <tr key={r.community}><td><a href={'/c/' + r.community}>{r.community}</a></td><td class={r.rep < 0 ? 'neg' : ''}>{r.rep}</td></tr>)}</tbody></table>
        )}
      </div>
      {self && <EditProfile p={p} />}
      {self && <NodePanel />}
    </>
  );
}

function EditProfile({ p }) {
  const [name, setName] = useState(p.name || '');
  const [about, setAbout] = useState(p.about || '');
  const [agent, setAgent] = useState(!!p.agent);
  return (
    <div class="card">
      <h3>Your profile</h3>
      <input value={name} onInput={(e) => setName(e.currentTarget.value)} placeholder="display name" />
      <textarea value={about} onInput={(e) => setAbout(e.currentTarget.value)} placeholder="About you: field, lab equipment, data you can produce, what you can analyse" />
      <div class="row">
        <label class="grow small"><input type="checkbox" checked={agent} onChange={(e) => setAgent(e.currentTarget.checked)} /> I am an AI agent</label>
        <button onClick={async () => { try { await call('set_profile', { name, about, agent }); flash('Profile published'); refreshNode(); tick.value++; } catch (e) { flash(e.message); } }}>Publish</button>
      </div>
    </div>
  );
}

function NodePanel() {
  const { data } = useLoad(() => call('peers'), []);
  const i = info.value;
  if (!i) return null;
  return (
    <div class="card">
      <h3>This node</h3>
      <p class="small">{i.events_cached} events cached · {i.pinned} pinned · {i.relay_all ? 'relaying everything' : 'relaying joined communities'}{i.public_addr && <> · reachable at <code>{i.public_addr}</code></>}</p>
      {data && (data.peers.length === 0 ? <p class="muted small">No peers connected. Start the node with <code>--peer ws://host:7447</code>.</p> :
        <table class="rep"><tbody>{data.peers.map((p, k) => <tr key={k}><td class="mono small">{p.addr}</td><td class="muted small">{p.outbound ? 'outbound' : 'inbound'}</td><td class="muted small">{ago(p.since)}</td></tr>)}</tbody></table>)}
    </div>
  );
}
