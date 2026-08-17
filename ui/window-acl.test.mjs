/* Кнопки окна против ACL Tauri.
 *
 * Светофор оконного режима уже был декорацией: JS звал minimize и
 * toggleMaximize, а политика разрешений их отклоняла — отказ обещания глотался,
 * и кнопки просто «не работали». Ошибка не в коде кнопок, а в зазоре между
 * двумя файлами: bridge.js зовёт, capabilities/default.json разрешает.
 * Тест держит этот зазор закрытым с обеих сторон. */

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const bridge = readFileSync(new URL('./bridge.js', import.meta.url), 'utf8');
const cap = JSON.parse(readFileSync(new URL('../src-tauri/capabilities/default.json', import.meta.url), 'utf8'));

/** Какое разрешение нужно каждому оконному вызову. null — хватает core:default. */
const NEEDS = {
  minimize: 'core:window:allow-minimize',
  toggleMaximize: 'core:window:allow-toggle-maximize',
  close: 'core:window:allow-close',
  isFullscreen: 'core:window:allow-is-fullscreen',
  setFullscreen: 'core:window:allow-set-fullscreen',
  onFocusChanged: null, // событие, покрыто core:default
};

test('каждый оконный вызов моста разрешён политикой', () => {
  const used = [...bridge.matchAll(/\b(?:self\(\)|w)\.(\w+)\(/g)].map((m) => m[1])
    .filter((name) => name !== 'getCurrentWindow');
  assert.ok(used.includes('minimize') && used.includes('toggleMaximize'), 'кнопки окна пропали из моста');
  for (const api of new Set(used)) {
    assert.ok(api in NEEDS,
      `мост зовёт window.${api}, а тест про него не знает — допиши разрешение в NEEDS и в capabilities`);
    const perm = NEEDS[api];
    if (perm) {
      assert.ok(cap.permissions.includes(perm),
        `window.${api} отклонит ACL: в capabilities нет ${perm} — кнопка станет декорацией`);
    }
  }
});

test('отказ оконного вызова не глотается молча', () => {
  // Иначе следующая такая поломка снова будет выглядеть как «не работает»,
  // а не как строка в логе.
  for (const call of ['minimize()', 'toggleMaximize()', 'close()']) {
    assert.ok(new RegExp(`guard\\(self\\(\\)\\.${call.replace(/[()]/g, '\\$&')}`).test(bridge),
      `${call} без guard — отказ уйдёт в тишину`);
  }
});
