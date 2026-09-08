import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { parseHTML } from 'linkedom';

const code = readFileSync(new URL('./theme.js', import.meta.url), 'utf8');
const rgb = value => [1, 3, 5].map(offset => parseInt(value.slice(offset, offset + 2), 16));
const luminance = value => rgb(value).map(n => { const x = n / 255; return x <= .04045 ? x / 12.92 : ((x + .055) / 1.055) ** 2.4; }).reduce((sum, value, index) => sum + value * [.2126, .7152, .0722][index], 0);
const contrast = (a, b) => { const x = luminance(a), y = luminance(b); return (Math.max(x, y) + .05) / (Math.min(x, y) + .05); };
function fixture() {
  const { window, document } = parseHTML('<html><head></head><body></body></html>');
  window.jarvis = {};
  new Function('window', 'document', 'CustomEvent', code)(window, document, window.CustomEvent);
  return { theme: window.jarvisTheme, token: name => document.documentElement.style.getPropertyValue(name) };
}
for (const theme of ['light', 'dark']) {
  test(`${theme} custom colors keep button labels and accent text readable across the RGB gamut`, () => {
    const f = fixture(), channels = [0, 48, 96, 144, 192, 255];
    for (const r of channels) for (const g of channels) for (const b of channels) {
      const base = '#' + [r, g, b].map(n => n.toString(16).padStart(2, '0')).join('');
      f.theme.adopt({ theme, paint: 'custom', accent: base });
      assert.ok(contrast(f.token('--accent'), f.token('--on-accent')) >= 4.5, `${base}: button label has insufficient contrast`);
      const paper = theme === 'dark' ? '#1e211f' : '#ffffff';
      assert.ok(contrast(f.token('--accent-text'), paper) >= 4.5, `${base}: accent text is unreadable on paper`);
      assert.ok(contrast(f.token('--accent-text'), f.token('--surface-2')) >= 4.5, `${base}: accent text is unreadable on a tinted surface`);
    }
  });
}

test('the reported dark orange CTA uses a dark label and switching to a preset clears custom overrides', () => {
  const f = fixture();
  f.theme.adopt({ theme: 'dark', paint: 'custom', accent: '#b64c18' });
  assert.equal(f.token('--accent'), '#D29070');
  assert.equal(f.token('--on-accent'), '#000000');
  f.theme.adopt({ paint: 'clover' });
  for (const name of ['--accent', '--accent-text', '--on-accent', '--surface-2']) assert.equal(f.token(name) || '', '');
});
