import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import * as P from '../src/lib/proto.js';
import { TOOLS, call } from '../src/lib/api.js';
import { decode, encode, fakeNode } from './fake-node.js';

const hex = (b) => Buffer.from(b).toString('hex');

describe('call()', () => {
  it('POSTs the protobuf request and decodes the reply', async () => {
    const calls = fakeNode({ post: (r) => ({ ok: true, id: 'x' + r.title }) });
    const a = await call('post', { community: 'math', title: 'T', content: 'c', refs: ['r1', 'r2'] });
    expect(a).toMatchObject({ ok: true, id: 'xT', count: 0 });
    const { opts, req } = calls[0];
    expect(opts.method).toBe('POST');
    expect(opts.headers['Content-Type']).toBe('application/x-protobuf');
    expect(req).toMatchObject({ community: 'math', title: 'T', content: 'c', refs: ['r1', 'r2'], parent: '' });
  });

  it('defaults to an empty request', async () => {
    const calls = fakeNode({ info: () => ({ pubkey: 'p', joined: ['m'] }) });
    expect((await call('info')).joined).toEqual(['m']);
    expect(calls[0].opts.body.length).toBe(0);
  });

  it('throws the node error message', async () => {
    fakeNode({ rate: () => new Error('score must be in -10..10') });
    await expect(call('rate', { score: 50 })).rejects.toThrow('score must be in -10..10');
  });

  it('falls back to the HTTP status when the error body is not protobuf', async () => {
    globalThis.fetch = async () => new Response(new Uint8Array([0xff, 0xff, 0xff]), { status: 502 });
    await expect(call('info')).rejects.toThrow('HTTP 502');
  });

  it('round-trips nested messages', async () => {
    const tree = { root: { id: 'r', author_rep: -2.5, replies: [{ id: 'c', replies: [{ id: 'g' }] }] }, quoted_by: [], ref_events: [], thread_root: '' };
    fakeNode({ thread: () => tree });
    const t = await call('thread', { id: 'r' });
    expect(t.root.author_rep).toBe(-2.5);
    expect(t.root.replies[0].replies[0].id).toBe('g');
  });
});

describe('wire compatibility with the Rust node', () => {
  // Same vectors as `pb::golden` in src/pb.rs.
  it.each([
    ['RateRequest', { score: -3, event: 'ab', reason: 'é' }, '0805120261622a02c3a9'],
    ['FetchRequest', { filter: { kinds: [11, 1111], tags: [{ name: 'c', values: ['m'] }], since: 1700000000, limit: 5 } }, '0a151a030bd70822060a016312016d2880e2cfaa063805'],
    ['Ack', { ok: true, id: 'x', count: 3, rep: -1.5, community: 'm' }, '0801120178180321000000000000f8bf2a016d'],
    ['IdRequest', { id: 'i', thread: true }, '0a01691001'],
  ])('%s encodes byte-identically', (type, obj, want) => {
    expect(hex(encode(type, obj))).toBe(want);
    expect(decode(type, Buffer.from(want, 'hex'))).toMatchObject(obj);
  });
});

describe('TOOLS', () => {
  it('mirrors the tools! table in crates/proto', () => {
    const src = readFileSync(resolve('../crates/proto/src/lib.rs'), 'utf8');
    const table = src.slice(src.indexOf('macro_rules! tools'));
    const rust = Object.fromEntries([...table.matchAll(/^\s+(\w+): (\w+) => (\w+), (?:true|false);/gm)].map((m) => [m[1], [m[2], m[3]]]));
    expect(Object.keys(rust).length).toBe(21);
    expect(TOOLS).toEqual(rust);
  });

  it('only names messages that exist in the generated codec', () => {
    for (const [rq, rs] of Object.values(TOOLS)) {
      expect(typeof P['write' + rq]).toBe('function');
      expect(typeof P['read' + rs]).toBe('function');
    }
  });
});
