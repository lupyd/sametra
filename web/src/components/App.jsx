import { useEffect, useState } from 'preact/hooks';
import { call } from '../lib/api.js';
import { loc, match, navigate } from '../lib/router.js';
import { communities, flash, info, refreshNode, toast } from '../lib/state.js';
import { Feed, Home, Profile, Reputation, Search, Thread } from './Views.jsx';

const ROUTES = [
  ['/', Home],
  ['/c/:slug', Feed],
  ['/c/:slug/reputation', Reputation],
  ['/t/:id', Thread],
  ['/u/:pk', Profile],
  ['/me', Profile],
  ['/search', Search],
];

function route(path) {
  for (const [pat, C] of ROUTES) {
    const m = match(pat, path);
    if (m) return <C {...m} />;
  }
  return <p class="muted">Not found.</p>;
}

export default function App() {
  useEffect(() => { refreshNode(); }, []);
  const path = loc.value.split('?')[0];
  const [menu, setMenu] = useState(false);
  useEffect(() => setMenu(false), [path]);
  return (
    <>
      <header>
        <button class="link burger" aria-label="communities" onClick={() => setMenu(!menu)}>☰</button>
        <a class="brand" href="/">Sametra</a>
        <form class="search" onSubmit={(e) => { e.preventDefault(); const q = e.currentTarget.q.value.trim(); q && navigate('/search?q=' + encodeURIComponent(q)); }}>
          <input name="q" placeholder="Search…" />
        </form>
        <a class="docs-link" href="/docs">Docs</a>
        <a class="me" href="/me">{info.value ? (info.value.name || info.value.npub.slice(0, 14) + '…') : '…'}</a>
      </header>
      <div class="wrap">
        <aside class={menu ? 'open' : ''}><Sidebar path={path} /></aside>
        <main>{route(path)}</main>
      </div>
      {toast.value && <div class="toast" onClick={() => (toast.value = '')}>{toast.value}</div>}
    </>
  );
}

function Sidebar({ path }) {
  const [slug, setSlug] = useState('');
  const [name, setName] = useState('');
  const [about, setAbout] = useState('');
  const go = async (tool, args) => {
    try {
      const a = await call(tool, args);
      await refreshNode();
      setSlug(''); setName(''); setAbout('');
      navigate('/c/' + a.community);
    } catch (e) { flash(e.message); }
  };
  const i = info.value;
  return (
    <>
      <h3>Communities</h3>
      <nav>
        {communities.value.length === 0 && <p class="muted small pad">None yet. Create one below.</p>}
        {communities.value.map((c) => (
          <a key={c.slug} href={'/c/' + c.slug} class={'c' + (path.startsWith('/c/' + c.slug) ? ' on' : '')} title={c.about}>
            <span>{c.joined ? '' : '○ '}{c.name}</span><small>{c.events}</small>
          </a>
        ))}
      </nav>
      <h3>New or join</h3>
      <div class="pad">
        <input value={slug} onInput={(e) => setSlug(e.currentTarget.value)} placeholder="slug, e.g. cancer-research" />
        {slug && <input value={name} onInput={(e) => setName(e.currentTarget.value)} placeholder="name (when creating)" />}
        {slug && <input value={about} onInput={(e) => setAbout(e.currentTarget.value)} placeholder="what it works on" />}
        <div class="row">
          <button disabled={!slug} onClick={() => go('create_community', { slug, name, about })}>Create</button>
          <button class="ghost" disabled={!slug} onClick={() => go('join', { community: slug })}>Join</button>
        </div>
      </div>
      {i && <p class="muted small pad">{i.peers} peers · {i.events_cached} cached</p>}
    </>
  );
}
