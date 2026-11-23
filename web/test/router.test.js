import { beforeEach, describe, expect, it, vi } from 'vitest';
import { loc, match, navigate, query } from '../src/lib/router.js';

beforeEach(() => navigate('/', true));

describe('match', () => {
  it.each([
    ['/', '/', {}],
    ['/c/:slug', '/c/math', { slug: 'math' }],
    ['/c/:slug', '/c/math?x=1', { slug: 'math' }],
    ['/c/:slug/reputation', '/c/a%20b/reputation', { slug: 'a b' }],
    ['/t/:id', '/t/abc', { id: 'abc' }],
    ['/c/:slug', '/c/math/reputation', null],
    ['/c/:slug', '/t/math', null],
    ['/me', '/', null],
    ['/search', '/search?q=x', {}],
  ])('%s vs %s', (pat, path, want) => expect(match(pat, path)).toEqual(want));
});

describe('navigate', () => {
  it('pushes history and updates the signal', () => {
    const push = vi.spyOn(history, 'pushState');
    navigate('/c/math');
    expect(loc.value).toBe('/c/math');
    expect(location.pathname).toBe('/c/math');
    expect(push).toHaveBeenCalledTimes(1);
  });

  it('can replace instead of push, and ignores no-op navigations', () => {
    navigate('/x');
    const push = vi.spyOn(history, 'pushState');
    const repl = vi.spyOn(history, 'replaceState');
    navigate('/y', true);
    navigate('/y');
    expect(repl).toHaveBeenCalledTimes(1);
    expect(push).not.toHaveBeenCalled();
    expect(loc.value).toBe('/y');
  });

  it('follows the back button', () => {
    history.pushState(null, '', '/t/1?x=2');
    dispatchEvent(new PopStateEvent('popstate'));
    expect(loc.value).toBe('/t/1?x=2');
  });
});

describe('link interception', () => {
  const click = (attrs, init = {}) => {
    const a = document.createElement('a');
    for (const [k, v] of Object.entries(attrs)) a.setAttribute(k, v);
    const span = document.createElement('span');
    a.appendChild(span);
    document.body.appendChild(a);
    const ev = new MouseEvent('click', { bubbles: true, cancelable: true, ...init });
    span.dispatchEvent(ev);
    a.remove();
    return ev;
  };

  it('routes same-origin links client-side', () => {
    const ev = click({ href: '/c/bio' });
    expect(ev.defaultPrevented).toBe(true);
    expect(loc.value).toBe('/c/bio');
  });

  it.each([
    [{ href: 'https://example.org/' }, {}],
    [{ href: '/c/x', target: '_blank' }, {}],
    [{ href: '/c/x' }, { ctrlKey: true }],
    [{ href: '/c/x' }, { metaKey: true }],
    [{ href: '/c/x' }, { shiftKey: true }],
    [{ href: '/c/x' }, { button: 1 }],
    [{ href: '/docs' }, {}],
    [{ href: '/docs/architecture#the-network' }, {}],
  ])('leaves %o %o to the browser', (attrs, init) => {
    const ev = click(attrs, init);
    expect(ev.defaultPrevented).toBe(false);
    expect(loc.value).toBe('/');
  });
});

describe('query', () => {
  it('reads the current query string', () => {
    navigate('/search?q=gene%20drive&x=1');
    expect(query('q')).toBe('gene drive');
    expect(query('missing')).toBe('');
    navigate('/search');
    expect(query('q')).toBe('');
  });
});
