/* Панель целиком в настоящем DOM: разметка, скрипты и клики.
 *
 * Остальные тесты проверяют модули на подставном DOM — это ловит логику, но не
 * ловит того, чем панель ломается на живом маке: разъехавшиеся id, порядок
 * скриптов, кнопка, которая никуда не подключена. Здесь грузится НАСТОЯЩИЙ
 * index.html со всеми скриптами (кроме моста к Tauri — его подменяем), и по
 * кнопкам действительно кликают.
 *
 * Это не замена запуску приложения, но именно то, что можно проверить в CI на
 * macOS: панель поднимается, чат открывается, новые экраны открываются и
 * спрашивают у бэкенда ровно то, что должны.
 */

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { parseHTML } from 'linkedom';

const HERE = new URL('./', import.meta.url);
const read = (name) => readFileSync(new URL(name, HERE), 'utf8');

/* Скрипты панели в том же порядке, что в index.html. bridge.js пропускаем: он
 * зовёт Tauri, которого в тесте нет, — вместо него ставим свой window.jarvis. */
const SCRIPTS = [
  'theme.js',
  // подписи клавиш: renderer зовёт window.jarvisKeys уже при первой отрисовке
  'keys.js',
  'markdown.js',
  'diffview.js',
  // каталог агентов: renderer и settings2 спрашивают его уже на первой отрисовке
  'agents.js',
  'question-answer.js',
  'settings2.js',
  'voice-history.js',
  'loops.js',
  'bundle.js',
  'changes.js',
  'search.js',
  // чат с главным агентом: вкладка «Джарвис» зовёт его initAgentChat
  'agent-chat.js',
  'renderer.js',
];

/* Мост-заглушка. Любой неизвестный метод возвращает разумную пустоту, иначе
 * тест превратился бы в список из сотни моков и ломался бы от каждой新ой
 * команды. Вызовы записываем — по ним и проверяем, что кнопки делают дело. */
function makeBridge(calls, data = {}) {
  const subs = {};
  const target = {
    getState: async () => data.state || [],
    getSettings: async () => ({}),
    getMeta: async () => ({ version: 'test' }),
    getLimit: async () => null,
    sessionChanges: async () => ({ ok: true, branch: 'dev', files: data.files || [] }),
    sessionChangeDiff: async () => ({ ok: true, hunks: [] }),
    sessionSearch: async () => ({ ok: true, hits: data.hits || [], capped: false }),
    // Открытие чата отдаёт ленту ходов — панель сразу читает items/spans.
    openChat: async () => ({ ok: true, items: [], spans: [], cards: {} }),
    chatHistory: async () => ({ ok: true, items: [] }),
    // Списочные команды обязаны отдавать список: панель зовёт .find/.map по
    // ним прямо на старте, и объект вместо массива гасит весь запуск.
    getPlugins: async () => [],
    getModels: async () => [],
    getAgents: async () => [],
    machinesList: async () => [{ id: 'local', name: 'Эта машина', kind: 'local' }],
    getCommands: async () => [],
    getPrompts: async () => [],
    transcriptsGet: async () => [],
    historyGet: async () => ({ ok: true, items: [] }),
    loopsGet: async () => ({ ok: true, loops: [] }),
    bundleGet: async () => ({ ok: true, bundles: [] }),
    // главный агент: нить разговора и прошлая переписка
    agentChatState: async () => ({ sessionId: data.agentSession || null }),
    agentChatHistory: async () => data.agentHistory || { ok: true, items: [] },
    // своё имя чата: по умолчанию удачно, тест на отказ подсовывает свой ответ
    renameSession: async (id, title) => data.rename || { ok: true, name: title || null, title },
  };
  return new Proxy(target, {
    get(obj, prop) {
      if (typeof prop !== 'string') return undefined;
      if (prop in obj) {
        return (...args) => {
          calls.push([prop, ...args]);
          return obj[prop](...args);
        };
      }
      // on*(cb) — подписка: запоминаем, чтобы тест мог толкнуть событие.
      if (/^on[A-Z]/.test(prop)) {
        return (cb) => {
          subs[prop] = cb;
          return () => {};
        };
      }
      return (...args) => {
        calls.push([prop, ...args]);
        return Promise.resolve({ ok: true });
      };
    },
  });
}

