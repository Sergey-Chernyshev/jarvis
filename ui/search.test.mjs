/* Экран поиска по проекту на подставном DOM. */

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
  const src = readFileSync(new URL('./search.js', import.meta.url), 'utf8');
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

const HITS = [
  { path: 'src/main.rs', line: 12, text: 'let x = 1;' },
  { path: 'src/main.rs', line: 40, text: 'let y = 2;' },
  { path: 'ui/app.js', line: 3, text: 'const a = 2;' },
];

function harness(res = { ok: true, hits: HITS, capped: false }) {
  globalThis.document = makeDom();
  const api = load();
  const root = globalThis.document.createElement('div');
  const calls = [];
  api.mount(root, { sessionSearch: async (id, q) => { calls.push(q); return res; } }, (p) => calls.push(['open', p]));
  return { api, root, calls };
}

test('итог поиска говорит словами, включая «ничего»', () => {
  globalThis.document = makeDom();
  const api = load();
  assert.match(api.summary(HITS, false, 'x'), /3 совпадения в 2 файлах/);
  assert.equal(api.summary([], false, 'x'), 'ничего не нашлось');
  assert.equal(api.summary([], false, ''), 'что искать?');
  assert.match(api.summary(HITS, true, 'x'), /показаны первые/);
});

test('совпадения группируются по файлам', () => {
  globalThis.document = makeDom();
  const api = load();
  const g = api.group(HITS);
  assert.equal(g.length, 2);
  assert.equal(g[0].lines.length, 2, 'две строки одного файла должны быть вместе');
});

test('поиск показывает пути, номера строк и сам текст', async () => {
  const { api, root, calls } = harness();
  api.open('s1');
  await api.run('искомое');
  assert.deepEqual(calls, ['искомое']);
  const text = textOf(root);
  assert.match(text, /src\/main\.rs/);
  assert.match(text, /12/);
  assert.match(text, /let x = 1;/);
});

test('пустой запрос не ходит в бэкенд', async () => {
  const { api, calls } = harness();
  api.open('s1');
  await api.run('   ');
  assert.equal(calls.length, 0, 'зря сходили за пустотой');
});

test('клик по файлу просит открыть именно его', async () => {
  const { api, root, calls } = harness();
  api.open('s1');
  await api.run('x');
  find(root, (n) => n.className.includes('srch-path'))[0].listeners.click[0]();
  assert.deepEqual(calls[calls.length - 1], ['open', 'src/main.rs']);
});

test('отказ показывается словами, а не пустым списком', async () => {
  const { api, root } = harness({ ok: false, error: 'grep не нашёлся' });
  api.open('s1');
  await api.run('x');
  assert.match(textOf(root), /grep не нашёлся/);
});
