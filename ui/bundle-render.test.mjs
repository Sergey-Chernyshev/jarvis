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
      getAttribute(k) { return node.attrs[k] ?? null; },
      removeAttribute(k) { delete node.attrs[k]; },
      contains(other) { return node === other || node.children.some(c => c.contains?.(other)); },
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
      bundlePlaces: async (machine) => ({
        ok: true, home: '/home/bob',
        known: machine === 'local' ? ['/home/bob/jarvis', '/home/bob/lct'] : ['/srv/app'],
      }),
      bundleBrowse: async (machine, path) => {
        const p = path || '/home/bob';
        return p === '/home/bob'
          ? { ok: true, path: p, parent: '/home', dirs: ['jarvis', 'проекты'] }
          : { ok: true, path: p, parent: '/home/bob', dirs: [] };
      },
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
  assert.ok(text.includes('команда · клевер-релиз'), 'шапки нет');
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
  assert.ok(text.includes('Создадим папку и Git-репозиторий'), 'обещание автоинициализации пропало');
  const sel = find(root, (n) => n.tag === 'select')[0];
  assert.ok(sel, 'выбора машины нет — снова поле по памяти');
  const opts = textOf(sel);
  assert.ok(opts.includes('Эта машина'), 'нет локальной машины');
  assert.ok(opts.includes('terminalka'), 'нет узла из настроек');
});

test('директория выбирается обзором, а не по памяти', async () => {
  const { root } = await load({ bundles: [] });
  const pick = find(root, (n) => n.textContent === 'выбрать…')[0];
  assert.ok(pick, 'кнопки обзора нет — снова ввод по памяти');
  pick.listeners.click[0]();
  await new Promise((r) => setTimeout(r, 0));
  await new Promise((r) => setTimeout(r, 0));

  const text = textOf(root);
  assert.ok(text.includes('/home/bob'), 'обзор не начался с дома машины');
  assert.ok(text.includes('известные проекты'), 'известных проектов нет');
  assert.ok(text.includes('jarvis'), 'известный проект не показан');

  // Спуск в подкаталог — по клику, как в любом проводнике.
  const row = find(root, (n) => n.classList.contains('bd-dir-row') && n.textContent === 'проекты')[0];
  assert.ok(row, 'подкаталог не показан');
  row.listeners.click[0]();
  await new Promise((r) => setTimeout(r, 0));
  assert.ok(textOf(root).includes('/home/bob/проекты'), 'спуск не сработал');

  // «Выбрать эту директорию» заполняет поле.
  const choose = find(root, (n) => n.textContent === 'Выбрать эту директорию')[0];
  choose.listeners.click[0]();
  await new Promise((r) => setTimeout(r, 0));
  const dirInput = find(root, (n) => n.tag === 'input' && n.value === '/home/bob/проекты')[0];
  assert.ok(dirInput, 'выбранный путь не попал в поле директории');
  assert.ok(!find(root, (n) => n.classList.contains('lp-shade')).length, 'обзор не закрылся после выбора');
});

test('известный проект узла выбирается одним кликом', async () => {
  const { root } = await load({ bundles: [] });
  // Выбираем узел — известные проекты должны стать узловыми.
  const sel = find(root, (n) => n.tag === 'select')[0];
  sel.value = 'terminalka';
  sel.listeners.change[0]();
  const pick = find(root, (n) => n.textContent === 'выбрать…')[0];
  pick.listeners.click[0]();
  await new Promise((r) => setTimeout(r, 0));
  await new Promise((r) => setTimeout(r, 0));
  const chip = find(root, (n) => n.classList.contains('lp-chip') && n.textContent === 'app')[0];
  assert.ok(chip, 'известный проект узла не показан');
  chip.listeners.click[0]();
  const dirInput = find(root, (n) => n.tag === 'input' && n.value === '/srv/app')[0];
  assert.ok(dirInput, 'клик по известному проекту не заполнил директорию');
});

test('transport failure preserves the new hand task and explains the failure', async () => {
  const { root, window } = await load(CONSOLE);
  const task = find(root, n => n.tag === 'textarea')[0];
  task.value = 'Сохранить мою задачу'; await task.listeners.input[0]({ target: task });
  window.jarvis.bundleAddHand = async () => { throw new Error('Узел не отвечает'); };
  await find(root, n => n.textContent === '+ исполнитель')[0].listeners.click[0]({});
  assert.equal(task.value, 'Сохранить мою задачу');
  assert.match(textOf(root), /Узел не отвечает/);
});


