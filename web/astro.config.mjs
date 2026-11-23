import { defineConfig } from 'astro/config';
import preact from '@astrojs/preact';

// Static SPA: one page, one Preact island, client-side routing.
// The node serves dist/ (embedded in its binary) with an index.html fallback.
export default defineConfig({
  integrations: [preact()],
  build: { inlineStylesheets: 'never', assets: '_astro' },
  devToolbar: { enabled: false },
  // docs code blocks follow the page's light/dark theme (see docs.css)
  markdown: { shikiConfig: { themes: { light: 'github-light', dark: 'github-dark' } } },
  vite: {
    server: { proxy: { '/api': process.env.SAMETRA_API || 'http://127.0.0.1:7447' } },
    build: { modulePreload: { polyfill: false } },
  },
});
