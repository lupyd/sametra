// Message bodies: markdown by default (CommonMark + GFM + $math$), rendered with
// marked. Safety: raw HTML is shown as text, links are limited to safe schemes,
// images become links (the CSP blocks remote images anyway), and nostr:npub
// mentions become @name links. Math is kept as marked-up TeX source for now; a
// typesetter can be added later without changing stored posts.
import { Marked } from 'marked';

const esc = (s) => String(s).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]);
const SAFE = /^(https?:|mailto:|nostr:|ipfs:|\/|#)/i;

const math = {
  extensions: [
    {
      name: 'blockMath', level: 'block',
      start: (src) => src.indexOf('$$'),
      tokenizer(src) {
        const m = /^\$\$([\s\S]+?)\$\$(?:\n|$)/.exec(src);
        if (m) return { type: 'blockMath', raw: m[0], text: m[1].trim() };
      },
      renderer: (t) => `<div class="math" data-tex="block">${esc(t.text)}</div>\n`,
    },
    {
      name: 'inlineMath', level: 'inline',
      start: (src) => src.indexOf('$'),
      tokenizer(src) {
        const m = /^\$([^\s$](?:[^$\n]*?[^\s$])?)\$(?!\d)/.exec(src);
        if (m) return { type: 'inlineMath', raw: m[0], text: m[1] };
      },
      renderer: (t) => `<span class="math" data-tex="inline">${esc(t.text)}</span>`,
    },
  ],
};

/** Build a renderer; `names(npub)` returns a display name for a mention, if known. */
export function makeMarkdown(names = () => null) {
  const md = new Marked({ gfm: true, breaks: true });
  md.use(math, {
    renderer: {
      html: ({ text }) => esc(text),
      link({ href, title, tokens }) {
        const inner = this.parser.parseInline(tokens);
        if (!SAFE.test(href)) return inner;
        const m = /^nostr:(npub1[02-9ac-hj-np-z]+)$/.exec(href);
        if (m) return `<a class="mention" href="/u/${m[1]}">${inner}</a>`;
        const ext = /^https?:/i.test(href);
        return `<a href="${esc(href)}"${title ? ` title="${esc(title)}"` : ''}${ext ? ' rel="noopener nofollow ugc" target="_blank"' : ''}>${inner}</a>`;
      },
      image: ({ href, text }) => (SAFE.test(href) ? `<a href="${esc(href)}" rel="noopener nofollow ugc">${esc(text || href)}</a>` : esc(text)),
      code: ({ text, lang }) => `<pre><code${lang ? ` class="language-${esc(lang.split(/\s/)[0])}"` : ''}>${esc(text)}</code></pre>\n`,
    },
  });
  // nostr:npub… written bare in the text (NIP-27) reads as @name
  md.use({
    extensions: [{
      name: 'nostrMention', level: 'inline',
      start: (src) => src.indexOf('nostr:npub1'),
      tokenizer(src) {
        const m = /^nostr:(npub1[02-9ac-hj-np-z]{58})/.exec(src);
        if (m) return { type: 'nostrMention', raw: m[0], npub: m[1] };
      },
      renderer: (t) => `<a class="mention" href="/u/${t.npub}">@${esc(names(t.npub) || t.npub.slice(0, 12) + '…')}</a>`,
    }],
  });
  return (text) => md.parse(text || '');
}
