/* Экран «Изменения задачи» на подставном DOM.
 *
 * Проверяется настоящий модуль: свод, строки файлов, выбор галочками, коммит
 * выбранного и отказ откатывать новый файл. Подставной setAttribute строг к
 * именам, как настоящий, — урок экрана «Циклов», молча падавшего именно там. */

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

function makeDom() {
  const mk = (tag) => {
    let text = '';
    const node = {
      tag,
      nodeType: 1,
      className: '',
      hidden: false,
      value: '',
      checked: false,
      disabled: false,
      parent: null,
      children: [],
      attrs: {},
      listeners: {},
      classList: {
        add(c) { node.className = `${node.className} ${c}`.trim(); },
        remove(c) { node.className = node.className.split(' ').filter((x) => x !== c).join(' '); },
        contains(c) { return node.className.split(' ').includes(c); },
        toggle(c, on) { on ? node.classList.add(c) : node.classList.remove(c); },
      },
      appendChild(k) { node.children.push(k); k.parent = node; return k; },
      addEventListener(ev, fn) { (node.listeners[ev] ||= []).push(fn); },
      setAttribute(k, v) {
        if (!/^[A-Za-z_:][-A-Za-z0-9_:.]*$/.test(k)) {
          throw new Error(`InvalidCharacterError: имя атрибута «${k}» недопустимо`);
        }
        node.attrs[k] = v;
      },
      querySelector(sel) {
        const want = sel.replace('.', '');
        const walk = (x) => {
          if (x.className.split(' ').includes(want)) return x;
          for (const c of x.children) { const hit = walk(c); if (hit) return hit; }
          return null;
        };
        return walk(node);
      },
    };
    Object.defineProperty(node, 'textContent', {
      get: () => text,
      set: (v) => { text = String(v); node.children = []; },
    });
    return node;
  };
  return { createElement: (t) => mk(t) };
}

function load() {
  const src = readFileSync(new URL('./changes.js', import.meta.url), 'utf8');
  const module = { exports: {} };
  new Function('module', 'document', 'globalThis', src)(module, globalThis.document, globalThis);
  return module.exports;
}

function textOf(node) {
  return [node.textContent || '', ...node.children.map(textOf)].join(' ');
}

function find(node, pred, out = []) {
  if (pred(node)) out.push(node);
  for (const k of node.children) find(k, pred, out);
  return out;
}

const FILES = [
  { path: 'src/main.rs', state: 'изменён', added: 12, removed: 3, untracked: false },
  { path: 'ui/new.js', state: 'новый', added: 0, removed: 0, untracked: true },
];

function harness(files = FILES, extra = {}) {
  globalThis.document = makeDom();
  const api = load();
  const root = globalThis.document.createElement('div');
  const calls = [];
  const bridge = {
    sessionChanges: async () => { calls.push(['changes']); return { ok: true, branch: 'dev', files }; },
    sessionChangeDiff: async (id, path) => { calls.push(['diff', path]); return { ok: true, hunks: [] }; },
    sessionCommit: async (id, message, paths) => { calls.push(['commit', message, paths]); return { ok: true, sha: 'abc1234' }; },
    sessionRevert: async (id, path) => { calls.push(['revert', path]); return { ok: true }; },
    sessionReview: async () => { calls.push(['review']); return { ok: true, verdict: 'return', text: 'тесты сняты, а не починены' }; },
    sendReply: async (id, text) => { calls.push(['reply', text]); return { ok: true }; },
    sessionTouched: async (id, path) => { calls.push(['touched', path]); return { ok: true, touched: [{ line: 3, name: 'parse_status', kind: 'функция' }] }; },
    ...extra,
  };
  const toasts = [];
  api.mount(root, bridge, (t) => toasts.push(t));
  return { api, root, calls, toasts };
}

test('свод считает файлы и строки, а не пересказывает список', () => {
  globalThis.document = makeDom();
  const api = load();
  assert.equal(api.summary(FILES), '2 файла · +12 −3');
  assert.equal(api.summary([]), 'Изменений нет');
  assert.equal(api.summary([FILES[0]]), '1 файл · +12 −3');
});

test('у нового файла счётчиков нет — «+0 −0» было бы враньём', () => {
  globalThis.document = makeDom();
  const api = load();
  assert.equal(api.row(FILES[0]).counts, '+12 −3');
  assert.equal(api.row(FILES[1]).counts, '');
  assert.equal(api.row(FILES[1]).canRevert, false, 'откатывать неотслеживаемый нечем');
  assert.equal(api.row(FILES[0]).canRevert, true);
});

