/* Отрисовка режима «Связка» на подставном DOM.
 *
 * Проверяется настоящий модуль, а не строки исходника: старт с пачкой задач,
 * пересчёт кнопки запуска, пульт с очередью слияний и гейт кнопки «влить».
 * Подставной setAttribute строг к именам, как настоящий, — урок экрана
 * «Циклов», который молча падал именно там. */

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
      remove() {
        if (node.parent) node.parent.children = node.parent.children.filter((k) => k !== node);
        node.parent = null;
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
    // Настоящий DOM на запись textContent сносит всех детей — фейк обязан
    // делать то же, иначе перерисовки наслаиваются и тесты видят призраков.
    Object.defineProperty(node, 'textContent', {
      get: () => text,
      set: (v) => { text = String(v); node.children = []; },
    });
    return node;
  };
  return { createElement: (t) => mk(t), addEventListener() {}, removeEventListener() {} };
}

function textOf(node) {
  return [node.textContent || '', ...node.children.map(textOf)].join(' ');
}

function find(node, pred, out = []) {
  if (pred(node)) out.push(node);
  for (const k of node.children) find(k, pred, out);
  return out;
}

const DRAFT = {
  id: '', name: '', machine: 'local', dir: '', base: '',
  gates: [{ name: 'тесты', command: 'cargo test' }],
  budgetTokens: 60000, paused: false,
  hands: [{ task: '' }, { task: '' }, { task: '' }],
  events: [],
};

async function load(state) {
  const src = readFileSync(new URL('./bundle.js', import.meta.url), 'utf8');
  const document = makeDom();
  const calls = [];
  const window = {
    jarvis: {
      bundleGet: async () => ({ ok: true, ...state }),
      bundleDraft: async () => ({ ok: true, item: JSON.parse(JSON.stringify(DRAFT)) }),
      bundleSave: async (item) => { calls.push(['save', item]); return { ok: true, id: 'b1', problems: [] }; },
      bundleStart: async (id) => { calls.push(['start', id]); return { ok: true }; },
      bundleAddHand: async (id, task) => { calls.push(['hand', task]); return { ok: true }; },
      bundlePause: async () => ({ ok: true }),
      bundleMerge: async (id, hand) => { calls.push(['merge', hand]); return { ok: true }; },
      bundleRemove: async () => ({ ok: true }),
      onBundleState: () => {},
      machinesList: async () => [
        { id: 'local', name: 'Эта машина', kind: 'local' },
        { id: 'terminalka', name: 'terminalka', kind: 'remote', sshHost: 'desktop@149.33.48.114' },
      ],
    },
    openSessionById: (id) => calls.push(['open', id]),
  };
  const fn = new Function('document', 'window', `${src}; return window.initBundle;`);
  const init = fn(document, window);
  const root = document.createElement('div');
  init(root);
  await new Promise((r) => setTimeout(r, 0));
  await new Promise((r) => setTimeout(r, 0));
  return { root, window, calls };
}

test('первый вход — старт с пачкой задач, запуск считает только заполненные', async () => {
  const { root } = await load({ bundles: [] });
  const text = textOf(root);
  assert.ok(text.includes('несколько чатов разом'), `не старт: «${text.slice(0, 90)}»`);
  assert.ok(text.includes('ветка и worktree создаются сами'), 'обещание из дизайна пропало');

  const btn = find(root, (n) => (n.textContent || '').startsWith('Запустить'))[0];
  assert.ok(btn, 'кнопки запуска нет');
  assert.equal(btn.textContent, 'Запустить 0 чатов');
  assert.ok(btn.disabled, 'пустую связку нельзя запускать');

  // Человек заполнил две задачи — кнопка пересчиталась.
  const tasks = find(root, (n) => n.tag === 'textarea');
  assert.ok(tasks.length >= 3, 'рук меньше трёх');
  for (const [i, ta] of [tasks[0], tasks[1]].entries()) {
    ta.value = `задача ${i}`;
    ta.listeners.input[0]({ target: ta });
  }
  assert.equal(btn.textContent, 'Запустить 2 чата');
  assert.ok(!btn.disabled);
});

