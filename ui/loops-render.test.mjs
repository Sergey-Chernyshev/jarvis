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
import { createRequire } from 'node:module';

/* Конструктор пайплайна — отдельный модуль, и в приложении он приезжает
 * глобалом. В песочнице теста его надо подать руками, иначе редактор графа
 * молча не нарисуется и проверять будет нечего. */
const JarvisPipeline = createRequire(import.meta.url)('./pipeline.js');

/** DOM ровно в том объёме, который нужен модулю, — включая строгость настоящего. */
function makeDom() {
  const mk = (tag) => {
    let text = '';
    const node = {
      tag,
      nodeType: 1,
      className: '',
      innerHTML: '',
      hidden: false,
      value: '',
      parent: null,
      children: [],
      attrs: {},
      listeners: {},
      classList: {
        add(c) { node.className = `${node.className} ${c}`.trim(); },
        remove(c) { node.className = node.className.split(' ').filter((x) => x !== c).join(' '); },
        toggle(c, on) { if (on) this.add(c); else this.remove(c); },
        contains(c) { return node.className.split(' ').includes(c); },
      },
      remove() {
        if (node.parent) node.parent.children = node.parent.children.filter((k) => k !== node);
        node.parent = null;
      },
      appendChild(k) { node.children.push(k); k.parent = node; return k; },
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
    // Настоящий DOM на запись textContent сносит всех детей — фейк обязан
    // делать то же, иначе перерисовки наслаиваются и тесты видят призраков.
    Object.defineProperty(node, 'textContent', {
      get: () => text,
      set: (v) => { text = String(v); node.children = []; },
    });
    return node;
  };
  return {
    createElement: (t) => mk(t),
    // Каталог вешает Esc на документ — заглушек достаточно.
    addEventListener() {},
    removeEventListener() {},
  };
}

/** Найти в поддереве все узлы, прошедшие проверку. */
function find(node, pred, out = []) {
  if (pred(node)) out.push(node);
  for (const k of node.children) find(k, pred, out);
  return out;
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
      loopsDraft: async (t) => { calls.push(['draft', t]); return { ok: true, item: { id: '', name: '', agent: 'claude', exit: { gates: [], critic: { enabled: true, model: 'opus' } }, source: {}, sandbox: {}, memory: {}, schedule: { wake: 'manual' }, limits: {}, sampling: {} } }; },
      loopsSave: async (item) => {
        calls.push(['save', item]);
        // Мок ведёт себя как настоящий стор: сохранённый цикл появляется в
        // состоянии. Иначе панель после сохранения искала бы несуществующий
        // цикл — и проверялся бы не тот путь, что в жизни.
        const saved = { ...item, id: item.id || 'a', run: null, problems: [] };
        state.loops = [...(state.loops || []).filter((x) => x.id !== saved.id), saved];
        return { ok: true, id: saved.id, problems: [] };
      },
      loopsStart: async () => ({ ok: true }),
      loopsDiff: async () => ({ ok: true, diff: '' }),
      loopsCompose: async (text, item) => {
        calls.push(['compose', text, item]);
        return {
          ok: true,
          problems: [],
          item: {
            id: item && item.id, name: 'ночной test-fix', agent: 'claude',
            source: { goal: 'чинить флаки', command: 'cargo test' },
            sandbox: { repo: '/repo', branch: 'loop/{name}-{n}', worktree: true },
            exit: { gates: [{ name: 'тесты', command: 'cargo test' }], critic: { enabled: true, model: 'opus' }, streak: 2 },
            memory: { enabled: true, file: 'notes.md' },
            schedule: { wake: { daily: { at: '02:00' } }, resumeAfterLimit: true, keepAwake: true },
            limits: { tokens: 200000, iterations: 20, minutes: 480, stopOnDrift: true },
            sampling: { every: 3 },
          },
        };
      },
      loopsCatalog: async () => ({
        ok: true,
        models: {
          claude: [{ id: 'fable', label: 'Fable' }, { id: 'opus', label: 'Opus' }],
          codex: [{ id: 'gpt-5.5', label: 'GPT-5.5' }],
        },
        presets: [
          { id: 'src-gh-label', slot: 'source', category: 'GitHub', name: 'issue с меткой agent', hint: 'классика', command: 'gh issue list --label agent' },
          { id: 'gate-cargo-test', slot: 'gate', category: 'Rust', name: 'тесты', hint: 'все тесты', command: 'cargo test' },
        ],
      }),
      onLoopsState: () => {},
      loopsBpmnExport: async (id, path, open) => { calls.push(['bpmn-export', id, path, open]); return { ok: true, path: '/tmp/x.bpmn' }; },
      loopsBpmnImport: async (id) => { calls.push(['bpmn-import', id]); return { ok: true, problems: [] }; },
      loopsBpmnUnlink: async (id) => { calls.push(['bpmn-unlink', id]); return { ok: true }; },
    },
  };
  // Конструктор пайплайна берёт `document` глобалом — он отдельный модуль и
  // про песочницу loops.js не знает. Подставляем ТОТ ЖЕ подставной документ,
  // иначе граф рисовался бы в пустоту.
  globalThis.document = document;
  const fn = new Function('document', 'window', 'JarvisPipeline', `${src}; return window.initLoops;`);
  const init = fn(document, window, JarvisPipeline);
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

