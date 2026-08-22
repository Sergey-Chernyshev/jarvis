/* Headless-обвязка интерфейса: окно чата целиком, без окна.
 *
 * Зачем она есть. Логику взаимодействия — порядок строк, черновики,
 * маршрутизацию событий по чатам, карточки подтверждения — раньше проверяли
 * мышью в ЖИВОМ окне приложения, где в это же время работал человек. Так
 * делать нельзя: синтетический клик уходит в чужой сеанс. Здесь то же самое
 * проверяется без окна: поднимаем настоящую разметку agent-chat.html в
 * linkedom, отдаём окну заглушку моста и дёргаем ОБРАБОТЧИКИ напрямую.
 *
 * Обвязка постоянная, а не разовая: тесты, которые уже поднимали окно каждый
 * своим boot(), собирали одно и то же руками. Здесь этот слой один, и новый
 * тест пишется в три строки:
 *
 *     const d = makeDaemon(['Первый', 'Второй']);
 *     const w = await mountAgentChat({ daemon: d });
 *     await w.open('Второй');
 *
 * Чего обвязка НЕ проверяет: вёрстку, z-order, реальные размеры и попадание
 * мышью. linkedom не считает геометрию — offsetTop/offsetWidth здесь нули, а
 * CSS не применяется вовсе. Всё это человек смотрит глазами по пассивным
 * скриншотам; сюда такие проверки тащить нечестно — они были бы зелёными на
 * любой сломанной вёрстке.
 */

import { readFileSync } from 'node:fs';
import { parseHTML } from 'linkedom';

const HERE = new URL('./', import.meta.url);

export const read = (name) => readFileSync(new URL(name, HERE), 'utf8');
export const tick = () => new Promise((r) => setTimeout(r, 0));
export const wait = (ms) => new Promise((r) => setTimeout(r, ms));

/* Совпадение по имени строки: тест называет чат так же, как человек его видит,
 * — строкой или выражением, а не внутренним id. */
const matches = (name, want) => (want instanceof RegExp ? want.test(name) : name === want);

/* ─────────────────────────────────────────────────────────────────────────────
 * Заглушка демона: не «функция вызвана», а настоящее состояние.
 *
 * Заглушка, которая на любой вызов отвечает {ok:true}, ловит только вызов.
 * Человек же жаловался не на вызов, а на то, что СТРОКИ НЕ ТАМ. Поэтому здесь
 * держим свой список чатов и переставляем его так же, как ядро: список — это
 * порядок полки, и активность его не трогает.
 * ────────────────────────────────────────────────────────────────────────────*/
