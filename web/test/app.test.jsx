import { render } from 'preact';
import { act } from 'preact/test-utils';
import { afterEach, describe, expect, it, vi } from 'vitest';
import App from '../src/components/App.jsx';
import { loc, navigate } from '../src/lib/router.js';
import { cites, communities, info, toast } from '../src/lib/state.js';
import { fakeNode, item } from './fake-node.js';

const ME = 'e'.repeat(64);
const ROOT = 'a'.repeat(64);
let root, calls;

const base = {
  info: () => ({ pubkey: ME, npub: 'npub1mememememe', name: 'Me', events_cached: 7, pinned: 1, peers: 1, joined: ['math'], relay_all: true }),
  communities: () => ({ communities: [{ slug: 'math', name: 'Mathematics', about: 'proofs', events: 3, joined: true }, { slug: 'bio', name: 'Biology', events: 1 }] }),
  feed: () => ({ items: [item({ reply_count: 2 })] }),
};

const settle = () => act(async () => { for (let i = 0; i < 8; i++) await new Promise((r) => setTimeout(r, 0)); });

async function mount(path, handlers = {}) {
  calls = fakeNode({ ...base, ...handlers });
  navigate(path, true);
  root = document.createElement('div');
  document.body.appendChild(root);
  await act(() => render(<App />, root));
  await settle();
  return root;
}
afterEach(() => {
  render(null, root);
  root.remove();
  info.value = null;
  communities.value = [];
  cites.value = [];
  toast.value = '';
});

const $ = (sel) => root.querySelector(sel);
const $$ = (sel) => [...root.querySelectorAll(sel)];
const button = (text, scope = root) => [...scope.querySelectorAll('button')].find((b) => b.textContent.trim() === text);
const byPlaceholder = (p) => $$('input, textarea').find((e) => (e.getAttribute('placeholder') || '').startsWith(p));
const type = (el, v) => act(() => { el.value = v; el.dispatchEvent(new Event('input', { bubbles: true })); });
const click = async (el) => { await act(() => el.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }))); await settle(); };
const sent = (tool) => calls.filter((c) => c.tool === tool).map((c) => c.req);

describe('shell', () => {
  it('shows identity, communities and node stats', async () => {
    await mount('/c/math');
    expect($('header .me').textContent).toBe('Me');
    const links = $$('aside nav a');
    expect(links.map((a) => a.getAttribute('href'))).toEqual(['/c/math', '/c/bio']);
    expect(links[0].className).toContain('on');
    expect(links[1].textContent).toContain('○ Biology'); // not joined
    expect($('aside').textContent).toContain('1 peers · 7 cached');
    expect($('header a.docs-link').getAttribute('href')).toBe('/docs');
  });

  it('home redirects to the first joined community', async () => {
    await mount('/');
    expect(loc.value).toBe('/c/math');
    expect(sent('feed')[0]).toMatchObject({ community: 'math' });
  });

  it('home welcomes a fresh node', async () => {
    await mount('/', { communities: () => ({ communities: [] }) });
    expect(root.textContent).toContain('Welcome to Sametra');
    expect(root.textContent).toContain('None yet');
    expect($('main a[href="/docs"]').textContent).toBe('Read the docs');
  });

  it('unknown routes say so', async () => {
    await mount('/x/y/z');
    expect($('main').textContent).toBe('Not found.');
  });

  it('header search navigates', async () => {
    await mount('/c/math');
    const input = $('header form input');
    input.value = ' gene drive ';
    await act(() => $('header form').dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })));
    await settle();
    expect(loc.value).toBe('/search?q=gene%20drive');
    expect(sent('search').at(-1)).toMatchObject({ query: 'gene drive', limit: 50 });
  });

  it('node errors show as a dismissable toast', async () => {
    await mount('/c/math', { info: () => new Error('node down') });
    const t = $('.toast');
    expect(t.textContent).toBe('node down');
    await click(t);
    expect($('.toast')).toBeNull();
  });

  it('creates and joins communities from the sidebar', async () => {
    await mount('/c/math', { create_community: (r) => ({ ok: true, community: r.slug }), join: (r) => ({ ok: true, community: r.community }) });
    expect(button('Create').disabled).toBe(true);
    await type(byPlaceholder('slug'), 'oncology');
    await type(byPlaceholder('name'), 'Oncology');
    await type(byPlaceholder('what it works on'), 'tumours');
    await click(button('Create'));
    expect(sent('create_community')).toEqual([{ slug: 'oncology', name: 'Oncology', about: 'tumours' }]);
    expect(loc.value).toBe('/c/oncology');
    expect(byPlaceholder('slug').value).toBe('');
    await type(byPlaceholder('slug'), 'chem');
    await click(button('Join'));
    expect(sent('join')).toEqual([{ community: 'chem' }]);
    expect(loc.value).toBe('/c/chem');
  });
});

