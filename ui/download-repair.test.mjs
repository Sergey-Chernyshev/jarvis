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

test('fresh model progress authoritatively clears its stale error before rendering', async () => {
  const settings = await readFile(new URL('./settings2.js', import.meta.url), 'utf8');

  assert.match(settings, /function clearDownloadError\(id\)/);
  assert.match(settings, /data-download-error/);
  assert.doesNotMatch(
    settings,
    /for \(const id of ids\) clearDownloadError\(id\)/,
    'a click is not proof that the backend accepted a bulk retry',
  );
  assert.doesNotMatch(
    settings,
    /activeDownload = 'hey_jarvis'; clearDownloadError/,
    'a click is not proof that the legacy wake retry started',
  );

  const progressStart = settings.indexOf('window.jarvis.onModelInstallProgress');
  assert.notEqual(progressStart, -1, 'model progress subscription exists');
  const progressBlock = settings.slice(progressStart, progressStart + 900);
  const clearIndex = progressBlock.indexOf('clearDownloadError(id)');
  const renderIndex = progressBlock.indexOf('data-model');
  assert.ok(clearIndex >= 0, 'fresh progress clears the stale error');
  assert.ok(renderIndex > clearIndex, 'error clears before progress is rendered');

  const legacyProgressStart = settings.indexOf('window.jarvis.onSttInstallProgress');
  assert.notEqual(legacyProgressStart, -1, 'legacy progress subscription exists');
  const legacyProgressBlock = settings.slice(legacyProgressStart, legacyProgressStart + 650);
  assert.match(
    legacyProgressBlock,
    /clearDownloadError\(activeDownload\)/,
    'legacy progress also authoritatively clears its stale error',
  );
});

test('model selection uses an accessible Jarvis checkbox instead of native styling', async () => {
  const settings = await readFile(new URL('./settings2.js', import.meta.url), 'utf8');

  assert.match(settings, /input\.model-check/);
  assert.match(settings, /'aria-label':\s*'Выбрать для установки:/);
  assert.match(settings, /#settings2 \.model-check\s*\{/);
  assert.match(settings, /#settings2 \.model-check:hover/);
  assert.match(settings, /#settings2 \.model-check:focus-visible/);
  assert.match(settings, /#settings2 \.model-check:checked/);
  assert.match(settings, /#settings2 \.model-check:disabled/);
  assert.doesNotMatch(settings, /margin-right:6px;vertical-align:middle/);
});
