/* Конструктор пайплайна: правки черновика и его отрисовка. */

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
  const src = readFileSync(new URL('./pipeline.js', import.meta.url), 'utf8');
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


function api() {
  globalThis.document = makeDom();
  return load();
}

test('новый шаг встаёт в конец и не остаётся недостижимым', () => {
  const P = api();
  const p = { start: '', steps: [] };
  P.addStep(p, 'agent', 'правка');
  P.addStep(p, 'shell', 'тесты');
  assert.equal(p.steps.length, 2);
  // Прошлый последний вёл в конец — теперь ведёт в новый шаг, иначе до него
  // никогда не дошла бы очередь.
  assert.equal(p.steps[0].next[0].to, p.steps[1].id);
  assert.equal(p.steps[1].next[0].to, '', 'последний шаг заканчивает пайплайн');
});

test('идентификатор шага — имя, а не случайные буквы: он виден в переменных', () => {
  const P = api();
  const p = { start: '', steps: [] };
  const a = P.addStep(p, 'shell', 'тесты');
  const b = P.addStep(p, 'shell', 'тесты');
  assert.equal(a.id, 'тесты');
  assert.equal(b.id, 'тесты-2', 'дубль не должен затирать первый');
});

test('удаление шага не оставляет ссылок в никуда', () => {
  const P = api();
  const p = { start: '', steps: [] };
  P.addStep(p, 'agent', 'правка');
  P.addStep(p, 'shell', 'тесты');
  P.removeStep(p, 'тесты');
  assert.equal(p.steps.length, 1);
  assert.equal(p.steps[0].next[0].to, '', 'переход в удалённый стал концом');
});

test('переходы читаются человеческой строкой', () => {
  const P = api();
  const p = { start: '', steps: [{ id: 'правка', name: 'правка', kind: 'agent', next: [] }] };
  assert.equal(P.flowLine(p, { to: 'правка', cond: 'fail' }), 'если не вышло → правка');
  assert.equal(P.flowLine(p, { to: '', cond: 'always' }), 'всегда → конец');
  assert.equal(P.flowLine(p, { to: '', cond: 'verdict', verdict: 'return' }), 'если вердикт «return» → конец');
});

test('заготовки — рабочие пайплайны, а не заглушки', () => {
  const P = api();
  for (const pr of P.presets()) {
    const p = pr.pipeline;
    assert.ok(p.steps.length >= 2, pr.id);
    const ids = new Set(p.steps.map((s) => s.id));
    assert.ok(ids.has(p.start), `старт ${p.start} не найден в ${pr.id}`);
    for (const s of p.steps) {
      for (const f of s.next || []) {
        assert.ok(!f.to || ids.has(f.to), `${pr.id}: переход в никуда «${f.to}»`);
      }
    }
    // Каждый шаг достижим от старта — иначе заготовка учит плохому.
    const seen = new Set([p.start]);
    const stack = [p.start];
    while (stack.length) {
      // pop ВНЕ предиката: внутри find он опустошал бы стек на каждой
      // итерации — обход тогда «теряет» шаги и врёт про недостижимость.
      const id = stack.pop();
      const cur = p.steps.find((s) => s.id === id);
      for (const f of (cur && cur.next) || []) if (f.to && !seen.has(f.to)) { seen.add(f.to); stack.push(f.to); }
    }
    assert.equal(seen.size, p.steps.length, `${pr.id}: есть недостижимые шаги`);
  }
});

test('редактор рисует шаги, их переходы и кнопки добавления', () => {
  const P = api();
  const host = globalThis.document.createElement('div');
  const p = { start: '', steps: [] };
  P.addStep(p, 'agent', 'правка');
  P.addStep(p, 'shell', 'тесты');
  P.renderTo(host, p, () => {});
  const text = textOf(host);
  assert.match(text, /правка/);
  assert.match(text, /тесты/);
  assert.match(text, /добавить шаг/);
  assert.match(text, /заготовки/);
  assert.match(text, /старт/, 'первый шаг не помечен стартом');
});

test('заготовка из редактора заменяет черновик целиком', () => {
  const P = api();
  const host = globalThis.document.createElement('div');
  const p = { start: '', steps: [] };
  let changed = 0;
  P.renderTo(host, p, () => { changed += 1; });
  const btn = find(host, (n) => n.textContent === 'разведка → правка → тесты')[0];
  assert.ok(btn, 'кнопки заготовки нет');
  btn.listeners.click[0]();
  assert.ok(p.steps.length >= 4, 'заготовка не подставилась');
  assert.equal(p.start, 'разведка');
  assert.ok(changed > 0, 'конструктор не узнал о правке');
});