test('модель критика выбирается из списка, а не вводится по памяти', async () => {
  const { root } = await loadLoops({ loops: [], templates: TEMPLATES });
  const scratch = root.querySelector('.lp-scratch');
  scratch.children.find((c) => c.textContent === 'Собрать с нуля').listeners.click[0]();
  await new Promise((r) => setTimeout(r, 0));
  const selects = find(root, (n) => n.tag === 'select');
  const model = selects.find((sel) => n2text(sel).includes('Opus'));
  assert.ok(model, 'селекта моделей нет — значит опять поле по памяти');
  assert.ok(n2text(model).includes('Fable'), 'в списке нет Fable');
});

test('заготовка из каталога вставляется в источник и попадает в объяснение', async () => {
  const { root } = await loadLoops({ loops: [], templates: TEMPLATES });
  root.querySelector('.lp-scratch').children.find((c) => c.textContent === 'Собрать с нуля').listeners.click[0]();
  await new Promise((r) => setTimeout(r, 0));
  assert.ok(textOf(root).includes('источник задач пока пуст'), 'объяснение не отражает пустой источник');

  const fromCatalog = find(root, (n) => n.textContent === 'из каталога')[0];
  assert.ok(fromCatalog, 'кнопки каталога у источника нет');
  fromCatalog.listeners.click[0]();
  await new Promise((r) => setTimeout(r, 0));
  assert.ok(textOf(root).includes('issue с меткой agent'), 'карточки каталога не нарисовались');

  const card = find(root, (n) => n.classList.contains('lp-cat-card'))[0];
  card.listeners.click[0]();
  await new Promise((r) => setTimeout(r, 0));
  assert.ok(textOf(root).includes('возьмёт задачи у команды-источника'),
    'после выбора заготовки объяснение не пересчиталось');
  assert.ok(!find(root, (n) => n.classList.contains('lp-shade')).length, 'каталог не закрылся после выбора');
});