async function boot(data = {}) {
  // linkedom, а не jsdom: нам нужен настоящий DOM с событиями, а не браузер.
  // Скрипты панели выполняем сами — ровно те же файлы, что грузит index.html.
  const { window, document } = parseHTML(read('index.html'));
  const calls = [];
  const subs = {};
  // Подписки ловим отдельно: Proxy отдаёт функцию, а нам нужен сам колбэк.
  window.jarvis = new Proxy(makeBridge(calls, data), {
    get(obj, prop) {
      if (typeof prop === 'string' && /^on[A-Z]/.test(prop)) {
        return (cb) => { subs[prop] = cb; return () => {}; };
      }
      return obj[prop];
    },
  });
  window.matchMedia = window.matchMedia || (() => ({ matches: false, addEventListener() {}, removeEventListener() {} }));
  window.ResizeObserver = window.ResizeObserver || class { observe() {} unobserve() {} disconnect() {} };
  window.IntersectionObserver = window.IntersectionObserver || class { observe() {} unobserve() {} disconnect() {} };
  window.scrollTo = () => {};
  // Панель помнит мелочи (тема, размеры) в localStorage; в тесте достаточно
  // честной пустой памяти, иначе первый же скрипт падает на ReferenceError.
  const memory = new Map();
  window.localStorage = {
    getItem: (k) => (memory.has(k) ? memory.get(k) : null),
    setItem: (k, v) => memory.set(k, String(v)),
    removeItem: (k) => memory.delete(k),
    clear: () => memory.clear(),
  };
  window.requestAnimationFrame = (cb) => setTimeout(() => cb(Date.now()), 0);
  window.cancelAnimationFrame = (id) => clearTimeout(id);
  document.hasFocus = () => true;

  for (const name of SCRIPTS) {
    // Скрипт панели, выполненный в её окне: ошибка здесь — ровно та, от
    // которой на маке остаётся белый экран.
    const fn = new Function(
      'window', 'document', 'globalThis', 'localStorage', 'navigator',
      'setTimeout', 'clearTimeout', 'setInterval', 'clearInterval',
      'requestAnimationFrame', 'cancelAnimationFrame',
      read(name)
    );
    fn(
      window, document, window, window.localStorage, window.navigator || { platform: 'MacIntel' },
      // Периодику панели глушим: она нужна живому окну, а тест иначе никогда
      // не закончится — процесс держат её таймеры.
      setTimeout, clearTimeout, () => 0, () => {},
      window.requestAnimationFrame, window.cancelAnimationFrame
    );
  }
  // Дать промисам старта (getState/getSettings/getMeta) отработать.
  await new Promise((r) => setTimeout(r, 0));
  return { window, doc: document, calls, subs };
}

/* Клик и клавиша: linkedom даёт Event, а специализированных конструкторов у
 * него нет — обработчики панели смотрят только на key и на всплытие. */
function click(doc) {
  return new doc.defaultView.Event('click', { bubbles: true });
}

function key(doc, k) {
  const e = new doc.defaultView.Event('keydown', { bubbles: true });
  e.key = k;
  return e;
}

const SESSION = {
  id: 's1',
  status: 'waiting',
  detail: 'ждёт ответа',
  project: 'jarvis',
  cwd: '/srv/jarvis',
  agent: 'claude',
  tmuxPane: '%1',
  updatedAt: Date.now(),
  createdAt: Date.now(),
};

test('панель поднимается со всеми скриптами и рисует список сессий', async () => {
  const { doc, subs } = await boot({ state: [SESSION] });
  assert.ok(doc.getElementById('list'), 'корень списка не найден');
  // Толкаем состояние тем же путём, каким его шлёт демон.
  assert.ok(subs.onState, 'панель не подписалась на состояние');
  subs.onState([SESSION]);
  await new Promise((r) => setTimeout(r, 0));
  const text = doc.getElementById('list').textContent;
  assert.match(text, /jarvis/, 'сессия не появилась в списке: ' + text);
});

/* Третий агент — такой же житель списка: бейдж с его именем, модель короткой
 * подписью из каталога, а не сырым id вроде «kimi-code/k3-256k». */