describe('feed', () => {
  it('lists threads with metadata and sorts on request', async () => {
    await mount('/c/math', {
      feed: (r) => ({ items: r.sort === 'new' ? [item({ title: 'Newest' })] : [item({ author_is_agent: true, author_rep: 4.5, ratings: 2, score: -4, refs: ['c'.repeat(64)], reply_count: 2 })] }),
    });
    expect(sent('feed')[0]).toEqual({ community: 'math', sort: 'top', limit: 100 });
    expect($('main h2').textContent).toBe('Mathematics');
    expect($('main .about').textContent).toBe('proofs');
    const card = $('article.card');
    expect(card.querySelector('a.title').getAttribute('href')).toBe('/t/' + ROOT);
    expect(card.textContent).toContain('2 replies');
    const tags = [...card.querySelectorAll('.tag')].map((t) => t.textContent);
    expect(tags).toEqual(expect.arrayContaining(['rep 4.5', 'score -4', 'agent']));
    expect(card.querySelector('.meta a[href^="/t/cccc"]').textContent).toBe('cccccccc');
    const sel = $('select');
    sel.value = 'new';
    await act(() => sel.dispatchEvent(new Event('change', { bubbles: true })));
    await settle();
    expect(sent('feed').at(-1).sort).toBe('new');
    expect($('a.title').textContent).toBe('Newest');
  });

  it('de-emphasises low-trust content without hiding it', async () => {
    await mount('/c/math', { feed: () => ({ items: [item({ low_trust: true, author_rep: -6 })] }) });
    expect($('article').className).toContain('low');
    expect($('article').textContent).toContain('Body text');
    expect($$('.tag.bad').map((t) => t.textContent)).toEqual(['rep -6', 'low trust']);
  });

  it('shows an empty state and a join button for unjoined communities', async () => {
    await mount('/c/bio', { feed: () => ({ items: [] }), join: () => ({ ok: true, community: 'bio' }) });
    expect(root.textContent).toContain('Nothing here yet');
    await click(button('join'));
    expect(sent('join')).toEqual([{ community: 'bio' }]);
  });

  it('composer posts a thread with pending citations and opens it', async () => {
    await mount('/c/math', { post: () => ({ ok: true, id: 'f'.repeat(64) }), thread: () => ({ root: item({ id: 'f'.repeat(64) }) }) });
    cites.value = ['c'.repeat(64)];
    expect(byPlaceholder('Details')).toBeUndefined();
    await type(byPlaceholder('Title'), 'Is P = NP?');
    expect(button('Post').disabled).toBe(true);
    await type(byPlaceholder('Details'), 'Asking for a friend');
    expect(root.textContent).toContain('citing cccccccc');
    await click(button('Post'));
    expect(sent('post')).toEqual([{ community: 'math', title: 'Is P = NP?', content: 'Asking for a friend', parent: '', refs: ['c'.repeat(64)] }]);
    expect(loc.value).toBe('/t/' + 'f'.repeat(64));
    expect(cites.value).toEqual([]);
  });

  it('composer surfaces node errors', async () => {
    await mount('/c/math', { post: () => new Error('community slug must be…') });
    await type(byPlaceholder('Title'), 't');
    await type(byPlaceholder('Details'), 'b');
    await click(button('Post'));
    expect(toast.value).toBe('community slug must be…');
    expect(loc.value).toBe('/c/math');
  });
});

