// A fake node behind fetch(): decodes protobuf requests, answers with protobuf.
import { PbfReader, PbfWriter } from 'pbf';
import * as P from '../src/lib/proto.js';
import { TOOLS } from '../src/lib/api.js';

export function encode(type, obj) {
  const w = new PbfWriter();
  P['write' + type](obj, w);
  return w.finish();
}
export function decode(type, bytes) {
  return P['read' + type](new PbfReader(bytes));
}

/** Install handlers { tool: (req) => resp | Error }. Returns the call log. */
export function fakeNode(handlers) {
  const calls = [];
  globalThis.fetch = async (url, opts) => {
    const tool = url.replace(/^\/api\//, '');
    const [rq, rs] = TOOLS[tool];
    const req = decode(rq, new Uint8Array(opts.body));
    calls.push({ tool, req, opts });
    const h = handlers[tool];
    let out;
    try {
      out = h ? await h(req) : {};
    } catch (e) {
      out = e;
    }
    if (out instanceof Error) return new Response(encode('Error', { message: out.message }), { status: 400 });
    return new Response(encode(rs, out), { status: 200 });
  };
  return calls;
}

export const item = (o = {}) => ({
  id: 'a'.repeat(64), kind: 11, author: 'b'.repeat(64), author_name: 'Alice', author_is_agent: false, author_rep: 0,
  score: 0, ratings: 0, low_trust: false, title: 'A title', content: 'Body text', created_at: Math.floor(Date.now() / 1e3),
  parent: '', root: '', depth: 0, children: 0, refs: [], pinned: false, mine: false, reply_count: 0, replies: [], ...o,
});
