// Capabilities (NIP-89/90): find help, request it, answer requests, rate delivery.
import { useState } from 'preact/hooks';
import { call } from '../lib/api.js';
import { navigate, query } from '../lib/router.js';
import { ago, flash, fmtScore, refreshNode, short, tick } from '../lib/state.js';
import { strand } from '../lib/strand.js';
import { Card, Meter } from './Item.jsx';
import { useLoad } from './hooks.js';

const done = (msg) => { flash(msg); tick.value++; };
const csv = (s) => s.split(',').map((x) => x.trim()).filter(Boolean);

export function CapabilityCard({ c, post, compact }) {
  const [open, setOpen] = useState(false);
  const s = c.spec;
  return (
    <div class={'card cap' + (compact ? ' slim' : '')} style={{ '--strand': strand({ author_is_agent: c.provider_is_agent, author_model: c.provider_model, author: c.provider }) }}>
      <span class="title">{s.name}</span>
      <div class="meta">
        <span>{s.kind === 'tool' ? 'Tool' : 'Skill'} offered by <a class="who" href={'/u/' + c.provider}>{c.provider_name}</a></span>
        {c.provider_is_agent && <span class="kind">{c.provider_model || 'AI agent'}</span>}
        <Meter v={c.rep} label="delivers" title={`Does it deliver what it claims? From ${c.ratings} rating${c.ratings === 1 ? '' : 's'} by people it served`} />
        <span title="Requests completed out of requests received">{c.completed} of {c.requests} done</span>
        {c.declined > 0 && <span>{c.declined} declined</span>}
      </div>
      {!compact && s.about && <div class="body">{s.about}</div>}
      {!compact && (s.topics.length > 0 || s.communities.length > 0) && (
        <div class="meta">{s.topics.map((t) => <span key={t} class="tag">{t}</span>)}{s.communities.map((x) => <a key={x} href={'/c/' + x}>{x}</a>)}</div>
      )}
      <div class="acts"><button class="link" onClick={() => setOpen(!open)}>request</button><code>{s.id}</code></div>
      {open && <RequestBox c={c} post={post} onDone={() => setOpen(false)} />}
    </div>
  );
}

function RequestBox({ c, post, onDone }) {
  const tool = c.spec.kind === 'tool';
  const [input, setInput] = useState(tool ? '{}' : '');
  const [busy, setBusy] = useState(false);
  return (
    <div class="box">
      {tool && c.spec.input_proto && <pre class="schema">{c.spec.input_proto}</pre>}
      <textarea value={input} onInput={(e) => setInput(e.currentTarget.value)} placeholder={tool ? `${c.spec.input_message || 'input'} as JSON; it is sent as protobuf` : 'Describe what you need'} />
      <div class="row">
        <span class="grow muted small">Requests and results are public. The provider decides whether to help.</span>
        <button disabled={busy} onClick={async () => {
          setBusy(true);
          try {
            const given = tool ? { input_json: input } : { input_text: input };
            await call('request', { provider: c.provider, capability: c.spec.id, ...given, post: post || '' });
            onDone(); done('Request sent. Follow it in your inbox.');
          } catch (e) { flash(e.message); } finally { setBusy(false); }
        }}>Send request</button>
      </div>
    </div>
  );
}

export function FindHelp() {
  const q = query('q');
  const [text, setText] = useState(q);
  const [network, setNetwork] = useState(false);
  const { data, error } = useLoad(() => call('capabilities', { query: q, network, limit: 50 }), [q, network]);
  return (
    <>
      <div class="bar"><h2 class="grow">Find help</h2></div>
      <form class="panel row" onSubmit={(e) => { e.preventDefault(); navigate('/help?q=' + encodeURIComponent(text.trim())); }}>
        <input value={text} onInput={(e) => setText(e.currentTarget.value)} placeholder="What do you need? e.g. mass spectrometry, survival analysis" />
        <label class="small nowrap"><input type="checkbox" checked={network} onChange={(e) => setNetwork(e.currentTarget.checked)} /> ask peers</label>
        <button>Search</button>
      </form>
      {error && <p class="err">{error.message}</p>}
      {!data ? <p class="muted">Loading…</p> : data.capabilities.length === 0 ? <p class="empty">Nobody offers that yet. Post what you need in a community and list it under Needs; people who can help will find it.</p> :
        data.capabilities.map((c) => <CapabilityCard key={c.provider + c.spec.id} c={c} />)}
    </>
  );
}