export function makeDaemon(spec = {}) {
  const input = Array.isArray(spec) ? { names: spec } : spec;
  const names = input.names || ['Первый', 'Второй'];

  const d = {
    /* Порядок массива = порядок полки. Ядро отдаёт чаты в этом порядке, и
     * никакой сортировки по активности здесь нет и быть не должно. */
    chats: names.map((name, i) =>
      typeof name === 'string'
        ? { id: 'c' + (i + 1), name, sessionId: 's' + (i + 1), turns: 2, at: 1000 * (i + 1), preview: 'последняя реплика' }
        : { turns: 2, at: 1000 * (i + 1), preview: '', ...name }),
    current: null,
    drafts: new Map(), // chatId → { text, caret }
    history: new Map(Object.entries(input.history || {})), // chatId → [реплики]
    hidden: 0,
    seq: names.length,
    /* Что вернёт agent_send. Отказ приезжает РАЗРЕШЁННЫМ промисом — на этом уже
     * спотыкались, поэтому подменяется отдельно от исключения. */
    sendResult: (args) => ({ ok: true, chatId: args.chatId }),
    confirmResult: () => ({ ok: true }),
  };
  d.current = input.current || d.chats[0]?.id || null;

  d.byId = (id) => d.chats.find((c) => c.id === id);
  d.order = () => d.chats.map((c) => c.id);
  d.nameOrder = () => d.chats.map((c) => c.name);
  /* Ответ ядра на любую команду списка: полный список + кто открыт. Метку
   * current проставляем на лету — она свойство ответа, а не строки. */
  d.book = () => ({
    ok: true,
    current: d.current,
    hidden: d.hidden,
    chats: d.chats.map((c) => ({ ...c, current: c.id === d.current })),
  });

  d.commands = {
    agent_chat_state: () => ({ sessionId: d.byId(d.current)?.sessionId || null, chatId: d.current }),
    agent_chat_history: ({ chatId }) => {
      const items = d.history.get(chatId || d.current) || [];
      return { ok: true, items, total: items.length };
    },
    agent_chats_list: () => d.book(),
    agent_chat_switch: ({ chatId }) => { d.current = chatId; return d.book(); },
    /* Новый чат встаёт В КОНЕЦ — место в списке задаёт человек, а не ядро.
     * Вставка в начало была бы тем же «активность двигает строку», только на
     * рождении: полка молча переехала бы на одну позицию. */
    agent_chat_create: ({ name } = {}) => {
      const id = 'c' + ++d.seq;
      d.chats.push({ id, name: name || 'Новый чат', sessionId: null, turns: 0, at: null, preview: '' });
      d.current = id;
      return d.book();
    },
    agent_chat_reorder: ({ chatId, toIndex }) => {
      const from = d.chats.findIndex((c) => c.id === chatId);
      if (from < 0) return { ok: false, error: 'нет такого чата' };
      const [moved] = d.chats.splice(from, 1);
      d.chats.splice(toIndex, 0, moved);
      return d.book();
    },
    agent_chat_rename: ({ chatId, name }) => {
      const c = d.byId(chatId);
      if (c) c.name = name;
      return d.book();
    },
    /* Убрать чат из списка. Черновик ядро при этом НЕ трогает — и не должно:
     * этим же вызовом сделано обратимое «Скрыть», после которого разговор
     * возвращается строкой «с диска». Стереть черновик — отдельное решение
     * окна, оно шлёт пустой agent_draft_set. Если свалить это в ядро, тест
     * «скрытие черновик оставляет» станет зелёным по случайности. */
    agent_chat_delete: ({ chatId }) => {
      d.chats = d.chats.filter((c) => c.id !== chatId);
      if (d.current === chatId) d.current = d.chats[0]?.id || null;
      return d.book();
    },
    agent_history_hide: () => { d.hidden += 1; return d.book(); },
    agent_history_unhide_all: () => { d.hidden = 0; return d.book(); },
    agent_history_forget: () => d.book(),
    agent_chat_reset: () => d.book(),
    agent_drafts_get: () => {
      const out = {};
      for (const [id, v] of d.drafts) out[id] = { text: v.text, caret: v.caret };
      return { ok: true, drafts: out };
    },
    agent_draft_set: ({ chatId, text, caret }) => {
      if (String(text || '').trim()) d.drafts.set(chatId, { text, caret: caret | 0 });
      else d.drafts.delete(chatId);
      return { ok: true };
    },
    agent_send: (args) => d.sendResult(args),
    agent_confirm: (args) => d.confirmResult(args),
  };

  return d;
}

/* ─────────────────────────────────────────────────────────────────────────────
 * Поднятие окна чата
 * ────────────────────────────────────────────────────────────────────────────*/

/**
 * Поднимает окно agent-chat.html в headless-DOM и возвращает ручки к нему.
 *
 * @param {object}   [o]
 * @param {object}   [o.daemon]  заглушка ядра из makeDaemon() — держит состояние
 * @param {object}   [o.replies] точечные подмены: cmd → (args) => ответ. Важнее daemon
 * @param {object}   [o.state]   ответ agent_chat_state, если демон не нужен
 * @param {number}   [o.width]   ширина окна: от неё зависит, свёрнута ли колонка
 */
