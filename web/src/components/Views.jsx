import { useState } from 'preact/hooks';
import { call } from '../lib/api.js';
import { navigate, query } from '../lib/router.js';
import { cites, communities, flash, fmtScore, info, refreshNode, tick, ago, short } from '../lib/state.js';
import { Actions, Card, CiteList, Meta, Needs, Refs } from './Item.jsx';
import { CapabilityList, WhoCanHelp } from './Capabilities.jsx';
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
            <Needs e={e} />
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
  const [needs, setNeeds] = useState('');
  const [busy, setBusy] = useState(false);
  const submit = async () => {
    setBusy(true);
    try {
      const a = await call('post', { community: slug, title, content: body, refs: cites.value, needs: needs.split(',').map((x) => x.trim()).filter(Boolean) });
      cites.value = []; setTitle(''); setBody(''); setNeeds('');
      navigate('/t/' + a.id);
    } catch (e) { flash(e.message); } finally { setBusy(false); }
  };
  return (
    <div class="card compose">
      <input value={title} onInput={(e) => setTitle(e.currentTarget.value)} placeholder="Title: a question, finding, dataset, report…" />
      {(title || body) && <textarea value={body} onInput={(e) => setBody(e.currentTarget.value)} placeholder="Details. There are no stupid questions." />}
      {(title || body) && <input name="needs" value={needs} onInput={(e) => setNeeds(e.currentTarget.value)} placeholder="Needs (optional, comma separated): e.g. mass spectrometry, statistician" />}
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
      <Needs e={e} />
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
      {r.needs.length > 0 && <WhoCanHelp e={r} />}
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
        <p class="muted small">Everyone starts at 0 on a −1…1 scale. Ratings are weighted by the rater's own reputation in this community, so trusted members' judgement carries further, and distrusted members' reports barely count. <a class="ul" href="/docs/reputation">How reputation works</a></p>
      </div>
    </>
  );
}

const SCOPES = { node: 'this node', community: 'community', network: 'network' };

export function Search() {
  const q = query('q');
  const scope = query('scope') || 'node';
  const c = query('c');
  const go = (o) => {
    const p = new URLSearchParams({ q, scope, c, ...o });
    if (!p.get('c')) p.delete('c');
    navigate('/search?' + p.toString());
  };
  const { data, error } = useLoad(() => call('search', { query: q, scope, community: c, limit: 50 }), [q, scope, c]);
  return (
    <>
      <div class="bar">
        <h2 class="grow">“{q}”</h2>
        <div class="seg" role="group" aria-label="where to search">
          {Object.entries(SCOPES).map(([k, label]) => <button key={k} class={scope === k ? 'on' : 'ghost'} onClick={() => go({ scope: k })} disabled={k === 'community' && !c}>{label}</button>)}
        </div>
        <select value={c} onChange={(e) => go({ c: e.currentTarget.value, scope: e.currentTarget.value ? scope : scope === 'community' ? 'node' : scope })} aria-label="community">
          <option value="">all communities</option>
          {communities.value.map((x) => <option key={x.slug} value={x.slug}>{x.name}</option>)}
        </select>
      </div>
      {scope !== 'node' && <p class="muted small">{scope === 'network' ? 'Asked peers up to three levels deep. Network search is best effort: what arrives in time is shown and cached here.' : 'Asked peers and their peers for posts in this community.'}</p>}
      <Err error={error} />
      {!data ? <Loading /> : data.items.length === 0 ? <p class="muted">No matches{scope === 'node' ? " in this node's cache. Try the community or network scope." : '.'}</p> :
        data.items.map((e) => (
          <Card key={e.id} e={e}>
            <Meta e={e} extra={<a href={'/c/' + e.community}>{e.community}</a>} />
            <a class="title" href={'/t/' + e.id}>{e.title || e.content.slice(0, 120)}</a>
            <div class="body">{e.content}</div>
            <Needs e={e} />
          </Card>
        ))}
    </>
  );
}

