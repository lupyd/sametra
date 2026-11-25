import { render } from 'preact';
import { act } from 'preact/test-utils';
import { afterEach, describe, expect, it, vi } from 'vitest';
import App from '../src/components/App.jsx';
import { loc, navigate } from '../src/lib/router.js';
import { cites, info, toast } from '../src/lib/state.js';
import { fakeNode, item } from './fake-node.js';

const ME = 'e'.repeat(64);
const ROOT = 'a'.repeat(64);
let root, calls;

const base = {
  info: () => ({ pubkey: ME, npub: 'npub1mememememe', name: 'Me', events_cached: 7, pinned: 1, peers: 1, following: [], relay_all: true }),
  feed: () => ({ items: [item({ reply_count: 2 })] }),
};

// two rounds, so loads started by components that a first load rendered also finish
const settle = async () => { for (let k = 0; k < 2; k++) await act(async () => { for (let i = 0; i < 8; i++) await new Promise((r) => setTimeout(r, 0)); }); };

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
  it('shows identity, followed topics and node stats', async () => {
    const F = 'f'.repeat(64);
    await mount('/', { info: () => ({ ...base.info(), following: [F] }), event: () => ({ item: item({ id: F, title: 'A followed topic' }) }) });
    expect($('header .me').textContent).toBe('Me');
    const links = $$('aside nav a');
    expect(links.map((a) => a.getAttribute('href'))).toEqual(['/', '/t/' + F]);
    expect(links[0].className).toContain('on');
    expect(links[1].textContent).toBe('A followed topic');
    expect($('aside').textContent).toContain('Connected to 1 peer. 7 events kept on this node.');
    expect($('header button.theme')).not.toBeNull();
    expect($$('header nav.top a').map((a) => a.getAttribute('href'))).toEqual(['/help', '/inbox', '/docs']);
  });

  it('home lists the top-level topics', async () => {
    await mount('/');
    expect(sent('feed')[0]).toEqual({ parent: '', sort: 'top', limit: 100 });
    expect($('main h2').textContent).toBe('Topics');
  });

  it('home welcomes a fresh node', async () => {
    await mount('/', { feed: () => ({ items: [] }) });
    expect(root.textContent).toContain('Welcome to Sametra');
    expect(root.textContent).toContain('Follow a topic to keep it');
    expect($('main a[href="/docs"]').textContent).toBe('Read how it works');
  });

  it('unknown routes say so', async () => {
    await mount('/x/y/z');
    expect($('main').textContent).toContain('There is nothing at this address');
  });

  it('header search navigates', async () => {
    await mount('/');
    const input = $('header form input');
    input.value = ' gene drive ';
    await act(() => $('header form').dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })));
    await settle();
    expect(loc.value).toBe('/search?q=gene%20drive');
    expect(sent('search').at(-1)).toMatchObject({ query: 'gene drive', limit: 50 });
  });

  it('node errors show as a dismissable toast', async () => {
    await mount('/', { info: () => new Error('node down') });
    const t = $('.toast');
    expect(t.textContent).toBe('node down');
    await click(t);
    expect($('.toast')).toBeNull();
  });

});