const fieldInput = (root, label) => {
  const field = find(root, n => n.classList.contains('bd-field') &&
    n.children.some(c => c.classList.contains('bd-label') && c.textContent === label))[0];
  assert.ok(field, `Missing field ${label}`);
  return find(field, n => n.tag === 'input')[0];
};
const fill = async (input, value) => {
  input.value = value;
  await input.listeners.input[0]({ target: input });
};

test('changing machine clears the selected directory, preserves the draft, and requires a new host path', async () => {
  const { root, calls } = await load({ bundles: [] });
  const name = fieldInput(root, 'Название команды');
  const amount = fieldInput(root, 'Ориентир расхода');
  const directory = fieldInput(root, 'директория');
  const task = find(root, n => n.tag === 'textarea')[0];
  await fill(name, 'Подготовка релиза');
  await fill(amount, '42000');
  await fill(task, 'Добавь проверки входа');
  const [machine, agent] = find(root, n => n.tag === 'select');
  agent.value = 'codex'; agent.listeners.change[0]();
  const picker = find(root, n => n.textContent === 'выбрать…')[0];
  await picker.listeners.click[0]();
  await new Promise(r => setTimeout(r, 0));
  await find(root, n => n.classList.contains('lp-chip') && n.textContent === 'jarvis')[0].listeners.click[0]();
  assert.equal(directory.value, '/home/bob/jarvis');

  machine.value = 'terminalka'; machine.listeners.change[0]();
  assert.equal(directory.value, '', 'a local path must not silently become a remote path');
  assert.equal(name.value, 'Подготовка релиза');
  assert.equal(amount.value, '42000');
  assert.equal(task.value, 'Добавь проверки входа');
  assert.equal(agent.value, 'codex');
  const start = find(root, n => n.textContent === 'Запустить 1 чат')[0];
  await start.listeners.click[0]();
  assert.equal(calls.length, 0, 'no save/start may run with an invalidated directory');
  assert.match(textOf(root), /Выберите директорию.*terminalka/);

  await picker.listeners.click[0]();
  await new Promise(r => setTimeout(r, 0));
  await find(root, n => n.classList.contains('lp-chip') && n.textContent === 'app')[0].listeners.click[0]();
  machine.listeners.change[0]();
  assert.equal(directory.value, '/srv/app', 'unchanged machine must keep its path');
  await start.listeners.click[0]();
  const saved = calls.find(c => c[0] === 'save')[1];
  assert.equal(saved.machine, 'terminalka');
  assert.equal(saved.dir, '/srv/app');
  assert.equal(saved.agent, 'codex');
  assert.equal(saved.name, 'Подготовка релиза');
  assert.equal(saved.budgetTokens, 42000);
  assert.equal(saved.hands[0].task, 'Добавь проверки входа');
  assert.deepEqual(saved.gates, DRAFT.gates);
  assert.deepEqual(calls.find(c => c[0] === 'start'), ['start', 'b1']);
});

test('legacy drafts use Claude and explicitly label advisory token units', async () => {
  const { root, calls } = await load({ bundles: [] });
  const agent = find(root, n => n.tag === 'select' && n.attrs['aria-label'] === 'Агент')[0];
  assert.equal(agent.value, 'claude');
  assert.match(textOf(agent), /Claude Code.*Codex/);
  assert.match(textOf(root), /проверим CLI на выбранной машине/);
  assert.match(textOf(root), /Токенов на исполнителя. Не ограничивает работу/);
  await fill(fieldInput(root, 'директория'), '/repo');
  await fill(find(root, n => n.tag === 'textarea')[0], 'Задача');
  await find(root, n => n.textContent === 'Запустить 1 чат')[0].listeners.click[0]();
  assert.equal(calls.find(c => c[0] === 'save')[1].agent, 'claude');

  const view = await load(CONSOLE);
  const meta = find(view.root, n => n.classList.contains('bd-card-meta'))[0];
  assert.match(textOf(meta), /Расход: 21\s000 токенов/);
  assert.match(textOf(meta), /Ориентир: 60\s000 токенов/);
  assert.ok(!textOf(meta).includes(' / '), 'an advisory estimate must not look like a hard token quota');
});