test('сессия третьего агента рисуется бейджами, а не многоточием', async () => {
  const kimi = { ...SESSION, id: 's2', agent: 'kimi', model: 'kimi-code/k3-256k' };
  const { doc, subs } = await boot({ state: [kimi] });
  subs.onState([kimi]);
  await new Promise((r) => setTimeout(r, 0));
  const badges = [...doc.querySelectorAll('#list .badge')].map((b) => b.textContent);
  assert.ok(badges.includes('kimi'), 'бейджа агента нет: ' + badges.join(' | '));
  assert.ok(badges.includes('K3-256k'), 'модель показана сырым id: ' + badges.join(' | '));
});

test('новые кнопки есть в шапке чата и подключены к своим экранам', async () => {
  const { doc, window } = await boot();
  for (const id of ['changesBtn', 'searchBtn', 'previewBtn']) {
    assert.ok(doc.getElementById(id), `кнопки ${id} нет в разметке`);
  }
  for (const id of ['chgWrap', 'srchWrap', 'chgBody', 'srchBody']) {
    assert.ok(doc.getElementById(id), `панели ${id} нет в разметке`);
  }
  // Скрипты экранов загрузились и объявили себя — иначе клик дал бы
  // ReferenceError уже на живом маке.
  assert.equal(typeof window.JarvisChanges, 'object');
  assert.equal(typeof window.JarvisSearch, 'object');
});

test('клик по «изменениям» открывает панель и спрашивает свод у бэкенда', async () => {
  const { doc, calls, subs } = await boot({
    state: [SESSION],
    files: [{ path: 'src/main.rs', state: 'изменён', added: 3, removed: 1, untracked: false }],
  });
  subs.onState([SESSION]);
  await new Promise((r) => setTimeout(r, 0));
  // Открываем чат так же, как человек: кликом по строке списка.
  const row = doc.querySelector('#list .row, #list [data-id], #list > *');
  assert.ok(row, 'строки сессии нет');
  row.dispatchEvent(click(doc));
  await new Promise((r) => setTimeout(r, 0));

  const btn = doc.getElementById('changesBtn');
  assert.equal(btn.hidden, false, 'кнопка изменений скрыта у сессии с каталогом');
  btn.dispatchEvent(click(doc));
  await new Promise((r) => setTimeout(r, 10));

  assert.equal(doc.getElementById('chgWrap').hidden, false, 'панель изменений не открылась');
  assert.ok(calls.some((c) => c[0] === 'sessionChanges'), 'свод у бэкенда не спрошен: ' + JSON.stringify(calls.map((c) => c[0])));
  assert.match(doc.getElementById('chgBody').textContent, /src\/main\.rs/, 'файл не показан');
});

test('клик по «поиску» открывает панель и ищет по проекту', async () => {
  const { doc, calls, subs } = await boot({
    state: [SESSION],
    hits: [{ path: 'src/main.rs', line: 7, text: 'fn искомое() {}' }],
  });
  subs.onState([SESSION]);
  await new Promise((r) => setTimeout(r, 0));
  doc.querySelector('#list > *').dispatchEvent(click(doc));
  await new Promise((r) => setTimeout(r, 0));

  doc.getElementById('searchBtn').dispatchEvent(click(doc));
  await new Promise((r) => setTimeout(r, 10));
  assert.equal(doc.getElementById('srchWrap').hidden, false, 'панель поиска не открылась');

  const input = doc.querySelector('#srchBody .srch-input');
  assert.ok(input, 'поля поиска нет');
  input.value = 'искомое';
  doc.querySelector('#srchBody .srch-go').dispatchEvent(click(doc));
  await new Promise((r) => setTimeout(r, 10));
  assert.ok(calls.some((c) => c[0] === 'sessionSearch' && c[2] === 'искомое'), 'запрос не ушёл');
  assert.match(doc.getElementById('srchBody').textContent, /fn искомое/, 'находка не показана');
});

test('Esc закрывает открытый экран, а не выкидывает из чата', async () => {
  const { doc, subs } = await boot({ state: [SESSION] });
  subs.onState([SESSION]);
  await new Promise((r) => setTimeout(r, 0));
  doc.querySelector('#list > *').dispatchEvent(click(doc));
  await new Promise((r) => setTimeout(r, 0));
  doc.getElementById('changesBtn').dispatchEvent(click(doc));
  await new Promise((r) => setTimeout(r, 10));

  doc.defaultView.dispatchEvent(key(doc, 'Escape'));
  await new Promise((r) => setTimeout(r, 0));
  assert.equal(doc.getElementById('chgWrap').hidden, true, 'Esc не закрыл панель изменений');
  assert.equal(doc.getElementById('chat').hidden, false, 'Esc заодно выкинул из чата');
});

