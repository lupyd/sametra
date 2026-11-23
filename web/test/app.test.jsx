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
    expect($$('header nav.top a').map((a) => a.getAttribute('href'))).toEqual(['/help', '/inbox', '/docs']);
    expect($('aside a[href="/communities"]')).not.toBeNull();
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
      feed: (r) => ({ items: r.sort === 'new' ? [item({ title: 'Newest' })] : [item({ author_is_agent: true, author_rep: 0.45, ratings: 2, score: -0.4, refs: ['c'.repeat(64)], reply_count: 2 })] }),
    });
    expect(sent('feed')[0]).toEqual({ community: 'math', sort: 'top', limit: 100 });
    expect($('main h2').textContent).toBe('Mathematics');
    expect($('main .about').textContent).toBe('proofs');
    const card = $('article.card');
    expect(card.querySelector('a.title').getAttribute('href')).toBe('/t/' + ROOT);
    expect(card.textContent).toContain('2 replies');
    const tags = [...card.querySelectorAll('.tag')].map((t) => t.textContent);
    expect(tags).toEqual(expect.arrayContaining(['rep 0.45', 'score -0.4', 'agent']));
    expect(card.querySelector('.meta a[href^="/t/cccc"]').textContent).toBe('cccccccc');
    const sel = $('select');
    sel.value = 'new';
    await act(() => sel.dispatchEvent(new Event('change', { bubbles: true })));
    await settle();
    expect(sent('feed').at(-1).sort).toBe('new');
    expect($('a.title').textContent).toBe('Newest');
  });

  it('de-emphasises low-trust content without hiding it', async () => {
    await mount('/c/math', { feed: () => ({ items: [item({ low_trust: true, author_rep: -0.6 })] }) });
    expect($('article').className).toContain('low');
    expect($('article').textContent).toContain('Body text');
    expect($$('.tag.bad').map((t) => t.textContent)).toEqual(['rep -0.6', 'low trust']);
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
    expect(sent('post')).toEqual([{ community: 'math', title: 'Is P = NP?', content: 'Asking for a friend', parent: '', refs: ['c'.repeat(64)], needs: [] }]);
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
    expect(sent('post')).toEqual([{ community: '', title: '', content: 'my answer', parent: ROOT, refs: [], needs: [] }]);
    expect(toast.value).toBe('Replied');
    expect(byPlaceholder('Reply')).toBeUndefined();
  });

  it('rates with the slider', async () => {
    await mount('/t/' + ROOT, { thread: () => thread(), rate: () => ({ ok: true, rep: -0.12 }) });
    await click(button('rate', $('#e' + R1 + ' > .acts')));
    const slider = $('input[type=range]');
    expect([slider.min, slider.max, slider.step]).toEqual(['-1', '1', '0.1']);
    await type(slider, '-0.4');
    expect($('.box b').textContent).toBe('-0.4');
    await type(byPlaceholder('reason'), 'not replicated');
    await click(button('Rate'));
    expect(sent('rate')).toEqual([{ score: -0.4, event: R1, target: '', community: '', reason: 'not replicated', aspect: 'trust', capability: '' }]);
    expect(toast.value).toContain('-0.12');
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
    await mount('/c/math/reputation', { reputation: () => ({ entries: [{ pubkey: 'p1', name: 'Good', rep: 0.75, raters: 4 }, { pubkey: 'p2', name: 'Troll', rep: -0.8, raters: 6 }] }) });
    expect(sent('reputation')[0]).toEqual({ community: 'math', pubkey: '' });
    const rows = $$('table.rep tbody tr').map((r) => [...r.children].map((c) => c.textContent));
    expect(rows).toEqual([['Good', '0.75', '4'], ['Troll', '-0.8', '6']]);
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

describe('relevance, needs and search scopes', () => {
  it('rates relevance separately from accuracy', async () => {
    await mount('/t/' + ROOT, { thread: () => ({ root: item({ relevance: 0.25, relevance_ratings: 3, cited_by: 4 }), quoted_by: [], ref_events: [], thread_root: '' }), rate: () => ({ ok: true, rep: 0.31 }) });
    const tags = $$('.tag').map((t) => t.textContent);
    expect(tags).toEqual(expect.arrayContaining(['relevance 0.25', 'cited 4']));
    await click(button('rate'));
    await click(button('relevance'));
    expect(byPlaceholder('reason').getAttribute('placeholder')).toContain('off-topic');
    await type($('input[type=range]'), '0.6');
    await click(button('Rate'));
    expect(sent('rate')[0]).toMatchObject({ event: ROOT, score: 0.6, aspect: 'relevance' });
    expect(toast.value).toContain('This message now 0.31');
  });

  it('posts needs and suggests who can help', async () => {
    const helper = { provider: 'h'.repeat(64), provider_name: 'Lab agent', provider_is_agent: true, rep: 0.62, ratings: 3, requests: 4, completed: 3, declined: 0, spec: { id: 'assay', name: 'Plasma assay', kind: 'tool', about: '', input_schema: '{"type":"object"}', topics: [], communities: [] } };
    await mount('/c/math', {
      post: () => ({ ok: true, id: 'f'.repeat(64) }),
      thread: () => ({ root: item({ id: 'f'.repeat(64), needs: ['plasma assay'] }), quoted_by: [], ref_events: [], thread_root: '' }),
      capabilities: () => ({ capabilities: [helper] }),
      request: () => ({ ok: true, id: 'r'.repeat(64) }),
    });
    await type(byPlaceholder('Title'), 'Need marker X');
    await type(byPlaceholder('Details'), 'two samples');
    await type(byPlaceholder('Needs'), 'plasma assay, statistician ,');
    await click(button('Post'));
    expect(sent('post')[0].needs).toEqual(['plasma assay', 'statistician']);
    expect(loc.value).toBe('/t/' + 'f'.repeat(64));
    await settle(); // thread, then its "who can help" suggestions
    expect($('a.tag.need').getAttribute('href')).toBe('/help?q=plasma%20assay');
    await settle();
    expect($$('h3.section').map((h) => h.textContent)).toContain('Who can help');
    expect(sent('capabilities').at(-1)).toMatchObject({ query: 'plasma assay', limit: 5 });
    const card = $('.card.cap');
    expect(card.textContent).toContain('Plasma assay');
    expect(card.textContent).toContain('delivers 0.62');
    await click(button('request', card));
    expect($('pre.schema').textContent).toBe('{"type":"object"}');
    await type(byPlaceholder('JSON input'), '{"sample":"S1"}');
    await click(button('Send request', card));
    expect(sent('request')).toEqual([{ provider: 'h'.repeat(64), capability: 'assay', input: '{"sample":"S1"}', post: 'f'.repeat(64), community: '' }]);
  });

  it('search scopes and community filter', async () => {
    await mount('/search?q=marker', { search: () => ({ items: [item({ needs: ['assay'] })] }) });
    expect(sent('search').at(-1)).toMatchObject({ query: 'marker', scope: 'node', community: '' });
    expect(button('community').disabled).toBe(true);
    await click(button('network'));
    expect(loc.value).toBe('/search?q=marker&scope=network');
    expect(sent('search').at(-1).scope).toBe('network');
    expect(root.textContent).toContain('best effort');
    const sel = $('select[aria-label=community]');
    sel.value = 'math';
    await act(() => sel.dispatchEvent(new Event('change', { bubbles: true })));
    await settle();
    expect(sent('search').at(-1)).toMatchObject({ scope: 'network', community: 'math' });
    await click(button('community'));
    expect(sent('search').at(-1)).toMatchObject({ scope: 'community', community: 'math' });
  });

  it('finds and joins communities', async () => {
    await mount('/communities?q=cancer', {
      find_communities: (r) => ({ communities: r.query === 'cancer' ? [{ slug: 'onco', name: 'Oncology', about: 'tumours', members: 12, events: 40, joined: false }] : [] }),
      join: () => ({ ok: true, community: 'onco' }),
    });
    expect(sent('find_communities')[0]).toMatchObject({ query: 'cancer', network: false });
    expect(root.textContent).toContain('12 members');
    await click(button('join'));
    expect(sent('join')).toEqual([{ community: 'onco' }]);
    expect(loc.value).toBe('/c/onco');
  });
});

describe('capabilities', () => {
  const job = (o) => ({ id: 'j'.repeat(64), requester: 'r'.repeat(64), requester_name: 'Asker', provider: ME, provider_name: 'Me', capability: 'assay', input: '{"sample":"S1"}', post: '', community: 'onco', created_at: 1, status: 'pending', status_text: '', result: '', result_id: '', updated_at: Math.floor(Date.now() / 1e3), mine: false, rated: false, ...o });

  it('find help searches capabilities', async () => {
    await mount('/help?q=histology', { capabilities: () => ({ capabilities: [] }) });
    expect(sent('capabilities')[0]).toMatchObject({ query: 'histology', network: false });
    expect(root.textContent).toContain('Nobody offers that yet');
    const cb = $('input[type=checkbox]');
    cb.checked = true;
    await act(() => cb.dispatchEvent(new Event('change', { bubbles: true })));
    await settle();
    expect(sent('capabilities').at(-1).network).toBe(true);
  });

  it('providers accept and deliver from the inbox', async () => {
    let state = job({});
    await mount('/inbox', {
      inbox: (r) => ({ jobs: r.role === 'provider' ? [state] : [] }),
      respond: (r) => { state = job({ status: r.action === 'accept' ? 'accepted' : 'done', result: r.content }); return { ok: true }; },
    });
    expect(sent('inbox')[0]).toMatchObject({ role: 'provider' });
    expect($('.job pre.input').textContent).toBe('{"sample":"S1"}');
    await click(button('Accept'));
    expect(sent('respond')[0]).toEqual({ request: 'j'.repeat(64), action: 'accept', content: '' });
    await type(byPlaceholder('Result'), '42 ng/ml');
    await click(button('Deliver result'));
    expect(sent('respond')[1]).toEqual({ request: 'j'.repeat(64), action: 'result', content: '42 ng/ml' });
    await click(button('my requests'));
    expect(sent('inbox').at(-1)).toMatchObject({ role: 'requester' });
    expect(root.textContent).toContain('not requested anything yet');
  });

  it('requesters rate delivered results', async () => {
    await mount('/inbox', { inbox: () => ({ jobs: [job({ mine: true, status: 'done', result: '42 ng/ml', result_id: 'x'.repeat(64) })] }), rate: () => ({ ok: true, rep: 0.74 }) });
    expect($('.result').textContent).toBe('42 ng/ml');
    expect(button('Accept')).toBeUndefined();
    await type($('.job input[type=range]'), '0.8');
    await click(button('Rate'));
    expect(sent('rate')[0]).toMatchObject({ event: 'x'.repeat(64), score: 0.8, aspect: 'capability' });
    expect(toast.value).toContain('assay now 0.74');
  });

  it('offers and withdraws capabilities from your profile', async () => {
    const mineCap = { provider: ME, provider_name: 'Me', provider_is_agent: false, rep: 0, ratings: 0, requests: 0, completed: 0, declined: 0, spec: { id: 'stats', name: 'Survival analysis', kind: 'skill', about: '', input_schema: '', topics: [], communities: [] } };
    await mount('/me', {
      profile: () => ({ pubkey: ME, npub: 'npub1me', profile: { name: 'Me' }, reputation: [], last_seen: 0 }),
      capabilities: (r) => ({ capabilities: r.provider === ME ? [mineCap] : [] }),
      set_capability: () => ({ ok: true }),
    });
    await settle(); // capabilities load after the profile
    expect(sent('capabilities')[0]).toMatchObject({ provider: ME });
    expect(root.textContent).toContain('Survival analysis');
    await click(button('withdraw stats'));
    expect(sent('set_capability')[0]).toMatchObject({ id: 'stats', delete: true });
    const kind = $('select[aria-label=kind]');
    kind.value = 'tool';
    await act(() => kind.dispatchEvent(new Event('change', { bubbles: true })));
    await settle();
    await type(byPlaceholder('id, e.g.'), 'assay');
    await type(byPlaceholder('name'), 'Plasma assay');
    await type(byPlaceholder('topics'), 'proteomics, plasma');
    await type(byPlaceholder('input JSON Schema'), '{"type":"object"}');
    await click(button('Offer capability'));
    expect(sent('set_capability')[1]).toMatchObject({ id: 'assay', name: 'Plasma assay', kind: 'tool', topics: ['proteomics', 'plasma'], communities: [], input_schema: '{"type":"object"}' });
  });
});
