import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { readFile } from 'node:fs/promises';
import test from 'node:test';

const require = createRequire(import.meta.url);
let Repair = null;
try {
  Repair = require('./download-repair.js');
} catch {
  // RED state: the production classifier does not exist yet.
}

test('Whisper transport failure recommends configuring a proxy', () => {
  assert.ok(Repair, 'download repair classifier is available');
  assert.equal(
    Repair.actionFor(
      'Whisper-модель: не удалось подключиться — напрямую к huggingface.co: '
        + 'error sending request for url (https://huggingface.co/model.bin)',
    ),
    'proxy',
  );
});

test('other transport failures recommend a proxy but HTTP and disk failures do not', () => {
  assert.ok(Repair, 'download repair classifier is available');
  for (const message of [
    'DNS lookup failed for huggingface.co',
    'connection refused while downloading model',
    'request timed out after 30 seconds',
    'proxy CONNECT failed',
    'network is unreachable',
  ]) {
    assert.equal(Repair.actionFor(message), 'proxy', message);
  }
  for (const message of [
    'HTTP 404 Not Found',
    'checksum mismatch',
    'No space left on device',
    'permission denied',
  ]) {
    assert.equal(Repair.actionFor(message), null, message);
  }
});

test('main UI loads repair classifier before settings and exposes the proxy CTA contract', async () => {
  const [html, settings] = await Promise.all([
    readFile(new URL('./index.html', import.meta.url), 'utf8'),
    readFile(new URL('./settings2.js', import.meta.url), 'utf8'),
  ]);
  const repairIndex = html.indexOf('<script src="./download-repair.js"></script>');
  const settingsIndex = html.indexOf('<script src="./settings2.js"></script>');

  assert.notEqual(repairIndex, -1, 'main UI loads download repair classifier');
  assert.ok(repairIndex < settingsIndex, 'classifier loads before settings');
  assert.match(settings, /JarvisDownloadRepair\.actionFor/);
  assert.match(settings, /Настроить прокси/);
  assert.match(settings, /id:\s*'s2-egress-proxy'/);
  assert.match(settings, /загрузкам моделей/);
  assert.match(settings, /function focusPendingInActivePane\(\)/);
  assert.match(
    settings,
    /if \(node && !node\.childNodes\.length\) reRenderPane\(pane\);\s*focusPendingInActivePane\(\);/,
    'proxy field is focused even when the service pane was already rendered',
  );
});
