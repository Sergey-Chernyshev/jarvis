/* Экран «Локальные модели» в настоящем DOM.
 *
 * Ловит ровно тот случай, ради которого экран и переделан: движок собран без
 * cargo-фичи (whisper-native / wakeword-ort), а онбординг всё равно предлагал
 * галочку и качал сотни мегабайт в пустоту. Тест кликает по живой разметке —
 * подставного рендера тут недостаточно.
 */

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { parseHTML } from 'linkedom';

const HERE = new URL('./', import.meta.url);
const read = (name) => readFileSync(new URL(name, HERE), 'utf8');

function capability(id, available, ready = false) {
  return { id, label: id, ready, available, required: false, detail: '' };
}

/* Онбординг живёт вне общего моста и зовёт Tauri напрямую — подменяем invoke. */
async function boot(capabilities) {
  const { window, document } = parseHTML(read('onboarding.html'));
  const calls = [];
  const snapshot = {
    coreReady: true,
    agents: [],
    transport: [],
    capabilities,
    warnings: [],
    proxyConfigured: false,
    job: { state: 'idle', kind: '', tasks: [], steps: [], failures: [] },
  };
  window.__TAURI__ = {
    core: {
      invoke: async (cmd, args) => {
        calls.push([cmd, args]);
        return cmd === 'onboarding_get' ? snapshot : { ok: true };
      },
    },
    event: { listen: async () => () => {} },
  };
  window.requestAnimationFrame = (cb) => setTimeout(() => cb(Date.now()), 0);
  for (const name of ['onboarding-state.js', 'onboarding.js']) {
    const fn = new Function(
      'window', 'document', 'globalThis', 'navigator', 'setTimeout', 'requestAnimationFrame',
      read(name),
    );
    fn(window, document, window, { userAgent: 'X11; Linux x86_64' }, setTimeout, window.requestAnimationFrame);
  }
  await new Promise((r) => setTimeout(r, 0));
  // Переход на экран моделей — тем же путём, что и у пользователя: лентой этапов.
  const step = document.querySelector('.rail-step[data-screen="capabilities"]');
  step.dispatchEvent(new window.Event('click', { bubbles: true }));
  await new Promise((r) => setTimeout(r, 0));
  return { window, doc: document, calls };
}

function rows(doc) {
  return [...doc.querySelectorAll('#content .item')].map((item) => ({
    text: item.textContent,
    selectable: Boolean(item.parentElement && item.parentElement.classList.contains('capability')),
  }));
}

async function check(doc, window, title) {
  const label = [...doc.querySelectorAll('#content .capability')]
    .find((node) => node.textContent.includes(title));
  assert.ok(label, `нет выбираемой строки «${title}»`);
  const input = label.querySelector('input');
  input.checked = true;
  input.dispatchEvent(new window.Event('change', { bubbles: true }));
  await new Promise((r) => setTimeout(r, 0));
}

test('модель без вкомпилированного движка нельзя выбрать и скачать', async () => {
  const { doc, window, calls } = await boot([
    capability('whisper-turbo', false),
    capability('hey_jarvis', false),
    capability('silero', true),
  ]);
  const list = rows(doc);
  const whisper = list.find((row) => row.text.includes('Whisper'));
  const wake = list.find((row) => row.text.includes('Голосовая активация'));
  assert.ok(whisper && !whisper.selectable, 'Whisper всё ещё предлагают скачать');
  assert.ok(wake && !wake.selectable, 'wake-word всё ещё предлагают скачать');
  assert.match(whisper.text, /недоступно в этой сборке/, 'причина не названа: ' + whisper.text);
  assert.match(wake.text, /недоступно в этой сборке/, 'причина не названа: ' + wake.text);
  assert.equal(doc.querySelectorAll('#content .capability input').length, 1, 'лишние галочки');

  // Доступное — качается, и в план не просачивается ничего из отключённого.
  await check(doc, window, 'Silero');
  doc.getElementById('primary').dispatchEvent(new window.Event('click', { bubbles: true }));
  await new Promise((r) => setTimeout(r, 0));
  const install = calls.find(([cmd]) => cmd === 'models_install');
  assert.deepEqual(install && install[1], { ids: ['silero'] });
});

/* Старый бэкенд про сборку молчит: раз он не сказал «нельзя» — не отнимаем
 * модель, которая у него работала. Скрывать по догадке хуже, чем показать. */
test('без данных о сборке модель остаётся доступной', async () => {
  const { doc, window, calls } = await boot([{ id: 'whisper-turbo', ready: false }]);
  await check(doc, window, 'Whisper large-v3-turbo');
  doc.getElementById('primary').dispatchEvent(new window.Event('click', { bubbles: true }));
  await new Promise((r) => setTimeout(r, 0));
  const install = calls.find(([cmd]) => cmd === 'models_install');
  assert.deepEqual(install && install[1], { ids: ['whisper-turbo'] });
});
