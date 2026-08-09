/* Отрисовка режима «Циклы» на подставном DOM.
 *
 * Экран режима был пуст, а причина оказалась в помощнике `el`: вторым
 * аргументом он всегда считал атрибуты, а половина вызовов передавала туда
 * массив детей. Получалось имя атрибута «0» — недопустимое, — и исключение
 * уносило весь экран. Ни один тест на строки этого поймать не мог, поэтому
 * тут гоняется НАСТОЯЩАЯ отрисовка. */

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

/** DOM ровно в том объёме, который нужен модулю, — включая строгость настоящего. */
function makeDom() {
  const mk = (tag) => {
    const node = {
      tag,
      nodeType: 1,
      className: '',
      textContent: '',
      innerHTML: '',
      hidden: false,
      children: [],
      attrs: {},
      listeners: {},
      classList: {
        add(c) { node.className = `${node.className} ${c}`.trim(); },
        contains(c) { return node.className.split(' ').includes(c); },
      },
      appendChild(k) { node.children.push(k); return k; },
      addEventListener(ev, fn) { (node.listeners[ev] ||= []).push(fn); },
      setAttribute(k, v) {
        // Настоящий DOM бросает InvalidCharacterError на имя, начинающееся с
        // цифры. Без этой строгости подставной DOM проглотил бы ту самую
        // ошибку, ради которой тест и написан.
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
    return node;
  };
  return { createElement: (t) => mk(t) };
}

/** Собираем весь текст поддерева — по нему и проверяем, что нарисовалось. */
function textOf(node) {
  return [node.textContent || '', ...node.children.map(textOf)].join(' ');
}

/** Поднимаем модуль в песочнице с нашими document/window. */
async function loadLoops(state) {
  const src = readFileSync(new URL('./loops.js', import.meta.url), 'utf8');
  const document = makeDom();
  const calls = [];
  const window = {
    jarvis: {
      loopsGet: async () => ({ ok: true, ...state }),
      loopsDraft: async (t) => { calls.push(['draft', t]); return { ok: true, item: { id: '', name: '', exit: { gates: [], critic: {} }, source: {}, sandbox: {}, memory: {}, schedule: { wake: 'manual' }, limits: {}, sampling: {} } }; },
      loopsSave: async () => ({ ok: true, id: 'a', problems: [] }),
      loopsStart: async () => ({ ok: true }),
      loopsDiff: async () => ({ ok: true, diff: '' }),
      onLoopsState: () => {},
    },
  };
  const fn = new Function('document', 'window', `${src}; return window.initLoops;`);
  const init = fn(document, window);
  const root = document.createElement('div');
  init(root);
  // Первая отрисовка идёт с пустым состоянием, настоящее приезжает следом —
  // ровно как в приложении. Ждём этот круг, иначе проверяли бы заглушку.
  await new Promise((r) => setTimeout(r, 0));
  return { root, window, calls };
}

const TEMPLATES = [
  { id: 'test-fix', name: 'ночной test-fix', hint: '02:00 · до 20 итераций' },
  { id: 'triage', name: 'утренний триаж', hint: '07:00 · до 5 итераций' },
];

test('на пустом месте рисуется библиотека, а не пустой экран', async () => {
  const { root } = await loadLoops({ loops: [], templates: TEMPLATES });
  const text = textOf(root);
  assert.ok(text.includes('Библиотека шаблонов'), `экран пуст: «${text.slice(0, 120)}»`);
  assert.ok(text.includes('ночной test-fix'), 'шаблоны не нарисовались');
  assert.ok(text.includes('Собрать с нуля'), 'нет входа в конструктор с нуля');
});

test('вход в конструктор виден и когда циклы уже есть', async () => {
  const loop = {
    id: 'a', name: 'мой цикл', wakeLabel: 'каждый день в 02:00',
    problems: [], pendingReview: 0, run: null,
    exit: { gates: [], critic: {}, streak: 2 }, source: {}, sandbox: {},
    memory: {}, schedule: { wake: 'manual' }, limits: {}, sampling: { every: 3 },
  };
  const { root } = await loadLoops({ loops: [loop], templates: TEMPLATES });
  const text = textOf(root);
  assert.ok(text.includes('Новый цикл'), 'кнопка нового цикла пропала при непустом списке');
  assert.ok(text.includes('мой цикл'), 'свой цикл не показан');
  assert.ok(text.includes('из шаблона'), 'шаблоны недоступны из основного экрана');
});

test('конструктор открывается и показывает все пять шагов', async () => {
  const { root, window } = await loadLoops({ loops: [], templates: TEMPLATES });
  // «Собрать с нуля» — та же дорога, которой идёт человек.
  const scratch = root.querySelector('.lp-scratch');
  const btn = scratch.children.find((c) => c.textContent === 'Собрать с нуля');
  assert.ok(btn, 'кнопки «Собрать с нуля» нет');
  btn.listeners.click[0]();
  await new Promise((r) => setTimeout(r, 0));
  const text = textOf(root);
  assert.ok(text.includes('Новый цикл'), 'конструктор не открылся');
  for (const step of ['откуда берутся задачи', 'песочница агента', 'условие выхода', 'ограничители']) {
    assert.ok(text.includes(step), `в конструкторе нет шага «${step}»`);
  }
  assert.ok(text.includes('Создать цикл'), 'нечем сохранить новый цикл');
});
