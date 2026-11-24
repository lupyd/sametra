// The Sametra mark: three independent strokes woven around a shared centre
// (geometry by agy; see public/favicon.svg). Inherits colour from the text.
import { theme, toggleTheme } from '../lib/theme.js';

export function Mark({ size = 26 }) {
  return (
    <svg class="mark" width={size} height={size} viewBox="0 0 64 64" fill="none" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      <defs><path id="mark-lobe" d="M 24.6 25.2 C 26.5 18 28.5 11 32 11 C 39.5 11 43.5 27 38 36 C 34 40 30 40 26 36 C 24.2 33.2 23 31.5 22.2 29.8" /></defs>
      <g transform="translate(32 33) scale(1.25) translate(-32 -33)" stroke-width="4.8">
        <use href="#mark-lobe" />
        <use href="#mark-lobe" transform="rotate(120 32 33)" />
        <use href="#mark-lobe" transform="rotate(240 32 33)" />
        <circle cx="32" cy="33" r="3.4" fill="var(--acc)" stroke="none" />
      </g>
    </svg>
  );
}

export function ThemeToggle() {
  const dark = theme.value === 'dark';
  return (
    <button class="theme" onClick={toggleTheme} aria-label={dark ? 'Switch to light theme' : 'Switch to dark theme'} title={dark ? 'Light theme' : 'Dark theme'}>
      <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true">
        {dark
          ? <><circle cx="12" cy="12" r="4.2" /><path d="M12 2.5v2.2M12 19.3v2.2M2.5 12h2.2M19.3 12h2.2M5.3 5.3l1.6 1.6M17.1 17.1l1.6 1.6M5.3 18.7l1.6-1.6M17.1 6.9l1.6-1.6" /></>
          : <path d="M20.5 14.6A8.6 8.6 0 0 1 9.4 3.5a8.6 8.6 0 1 0 11.1 11.1z" />}
      </svg>
    </button>
  );
}
