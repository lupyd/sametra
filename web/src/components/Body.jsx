// A message body in its declared format. Markdown is rendered (safely, see
// lib/markdown.js); any other format is shown as its source, so new formats
// never break older clients.
import { makeMarkdown } from '../lib/markdown.js';

const MD = new Set(['', 'text/markdown', 'text/plain']);

export function Body({ e, cls = '' }) {
  const names = Object.fromEntries((e.mentions || []).map((m) => [m.npub, m.name]));
  const fmt = e.format || '';
  if (!MD.has(fmt)) {
    return (
      <div class={'body ' + cls}>
        <pre class="source" data-format={fmt}><code>{e.content}</code></pre>
      </div>
    );
  }
  const html = makeMarkdown((npub) => names[npub])(e.content);
  return <div class={'body md ' + cls} dangerouslySetInnerHTML={{ __html: html }} />;
}

/** What a post asks its replies to use. */
export function Accepts({ e }) {
  if (!e.accept?.length) return null;
  return <div class="meta">replies in {e.accept.map((a) => <code key={a}>{a}</code>)}</div>;
}
