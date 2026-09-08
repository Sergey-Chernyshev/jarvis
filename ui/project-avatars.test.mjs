import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { parseHTML } from 'linkedom';

const code = readFileSync(new URL('./project-avatars.js', import.meta.url), 'utf8');
const PNG = 'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO6RnvYAAAAASUVORK5CYII=';
const OTHER_PNG = 'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVQIHWP4z8DwHwAFgAI/ScLbtAAAAABJRU5ErkJggg==';
const flush = () => new Promise(resolve => setImmediate(resolve));
const deferred = () => { let resolve; const promise = new Promise(yes => { resolve = yes; }); return { promise, resolve }; };
const candidate = (path = '/work/app/icon.png', dataUrl = PNG) => ({ path, name: path.split('/').at(-1), dataUrl });

function fixture({ discover = async () => ({ ok: true, candidates: [] }), width = 512, height = 256, imageError = false, output = () => OTHER_PNG } = {}) {
  const { document } = parseHTML('<html><body></body></html>');
  // Linkedom hard-codes window.Image; a plain browser facade lets these tests
  // control decode completion while retaining real DOM nodes and events.
  const window = { atob, setTimeout, clearTimeout };
  const calls = [], imageSources = [], canvases = [], fileReads = [];
  const createElement = document.createElement.bind(document);
  document.createElement = (name, ...args) => {
    const element = createElement(name, ...args);
    if (name === 'canvas') {
      const canvas = { element, draws: [], formats: [] }; canvases.push(canvas);
      element.getContext = () => ({ drawImage: (...args) => canvas.draws.push(args) });
      element.toDataURL = format => { canvas.formats.push(format); return output(canvas.draws[0]?.[0].fixtureSource); };
    }
    return element;
  };
  window.Image = class {
    set src(value) {
      this.fixtureSource = value;
      imageSources.push(value);
      queueMicrotask(() => {
        if (imageError) this.onerror?.(new Error('Invalid image bytes'));
        else { this.naturalWidth = width; this.naturalHeight = height; this.width = width; this.height = height; this.onload?.(); }
      });
    }
  };
  window.FileReader = class {
    readAsDataURL(file) {
      fileReads.push(file);
      queueMicrotask(() => {
        if (file.readError) { this.error = new Error('Cannot read upload'); this.onerror?.({ target: this }); }
        else { this.result = file.dataUrl || PNG; this.onload?.({ target: this }); }
      });
    }
  };
  window.jarvisIcons = { create: () => document.createElement('svg') };
  window.jarvis = {
    projectsIconCandidates: (...args) => { calls.push({ name: 'projectsIconCandidates', args }); return discover(...args); },
    projectsSave: (...args) => { calls.push({ name: 'projectsSave', args }); return Promise.resolve({ ok: true }); },
  };
  new Function('window', 'document', code)(window, document);
  return { window, document, api: window.JarvisProjectAvatars, calls, imageSources, canvases, fileReads };
}

test('a custom project avatar has priority over discovery and keeps the chooser interactive', async () => {
  const f = fixture({ discover: async () => ({ ok: true, candidates: [candidate()] }) });
  let choices = 0;
  const avatar = f.api.create({ machine: 'local', cwd: '/work/app', avatar: { dataUrl: PNG } }, { className: 'test-avatar', onChoose: () => choices++ });
  f.document.body.append(avatar); await flush();
  assert.equal(avatar.tagName, 'BUTTON');
  assert.ok(avatar.classList.contains('test-avatar'));
  assert.equal(avatar.querySelector('img')?.getAttribute('src'), PNG);
  avatar.click(); assert.equal(choices, 1);
  assert.equal(f.calls.some(call => call.name === 'projectsSave'), false);
});

test('one discovered image is shown automatically without persisting project metadata', async () => {
  const f = fixture({ discover: async () => ({ ok: true, candidates: [candidate()] }) });
  const avatar = f.api.create({ machine: 'local', cwd: '/work/app' });
  f.document.body.append(avatar); await flush();
  assert.equal(avatar.querySelector('img')?.getAttribute('src'), OTHER_PNG);
  assert.deepEqual(f.calls.filter(call => call.name === 'projectsIconCandidates').map(call => call.args), [['local', '/work/app']]);
  assert.equal(f.calls.some(call => call.name === 'projectsSave'), false);
});

test('multiple candidates preserve a placeholder and expose a choice instead of selecting one', async () => {
  const f = fixture({ discover: async () => ({ ok: true, candidates: [candidate(), candidate('/work/app/other.png', OTHER_PNG)] }) });
  let choices = 0;
  const avatar = f.api.create({ machine: 'local', cwd: '/work/app' }, { onChoose: () => choices++ });
  f.document.body.append(avatar); await flush();
  assert.equal(avatar.querySelector('img'), null);
  assert.match(avatar.textContent, /2/);
  avatar.click(); assert.equal(choices, 1);
  assert.equal(f.calls.some(call => call.name === 'projectsSave'), false);
});

test('HTTP and active SVG sources never become rendered image sources', async () => {
  const unsafeSvg = 'data:image/svg+xml;base64,' + Buffer.from('<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script><image href="https://external.invalid/image.png"/></svg>').toString('base64');
  for (const source of ['https://external.invalid/avatar.png', unsafeSvg]) {
    const f = fixture({ discover: async () => ({ ok: true, candidates: [candidate('/work/app/unsafe.svg', source)] }) });
    const avatar = f.api.create({ machine: 'local', cwd: '/work/app', avatar: { dataUrl: source } });
    f.document.body.append(avatar); await flush();
    assert.equal(avatar.querySelector('img'), null);
    assert.equal(f.imageSources.includes(source), false);
    await assert.rejects(f.api.normalize(source));
    assert.equal(f.imageSources.includes(source), false, 'reject before handing active content to Image');
  }
});