/* ---------- вкладка «Джарвис»: разговор с главным агентом ----------
 *
 * Ради этого вкладка и заведена: раньше агент жил отдельным окном, и открывалось
 * оно с пустой лентой — даже когда разговор продолжался. Пустота молча и есть
 * то, что ловят эти тесты. */

// открыть вкладку так же, как человек: кликом по ней
async function openAgent(doc) {
  doc.getElementById('tabAgent').dispatchEvent(click(doc));
  await new Promise((r) => setTimeout(r, 0));
  return doc.getElementById('agLog');
}

test('вкладка «Джарвис» есть в панели и переключает на неё', async () => {
  const { doc } = await boot();
  const tab = doc.getElementById('tabAgent');
  assert.ok(tab, 'вкладки нет в разметке');
  assert.equal(doc.getElementById('agentPane').hidden, true, 'пане показан до клика');

  await openAgent(doc);
  assert.equal(doc.getElementById('agentPane').hidden, false, 'вкладка не открылась');
  assert.ok(tab.classList.contains('active'), 'вкладка не подсвечена');
  assert.equal(doc.getElementById('list').hidden, true, 'список сессий не уступил место');
  // соседние вкладки цифры не потеряли
  const keys = [...doc.querySelectorAll('.tabs .tabkey')].map((k) => k.getAttribute('data-key'));
  assert.deepEqual(keys, ['1', '2', '3', '4', '5', '6', '7'], 'номера вкладок разъехались: ' + keys.join(','));
});

test('цифра вкладки не декорация: ⌘7 открывает Джарвиса', async () => {
  const { doc } = await boot();
  const e = key(doc, '7');
  e.metaKey = true;
  e.ctrlKey = true; // isMod смотрит на ⌘ или Ctrl — зависит от ОС
  doc.defaultView.dispatchEvent(e);
  await new Promise((r) => setTimeout(r, 0));
  assert.equal(doc.getElementById('agentPane').hidden, false, 'хоткей вкладки ничего не открыл');
});

test('прошлая переписка рисуется в ленте, а не остаётся у агента в памяти', async () => {
  const { doc } = await boot({
    agentSession: 's-42',
    agentHistory: {
      ok: true,
      sessionId: 's-42',
      total: 3,
      items: [
        { role: 'user', kind: 'text', text: 'сколько сессий висит', ts: 1 },
        { role: 'assistant', kind: 'tool', text: 'state_get', ts: 2 },
        { role: 'assistant', kind: 'text', text: 'три, одна ждёт ответа', ts: 3 },
      ],
    },
  });
  const log = await openAgent(doc);

  assert.match(log.textContent, /сколько сессий висит/, 'реплики человека нет в ленте');
  assert.match(log.textContent, /три, одна ждёт ответа/, 'ответа агента нет в ленте');
  assert.ok(log.querySelector('.msg.user .bubble'), 'реплика человека не отличима от ответа');
  assert.ok(log.querySelector('.msg.assistant .bubble'), 'ответ агента нарисован не как реплика');
  // тул-вызов — чипом чата сессии, а не обычным текстом
  const chip = log.querySelector('.msg.tools .chip .tverb');
  assert.ok(chip, 'вызов инструмента нарисован текстом: ' + log.innerHTML);
  assert.equal(chip.textContent, 'state_get');
  assert.equal(doc.getElementById('agTag').hidden, false, 'нет метки продолжения при живом разговоре');
});

test('причина пустой ленты названа словами, а не показана пустотой', async () => {
  const { doc } = await boot({
    agentSession: 's-42',
    agentHistory: { ok: true, sessionId: 's-42', items: [], reason: 'транскрипт не найден — история недоступна' },
  });
  const log = await openAgent(doc);
  assert.match(log.textContent, /транскрипт не найден/, 'причина проглочена: ' + log.textContent);
});

