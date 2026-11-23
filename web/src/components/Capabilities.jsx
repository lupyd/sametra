// Capabilities (NIP-89/90): find help, request it, answer requests, rate delivery.
import { useState } from 'preact/hooks';
import { call } from '../lib/api.js';
import { navigate, query } from '../lib/router.js';
import { ago, flash, short, tick } from '../lib/state.js';
import { useLoad } from './hooks.js';

const done = (msg) => { flash(msg); tick.value++; };
const csv = (s) => s.split(',').map((x) => x.trim()).filter(Boolean);

export function CapabilityCard({ c, post, compact }) {
  const [open, setOpen] = useState(false);
  const s = c.spec;
  return (
    <div class={'card cap' + (compact ? ' slim' : '')}>
      <div class="meta">
        <b class="who">{s.name}</b>
        <span class="tag">{s.kind}</span>
        <span>by <a href={'/u/' + c.provider}>{c.provider_name}</a></span>
        {c.provider_is_agent && <span class="tag">agent</span>}
        <span class={'tag' + (c.rep < 0 ? ' bad' : '')} title={`does it deliver what it claims? ${c.ratings} ratings`}>delivers {c.rep}</span>
        <span title="completed / requested">{c.completed}/{c.requests} done</span>
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
      {tool && c.spec.input_schema && <pre class="schema">{c.spec.input_schema}</pre>}
      <textarea value={input} onInput={(e) => setInput(e.currentTarget.value)} placeholder={tool ? 'JSON input' : 'Describe what you need'} />
      <div class="row">
        <span class="grow muted small">Requests and results are public. The provider decides whether to help.</span>
        <button disabled={busy} onClick={async () => {
          setBusy(true);
          try {
            await call('request', { provider: c.provider, capability: c.spec.id, input, post: post || '' });
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
      <form class="card row" onSubmit={(e) => { e.preventDefault(); navigate('/help?q=' + encodeURIComponent(text.trim())); }}>
        <input value={text} onInput={(e) => setText(e.currentTarget.value)} placeholder="What do you need? e.g. mass spectrometry, survival analysis" />
        <label class="small nowrap"><input type="checkbox" checked={network} onChange={(e) => setNetwork(e.currentTarget.checked)} /> ask peers</label>
        <button>Search</button>
      </form>
      {error && <p class="err">{error.message}</p>}
      {!data ? <p class="muted">Loading…</p> : data.capabilities.length === 0 ? <p class="muted">Nobody offers that yet. Post your need in a community and tag it.</p> :
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
  const [f, setF] = useState({ id: '', name: '', kind: 'skill', about: '', topics: '', communities: '', input_schema: '' });
  const set = (k) => (e) => setF({ ...f, [k]: e.currentTarget.value });
  return (
    <div class="card">
      <h3>Offer a capability</h3>
      <p class="muted small">A <b>skill</b> describes what you can do (equipment, data, expertise). A <b>tool</b> is a strict call others make with JSON input.</p>
      <div class="row">
        <input name="cap-id" value={f.id} onInput={set('id')} placeholder="id, e.g. mass-spec" />
        <select value={f.kind} onChange={set('kind')} aria-label="kind"><option value="skill">skill</option><option value="tool">tool</option></select>
      </div>
      <input name="cap-name" value={f.name} onInput={set('name')} placeholder="name" />
      <textarea value={f.about} onInput={set('about')} placeholder="What it does, limits, turnaround" />
      <input name="cap-topics" value={f.topics} onInput={set('topics')} placeholder="topics, comma separated" />
      <input name="cap-communities" value={f.communities} onInput={set('communities')} placeholder="communities it serves, comma separated slugs" />
      {f.kind === 'tool' && <textarea class="mono" value={f.input_schema} onInput={set('input_schema')} placeholder='input JSON Schema, e.g. {"type":"object","required":["sample"]}' />}
      <div class="row"><span class="grow" /><button disabled={!f.id || !f.name} onClick={async () => {
        try {
          await call('set_capability', { ...f, topics: csv(f.topics), communities: csv(f.communities) });
          setF({ id: '', name: '', kind: 'skill', about: '', topics: '', communities: '', input_schema: '' });
          done('Capability published');
        } catch (e) { flash(e.message); }
      }}>Offer capability</button></div>
    </div>
  );
}

export function Inbox() {
  const [role, setRole] = useState('provider');
  const { data, error } = useLoad(() => call('inbox', { role, limit: 100 }), [role]);
  return (
    <>
      <div class="bar">
        <h2 class="grow">Inbox</h2>
        <div class="seg" role="tablist">
          <button class={role === 'provider' ? 'on' : 'ghost'} onClick={() => setRole('provider')}>requests to me</button>
          <button class={role === 'requester' ? 'on' : 'ghost'} onClick={() => setRole('requester')}>my requests</button>
        </div>
      </div>
      {error && <p class="err">{error.message}</p>}
      {!data ? <p class="muted">Loading…</p> : data.jobs.length === 0 ? <p class="muted">{role === 'provider' ? 'No requests yet. Offer capabilities on your profile.' : 'You have not requested anything yet. Try Find help.'}</p> :
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
          <input type="range" min="-10" max="10" value={score} onInput={(e) => setScore(+e.currentTarget.value)} style="max-width:160px" />
          <b class={score < 0 ? 'neg' : ''}>{score > 0 ? '+' + score : score}</b>
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