describe('feed', () => {
  it('lists threads with metadata and sorts on request', async () => {
    await mount('/', {
      feed: (r) => ({ items: r.sort === 'new' ? [item({ title: 'Newest' })] : [item({ author_is_agent: true, author_rep: 0.45, ratings: 2, score: -0.4, refs: ['c'.repeat(64)], reply_count: 2 })] }),
    });
    expect(sent('feed')[0]).toEqual({ parent: '', sort: 'top', limit: 100 });
    const card = $('article.card');
    expect(card.querySelector('a.title').getAttribute('href')).toBe('/t/' + ROOT);
    expect(card.textContent).toContain('2 posts under it');
    const meters = [...card.querySelectorAll('.meter')].map((m) => m.textContent);
    expect(meters).toEqual(['standing+0.5', 'accuracy-0.4']);
    expect(card.querySelector('.meter.neg').textContent).toBe('accuracy-0.4');
    expect(card.querySelector('.kind').textContent).toBe('AI agent');
    expect(card.style.getPropertyValue('--strand')).toMatch(/^var\(--s[1-6]\)$/);
    expect(card.querySelector('.meta a[href^="/t/cccc"]').textContent).toBe('cccccccc');
    await click(button('new'));
    expect(sent('feed').at(-1).sort).toBe('new');
    expect($('a.title').textContent).toBe('Newest');
  });

  it('de-emphasises low-trust content without hiding it', async () => {
    await mount('/', { feed: () => ({ items: [item({ low_trust: true, author_rep: -0.6 })] }) });
    expect($('article').className).toContain('low');
    expect($('article').textContent).toContain('Body text');
    expect($('article .meta').textContent).toContain('low trust');
    expect($('article .meter.neg').textContent).toBe('standing-0.6');
  });

  it('composer posts a thread with pending citations and opens it', async () => {
    await mount('/', { post: () => ({ ok: true, id: 'f'.repeat(64) }), thread: () => ({ root: item({ id: 'f'.repeat(64) }) }) });
    cites.value = ['c'.repeat(64)];
    expect(byPlaceholder('Details')).toBeUndefined();
    await type(byPlaceholder('Title'), 'Is P = NP?');
    expect(button('Post').disabled).toBe(true);
    await type(byPlaceholder('Details'), 'Asking for a friend');
    expect(root.textContent).toContain('builds on cccccccc');
    await click(button('Post'));
    expect(sent('post')).toEqual([{ title: 'Is P = NP?', content: 'Asking for a friend', parent: '', refs: ['c'.repeat(64)], needs: [], format: '', accept: [] }]);
    expect(loc.value).toBe('/t/' + 'f'.repeat(64));
    expect(cites.value).toEqual([]);
  });

  it('attaching a file stores it on IPFS and links it in the post', async () => {
    const CID = 'bafkreia3fjxcmwxpk6mpgisbsdfzsxiynumsnxtzd33jq5f6mxxrqudupe';
    await mount('/', { ipfs_add: (r) => ({ cid: CID, url: 'ipfs://' + CID, link: `![${r.name}](ipfs://${CID})`, size: r.data.length }) });
    await type(byPlaceholder('Title'), 'Figure');
    await type(byPlaceholder('Details'), 'Here it is');
    const input = $('label.attach input[type=file]');
    const file = new File([new Uint8Array([137, 80, 78, 71])], 'fig.png', { type: 'image/png' });
    Object.defineProperty(input, 'files', { value: [file] });
    await act(() => input.dispatchEvent(new Event('change', { bubbles: true })));
    await settle();
    const [req] = sent('ipfs_add');
    expect(req.name).toBe('fig.png');
    expect([...req.data]).toEqual([137, 80, 78, 71]);
    expect(byPlaceholder('Details').value).toBe(`Here it is\n![fig.png](ipfs://${CID})\n`);
  });

  it('composer surfaces node errors', async () => {
    await mount('/', { post: () => new Error('missing \'content\'') });
    await type(byPlaceholder('Title'), 't');
    await type(byPlaceholder('Details'), 'b');
    await click(button('Post'));
    expect(toast.value).toBe("missing 'content'");
    expect(loc.value).toBe('/');
  });
});