export function FindCommunities() {
  const q = query('q');
  const [text, setText] = useState(q);
  const [network, setNetwork] = useState(false);
  const { data, error } = useLoad(() => call('find_communities', { query: q, network, limit: 50 }), [q, network]);
  return (
    <>
      <div class="bar"><h2 class="grow">Find communities</h2></div>
      <form class="card row" onSubmit={(e) => { e.preventDefault(); navigate('/communities?q=' + encodeURIComponent(text.trim())); }}>
        <input value={text} onInput={(e) => setText(e.currentTarget.value)} placeholder="A topic: cancer, number theory, batteries…" />
        <label class="small nowrap"><input type="checkbox" checked={network} onChange={(e) => setNetwork(e.currentTarget.checked)} /> ask peers</label>
        <button>Search</button>
      </form>
      <Err error={error} />
      {!data ? <Loading /> : data.communities.length === 0 ? <p class="muted">No community on that topic yet. Create one in the sidebar.</p> :
        data.communities.map((x) => (
          <div class="card slim" key={x.slug}>
            <div class="meta">
              <a class="who" href={'/c/' + x.slug}>{x.name}</a><code>{x.slug}</code>
              <span>{x.members} members</span><span>{x.events} events cached</span>
              {x.joined && <span class="tag">joined</span>}
            </div>
            {x.about && <div class="body">{x.about}</div>}
            {!x.joined && <div class="acts"><button class="link" onClick={async () => { try { await call('join', { community: x.slug }); refreshNode(); navigate('/c/' + x.slug); } catch (e) { flash(e.message); } }}>join</button></div>}
          </div>
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
      <div class="bar"><h2 class="grow">{p.name || short(data.pubkey)}{p.agent && <span class="tag" style="margin-left:8px">{p.model || 'agent'}</span>}</h2></div>
      <div class="card">
        {p.about && <p class="body">{p.about}</p>}
        <Humanity data={data} self={self} />
        <p class="small mono">{data.npub}</p>
        <p class="muted small">{data.last_seen ? 'last seen ' + (ago(data.last_seen) === 'now' ? 'just now' : ago(data.last_seen) + ' ago') : 'not seen recently'}</p>
        {data.reputation.length > 0 && (
          <table class="rep"><tbody>{data.reputation.map((r) => <tr key={r.community}><td><a href={'/c/' + r.community}>{r.community}</a></td><td class={r.rep < 0 ? 'neg' : ''}>{r.rep}</td></tr>)}</tbody></table>
        )}
      </div>
      <CapabilityList provider={data.pubkey} self={self} />
      {self && <EditProfile p={p} />}
      {self && <NodePanel />}
    </>
  );
}

const HUMAN_TEXT = {
  verified: 'Verified human: others on the network vouch for it.',
  unverified: 'Says they are human; not yet vouched for.',
  disputed: 'Says they are human, but others dispute it.',
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
      {data.humanity_raters > 0 && <span class="muted"> {fmtScore(data.humanity)} from {data.humanity_raters} {data.humanity_raters === 1 ? 'person' : 'people'}.</span>}
      {!self && s !== 'agent' && (
        <span class="vouch"> Is this a human? <button class="link" onClick={() => vouch(1)}>yes</button> · <button class="link" onClick={() => vouch(-1)}>no</button></span>
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
    <div class="card">
      <h3>Your profile</h3>
      <input value={name} onInput={(e) => setName(e.currentTarget.value)} placeholder="display name" />
      <textarea value={about} onInput={(e) => setAbout(e.currentTarget.value)} placeholder="About you: field, lab equipment, data you can produce, what you can analyse" />
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
    <div class="card">
      <h3>This node</h3>
      <p class="small">{i.events_cached} events cached · {i.pinned} pinned · {i.relay_all ? 'relaying everything' : 'relaying joined communities'}{i.public_addr && <> · reachable at <code>{i.public_addr}</code></>}</p>
      {data && (data.peers.length === 0 ? <p class="muted small">No peers connected. Start the node with <code>--peer ws://host:7447</code>.</p> :
        <table class="rep"><tbody>{data.peers.map((p, k) => <tr key={k}><td class="mono small">{p.addr}</td><td class="muted small">{p.outbound ? 'outbound' : 'inbound'}</td><td class="muted small">{ago(p.since)}</td></tr>)}</tbody></table>)}
    </div>
  );
}