test('discovery coalesces identical requests and caches results by machine plus directory', async () => {
  const pending = deferred();
  const f = fixture({ discover: () => pending.promise });
  const first = f.api.discover('local', '/work/app');
  const second = f.api.discover('local', '/work/app');
  const refreshDuringRequest = f.api.discover('local', '/work/app', { refresh: true });
  await flush();
  assert.equal(f.calls.length, 1);
  pending.resolve({ ok: true, candidates: [candidate()] });
  const [a, b, refreshed] = await Promise.all([first, second, refreshDuringRequest]);
  assert.deepEqual(a, b);
  assert.deepEqual(a, refreshed);
  assert.deepEqual(await f.api.discover('local', '/work/app'), a);
  assert.equal(f.calls.length, 1);
  await f.api.discover('build-box', '/work/app');
  await f.api.discover('local', '/other/app');
  assert.deepEqual(f.calls.map(call => call.args), [['local', '/work/app'], ['build-box', '/work/app'], ['local', '/other/app']]);
  await f.api.discover('local', '/work/app', { refresh: true });
  assert.equal(f.calls.length, 4, 'an explicit refresh bypasses a settled cache entry');
});

test('relative or control-character paths do not invoke native discovery', async () => {
  const f = fixture();
  for (const cwd of ['', './project', '/work/\nproject', '/work/\0project']) {
    const result = await f.api.discover('build-box', cwd);
    assert.equal(result.ok, false); assert.deepEqual(result.candidates, []);
  }
  assert.equal(f.calls.length, 0);
});

test('discovery never exceeds two concurrent native requests and drains queued work', async () => {
  const pending = [], active = new Set(); let maximum = 0;
  const f = fixture({ discover: (machine, cwd) => {
    const request = deferred(); pending.push({ machine, cwd, ...request }); active.add(request);
    maximum = Math.max(maximum, active.size);
    return request.promise.finally(() => active.delete(request));
  } });
  const requests = ['one', 'two', 'three', 'four'].map(name => f.api.discover('local', '/' + name));
  await flush(); assert.equal(pending.length, 2);
  pending[0].resolve({ ok: true, candidates: [] }); await flush();
  assert.equal(pending.length, 3);
  pending[1].resolve({ ok: false, error: 'Temporarily offline', candidates: [] }); await flush();
  assert.equal(pending.length, 4, 'a failed request must release its queue slot');
  pending[2].resolve({ ok: true, candidates: [] }); pending[3].resolve({ ok: true, candidates: [] });
  await Promise.all(requests);
  assert.equal(maximum, 2); assert.equal(active.size, 0);
});

test('identical directory names on different machines keep their own discovered image', async () => {
  const f = fixture({ discover: async machine => ({ ok: true, candidates: [candidate('/work/app/icon.png', machine === 'local' ? PNG : OTHER_PNG)] }), output: source => source });
  const local = f.api.create({ machine: 'local', cwd: '/work/app' });
  const remote = f.api.create({ machine: 'build-box', cwd: '/work/app' });
  f.document.body.append(local, remote); await flush();
  assert.equal(local.querySelector('img')?.getAttribute('src'), PNG);
  assert.equal(remote.querySelector('img')?.getAttribute('src'), OTHER_PNG);
});

test('normalization constrains both dimensions to 128px and exports PNG', async () => {
  const f = fixture({ width: 512, height: 256 });
  assert.equal(await f.api.normalize(PNG), OTHER_PNG);
  const canvas = f.canvases.at(-1);
  assert.equal(canvas.element.width, 128); assert.equal(canvas.element.height, 64);
  assert.equal(canvas.draws.length, 1);
  assert.deepEqual(canvas.formats, ['image/png']);
  const small = fixture({ width: 24, height: 48 });
  await small.api.normalize(PNG);
  assert.equal(small.canvases[0].element.width, 24); assert.equal(small.canvases[0].element.height, 48);
});

test('normalization rejects undecodable or oversized images', async () => {
  await assert.rejects(fixture({ imageError: true }).api.normalize(PNG));
  await assert.rejects(fixture({ width: 8193, height: 1 }).api.normalize(PNG));
  await assert.rejects(fixture({ width: 7000, height: 7000 }).api.normalize(PNG));
});

test('uploads are decoded and normalized while oversized files fail before FileReader', async () => {
  const f = fixture();
  const upload = { name: 'avatar.png', type: 'image/png', size: 1024, dataUrl: PNG };
  assert.equal(await f.api.fromFile(upload), OTHER_PNG);
  assert.equal(f.fileReads.length, 1);
  await assert.rejects(f.api.fromFile({ ...upload, size: 5 * 1024 * 1024 + 1 }));
  assert.equal(f.fileReads.length, 1);
  await assert.rejects(f.api.fromFile({ ...upload, readError: true }));
});

test('offline projects reuse cached artwork without contacting the machine again', async () => {
  const f = fixture({ discover: async () => ({ ok: true, candidates: [candidate()] }) });
  const empty = f.api.create({ machine: 'offline', cwd: '/work/app', connection: { online: false } });
  f.document.body.append(empty); await flush();
  assert.equal(f.calls.length, 0);
  await f.api.discover('build-box', '/work/app');
  const cached = f.api.create({ machine: 'build-box', cwd: '/work/app', connection: { online: false } });
  f.document.body.append(cached); await flush();
  assert.equal(cached.querySelector('img')?.getAttribute('src'), OTHER_PNG);
  assert.equal(f.calls.length, 1);
});