describe('thread', () => {
  const R1 = '1'.repeat(64), R2 = '2'.repeat(64);
  const thread = (o = {}) => ({
    root: item({ mine: true, reply_count: 2, replies: [item({ id: R1, title: '', content: 'first', parent: ROOT, root: ROOT, depth: 1, replies: [item({ id: R2, title: '', content: 'nested', parent: R1, root: ROOT, depth: 2 })] })] }),
    quoted_by: [item({ id: 'q'.repeat(64), title: 'A citing thread' })],
    ref_events: [item({ id: 'r'.repeat(64), title: 'A cited source' })],
    path: [],
    ...o,
  });

  it('renders the nested tree and citations both ways', async () => {
    await mount('/t/' + ROOT, { thread: () => thread() });
    expect(sent('thread')[0]).toEqual({ id: ROOT, thread: false });
    expect($('h2.title').textContent).toBe('A title');
    expect($('#e' + R1 + ' > .body').textContent.trim()).toBe('first');
    expect($('#e' + R1 + ' .replies #e' + R2 + ' > .body').textContent.trim()).toBe('nested');
    expect($('a.back').getAttribute('href')).toBe('/');
    const sections = $$('h3.section').map((h) => h.textContent);
    expect(sections).toEqual(['Builds on', 'Built upon by']);
    expect($('a[href="/chain/' + ROOT + '"]')).not.toBeNull();
    expect(root.textContent).toContain('A cited source');
    expect(root.textContent).toContain('A citing thread');
    expect($$('a.btn').find((a) => a.textContent === 'Search here').getAttribute('href')).toBe('/search?w=' + ROOT);
    // every post under it opens as a topic of its own
    expect($('#e' + R1 + ' > .acts a[href="/t/' + R1 + '"]').textContent).toBe('open');
    expect($('#e' + ROOT + ' > .acts a[href="/t/' + ROOT + '"]')).toBeNull();
  });

  it('shows where a post sits', async () => {
    const P = 'p'.repeat(64);
    await mount('/t/' + R1, { thread: () => thread({ path: [item({ id: P, title: 'Classify all X' })] }) });
    expect($$('.crumbs a').map((a) => a.textContent)).toEqual(['Topics', 'Classify all X']);
    expect($$('.crumbs a')[1].getAttribute('href')).toBe('/t/' + P);
  });

  it('follows and unfollows the post it shows', async () => {
    let following = [];
    await mount('/t/' + ROOT, {
      thread: () => thread(),
      info: () => ({ ...base.info(), following }),
      follow: (r) => { following = [r.id]; return { ok: true }; },
      unfollow: () => { following = []; return { ok: true }; },
      event: () => ({ item: item({}) }),
    });
    await click(button('Follow'));
    expect(sent('follow')).toEqual([{ id: ROOT }]);
    expect(toast.value).toContain('keeps it and everything under it');
    expect(button('Following')).toBeDefined();
    await click(button('Following'));
    expect(sent('unfollow')).toEqual([{ id: ROOT }]);
  });

  it('shows who stands highest near it', async () => {
    await mount('/t/' + ROOT, { thread: () => thread(), reputation: () => ({ entries: [{ pubkey: 'p1', name: 'Good', rep: 0.75, raters: 4 }, { pubkey: 'p2', name: 'Troll', rep: -0.8, raters: 1 }] }) });
    expect(sent('reputation')[0]).toEqual({ near: ROOT, pubkey: '' });
    expect($$('h3.section').map((h) => h.textContent)).toContain('Standing here');
    const rows = $$('table.rep tbody tr').map((r) => [...r.children].map((c) => c.textContent));
    expect(rows).toEqual([['Good', '+0.8', '4 raters'], ['Troll', '-0.8', '1 rater']]);
    expect($('main a[href="/docs/reputation"]')).not.toBeNull();
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
    expect(sent('post')).toEqual([{ title: '', content: 'my answer', parent: ROOT, refs: [], needs: [], format: '', accept: [] }]);
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
    await type(byPlaceholder('Why'), 'not replicated');
    await click(button('Rate'));
    expect(sent('rate')).toEqual([{ score: -0.4, event: R1, target: '', reason: 'not replicated', aspect: 'trust', capability: '' }]);
    expect(toast.value).toBe("Rated. The author's standing is now -0.1");
  });

  it('cites, pins and deletes', async () => {
    const confirm = vi.fn(() => true);
    globalThis.confirm = confirm;
    await mount('/t/' + ROOT, { thread: () => thread(), pin: () => ({ ok: true, count: 3 }), delete: () => ({ ok: true }) });
    const acts = $('#e' + ROOT + ' > .acts');
    await click(button('build on', acts));
    await click(button('build on', acts));
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
  it('search results', async () => {
    await mount('/search?q=crispr', { search: () => ({ items: [item({ title: 'CRISPR' })] }) });
    expect($('main h2').textContent).toBe('“crispr”');
    expect($('article .meta a[href="/t/' + ROOT + '"]').textContent).toBe('open');
  });

  it('own profile can be edited and shows the node', async () => {
    await mount('/me', {
      profile: () => ({ pubkey: ME, npub: 'npub1me', profile: { name: 'Me', about: 'bio', agent: false }, rep: 0.4, best: [item({ id: '5'.repeat(64), title: 'My best post', score: 0.7 })], last_seen: Math.floor(Date.now() / 1e3) }),
      peers: () => ({ peers: [{ addr: 'ws://peer:42424', outbound: true, since: 1 }] }),
      set_profile: () => ({ ok: true }),
    });
    expect(sent('profile')[0]).toEqual({ pubkey: '' });
    await settle(); // NodePanel loads peers after the profile arrives
    expect(root.textContent).toContain('Last seen just now');
    expect(root.textContent).toContain('npub1me');
    expect($$('h3.section').map((h) => h.textContent)).toContain('Where they stand highest');
    expect($('a[href="/t/' + '5'.repeat(64) + '"]').textContent).toBe('My best post');
    expect(root.textContent).toContain('ws://peer:42424');
    expect(root.textContent).toContain('Relaying everything');
    expect(root.textContent).toContain('Standing by the post tree');
    await type(byPlaceholder('display name'), 'New name');
    const cb = $('input[type=checkbox]');
    cb.checked = true;
    await act(() => cb.dispatchEvent(new Event('change', { bubbles: true })));
    await type(byPlaceholder('model'), 'claude-opus-5-5');
    await click(button('Publish'));
    expect(sent('set_profile')).toEqual([{ name: 'New name', about: 'bio', agent: true, model: 'claude-opus-5-5' }]);
    expect(toast.value).toBe('Profile published');
  });

  it("someone else's profile is read-only", async () => {
    await mount('/u/' + 'b'.repeat(64), { profile: () => ({ pubkey: 'b'.repeat(64), npub: 'npub1them', profile: { name: 'Bot', agent: true }, last_seen: 0 }) });
    expect(sent('profile')[0]).toEqual({ pubkey: 'b'.repeat(64) });
    expect(root.textContent).toContain('agent');
    expect(root.textContent).toContain('Not seen recently');
    expect(button('Publish')).toBeUndefined();
    expect(root.textContent).not.toContain('This node');
  });
});

describe('relevance, needs and search scopes', () => {
  it('rates relevance separately from accuracy', async () => {
    await mount('/t/' + ROOT, { thread: () => ({ root: item({ relevance: 0.25, relevance_ratings: 3, cited_by: 4 }), quoted_by: [], ref_events: [], path: [] }), rate: () => ({ ok: true, rep: 0.31 }) });
    const meta = $('.meta').textContent;
    expect(meta).toContain('relevance+0.3');
    expect(meta).toContain('built on 4×');
    await click(button('rate'));
    await click(button('relevance'));
    expect(byPlaceholder('Why').getAttribute('placeholder')).toContain('off-topic');
    await type($('input[type=range]'), '0.6');
    await click(button('Rate'));
    expect(sent('rate')[0]).toMatchObject({ event: ROOT, score: 0.6, aspect: 'relevance' });
    expect(toast.value).toBe("Rated. This message's relevance is now +0.3");
  });

  it('posts needs and suggests who can help', async () => {
    const helper = { provider: 'h'.repeat(64), provider_name: 'Lab agent', provider_is_agent: true, rep: 0.62, ratings: 3, requests: 4, completed: 3, declined: 0, spec: { id: 'assay', name: 'Plasma assay', kind: 'tool', about: '', input_proto: 'syntax = "proto3"; message Sample { string sample = 1; }', input_message: 'Sample', topics: [] } };
    await mount('/', {
      post: () => ({ ok: true, id: 'f'.repeat(64) }),
      thread: () => ({ root: item({ id: 'f'.repeat(64), needs: ['plasma assay'] }), quoted_by: [], ref_events: [], path: [] }),
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
    expect(card.querySelector('.meter').textContent).toBe('delivers+0.6');
    await click(button('request', card));
    expect($('pre.schema').textContent).toBe('syntax = "proto3"; message Sample { string sample = 1; }');
    await type(byPlaceholder('Sample as JSON'), '{"sample":"S1"}');
    await click(button('Send request', card));
    expect(sent('request')).toMatchObject([{ provider: 'h'.repeat(64), capability: 'assay', input_json: '{"sample":"S1"}', input_text: '', post: 'f'.repeat(64) }]);
  });

  it('search scopes, and search within a post', async () => {
    await mount('/search?q=marker', { search: () => ({ items: [item({ needs: ['assay'] })] }) });
    expect(sent('search').at(-1)).toMatchObject({ query: 'marker', scope: 'node', within: '' });
    await click(button('network'));
    expect(loc.value).toBe('/search?q=marker&scope=network');
    expect(sent('search').at(-1).scope).toBe('network');
    expect(root.textContent).toContain('best effort');
    render(null, root);
    await mount('/search?q=marker&w=' + ROOT, { search: () => ({ items: [] }), event: () => ({ item: item({ title: 'The topic' }) }) });
    expect(sent('search').at(-1)).toMatchObject({ query: 'marker', within: ROOT });
    expect($('.within').textContent).toContain('Only under The topic');
    await click(button('search everywhere'));
    expect(loc.value).toBe('/search?q=marker&scope=node');
  });
});

describe('capabilities', () => {
  const job = (o) => ({ id: 'j'.repeat(64), requester: 'r'.repeat(64), requester_name: 'Asker', provider: ME, provider_name: 'Me', capability: 'assay', input: '{"sample":"S1"}', post: '', created_at: 1, status: 'pending', status_text: '', result: '', result_id: '', updated_at: Math.floor(Date.now() / 1e3), mine: false, rated: false, ...o });

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
    await click(button('requests to me'));
    expect(sent('inbox')[0]).toMatchObject({ role: 'provider' });
    expect($('.job pre.input').textContent).toBe('{"sample":"S1"}');
    await click(button('Accept'));
    expect(sent('respond')[0]).toMatchObject({ request: 'j'.repeat(64), action: 'accept', content: '' });
    await type(byPlaceholder('Result'), '42 ng/ml');
    await click(button('Deliver result'));
    expect(sent('respond')[1]).toMatchObject({ request: 'j'.repeat(64), action: 'result', content: '42 ng/ml' });
    await click(button('my requests'));
    expect(sent('inbox').at(-1)).toMatchObject({ role: 'requester' });
    expect(root.textContent).toContain('not requested anything yet');
  });

  it('requesters rate delivered results', async () => {
    await mount('/inbox', { inbox: () => ({ jobs: [job({ mine: true, status: 'done', result: '42 ng/ml', result_id: 'x'.repeat(64) })] }), rate: () => ({ ok: true, rep: 0.74 }) });
    await click(button('my requests'));
    expect($('.result').textContent).toBe('42 ng/ml');
    expect(button('Accept')).toBeUndefined();
    await type($('.job input[type=range]'), '0.8');
    await click(button('Rate'));
    expect(sent('rate')[0]).toMatchObject({ event: 'x'.repeat(64), score: 0.8, aspect: 'capability' });
    expect(toast.value).toContain('assay now 0.74');
  });

  it('offers and withdraws capabilities from your profile', async () => {
    const mineCap = { provider: ME, provider_name: 'Me', provider_is_agent: false, rep: 0, ratings: 0, requests: 0, completed: 0, declined: 0, spec: { id: 'stats', name: 'Survival analysis', kind: 'skill', about: '', topics: [] } };
    await mount('/me', {
      profile: () => ({ pubkey: ME, npub: 'npub1me', profile: { name: 'Me' }, last_seen: 0 }),
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
    await type(byPlaceholder('input as .proto'), 'syntax = "proto3"; message Sample { string sample = 1; }');
    await type(byPlaceholder('input message'), 'Sample');
    await click(button('Offer capability'));
    expect(sent('set_capability')[1]).toMatchObject({ id: 'assay', name: 'Plasma assay', kind: 'tool', topics: ['proteomics', 'plasma'], input_proto: 'syntax = "proto3"; message Sample { string sample = 1; }', input_message: 'Sample' });
  });
});

describe('humanity', () => {
  const prof = (o) => ({ pubkey: 'b'.repeat(64), npub: 'npub1them', profile: { name: 'Sam', about: '', agent: false, model: '' }, last_seen: 0, humanity: 0, humanity_raters: 0, human_status: 'unverified', ...o });

  it('shows the claim and lets you vouch', async () => {
    await mount('/u/' + 'b'.repeat(64), { profile: () => prof({}), rate: () => ({ ok: true, rep: 0.12 }) });
    expect($('.human').textContent).toContain('nobody has vouched for it yet');
    await click(button('yes'));
    expect(sent('rate')).toEqual([{ score: 1, event: '', target: 'b'.repeat(64), reason: '', aspect: 'human', capability: '' }]);
    expect(toast.value).toContain('Humanity now +0.1');
    await click(button('no'));
    expect(sent('rate')[1].score).toBe(-1);
  });

  it('shows verified and disputed claims, and agents by model', async () => {
    await mount('/u/' + 'b'.repeat(64), { profile: () => prof({ human_status: 'verified', humanity: 0.64, humanity_raters: 6 }) });
    expect($('.human').textContent).toContain('Verified human');
    expect($('.human').textContent).toContain('+0.6 from 6 people');
    render(null, root);
    await mount('/u/' + 'b'.repeat(64), { profile: () => prof({ human_status: 'disputed', humanity: -0.5, humanity_raters: 2 }) });
    expect($('.human .tag.bad').textContent).toBe('disputed human');
    render(null, root);
    await mount('/u/' + 'b'.repeat(64), { profile: () => prof({ human_status: 'agent', profile: { name: 'Bot', agent: true, model: 'llama-4' } }) });
    expect($('main .bar .kind').textContent).toBe('llama-4');
    expect(button('yes')).toBeUndefined();
  });

  it('posts carry the author kind', async () => {
    await mount('/', { feed: () => ({ items: [item({ id: '1'.repeat(64), author_is_agent: true, author_model: 'claude-opus-5-5' }), item({ id: '2'.repeat(64), author_human: 'verified' })] }) });
    const kinds = $$('article .kind').map((t) => t.textContent);
    expect(kinds).toEqual(['claude-opus-5-5', 'verified human']);
    const strands = $$('article').map((a) => a.style.getPropertyValue('--strand'));
    expect(strands[1]).toBe('var(--human)');
    expect(strands[0]).not.toBe(strands[1]);
  });
});

describe('theme and chains', () => {
  it('one button switches light and dark, and remembers it', async () => {
    await mount('/');
    const before = document.documentElement.dataset.theme || 'light';
    await click($('header button.theme'));
    const after = document.documentElement.dataset.theme;
    expect(after).not.toBe(before);
    expect(localStorage.getItem('sametra-theme')).toBe(after);
    await click($('header button.theme'));
    expect(document.documentElement.dataset.theme).toBe(before === 'dark' ? 'dark' : 'light');
    localStorage.removeItem('sametra-theme');
    delete document.documentElement.dataset.theme;
  });

  it('shows a chain of reasoning on strands, oldest first', async () => {
    const Q = 'a'.repeat(64), S1 = '1'.repeat(64), S2 = '2'.repeat(64);
    const node = (it, level, relation, builds_on) => ({ item: item(it), level, relation, builds_on });
    await mount('/chain/' + S1, {
      chain: () => ({
        focus: S1,
        nodes: [
          node({ id: Q, title: 'Why does X happen?', content: 'question' }, -1, '', []),
          node({ id: S1, title: '', content: 'hypothesis A', author_is_agent: true, author_model: 'claude-opus-5-5', author: 'c'.repeat(64) }, 0, 'reply', [Q]),
          node({ id: S2, title: 'Testing A', content: 'an experiment', author_is_agent: true, author_model: 'llama-4', author: 'l'.repeat(64) }, 1, 'cites', [S1]),
        ],
        contributors: [
          { pubkey: 'b'.repeat(64), name: 'Alice', human_status: 'verified', model: '', steps: 1 },
          { pubkey: 'c'.repeat(64), name: 'Claude agent', human_status: 'agent', model: 'claude-opus-5-5', steps: 1 },
          { pubkey: 'l'.repeat(64), name: 'Llama agent', human_status: 'agent', model: 'llama-4', steps: 1 },
        ],
      }),
    });
    expect(sent('chain')[0]).toEqual({ id: S1, depth: 6 });
    expect($('main h2').textContent).toBe('Why does X happen?');
    expect($('.weavers').textContent).toContain('claude-opus-5-5, 1 step');
    expect($('.weavers').textContent).toContain('verified human, 1 step');
    const steps = $$('ol.chain > li');
    expect(steps.map((li) => li.id)).toEqual(['s' + Q, 's' + S1, 's' + S2]);
    expect(steps[1].className).toBe('focus');
    expect(steps[1].querySelector('a.title')).toBeNull();
    expect(steps[1].querySelector('.joins').textContent).toBe('answers Why does X happen?');
    expect(steps[2].querySelector('.joins a').getAttribute('href')).toBe('#s' + S1);
    expect(steps[2].querySelector('.joins').textContent).toContain('builds on');
    const strands = steps.map((li) => li.style.getPropertyValue('--strand'));
    expect(strands[0]).toBe('var(--human)');
    expect(new Set(strands).size).toBe(3);
  });

  it('a lone post explains how to start a chain', async () => {
    await mount('/chain/' + ROOT, { chain: () => ({ focus: ROOT, nodes: [{ item: item({}), level: 0, relation: '', builds_on: [] }], contributors: [{ pubkey: 'b'.repeat(64), name: 'Alice', human_status: 'unverified', model: '', steps: 1 }] }) });
    expect($('.empty').textContent).toContain('Nothing builds on this yet');
  });
});


describe('formats', () => {
  it('replies can use the format a post asks for; other formats show as source', async () => {
    const asked = item({ accept: ['text/x-lean'], content: 'Prove **this**' });
    const json = item({ id: '3'.repeat(64), title: '', format: 'application/json', content: '{"a":1}', parent: ROOT, depth: 1 });
    await mount('/t/' + ROOT, { thread: () => ({ root: { ...asked, replies: [json] }, quoted_by: [], ref_events: [], path: [] }), post: () => ({ ok: true, id: 'n'.repeat(64) }) });
    expect($('#e' + ROOT + ' > .body strong').textContent).toBe('this');
    expect($('#e' + ROOT).textContent).toContain('replies in text/x-lean');
    expect($('#e' + '3'.repeat(64) + ' pre.source').dataset.format).toBe('application/json');
    await click(button('reply', $('#e' + ROOT + ' > .acts')));
    expect(byPlaceholder('Reply in text/x-lean')).toBeDefined();
    await type(byPlaceholder('Reply in'), 'theorem t : True := trivial');
    await click(button('Reply'));
    expect(sent('post')[0]).toMatchObject({ parent: ROOT, format: 'text/x-lean' });
  });
});

describe('posts are topics', () => {
  const R = 'r'.repeat(64);

  it('a reply can carry a title and opens as a topic of its own', async () => {
    await mount('/t/' + ROOT, {
      thread: () => ({ root: item({ replies: [item({ id: R, title: 'Case: X finite', content: 'needs a lemma', root: ROOT, parent: ROOT, depth: 1 })] }), quoted_by: [], ref_events: [], path: [] }),
      post: () => ({ ok: true, id: 'n'.repeat(64) }),
    });
    expect($('#e' + R + ' > a.title').getAttribute('href')).toBe('/t/' + R);
    await click(button('reply', $('#e' + R + ' > .acts')));
    await type(byPlaceholder('Title, if this opens'), 'The lemma');
    await type(byPlaceholder('Reply'), 'prove it');
    await click(button('Reply'));
    expect(sent('post')[0]).toMatchObject({ parent: R, title: 'The lemma', content: 'prove it' });
  });
});

describe('people and mentions', () => {
  const argon = { pubkey: 'a'.repeat(64), npub: 'npub1argon', name: 'Doctor Argon', about: 'noble gases', agent: true, model: 'claude-opus-5-5', human_status: 'agent', humanity: 0, last_seen: 1, capabilities: 2, handle: 'DoctorArgon', text_match: 1 };

  it('the inbox opens on what concerns you, and marks it read', async () => {
    const reply = item({ id: '7'.repeat(64), title: '', content: 'an answer', parent: ROOT, root: ROOT, depth: 1, author_name: 'Doctor Argon' });
    await mount('/inbox', {
      info: () => ({ pubkey: ME, npub: 'npub1me', name: 'Me', unread: 2 }),
      notifications: () => ({ notifications: [{ reason: 'mention', item: item({ id: '8'.repeat(64) }), unread: true }, { reason: 'reply', item: reply, unread: false }], unread: 1 }),
    });
    expect($('nav.top .count').textContent).toBe('2');
    expect(sent('notifications')[0]).toEqual({ limit: 100, mark_read: true });
    const cards = $$('article.card');
    expect(cards[0].textContent).toContain('mentioned you');
    expect(cards[0].querySelector('.who.unread')).not.toBeNull();
    expect(cards[1].textContent).toContain('answered your post');
    expect(cards[1].querySelector('a.title').getAttribute('href')).toBe('/t/' + ROOT + '#e' + '7'.repeat(64));
    expect(sent('info').length).toBeGreaterThan(1);
  });

  it('search shows matching people', async () => {
    await mount('/search?q=argon', { search: () => ({ items: [] }), find_people: () => ({ people: [argon] }) });
    expect(sent('find_people')[0]).toEqual({ query: 'argon', limit: 5, kind: '' });
    expect($('.people').textContent).toContain('@DoctorArgon');
    expect($('.people').textContent).toContain('offers 2 capabilities');
    expect($('.people a').getAttribute('href')).toBe('/u/' + 'a'.repeat(64));
  });

  it('typing @ suggests people and completes the handle', async () => {
    await mount('/', { find_people: () => ({ people: [argon] }), post: () => ({ ok: true, id: 'f'.repeat(64), mentioned: ['npub1argon'] }), thread: () => ({ root: item({ id: 'f'.repeat(64) }), quoted_by: [], ref_events: [], path: [] }) });
    await type(byPlaceholder('Title'), 'Odd line');
    await type(byPlaceholder('Details'), 'Could @doc');
    await new Promise((r) => setTimeout(r, 200));
    await settle();
    expect(sent('find_people').at(-1)).toMatchObject({ query: 'doc' });
    await click(button('Doctor Argon @DoctorArgon, claude-opus-5-5'));
    expect(byPlaceholder('Details').value).toBe('Could @DoctorArgon ');
    await click(button('Post'));
    expect(sent('post')[0].content).toBe('Could @DoctorArgon ');
  });
});

describe('embeddings', () => {
  it('the node panel says how standing is measured', async () => {
    await mount('/me', {
      info: () => ({ ...base.info(), embed_model: 'nomic-embed-text', vectors: 120, embed_pending: 3, embed_error: 'HTTP 503' }),
      profile: () => ({ pubkey: ME, npub: 'npub1me', profile: { name: 'Me' }, last_seen: 0 }),
      peers: () => ({ peers: [] }),
    });
    await settle();
    const t = $$('.panel').find((p) => p.textContent.includes('This node')).textContent;
    expect(t).toContain('Standing by meaning: 120 posts embedded with nomic-embed-text, 3 waiting.');
    expect(t).toContain('the post tree stands in');
  });
});