export async function mountAgentChat(o = {}) {
  const daemon = o.daemon || null;
  const replies = o.replies || {};
  const { window, document } = parseHTML(read('agent-chat.html'));
  const calls = [];
  const handlers = {};

  /* Ширину linkedom не знает, а от неё зависит, теснит колонка переписку или
   * кроет её. Ставим ВСЕГДА: окна linkedom делят это поле, и 460 из соседнего
   * теста утекли бы в следующий. */
  window.innerWidth = o.width || 0;

  window.__TAURI__ = {
    core: {
      invoke: async (cmd, args = {}) => {
        calls.push([cmd, args]);
        if (replies[cmd]) return replies[cmd](args);
        if (cmd === 'agent_chat_state' && o.state) return o.state;
        if (daemon && daemon.commands[cmd]) return daemon.commands[cmd](args);
        if (cmd === 'agent_chat_state') return o.state || { sessionId: null };
        if (cmd === 'agent_chat_history') return { ok: true, items: [], total: 0 };
        return { ok: true };
      },
    },
    event: {
      listen: async (name, cb) => { handlers[name] = cb; return () => {}; },
      // Своё же вещание возвращается в это окно — как на настоящей шине Tauri.
      emit: async (name, payload) => { if (handlers[name]) await handlers[name]({ payload }); },
    },
    window: { getCurrentWindow: () => ({ close: () => {} }) },
  };

  // Скрипты в том же порядке, что в agent-chat.html: маркдаун реплик общий с
  // панелью и грузится до чата.
  for (const name of ['markdown.js', 'agent-chat.js']) {
    new Function('window', 'document', 'globalThis', read(name))(window, document, window);
  }
  await tick();
  await tick();

  const doc = document;
  const g = (id) => doc.getElementById(id);
  const input = g('input');

  const raw = (node, name) => node.dispatchEvent(new window.Event(name, { bubbles: true }));
  const fireOn = async (node, name) => { raw(node, name); await tick(); };
  const hit = async (node) => { raw(node, 'click'); await tick(); await tick(); };

  const rows = () => [...doc.querySelectorAll('#chats .agchat')];
  const names = () => [...doc.querySelectorAll('#chats .agchat .agname')].map((n) => n.textContent);
  const rowOf = (want) => rows().find((r) => matches(r.querySelector('.agname')?.textContent, want));
  const need = (want) => {
    const row = rowOf(want);
    if (!row) throw new Error('в колонке нет строки ' + want + '; есть: ' + names().join(', '));
    return row;
  };

  const handle = {
    window, doc, input, calls, daemon,
    /* ---- что видно ---- */
    text: () => g('msgs').textContent,
    bubbles: () => [...doc.querySelectorAll('#msgs .msg.assistant .bubble')].map((b) => b.textContent),
    errors: () => [...doc.querySelectorAll('#msgs .msg.err')].map((e) => e.textContent),
    sub: () => g('sub').textContent,
    rows, names, rowOf,
    busy: () => [...doc.querySelectorAll('#chats .agchat.busy .agname')].map((n) => n.textContent),
    draftLine: (want) => need(want).querySelector('.agdraft')?.textContent || '',

    /* ---- что нажали ---- */
    hit,
    fireOn,
    open: async (want) => { await hit(need(want)); },
    /* «+ Новый чат» в шапке колонки — тот же путь, что у человека. */
    addChat: async () => { await hit(doc.querySelector('#chats .agnew')); },
    menu: async (want, label) => {
      await hit(need(want).querySelector('.agdots'));
      const item = [...doc.querySelectorAll('#chats .agmi')].find((m) => m.textContent === label);
      if (!item) throw new Error('в меню строки нет пункта «' + label + '»');
      await hit(item);
    },
    /* Набрать текст: значение плюс событие — как настоящая клавиатура. Где
     * стоял курсор, знает только человек, поэтому его называет тест. */
    type: async (text, caret) => {
      input.value = text;
      input.selectionStart = caret == null ? text.length : caret;
      input.selectionEnd = input.selectionStart;
      await fireOn(input, 'input');
    },
    // Уход фокуса — принудительный сброс черновика: полсекунды тишины до него не доживают
    blur: async () => { await fireOn(input, 'blur'); await tick(); },
    send: async () => { await hit(g('send')); },
    say: async (text) => {
      input.value = text;
      raw(g('send'), 'click');
      await tick();
      await tick();
    },

    /* Перетаскивание строки мышью. Не HTML5 drag&drop: у Tauri на macOS свой
     * перехватчик на уровне вебвью глушит dragstart/drop, поэтому в коде живут
     * обычные mousedown/mousemove/mouseup — их и шлём. */
    drag: async (from, to) => {
      const src = need(from);
      const grip = src.querySelector('.aggrab');
      if (!grip) throw new Error('у строки ' + from + ' нет зоны захвата');
      raw(grip, 'mousedown');
      const dst = need(to);
      raw(dst, 'mousemove');
      raw(dst, 'mouseup');
      await tick();
      await tick();
    },

    /* Полная перерисовка колонки. Окно пересобирает её на resize (shell
     * сбрасывается в null), и это единственный шов к перерисовке, доступный
     * снаружи, — заодно он честнее ручного вызова: так же делает и живое окно. */
    rerender: async () => {
      window.dispatchEvent(new window.Event('resize'));
      await tick();
      await tick();
    },

    /* ---- что прислал демон ---- */
    emit: async (payload) => { await handlers['agent:event']({ payload }); await tick(); },
    fire: async (name, payload) => {
      if (!handlers[name]) throw new Error('окно не слушает событие ' + name);
      await handlers[name]({ payload });
      await tick();
    },
    confirm: async (payload) => { await handlers['agent:confirm']({ payload }); await tick(); },
    confirmDone: async (payload) => { await handlers['agent:confirm-done']({ payload }); await tick(); },

    /* ---- карточка подтверждения ----
     *
     * Согласие принимается только у нажатия с признаками человека за
     * клавишами: подброшенный клик способен нажать «Разрешить» и согласиться
     * за человека — обойти ровно тот гейт, через который агент и спрашивает
     * разрешение. Поэтому нажатий здесь ДВА вида, и путать их нельзя.
     *
     * Числа берём у самого окна (window.JarvisConfirmArm), а не переписываем:
     * иначе тест разъедется с правилом молча, и «нажатие человека» перестанет
     * означать то же, что в приложении.
     */
    arm: () => window.JarvisConfirmArm,
    armCard: async () => { await armCard(window, cardOf(doc)); },

    /* Настоящее человеческое нажатие — правило одно на все тест-файлы, см.
     * humanPress() ниже. */
    humanPress: async (which) => {
      const box = cardOf(doc);
      if (!box) throw new Error('карточки на экране нет — нажимать не по чему');
      await humanPress(window, box, box.querySelector('.cbtn.' + which));
    },

    /* Слепой клик — почерк синтетики: окно не активно, карточка только
     * появилась, курсор к ней не ехал. Ровно то, что обязано не проходить. */
    blindPress: async (which) => {
      const btn = cardOf(doc)?.querySelector('.cbtn.' + which);
      if (!btn) throw new Error('у карточки нет кнопки .' + which);
      await hit(btn);
    },

    /* ---- что ушло демону ---- */
    invoked: (cmd) => calls.filter(([c]) => c === cmd).map(([, a]) => a),
    sent: () => calls.filter(([c]) => c === 'agent_send').map(([, a]) => a),
  };
  return handle;
}