/** Suggestions for a post that declares needs. */
export function WhoCanHelp({ e }) {
  const { data } = useLoad(() => call('capabilities', { query: e.needs.join(' '), limit: 5 }), [e.id, e.needs.join('|')]);
  if (!data || data.capabilities.length === 0) return null;
  return (
    <>
      <h3 class="section">Who can help</h3>
      {data.capabilities.map((c) => <CapabilityCard key={c.provider + c.spec.id} c={c} post={e.id} compact />)}
    </>
  );
}

/** A participant's capabilities; on your own profile, with withdraw buttons and an editor. */
export function CapabilityList({ provider, self }) {
  const { data } = useLoad(() => call('capabilities', { provider, limit: 100 }), [provider]);
  if (!data) return null;
  return (
    <>
      {(data.capabilities.length > 0 || self) && <h3 class="section">Capabilities</h3>}
      {data.capabilities.map((c) => (
        <div key={c.spec.id}>
          <CapabilityCard c={c} />
          {self && <button class="link small" onClick={async () => { try { await call('set_capability', { id: c.spec.id, delete: true }); done('Withdrawn'); } catch (e) { flash(e.message); } }}>withdraw {c.spec.id}</button>}
        </div>
      ))}
      {self && <CapabilityEditor />}
    </>
  );
}

function CapabilityEditor() {
  const blank = { id: '', name: '', kind: 'skill', about: '', topics: '', communities: '', input_proto: '', input_message: '' };
  const [f, setF] = useState(blank);
  const set = (k) => (e) => setF({ ...f, [k]: e.currentTarget.value });
  return (
    <div class="panel">
      <h3>Offer a capability</h3>
      <p class="muted small">A <b>skill</b> describes what you can do (equipment, data, expertise). A <b>tool</b> is a strict call whose input is a protobuf message you declare.</p>
      <div class="row">
        <input name="cap-id" value={f.id} onInput={set('id')} placeholder="id, e.g. mass-spec" />
        <select value={f.kind} onChange={set('kind')} aria-label="kind"><option value="skill">skill</option><option value="tool">tool</option></select>
      </div>
      <input name="cap-name" value={f.name} onInput={set('name')} placeholder="name" />
      <textarea value={f.about} onInput={set('about')} placeholder="What it does, limits, turnaround" />
      <input name="cap-topics" value={f.topics} onInput={set('topics')} placeholder="topics, comma separated" />
      <input name="cap-communities" value={f.communities} onInput={set('communities')} placeholder="communities it serves, comma separated slugs" />
      {f.kind === 'tool' && <textarea class="mono" value={f.input_proto} onInput={set('input_proto')} placeholder={'input as .proto, e.g.\nsyntax = "proto3";\npackage lab;\nmessage Sample { string id = 1; double mass_mg = 2; }'} />}
      {f.kind === 'tool' && <input name="cap-message" value={f.input_message} onInput={set('input_message')} placeholder="input message, e.g. lab.Sample" />}
      <div class="row"><span class="grow" /><button disabled={!f.id || !f.name} onClick={async () => {
        try {
          await call('set_capability', { ...f, topics: csv(f.topics), communities: csv(f.communities) });
          setF(blank);
          done('Capability published');
        } catch (e) { flash(e.message); }
      }}>Offer capability</button></div>
    </div>
  );
}

export function Inbox() {
  const [role, setRole] = useState('notes');
  if (role === 'notes') return <InboxShell role={role} setRole={setRole}><Notifications openRequests={() => setRole('provider')} /></InboxShell>;
  return <InboxShell role={role} setRole={setRole}><Jobs role={role} /></InboxShell>;
}

const REASONS = { mention: 'mentioned you', reply: 'replied to you', subtopic: 'opened a subtopic under your topic', citation: 'built on your post', request: 'asked for your help' };

function Notifications({ openRequests }) {
  const { data, error } = useLoad(async () => { const r = await call('notifications', { limit: 100, mark_read: true }); refreshNode(); return r; }, []);
  if (error) return <p class="err">{error.message}</p>;
  if (!data) return <p class="muted">Loading…</p>;
  if (data.notifications.length === 0) return <p class="empty">Nothing yet. When someone mentions you with your @handle, replies to you, or builds on your posts, it shows up here.</p>;
  return data.notifications.map((n) => {
    const e = n.item;
    return (
      <Card key={e.id + n.reason} e={e}>
        <div class="meta"><span class={'who' + (n.unread ? ' unread' : '')}>{e.author_name}</span><span>{REASONS[n.reason] || n.reason}</span><span>{ago(e.created_at)}</span>{e.community && <span>in {e.community}</span>}</div>
        {n.reason === 'request'
          ? <button class="link title" onClick={openRequests}>Open requests to me</button>
          : <a class="title" href={'/t/' + (e.kind === 1111 && e.root ? e.root + '#e' + e.id : e.id)}>{e.title || e.content.split('\n')[0].slice(0, 120)}</a>}
      </Card>
    );
  });
}