const CONSOLE = {
  bundles: [{
    id: 'b1', name: 'клевер-релиз', machine: 'local', dir: '/repo', base: 'main',
    gates: [], budgetTokens: 60000, paused: false, active: true,
    createdAt: 1, lastMergeAt: 0, problems: [],
    events: [{ at: 1, text: 'платёжка: конфликт при ребейзе' }],
    hotFiles: [{ file: 'shared/types.ts', hands: ['auth', 'платёжка'] }],
    hands: [
      { id: 'h1', name: 'auth', branch: 'team/auth', state: 'working', status: 'working', detail: 'верстает экран логина', tokens: 21000, sessionId: 's1', conflictFiles: [], attempt: 0, gatesOk: false, canMerge: false },
      { id: 'h2', name: 'редьюсер-тесты', branch: 'team/reducer', state: 'ready', status: 'done', detail: '', tokens: 30000, sessionId: 's2', queuePos: 1, conflictFiles: [], attempt: 0, gatesOk: true, canMerge: true },
      { id: 'h3', name: 'доки', branch: 'team/docs', state: 'ready', status: 'done', detail: '', tokens: 9000, sessionId: 's3', queuePos: 2, conflictFiles: [], attempt: 0, gatesOk: true, canMerge: false },
      { id: 'h4', name: 'платёжка', branch: 'team/billing', state: 'conflict', status: 'done', detail: '', tokens: 31000, sessionId: 's4', conflictFiles: ['shared/types.ts'], attempt: 2, gatesOk: false, canMerge: false },
    ],
  }],
};

test('пульт: очередь, гейт кнопки «влить», конфликт и горячие файлы', async () => {
  const { root, calls } = await load(CONSOLE);
  const text = textOf(root);
  assert.ok(text.includes('связка · клевер-релиз'), 'шапки нет');
  assert.ok(text.includes('верстает экран логина'), 'живой статус руки не показан');
  assert.ok(text.includes('чинит сам, попытка 2'), 'конфликт не рассказан человеческим языком');
  assert.ok(text.includes('shared/types.ts'), 'горячего файла нет');
  assert.ok(text.includes('встанут в очередь сами'), 'работающие не упомянуты в очереди');

  const merges = find(root, (n) => (n.textContent || '').startsWith('Влить в'));
  assert.equal(merges.length, 1, 'кнопка «влить» должна быть только у головы очереди');
  assert.ok(!merges[0].disabled, 'голова с зелёными гейтами должна вливаться');
  merges[0].listeners.click[0]({ stopPropagation() {} });
  await new Promise((r) => setTimeout(r, 0));
  assert.deepEqual(calls.filter((c) => c[0] === 'merge')[0], ['merge', 'h2']);
});

test('карточка руки открывает её чат', async () => {
  const { root, calls } = await load(CONSOLE);
  const card = find(root, (n) => n.classList.contains('bd-card'))[0];
  assert.ok(card, 'карточек нет');
  card.listeners.click[0]();
  assert.deepEqual(calls.filter((c) => c[0] === 'open')[0], ['open', 's1']);
});

test('голова без зелёных гейтов не вливается', async () => {
  const state = JSON.parse(JSON.stringify(CONSOLE));
  state.bundles[0].hands[1].canMerge = false;
  state.bundles[0].hands[1].gatesOk = false;
  const { root } = await load(state);
  const merge = find(root, (n) => (n.textContent || '').startsWith('Влить в'))[0];
  assert.ok(merge.attrs.disabled !== undefined || merge.disabled, 'кнопка обязана ждать зелёных гейтов');
});

test('машина выбирается из списка, директории хватает без git', async () => {
  const { root } = await load({ bundles: [] });
  const text = textOf(root);
  assert.ok(text.includes('директория'), 'поля директории нет');
  assert.ok(text.includes('создам и инициализирую сам'), 'обещание автоинициализации пропало');
  const sel = find(root, (n) => n.tag === 'select')[0];
  assert.ok(sel, 'выбора машины нет — снова поле по памяти');
  const opts = textOf(sel);
  assert.ok(opts.includes('Эта машина'), 'нет локальной машины');
  assert.ok(opts.includes('terminalka'), 'нет узла из настроек');
});
