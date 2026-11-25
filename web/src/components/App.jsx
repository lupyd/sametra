import { useEffect, useState } from 'preact/hooks';
import { call } from '../lib/api.js';
import { loc, match, navigate } from '../lib/router.js';
import { info, refreshNode, tick, toast } from '../lib/state.js';
import { FindHelp, Inbox } from './Capabilities.jsx';
import { Chain } from './Chain.jsx';
import { Mark, ThemeToggle } from './Mark.jsx';
import { Feed, Profile, Search, Thread } from './Views.jsx';
import { useLoad } from './hooks.js';

const ROUTES = [
  ['/', Feed],
  ['/t/:id', Thread],
  ['/chain/:id', Chain],
  ['/u/:pk', Profile],
  ['/me', Profile],
  ['/search', Search],
  ['/help', FindHelp],
  ['/inbox', Inbox],
];

function route(path) {
  for (const [pat, C] of ROUTES) {
    const m = match(pat, path);
    if (m) return <C {...m} />;
  }
  return <p class="empty">There is nothing at this address. Go to the topics, or search.</p>;
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
        <button class="link burger" aria-label="Show topics you follow" onClick={() => setMenu(!menu)}>☰</button>
        <a class="brand" href="/"><Mark />Sametra</a>
        <form class="search" role="search" onSubmit={(e) => { e.preventDefault(); const q = e.currentTarget.q.value.trim(); q && navigate('/search?q=' + encodeURIComponent(q)); }}>
          <input name="q" placeholder="Search posts" aria-label="Search posts" />
        </form>
        <nav class="top">
          <a href="/help" class={on('/help')}>Find help</a>
          <a href="/inbox" class={on('/inbox')}>Inbox{info.value?.unread > 0 && <b class="count" aria-label={`${info.value.unread} unread`}>{info.value.unread}</b>}</a>
          <a href="/docs">Docs</a>
        </nav>
        <a class="me" href="/me">{info.value ? (info.value.name || info.value.npub.slice(0, 14) + '…') : '…'}</a>
        <ThemeToggle />
      </header>
      <div class="wrap">
        <aside class={menu ? 'open' : ''}><Sidebar path={path} /></aside>
        <main>{route(path)}</main>
      </div>
      {toast.value && <div class="toast" role="status" onClick={() => (toast.value = '')}>{toast.value}</div>}
    </>
  );
}

/** The topics, and the posts this node follows. */
function Sidebar({ path }) {
  const i = info.value;
  const following = i?.following || [];
  return (
    <>
      <nav>
        <a href="/" class={'c' + (path === '/' ? ' on' : '')}><span>All topics</span></a>
      </nav>
      <h3>Following</h3>
      <nav>
        {following.length === 0 && <p class="muted small">Follow a topic to keep it, and everything under it, on this node.</p>}
        {following.map((id) => <FollowedTopic key={id} id={id} on={path === '/t/' + id} />)}
      </nav>
      {i && <p class="stats">Connected to {plural(i.peers, 'peer', 'peers')}. {plural(i.events_cached, 'event', 'events')} kept on this node.</p>}
    </>
  );
}

function FollowedTopic({ id, on }) {
  const { data } = useLoad(() => call('event', { id }), [id]);
  const it = data?.item;
  return (
    <a href={'/t/' + id} class={'c' + (on ? ' on' : '')} title={it?.content.slice(0, 200)}>
      <span>{it ? it.title || it.content.split('\n')[0].slice(0, 60) : id.slice(0, 8)}</span>
    </a>
  );
}

// keep `tick` imported so the sidebar refreshes with the views
void tick;
