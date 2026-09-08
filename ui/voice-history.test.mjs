import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { parseHTML } from 'linkedom';
const source = readFileSync(new URL('./voice-history.js', import.meta.url), 'utf8');
const tick = () => new Promise(resolve => setImmediate(resolve));
async function boot(overrides = {}) {
  const { window, document } = parseHTML('<html><head></head><body><div id="voicehist"></div></body></html>');
  const calls = [];
  const data = {
    transcriptsGet: async () => ({ items: [{ id: 1, ts: 1700000000, text: 'original text', source: 'dictation', hasAudio: true }] }),
    transcriptUpdate: async (id, text) => ({ ok: true, text }),
    transcriptDelete: async () => ({ ok: true }),
    transcriptEnhance: async () => ({ ok: true, result: 'enhanced' }),
    transcriptRetranscribe: async () => ({ ok: true, text: 'new recognition' }),
    dictionaryGet: async () => ({ words: [{ word: 'джарвис', replacement: 'Jarvis' }] }),
    dictionaryAdd: async (word, replacement) => ({ words: [{ word, replacement }] }),
    dictionaryRemove: async () => ({ words: [] }),
    promptsGet: async () => ({ prompts: [{ id: 'clean', name: 'Чистовик', desc: 'Описание', auto: true }] }),
    promptsGetSettings: async () => ({ smart: false }),
    promptsSetSmart: async () => ({ ok: true }),
    scratchpadGet: async () => ({ text: 'saved draft' }),
    scratchpadSet: async text => ({ ok: true, text }),
    ...overrides,
  };
  window.jarvis = Object.fromEntries(Object.entries(data).filter(([, fn]) => fn).map(([name, fn]) => [name, (...args) => { calls.push([name, ...args]); return fn(...args); }]));
  window.jarvisIcons = { create: () => document.createElement('i') };
  new Function('window', 'document', 'navigator', source)(window, document, { clipboard: { writeText: async () => {} } });
  await window.initVoiceHistory(document.getElementById('voicehist'));
  const clickText = (text, scope = document) => { const node = [...scope.querySelectorAll('button')].find(node => node.textContent === text); assert.ok(node, `button ${text}`); node.click(); return node; };
  const input = (selector, value) => { const node = document.querySelector(selector); node.value = value; node.dispatchEvent(new window.Event('input', { bubbles: true })); return node; };
  return { window, document, calls, data, clickText, input };
}
test('missing history bridge is a readable error, never an empty-history success', async () => {
  const { document } = await boot({ transcriptsGet: null });
  assert.match(document.querySelector('[data-k="history"].pane').textContent, /недоступна/);
  assert.doesNotMatch(document.querySelector('[data-k="insights"].pane').textContent, /0слов/);
});
test('audio capability survives normalization and duplicate host IDs are avoided', async () => {
  const { document } = await boot();
  assert.match(document.querySelector('.ent').textContent, /Распознать снова/);
  assert.equal(document.querySelectorAll('#voicehist').length, 1);
});
test('formatted output and original ASR remain distinct and independently copyable', async () => {
  const h = await boot({
    transcriptsGet: async () => ({ items: [{ id: 1, ts: 1700000000, text: 'Проверь Jarvis.', rawText: 'проверь джарвис', source: 'dictation', appliedStyle: 'clean' }] }),
    copyText: async () => {},
  });
  assert.equal(h.document.querySelector('.vh-origin-label').textContent, 'Отформатировано');
  assert.equal(h.document.querySelector('.vh-original-text').textContent, 'проверь джарвис');
  h.clickText('Копировать исходный текст'); await tick();
  assert.deepEqual(h.calls.find(call => call[0] === 'copyText'), ['copyText', 'проверь джарвис']);
  assert.equal(h.document.querySelector('.vh-text').textContent, 'Проверь Jarvis.');
});
test('failed edit retains original history and editable draft; success updates acknowledged text', async () => {
  let fail = true;
  const { document, clickText, input } = await boot({ transcriptUpdate: async () => fail ? ({ ok: false, error: 'disk full' }) : ({ ok: true, text: 'persisted' }) });
  clickText('Изменить'); input('.vh-editor textarea', 'draft'); clickText('Сохранить'); await tick();
  assert.equal(document.querySelector('.vh-text').textContent, 'original text');
  assert.equal(document.querySelector('.vh-editor textarea').value, 'draft');
  assert.match(document.querySelector('.vh-editor [role="alert"]').textContent, /disk full/);
  fail = false; clickText('Сохранить'); await tick();
  assert.equal(document.querySelector('.vh-text').textContent, 'persisted');
  assert.equal(document.querySelector('.vh-editor'), null);
});
test('failed delete does not remove a transcript or announce a false success', async () => {
  const { document, clickText } = await boot({ transcriptDelete: async () => ({ ok: false, error: 'read only' }) });
  clickText('Удалить'); await tick();
  assert.equal(document.querySelectorAll('.ent').length, 1);
  assert.match(document.querySelector('.ent [role="alert"]').textContent, /read only/);
});
test('dictionary add and remove retain acknowledged list on rejection', async () => {
  const { window, document, clickText, input } = await boot({ dictionaryAdd: async () => { throw new Error('write blocked'); }, dictionaryRemove: async () => ({ ok: false, error: 'write blocked' }) });
  clickText('Словарь'); input('[aria-label="Распознанное слово или фраза"]', 'таури'); input('[aria-label="Правильная запись"]', 'Tauri');
  document.querySelector('.addrow').dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true })); await tick();
  assert.equal(document.querySelectorAll('.lrow').length, 1); assert.equal(document.querySelector('.addrow input').value, 'таури');
  document.querySelector('.lrow .x').click(); await tick(); assert.equal(document.querySelectorAll('.lrow').length, 1);
});
test('smart-mode switch only changes after saved acknowledgement; fixed styles have no fake toggles', async () => {
  const { document, clickText } = await boot({ promptsSetSmart: async () => ({ ok: false, error: 'cannot save' }) });
  clickText('Преобразования'); assert.equal(document.querySelectorAll('[role="switch"]').length, 1);
  document.querySelector('[role="switch"]').click(); await tick();
  assert.equal(document.querySelector('[role="switch"]').getAttribute('aria-checked'), 'false');
  assert.match(document.querySelector('[data-k="transforms"].pane').textContent, /cannot save/);
});
test('draft writes serialize and coalesce newer input; rejected save stays visible and retryable', async () => {
  let release;
  const writes = [];
  const { document, clickText, input } = await boot({ scratchpadSet: text => { writes.push(text); return new Promise(resolve => { release = () => resolve({ ok: true, text }); }); } });
  clickText('Черновик'); input('.scratch textarea', 'first'); input('.scratch textarea', 'newer');
  assert.deepEqual(writes, ['first']); release(); await tick(); assert.deepEqual(writes, ['first', 'newer']); release(); await tick();
  assert.match(document.querySelector('.vh-save-state').textContent, /Сохранено/);
  const failed = await boot({ scratchpadSet: async () => { throw new Error('disk failure'); } });
  failed.clickText('Черновик'); failed.input('.scratch textarea', 'unsaved'); await tick();
  assert.match(failed.document.querySelector('.vh-save-state').textContent, /Не сохранено/);
  assert.equal(failed.document.querySelector('.scratch textarea').value, 'unsaved');
});
test('failed scratch read disables editing and never replaces persisted data with empty text', async () => {
  const { document, calls } = await boot({ scratchpadGet: async () => { throw new Error('unreadable'); } });
  assert.equal(document.querySelector('.scratch textarea').disabled, true);
  assert.equal(calls.filter(([name]) => name === 'scratchpadSet').length, 0);
});
