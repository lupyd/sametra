import { defineConfig } from 'vitest/config';

// Unit/component tests run in happy-dom with Preact's JSX runtime.
export default defineConfig({
  oxc: { jsx: { runtime: 'automatic', importSource: 'preact' } },
  test: { environment: 'happy-dom', include: ['test/**/*.test.{js,jsx}'], restoreMocks: true },
});