test('разговора ещё не было — приглашение, а не вид поломки', async () => {
  const { doc } = await boot({ agentHistory: { ok: true, items: [], reason: 'нет сохранённого разговора' } });
  const log = await openAgent(doc);
  assert.equal(doc.getElementById('agTag').hidden, true, 'метка продолжения врёт про пустую нить');
  // «нет сохранённого разговора» — не беда, а нормальное первое открытие
  assert.doesNotMatch(log.textContent, /нет сохранённого разговора/);
  assert.match(log.textContent, /Спроси Джарвиса/, 'приглашения писать нет: ' + log.textContent);
});

test('поток ответа во вкладке — тот же, что в отдельном окне', async () => {
  const { doc, subs } = await boot();
  const log = await openAgent(doc);
  assert.ok(subs.onAgentEvent, 'вкладка не подписалась на поток агента');

  subs.onAgentEvent({ type: 'delta', text: 'ду' });
  subs.onAgentEvent({ type: 'delta', text: 'маю' });
  subs.onAgentEvent({ type: 'tool_use', name: 'state_get' });
  subs.onAgentEvent({ type: 'done', session_id: 's-new' });
  await new Promise((r) => setTimeout(r, 0));

  assert.match(log.textContent, /думаю/, 'дельты не собрались в один пузырь');
  assert.ok(log.querySelector('.msg.tools .chip'), 'тул-вызов из потока не показан чипом');
  assert.equal(doc.getElementById('agSend').disabled, false, 'вкладка осталась в «думает…»');
});

test('сообщение из вкладки уходит в ту же нить разговора', async () => {
  const { doc, calls } = await boot({ agentSession: 's-42' });
  await openAgent(doc);
  doc.getElementById('agInput').value = 'привет';
  doc.getElementById('agSend').dispatchEvent(click(doc));
  await new Promise((r) => setTimeout(r, 0));
  assert.ok(
    calls.some((c) => c[0] === 'agentSend' && c[1] === 'привет' && c[2] === 's-42'),
    'реплика ушла мимо продолжаемого разговора: ' + JSON.stringify(calls.filter((c) => c[0] === 'agentSend')),
  );
});

/* Своё имя чата (спека «имена чатов»): имя сильнее автозаголовка, правится из
 * строки и из меню действий, снимается пустым значением, а отказ — со словами. */

// Сессия, которой человек уже дал имя: title приходит с бэкенда уже собранным.
const NAMED = {
  ...SESSION,
  status: 'done',
  detail: '',
  name: 'БД',
  autoTitle: 'Fix the migration parser',
  title: 'БД',
};

async function withList(data) {
  const b = await boot(data);
  b.subs.onState(data.state);
  await new Promise((r) => setTimeout(r, 0));
  return b;
}

// Открыть правку через меню действий (⌘K) — тот же путь, что у человека.
function openRename(doc, re = /чату имя|Переименовать чат/) {
  doc.getElementById('actionsBtn').dispatchEvent(click(doc));
  const item = [...doc.querySelectorAll('#actionsPop .ap-item')].find((r) => re.test(r.textContent));
  assert.ok(item, 'пункта переименования нет в меню: ' + doc.getElementById('actionsPop').textContent);
  item.dispatchEvent(click(doc));
  return doc.querySelector('#list input.rename');
}

test('имя чата стоит в строке и не дублируется автозаголовком', async () => {
  const { doc } = await withList({ state: [NAMED] });
  const chip = doc.querySelector('#list .badge.chatname');
  assert.ok(chip, 'имени чата в строке нет');
  assert.equal(chip.textContent, 'БД');
  const summary = doc.querySelector('#list .summary').textContent;
  assert.ok(!summary.includes('Fix the migration'), 'автозаголовок повторяет имя: ' + summary);

  // безымянная сессия живёт как раньше — на автозаголовке
  const { doc: d2 } = await withList({ state: [{ ...SESSION, status: 'done', detail: '', title: 'Авто' }] });
  assert.equal(d2.querySelector('#list .badge.chatname'), null, 'чип появился без имени');
  assert.match(d2.querySelector('#list .summary').textContent, /Авто/);
});

test('имя чата ищется наравне с проектом', async () => {
  const { doc } = await withList({ state: [NAMED, { ...SESSION, id: 's2', project: 'другое' }] });
  const q = doc.getElementById('query');
  q.value = 'бд';
  q.dispatchEvent(new doc.defaultView.Event('input', { bubbles: true }));
  await new Promise((r) => setTimeout(r, 0));
  const rows = [...doc.querySelectorAll('#list .row')];
  assert.equal(rows.length, 1, 'по имени чата нашлось не то: ' + doc.getElementById('list').textContent);
  assert.equal(rows[0].dataset.sid, 's1');
});

