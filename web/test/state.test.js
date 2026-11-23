import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fakeNode } from './fake-node.js';

// state.js starts a background interval on import, so load it under fake timers.
let S;
beforeEach(async () => {
  vi.useFakeTimers();
  vi.resetModules();
  S = await import('../src/lib/state.js');
});
afterEach(() => {
  vi.useRealTimers();
  document.body.innerHTML = '';
});

describe('formatting', () => {
  it('ago', () => {
    const now = Date.now() / 1e3;
    expect(S.ago(now)).toBe('now');
    expect(S.ago(now - 59)).toBe('now');
    expect(S.ago(now - 60)).toBe('1m');
    expect(S.ago(now - 3599)).toBe('59m');
    expect(S.ago(now - 3600)).toBe('1h');
    expect(S.ago(now - 86400 * 3)).toBe('3d');
  });
  it('short', () => expect(S.short('abcdef0123456789')).toBe('abcdef01'));
});

describe('flash', () => {
  it('clears itself after 3.5 s', () => {
    S.flash('hi');
    expect(S.toast.value).toBe('hi');
    vi.advanceTimersByTime(3499);
    expect(S.toast.value).toBe('hi');
    vi.advanceTimersByTime(1);
    expect(S.toast.value).toBe('');
  });
  it('does not clear a newer message', () => {
    S.flash('first');
    vi.advanceTimersByTime(2000);
    S.flash('second');
    vi.advanceTimersByTime(1600);
    expect(S.toast.value).toBe('second');
  });
});

describe('refreshNode', () => {
  it('loads info and communities', async () => {
    fakeNode({ info: () => ({ name: 'me', peers: 2 }), communities: () => ({ communities: [{ slug: 'm', joined: true }] }) });
    await S.refreshNode();
    expect(S.info.value.name).toBe('me');
    expect(S.communities.value.map((c) => c.slug)).toEqual(['m']);
  });
  it('reports failures as a toast', async () => {
    fakeNode({ info: () => new Error('node down') });
    await S.refreshNode();
    expect(S.toast.value).toBe('node down');
  });
});

describe('background refresh', () => {
  it('ticks every 6 s', () => {
    fakeNode({});
    expect(S.tick.value).toBe(0);
    vi.advanceTimersByTime(6000);
    expect(S.tick.value).toBe(1);
    vi.advanceTimersByTime(12000);
    expect(S.tick.value).toBe(3);
  });
  it('pauses while the user is typing or has a box open', () => {
    fakeNode({});
    const input = document.createElement('input');
    document.body.appendChild(input);
    input.focus();
    vi.advanceTimersByTime(6000);
    expect(S.tick.value).toBe(0);
    input.blur();
    const box = document.createElement('div');
    box.className = 'box';
    document.body.appendChild(box);
    vi.advanceTimersByTime(6000);
    expect(S.tick.value).toBe(0);
    box.remove();
    vi.advanceTimersByTime(6000);
    expect(S.tick.value).toBe(1);
  });
});