describe('thread', () => {
  const R1 = '1'.repeat(64), R2 = '2'.repeat(64);
  const thread = (o = {}) => ({
    root: item({ mine: true, reply_count: 2, replies: [item({ id: R1, kind: 1111, title: '', content: 'first', parent: ROOT, replies: [item({ id: R2, kind: 1111, title: '', content: 'nested', parent: R1 })] })] }),
    quoted_by: [item({ id: 'q'.repeat(64), title: 'A citing thread', community: 'bio' })],
    ref_events: [item({ id: 'r'.repeat(64), title: 'A cited source' })],
    thread_root: '',
    ...o,
  });

  it('renders the nested tree and citations both ways', async () => {
    await mount('/t/' + ROOT, { thread: () => thread() });
    expect(sent('thread')[0]).toEqual({ id: ROOT, thread: false });
    expect($('h2.title').textContent).toBe('A title');
    expect($('#e' + R1 + ' > .body').textContent).toBe('first');
    expect($('#e' + R1 + ' .replies #e' + R2 + ' > .body').textContent).toBe('nested');
    expect($('a.back').getAttribute('href')).toBe('/c/math');
    const sections = $$('h3.section').map((h) => h.textContent);
    expect(sections).toEqual(['Cites', 'Cited by']);
    expect(root.textContent).toContain('A cited source');
    expect(root.textContent).toContain('A citing thread');
    expect(button('whole thread')).toBeUndefined();
  });

  it('links a reply back to its whole thread', async () => {
    await mount('/t/' + R1, { thread: () => thread({ thread_root: ROOT }) });
    expect($$('a.btn').find((a) => a.textContent === 'whole thread').getAttribute('href')).toBe('/t/' + ROOT);
  });

  it('shows lookup errors', async () => {
    await mount('/t/' + ROOT, { thread: () => new Error('event not found on this node or its peers') });
    expect($('.err').textContent).toBe('event not found on this node or its peers');
  });

  it('replies inline with citations', async () => {
    await mount('/t/' + ROOT, { thread: () => thread(), post: () => ({ ok: true, id: 'n'.repeat(64) }) });
    const top = $('#e' + ROOT);
    await click(button('reply', top.querySelector('.acts')));
    await type(byPlaceholder('Reply'), 'my answer');
    await click(button('Reply'));
    expect(sent('post')).toEqual([{ community: '', title: '', content: 'my answer', parent: ROOT, refs: [] }]);
    expect(toast.value).toBe('Replied');
    expect(byPlaceholder('Reply')).toBeUndefined();
  });

  it('rates with the slider', async () => {
    await mount('/t/' + ROOT, { thread: () => thread(), rate: () => ({ ok: true, rep: -1.25 }) });
    await click(button('rate', $('#e' + R1 + ' > .acts')));
    const slider = $('input[type=range]');
    expect([slider.min, slider.max]).toEqual(['-10', '10']);
    await type(slider, '-4');
    expect($('.box b').textContent).toBe('-4');
    await type(byPlaceholder('reason'), 'not replicated');
    await click(button('Rate'));
    expect(sent('rate')).toEqual([{ score: -4, event: R1, target: '', community: '', reason: 'not replicated' }]);
    expect(toast.value).toContain('-1.25');
  });

  it('cites, pins and deletes', async () => {
    const confirm = vi.fn(() => true);
    globalThis.confirm = confirm;
    await mount('/t/' + ROOT, { thread: () => thread(), pin: () => ({ ok: true, count: 3 }), delete: () => ({ ok: true }) });
    const acts = $('#e' + ROOT + ' > .acts');
    await click(button('cite', acts));
    await click(button('cite', acts));
    expect(cites.value).toEqual([ROOT]);
    await click(button('pin', acts));
    expect(sent('pin')).toEqual([{ id: ROOT, thread: true }]);
    expect(toast.value).toBe('Pinned 3');
    // only our own events offer delete
    expect(button('delete', $('#e' + R1 + ' > .acts'))).toBeUndefined();
    await click(button('delete', acts));
    expect(confirm).toHaveBeenCalled();
    expect(sent('delete')).toEqual([{ id: ROOT, thread: false }]);
    confirm.mockReturnValue(false);
    await click(button('delete', acts));
    expect(sent('delete').length).toBe(1);
    delete globalThis.confirm;
  });
});