test('правка имени уходит в демон по ↵ и не гасится пушем состояния', async () => {
  const { doc, calls, subs } = await withList({ state: [NAMED] });
  const inp = openRename(doc, /Переименовать чат/);
  assert.ok(inp, 'поле правки не открылось');
  assert.equal(inp.value, 'БД', 'в поле не подставлено текущее имя');

  // демон продолжает слать состояние — поле обязано пережить это
  subs.onState([NAMED]);
  await new Promise((r) => setTimeout(r, 0));
  assert.ok(doc.querySelector('#list input.rename'), 'пуш состояния стёр поле правки');

  inp.value = 'Миграции';
  inp.dispatchEvent(key(doc, 'Enter'));
  await new Promise((r) => setTimeout(r, 0));
  const call = calls.find((c) => c[0] === 'renameSession');
  assert.ok(call, 'имя никуда не ушло');
  assert.deepEqual([call[1], call[2]], ['s1', 'Миграции']);
});

test('пустое значение снимает имя и возвращает автозаголовок', async () => {
  const { doc, calls } = await withList({ state: [NAMED] });
  const inp = openRename(doc, /Переименовать чат/);
  inp.value = '   ';
  inp.dispatchEvent(key(doc, 'Enter'));
  await new Promise((r) => setTimeout(r, 0));
  assert.deepEqual(calls.find((c) => c[0] === 'renameSession').slice(1), ['s1', '   ']);

  // и отдельным пунктом меню — без открытия поля
  const { doc: d2, calls: c2 } = await withList({ state: [NAMED] });
  d2.getElementById('actionsBtn').dispatchEvent(click(d2));
  const back = [...d2.querySelectorAll('#actionsPop .ap-item')].find((r) => /автозаголовок/.test(r.textContent));
  assert.ok(back, 'пункта возврата к автозаголовку нет у именованного чата');
  back.dispatchEvent(click(d2));
  await new Promise((r) => setTimeout(r, 0));
  assert.deepEqual(c2.find((c) => c[0] === 'renameSession').slice(1), ['s1', '']);
});

test('отказ переименования называет причину, а не молчит', async () => {
  const { doc } = await withList({
    state: [NAMED],
    rename: { ok: false, error: 'имя длиннее 60 символов (77) — сократи' },
  });
  const inp = openRename(doc, /Переименовать чат/);
  inp.value = 'очень длинное имя';
  inp.dispatchEvent(key(doc, 'Enter'));
  await new Promise((r) => setTimeout(r, 0));
  const toast = doc.querySelector('.toast');
  assert.ok(toast, 'отказ прошёл молча — ни слова человеку');
  assert.match(toast.textContent, /сократи/);
});

test('сессия завершается вторым нажатием, а не первым', async () => {
  const { doc, calls, subs } = await boot({ state: [SESSION] });
  subs.onState([SESSION]);
  await new Promise((r) => setTimeout(r, 0));

  const open = () => doc.getElementById('actionsBtn').dispatchEvent(click(doc));
  const item = (re) => [...doc.querySelectorAll('#actionsPop .ap-item')].find((r) => re.test(r.textContent));

  open();
  const kill = item(/Завершить сессию/);
  assert.ok(kill, 'пункта «Завершить сессию» нет в меню действий');
  kill.dispatchEvent(click(doc));
  await new Promise((r) => setTimeout(r, 0));
  assert.ok(
    !calls.some((c) => c[0] === 'killSession'),
    'первое нажатие уже завершило сессию — подтверждения нет'
  );

  // Второе нажатие — то же самое место, но пункт уже спрашивает.
  open();
  const armed = item(/Точно завершить/);
  assert.ok(armed, 'подтверждение не показано: ' + doc.getElementById('actionsPop').textContent);
  armed.dispatchEvent(click(doc));
  await new Promise((r) => setTimeout(r, 0));
  const call = calls.find((c) => c[0] === 'killSession');
  assert.ok(call, 'сессия не завершена вторым нажатием');
  assert.equal(call[1], 's1', 'завершили не ту сессию');
});