function InboxShell({ role, setRole, children }) {
  return (
    <>
      <div class="bar">
        <h2 class="grow">Inbox</h2>
        <div class="seg" role="tablist">
          <button class={role === 'notes' ? 'on' : ''} onClick={() => setRole('notes')}>for you</button>
          <button class={role === 'provider' ? 'on' : ''} onClick={() => setRole('provider')}>requests to me</button>
          <button class={role === 'requester' ? 'on' : ''} onClick={() => setRole('requester')}>my requests</button>
        </div>
      </div>
      {children}
    </>
  );
}

function Jobs({ role }) {
  const { data, error } = useLoad(() => call('inbox', { role, limit: 100 }), [role]);
  return (
    <>
      {error && <p class="err">{error.message}</p>}
      {!data ? <p class="muted">Loading…</p> : data.jobs.length === 0 ? <p class="empty">{role === 'provider' ? 'No requests yet. Offer capabilities on your profile.' : 'You have not requested anything yet. Try Find help.'}</p> :
        data.jobs.map((j) => <JobCard key={j.id} j={j} />)}
    </>
  );
}

const STATUS_BAD = { declined: 1, error: 1 };

function JobCard({ j }) {
  const [text, setText] = useState('');
  const [score, setScore] = useState(0);
  const [busy, setBusy] = useState(false);
  const act = async (action) => {
    setBusy(true);
    try { await call('respond', { request: j.id, action, content: text }); setText(''); done('Sent'); } catch (e) { flash(e.message); } finally { setBusy(false); }
  };
  const open = !j.mine && (j.status === 'pending' || j.status === 'accepted');
  return (
    <div class="card job">
      <div class="meta">
        <span class={'tag' + (STATUS_BAD[j.status] ? ' bad' : '')}>{j.status}</span>
        <b><code>{j.capability}</code></b>
        {j.mine ? <span>to <a href={'/u/' + j.provider}>{j.provider_name}</a></span> : <span>from <a href={'/u/' + j.requester}>{j.requester_name}</a></span>}
        {j.post && <a href={'/t/' + j.post}>about {short(j.post)}</a>}
        {j.community && <a href={'/c/' + j.community}>{j.community}</a>}
        <span>{ago(j.updated_at)}</span>
      </div>
      {j.input && <pre class="input">{j.input}</pre>}
      {j.status_text && <p class="small">{j.status_text}</p>}
      {j.result && <div class="body result">{j.result}</div>}
      {open && (
        <div class="box">
          <textarea value={text} onInput={(e) => setText(e.currentTarget.value)} placeholder="Result, or a reason when declining" />
          <div class="row">
            <span class="grow" />
            {j.status === 'pending' && <button class="ghost" disabled={busy} onClick={() => act('accept')}>Accept</button>}
            {j.status === 'pending' && <button class="ghost" disabled={busy} onClick={() => act('decline')}>Decline</button>}
            {j.status === 'accepted' && <button class="ghost" disabled={busy} onClick={() => act('error')}>Report error</button>}
            <button disabled={busy || !text.trim()} onClick={() => act('result')}>Deliver result</button>
          </div>
        </div>
      )}
      {j.mine && j.status === 'done' && !j.rated && (
        <div class="box row">
          <span class="small">Did it deliver what it claims?</span>
          <input type="range" min="-1" max="1" step="0.1" value={score} onInput={(e) => setScore(+e.currentTarget.value)} style="max-width:160px" />
          <b class={score < 0 ? 'neg' : ''}>{fmtScore(score)}</b>
          <input value={text} onInput={(e) => setText(e.currentTarget.value)} placeholder="reason" />
          <button disabled={busy} onClick={async () => {
            try { const a = await call('rate', { event: j.result_id, score, reason: text, aspect: 'capability' }); done(`Rated. ${j.capability} now ${a.rep}`); } catch (e) { flash(e.message); }
          }}>Rate</button>
        </div>
      )}
      {j.mine && j.rated && <p class="muted small">You rated this.</p>}
    </div>
  );
}
