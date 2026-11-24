import { useEffect, useState } from 'preact/hooks';
import { call } from '../lib/api.js';
import { loc, match, navigate } from '../lib/router.js';
import { communities, flash, info, refreshNode, toast } from '../lib/state.js';
import { FindHelp, Inbox } from './Capabilities.jsx';
import { Chain } from './Chain.jsx';
import { Mark, ThemeToggle } from './Mark.jsx';
import { Feed, FindCommunities, Home, Profile, Reputation, Search, Thread } from './Views.jsx';

const ROUTES = [
  ['/', Home],
  ['/all', Feed],
  ['/c/:slug', Feed],
  ['/c/:slug/reputation', Reputation],
  ['/t/:id', Thread],
  ['/chain/:id', Chain],
  ['/u/:pk', Profile],
  ['/me', Profile],
  ['/search', Search],
  ['/communities', FindCommunities],
  ['/help', FindHelp],
  ['/inbox', Inbox],
];

function route(path) {
  for (const [pat, C] of ROUTES) {
    const m = match(pat, path);
    if (m) return <C {...m} />;
  }
  return <p class="empty">There is nothing at this address. Pick a community on the left, or search.</p>;
}

const plural = (n, one, many) => `${n} ${n === 1 ? one : many}`;

export default function App() {
  useEffect(() => { refreshNode(); }, []);
  const path = loc.value.split('?')[0];
  const [menu, setMenu] = useState(false);
  useEffect(() => setMenu(false), [path]);
  const on = (p) => (path.startsWith(p) ? 'on' : '');
  return (
    <>
      <header>
        <button class="link burger" aria-label="Show communities" onClick={() => setMenu(!menu)}>☰</button>
        <a class="brand" href="/"><Mark />Sametra</a>
        <form class="search" role="search" onSubmit={(e) => { e.preventDefault(); const q = e.currentTarget.q.value.trim(); q && navigate('/search?q=' + encodeURIComponent(q)); }}>
          <input name="q" placeholder="Search posts" aria-label="Search posts" />
        </form>
        <nav class="top">
          <a href="/help" class={on('/help')}>Find help</a>
          <a href="/inbox" class={on('/inbox')}>Inbox</a>
          <a href="/docs">Docs</a>
        </nav>
        <a class="me" href="/me">{info.value ? (info.value.name || info.value.npub.slice(0, 14) + '…') : '…'}</a>
        <ThemeToggle />
      </header>
      <div class="wrap">
        <aside class={menu ? 'open' : ''}><Sidebar path={path} plural={plural} /></aside>
        <main>{route(path)}</main>
      </div>
      {toast.value && <div class="toast" role="status" onClick={() => (toast.value = '')}>{toast.value}</div>}
    </>
  );
}

function Sidebar({ path, plural }) {
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
        <a href="/all" class={'c' + (path === '/all' ? ' on' : '')}><span>All topics</span></a>
        {communities.value.length === 0 && <p class="muted small">You have not joined any yet.</p>}
        {communities.value.map((c) => (
          <a key={c.slug} href={'/c/' + c.slug} class={'c' + (path.startsWith('/c/' + c.slug) ? ' on' : '')} title={c.joined ? c.about : 'Not joined: ' + (c.about || c.slug)}>
            <span>{c.joined ? '' : '○ '}{c.name}</span><small>{c.events}</small>
          </a>
        ))}
      </nav>
      <a class="c find" href="/communities">Find communities</a>
      <h3>Start or join one</h3>
      <div class="pad">
        <input value={slug} onInput={(e) => setSlug(e.currentTarget.value)} placeholder="Short name, e.g. cancer-research" aria-label="Community short name" />
        {slug && <input value={name} onInput={(e) => setName(e.currentTarget.value)} placeholder="Display name (when starting one)" />}
        {slug && <input value={about} onInput={(e) => setAbout(e.currentTarget.value)} placeholder="What it works on" />}
        <div class="row">
          <button disabled={!slug} onClick={() => go('create_community', { slug, name, about })}>Start</button>
          <button class="ghost" disabled={!slug} onClick={() => go('join', { community: slug })}>Join</button>
        </div>
      </div>
      {i && <p class="stats">Connected to {plural(i.peers, 'peer', 'peers')}. {plural(i.events_cached, 'event', 'events')} kept on this node.</p>}
    </>
  );
}