/* Карточка подтверждения: разметка одна на все исходы, поэтому её ручки живут
 * рядом с обвязкой, а не переписываются в каждом тесте. */
export const cardOf = (doc) => doc.querySelector('.msg.confirm .cbox');
export const cardBtn = (doc, which) => cardOf(doc)?.querySelector('.cbtn.' + which);

/**
 * Изобразить НАСТОЯЩЕЕ человеческое нажатие по карточке подтверждения.
 *
 * Согласие принимается только с признаками человека за клавишами: подброшенный
 * клик способен нажать «Разрешить» и согласиться за человека — обойти ровно тот
 * гейт, через который агент и спрашивает разрешение. Признаки: окно активно и
 * поднято не в это же мгновение, карточка пожила на экране, курсор к кнопке
 * ЕХАЛ (несколько разных точек), а не возник в ней.
 *
 * Отдельной функцией — чтобы правило было ОДНО на все тест-файлы: у окна и у
 * вкладки панели свои boot(), но человеческое нажатие у них обязано значить
 * одно и то же. Числа берём у самого окна, а не переписываем константами.
 *
 * @param {Window}  window  окно, в котором живёт карточка
 * @param {Element} box     .cbox карточки — за движениями курсора следит она
 * @param {Element} btn     кнопка, по которой жмут
 */
export async function humanPress(window, box, btn) {
  if (!btn) throw new Error('кнопки на карточке нет');
  await armCard(window, box);
  btn.dispatchEvent(new window.Event('click', { bubbles: true }));
  await tick();
  await tick();
}

/* Только признаки, без щелчка: нужно там, где щёлкают потом сами — например
 * двумя кликами подряд, проверяя, что повтор демону не уедет. */
export async function armCard(window, box) {
  const arm = window.JarvisConfirmArm;
  if (!arm) throw new Error('окно не отдало JarvisConfirmArm — правило согласия не видно тесту');
  if (!box) throw new Error('карточки на экране нет');
  window.dispatchEvent(new window.Event('focus')); // окно активно
  await wait(arm.ARM_MS + 60); // и поднято не в это же мгновение; карточка пожила
  for (let i = 0; i < arm.ARM_SPOTS; i++) {
    const e = new window.Event('mousemove', { bubbles: true });
    e.clientX = 120 + i * 17; // РАЗНЫЕ точки: телепорт в одну не считается
    e.clientY = 240 + i * 11;
    box.dispatchEvent(e);
  }
}