describe('other views', () => {
  it('reputation table', async () => {
    await mount('/c/math/reputation', { reputation: () => ({ entries: [{ pubkey: 'p1', name: 'Good', rep: 7.5, raters: 4 }, { pubkey: 'p2', name: 'Troll', rep: -8, raters: 6 }] }) });
    expect(sent('reputation')[0]).toEqual({ community: 'math', pubkey: '' });
    const rows = $$('table.rep tbody tr').map((r) => [...r.children].map((c) => c.textContent));
    expect(rows).toEqual([['Good', '7.5', '4'], ['Troll', '-8', '6']]);
    expect($$('td.neg').length).toBe(1);
    expect($('main a[href="/docs/reputation"]')).not.toBeNull();
  });

  it('reputation empty state', async () => {
    await mount('/c/math/reputation', { reputation: () => ({ entries: [] }) });
    expect(root.textContent).toContain('No ratings yet.');
  });

  it('search results', async () => {
    await mount('/search?q=crispr', { search: () => ({ items: [item({ community: 'bio', title: 'CRISPR' })] }) });
    expect($('main h2').textContent).toBe('“crispr”');
    expect($('article a[href="/c/bio"]')).not.toBeNull();
  });

  it('own profile can be edited and shows the node', async () => {
    await mount('/me', {
      profile: () => ({ pubkey: ME, npub: 'npub1me', profile: { name: 'Me', about: 'bio', agent: false }, reputation: [{ community: 'math', rep: 2 }], last_seen: Math.floor(Date.now() / 1e3) }),
      peers: () => ({ peers: [{ addr: 'ws://peer:7447', outbound: true, since: 1 }] }),
      set_profile: () => ({ ok: true }),
    });
    expect(sent('profile')[0]).toEqual({ pubkey: '' });
    await settle(); // NodePanel loads peers after the profile arrives
    expect(root.textContent).toContain('last seen just now');
    expect(root.textContent).toContain('npub1me');
    expect($('table.rep a[href="/c/math"]')).not.toBeNull();
    expect(root.textContent).toContain('ws://peer:7447');
    expect(root.textContent).toContain('relaying everything');
    await type(byPlaceholder('display name'), 'New name');
    const cb = $('input[type=checkbox]');
    cb.checked = true;
    await act(() => cb.dispatchEvent(new Event('change', { bubbles: true })));
    await click(button('Publish'));
    expect(sent('set_profile')).toEqual([{ name: 'New name', about: 'bio', agent: true }]);
    expect(toast.value).toBe('Profile published');
  });

  it("someone else's profile is read-only", async () => {
    await mount('/u/' + 'b'.repeat(64), { profile: () => ({ pubkey: 'b'.repeat(64), npub: 'npub1them', profile: { name: 'Bot', agent: true }, reputation: [], last_seen: 0 }) });
    expect(sent('profile')[0]).toEqual({ pubkey: 'b'.repeat(64) });
    expect(root.textContent).toContain('agent');
    expect(root.textContent).toContain('not seen recently');
    expect(button('Publish')).toBeUndefined();
    expect(root.textContent).not.toContain('This node');
  });
});