test('список показывает ветку, файлы и состояния', async () => {
  const { api, root } = harness();
  await api.open('s1');
  const text = textOf(root);
  assert.match(text, /dev/, 'ветка не показана');
  assert.match(text, /src\/main\.rs/);
  assert.match(text, /ui\/new\.js/);
  assert.match(text, /изменён/);
  assert.match(text, /новый/);
  // Кнопка отката есть только у отслеживаемого файла.
  const reverts = find(root, (n) => n.className.includes('chg-revert'));
  assert.equal(reverts.length, 1, 'кнопка отката должна быть ровно у одного файла');
});

test('коммит уходит с выбранными файлами, снятая галочка их убирает', async () => {
  const { api, root, calls, toasts } = harness();
  await api.open('s1');
  // Снимаем галочку с нового файла: принимаем только правку исходника.
  const picks = find(root, (n) => n.className.includes('chg-pick'));
  assert.equal(picks.length, 2);
  picks[1].checked = false;
  picks[1].listeners.change[0]();
  root.querySelector('.chg-msg').value = 'починил очередь';
  await root.querySelector('.chg-accept').listeners.click[0]();
  const commit = calls.find((c) => c[0] === 'commit');
  assert.deepEqual(commit, ['commit', 'починил очередь', ['src/main.rs']]);
  assert.ok(toasts.some((t) => /abc1234/.test(t)), 'об итоге сказали: ' + toasts.join('|'));
});

test('пустой выбор — не коммит, а внятный отказ', async () => {
  const { api, root, calls, toasts } = harness();
  await api.open('s1');
  for (const p of find(root, (n) => n.className.includes('chg-pick'))) {
    p.checked = false;
    p.listeners.change[0]();
  }
  await root.querySelector('.chg-accept').listeners.click[0]();
  assert.equal(calls.filter((c) => c[0] === 'commit').length, 0, 'коммит не должен уйти');
  assert.match(toasts.join('|'), /Не выбрано/);
});

test('чистое дерево говорит об этом словами', async () => {
  const { api, root } = harness([]);
  await api.open('s1');
  assert.match(textOf(root), /чистое/);
});

test('отказ git показывается, а не превращается в пустой список', async () => {
  const { api, root } = harness(FILES, {
    sessionChanges: async () => ({ ok: false, error: 'не репозиторий git' }),
  });
  await api.open('s1');
  assert.match(textOf(root), /не репозиторий git/);
});

test('клик по файлу просит дифф именно этого файла', async () => {
  const { api, root, calls } = harness();
  await api.open('s1');
  const path = find(root, (n) => n.className.includes('chg-path'))[0];
  await path.listeners.click[0]();
  assert.deepEqual(calls.find((c) => c[0] === 'diff'), ['diff', 'src/main.rs']);
});

test('ревью агентом показывает вердикт словом и его замечания', async () => {
  const { api, root, calls } = harness();
  await api.open('s1');
  await root.querySelector('.chg-review-btn').listeners.click[0]();
  assert.ok(calls.some((c) => c[0] === 'review'), 'ревью не запрошено');
  const text = textOf(root);
  assert.match(text, /есть замечания/, 'вердикт не назван словом');
  assert.match(text, /тесты сняты/, 'замечания не показаны');
});

test('вердикт «ok» не выдаёт себя за галочку, а говорит словами', () => {
  globalThis.document = makeDom();
  const api = load();
  assert.equal(api.verdictWord('ok'), 'можно принимать');
  assert.equal(api.verdictWord('ask'), 'нужен ты');
  assert.equal(api.verdictWord('return'), 'есть замечания');
  assert.equal(api.verdictWord('чушь'), 'не вышло');
});

test('отказ ревью не притворяется вердиктом', async () => {
  const { api, root } = harness(FILES, { sessionReview: async () => ({ ok: false, error: 'claude не найден' }) });
  await api.open('s1');
  await root.querySelector('.chg-review-btn').listeners.click[0]();
  assert.match(textOf(root), /claude не найден/);
});

test('замечание к строке несёт адрес и саму строку — агенту не придётся её искать', () => {
  globalThis.document = makeDom();
  const api = load();
  const t = api.noteText('src/main.rs', 42, '    let x = 1;', 'зачем тут единица?');
  assert.match(t, /src\/main\.rs:42/);
  assert.match(t, /> {4}let x = 1;|> let x = 1;/, 'строка не процитирована: ' + t);
  assert.match(t, /зачем тут единица\?/);
  // Без кода — только адрес и вопрос, без пустой цитаты.
  assert.equal(api.noteText('a.rs', 7, '   ', 'почему?'), 'a.rs:7\n\nпочему?');
});

test('открытый файл показывает, какие объявления тронуты', async () => {
  const { api, root, calls } = harness();
  await api.open('s1');
  await find(root, (n) => n.className.includes('chg-path'))[0].listeners.click[0]();
  await new Promise((r) => setTimeout(r, 0));
  assert.ok(calls.some((c) => c[0] === 'touched'), 'не спросили, что тронуто');
  assert.match(textOf(root), /тронуто: parse_status/);
});
