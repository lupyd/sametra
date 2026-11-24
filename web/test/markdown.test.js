import { describe, expect, it } from 'vitest';
import { makeMarkdown } from '../src/lib/markdown.js';

const md = makeMarkdown((npub) => (npub.startsWith('npub1x') ? 'Doctor Argon' : null));

describe('markdown bodies', () => {
  it('renders the usual markdown', () => {
    const h = md('# Claim\n\n*Marker X* is **up**.\n\n- a\n- b\n\n| n | p |\n|---|---|\n| 40 | 0.6 |\n');
    expect(h).toContain('<em>Marker X</em>');
    expect(h).toContain('<li>a</li>');
    expect(h).toContain('<table>');
  });

  it('never lets HTML or scripts through', () => {
    const h = md('<img src=x onerror=alert(1)> <script>alert(2)</script>\n\n[click](javascript:alert(3)) ![x](javascript:alert(4))');
    expect(h).not.toMatch(/<img|<script|javascript:/i);
    expect(h).toContain('&lt;img');
    expect(h).toContain('click');
  });

  it('keeps code and math machine-readable', () => {
    const h = md('Inline $a^2+b^2$ and\n\n$$\\sum_i i$$\n\n```lean4\ntheorem t : 1 + 1 = 2 := rfl\n```');
    expect(h).toContain('<span class="math" data-tex="inline">a^2+b^2</span>');
    expect(h).toContain('<div class="math" data-tex="block">\\sum_i i</div>');
    expect(h).toContain('<code class="language-lean4">theorem t : 1 + 1 = 2 := rfl');
    expect(md('costs $5 and $10')).not.toContain('class="math"');
  });

  it('links safely and shows mentions as @names', () => {
    const npub = 'npub1x' + 'q'.repeat(57);
    const h = md(`Ask nostr:${npub} or see [paper](https://example.org/p) and [cid](ipfs://bafyx)`);
    expect(h).toContain(`<a class="mention" href="/u/${npub}">@Doctor Argon</a>`);
    expect(h).toContain('href="https://example.org/p" rel="noopener nofollow ugc" target="_blank"');
    expect(h).toContain('href="ipfs://bafyx"');
  });
});