test('пайплайн: параллель рисуется шлюзами и честно описывается словами', async () => {
  const { root, calls } = await loadLoops({ loops: [], templates: TEMPLATES });
  root.querySelector('.lp-scratch').children.find((c) => c.textContent === 'Собрать с нуля').listeners.click[0]();
  await new Promise((r) => setTimeout(r, 0));

  // Переключаемся в режим графа.
  find(root, (n) => n.textContent === 'пайплайн')[0].listeners.click[0]();
  await new Promise((r) => setTimeout(r, 0));
  assert.ok(textOf(root).includes('Пайплайн пуст'), 'редактор графа не нарисовался');

  // Параллель одной кнопкой — и она обязана появиться на экране целиком.
  find(root, (n) => n.textContent === '∥ параллель')[0].listeners.click[0]();
  await new Promise((r) => setTimeout(r, 0));
  const text = textOf(root);
  assert.ok(text.includes('ветвление') && text.includes('слияние'), `шлюзов не видно: ${text.slice(0, 200)}`);
  assert.ok(text.includes('конфликт'), 'у слияния не спрашивается правило конфликта');

  // Абзац «как это будет работать» обязан говорить про ГРАФ, а не про цель и
  // гейты, которых у пайплайна нет.
  assert.ok(text.includes('пройдёт пайплайн из'), `объяснение осталось про линейный цикл: ${text.slice(0, 300)}`);
  assert.ok(text.includes('каждая в своём worktree'), 'про отдельные деревья веток не сказано');
  assert.ok(!text.includes('источник задач пока пуст'), 'объяснение жалуется на поля, которых в пайплайне нет');

  // И обмен с модельером на месте.
  const out = find(root, (n) => n.textContent === 'Выгрузить в .bpmn')[0];
  assert.ok(out, 'кнопки выгрузки в .bpmn нет');
  out.listeners.click[0]();
  await new Promise((r) => setTimeout(r, 0));
  assert.ok(calls.some((c) => c[0] === 'bpmn-export'), 'выгрузка не дошла до моста');
});

test('гейт из каталога добавляется с именем и командой', async () => {
  const { root } = await loadLoops({ loops: [], templates: TEMPLATES });
  root.querySelector('.lp-scratch').children.find((c) => c.textContent === 'Собрать с нуля').listeners.click[0]();
  await new Promise((r) => setTimeout(r, 0));
  const add = find(root, (n) => n.textContent === '+ из каталога')[0];
  assert.ok(add, 'кнопки «+ из каталога» нет');
  add.listeners.click[0]();
  await new Promise((r) => setTimeout(r, 0));
  const card = find(root, (n) => n.classList.contains('lp-cat-card'))[0];
  assert.ok(n2text(card).includes('cargo test'), 'в каталоге гейтов нет cargo test');
  card.listeners.click[0]();
  await new Promise((r) => setTimeout(r, 0));
  assert.ok(textOf(root).includes('прогонит гейт'), 'объяснение не увидело новый гейт');
});

/** Текст узла с детьми — как textOf, но от произвольного корня. */
function n2text(node) {
  return [node.textContent || '', ...node.children.map(n2text)].join(' ');
}

test('описание словами заполняет форму, но ничего не сохраняет', async () => {
  const { root, calls } = await loadLoops({ loops: [], templates: TEMPLATES });
  root.querySelector('.lp-scratch').children.find((c) => c.textContent === 'Собрать с нуля').listeners.click[0]();
  await new Promise((r) => setTimeout(r, 0));

  const area = find(root, (n) => n.classList.contains('lp-ask-text'))[0];
  assert.ok(area, 'поля описания нет');
  const go = find(root, (n) => n.textContent === 'Заполнить за меня')[0];
  assert.ok(go, 'кнопки сборки нет');

  // Слишком короткое описание модель не тревожит.
  area.value = 'ага';
  go.listeners.click[0]();
  await new Promise((r) => setTimeout(r, 0));
  assert.ok(!calls.some((c) => c[0] === 'compose'), 'позвали модель на пустяке');

  area.value = 'каждую ночь чинить флаки-тесты';
  go.listeners.click[0]();
  await new Promise((r) => setTimeout(r, 0));

  const compose = calls.find((c) => c[0] === 'compose');
  assert.ok(compose, 'модель не позвали');
  assert.ok(compose[2] && typeof compose[2] === 'object', 'заготовку не передали — выбор человека потеряется');

  const text = textOf(root);
  assert.ok(text.includes('ночной test-fix'), 'имя из ответа не попало в форму');
  assert.ok(text.includes('прогонит гейт'), 'объяснение не увидело заполненные поля');
  // Ключевое: заполнение — это черновик, а не сохранение.
  assert.ok(!calls.some((c) => c[0] === 'save'), 'форма сохранилась сама, без подтверждения');
});
