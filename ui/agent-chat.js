/* Чат с главным агентом Jarvis (фаза 7).
 * Шлёт сообщение в agent_send, слушает поток agent:event
 * (init/delta/tool_use/done/failed) и карточки подтверждения agent:confirm
 * (резолв через agent_confirm). Нить разговора переживает закрытие окна:
 * id хранится в настройках (agent_chat_state / agent_chat_reset), а прошлые
 * реплики приезжают из agent_chat_history.
 *
 * Разговор не один: чатов столько, сколько проектов (agent_chats_list и
 * соседи). Открытый чат и есть адресат следующей реплики — его же историю
 * рисует лента.
 *
 * И отвечают они разом: каждое событие потока помечено chatId, поэтому лента,
 * нить и занятость живут у КАЖДОГО чата своя (threads ниже). Смысл был именно
 * такой — дать промпт одному Джарвису и уйти работать со вторым; пока пузырь
 * был один на окно, переключаться во время ответа приходилось запрещать.
 *
 * Мест у чата два — вкладка панели и отдельное окно из трея, — но логика одна:
 * mount() навешивается на готовую разметку и получает команды демона объектом.
 * Две копии стрима разъехались бы на первой же правке; в этом коде так уже
 * было с парой кнопок запуска. */

(() => {
  /* Решение по карточке — событие на всю шину Tauri: демон о нём не
   * рассказывает (agent_confirm отвечает только спросившему), а вкладка и окно
   * из трея обязаны снять вопрос одновременно. Мост такого канала не знает —
   * зовём шину напрямую; без неё окно всё равно единственное. */
  const CONFIRM_DONE = 'agent:confirm-done';
  const bus = () => (window.__TAURI__ && window.__TAURI__.event) || null;
  /* Соседнему окну говорим только «выбор сделан», без исхода: исход знает
   * демон (он же и пришлёт его этим же событием с полем outcome). Свою догадку
   * тут вещать нельзя — «разрешено» за не случившееся действие и есть та ложь,
   * ради которой карточка вообще существует. */
  const confirmDone = (nonce) => { const e = bus(); if (e) e.emit(CONFIRM_DONE, { nonce, sent: true }); };
  const onConfirmDone = (cb) => { const e = bus(); if (e) e.listen(CONFIRM_DONE, (ev) => cb(ev.payload)); };

  /* Четыре исхода гейта (confirm_panel.rs, enum Outcome) — и все четыре надо
   * назвать. Булево врало дважды: «отклонено» тому, кто ничего не отклонял, и
   * «разрешено» за действие, которого не было. Исполняется ровно approved. */
  const OUTCOME = {
    approved: '✓ разрешено',
    rejected: '✕ отклонено',
    expired: '⏳ время вышло — действие не выполнено',
    stale: '⚠ цель изменилась, пока карточка ждала, — действие не выполнено',
  };
  const outcomeOf = (d) => {
    if (!d) return null;
    if (typeof d.outcome === 'string' && OUTCOME[d.outcome]) return d.outcome;
    if (d.outcome) return 'unknown'; // исход новее этого окна — но исход
    if (d.approved === true) return 'approved';
    if (d.approved === false) return 'rejected';
    return null; // решение отправлено, исхода ещё нет
  };
  const outcomeText = (o) => OUTCOME[o] || '⚠ чем кончилось — неизвестно, действие могло не выполниться';

  /* Что просит агент — словами. Внутреннее имя команды («sessions.control»)
   * человеку не говорит, на что он соглашается, а решает он именно это. */
  const CMD = {
    'sessions.reply': 'ответить в сессию',
    'sessions.control': 'сменить модель сессии',
    'sessions.rename': 'переименовать чат',
    'settings.set': 'изменить настройки',
  };
  const cmdTitle = (id) => 'Агент хочет ' + (CMD[id] || 'выполнить команду «' + (id || '?') + '»');

  /* Причина отказа человеку, а не в консоль. Сырое исключение в конкатенации
   * давало «[object Object]» либо стек, а «демон не назвал причину» — слово из
   * наших внутренностей и ни намёка, что делать дальше. */
  const RETRY = 'причина не названа — попробуй ещё раз, а если повторится, перезапусти Jarvis';
  const errText = (e) => {
    const m = e && typeof e === 'object' ? e.message || e.error || e.reason : e;
    return String(m == null ? '' : m).trim() || RETRY;
  };
  const why = (res) => (res && res.error) || RETRY;

  const cut = (s, n) => (s.length > n ? s.slice(0, n - 1) + '…' : s);
  const val = (v) => (v == null ? '—' : typeof v === 'object' ? JSON.stringify(v) : String(v));
  /* Демон знает по имени три id, всё остальное приезжает как kind:'other' —
   * то есть каждая новая команда агента показывалась бы человеку голым JSON.
   * Разворачиваем аргументы парами «ключ: значение». */
  const argsDesc = (args) => {
    const a = args && typeof args === 'object' ? args : {};
    const keys = Object.keys(a);
    if (!keys.length) return 'без параметров';
    return keys.map((k) => k + ': ' + cut(val(a[k]), 120)).join(' · ');
  };

  /* Когда был разговор — теми же словами, что в строке сессии (renderer.js,
   * startLabel): сегодня → ЧЧ:ММ, вчера → «вчера ЧЧ:ММ», раньше → ДД.ММ.
   * Копия, а не общий модуль: renderer.js в окно из трея не грузится. */
  const pad2 = (n) => String(n).padStart(2, '0');
  const sameDay = (a, b) =>
    a.getFullYear() === b.getFullYear() && a.getMonth() === b.getMonth() && a.getDate() === b.getDate();
  const whenLabel = (ms) => {
    if (!ms) return '';
    const d = new Date(ms);
    const now = new Date();
    const hm = pad2(d.getHours()) + ':' + pad2(d.getMinutes());
    if (sameDay(d, now)) return hm;
    const yest = new Date(now);
    yest.setDate(now.getDate() - 1);
    if (sameDay(d, yest)) return 'вчера ' + hm;
    return pad2(d.getDate()) + '.' + pad2(d.getMonth() + 1);
  };
  /* Шеврон свёрнутого списка — тот же, что у Insight в ответе агента: поворот
   * держит CSS, поэтому знак один и «раскрыто» читается одинаково. */
  const chevron = () => {
    const md = window.JarvisMarkdown;
    const svg = md.svgEl('svg', { width: '9', height: '9', viewBox: '0 0 10 10', fill: 'none', class: 'agchev' });
    svg.appendChild(md.svgEl('path', {
      d: 'M3 2 L7 5 L3 8', stroke: 'currentColor',
      'stroke-width': '1.4', 'stroke-linecap': 'round', 'stroke-linejoin': 'round',
    }));
    return svg;
  };

  /* Насколько разговор большой и когда он был — одной строкой. «Пока ни одной
   * реплики» честнее прочерка: у свежего чата нити ещё нет, и это не поломка. */
  const metaOf = (c) => {
    const n = Number(c.turns) || 0;
    if (!n) return 'пока ни одной реплики';
    const size = n + ' ' + window.JarvisMarkdown.plural(n, 'реплика', 'реплики', 'реплик');
    const when = whenLabel(c.at);
    return when ? when + ' · ' + size : size;
  };

  /** Смонтировать чат на разметку `els` поверх команд `api`. */
  function mount(els, api) {
    const { msgs, input, sendBtn, sub, tag, newBtn, chatsRow } = els;
    const hintTpl = msgs.querySelector('.hint').cloneNode(true);

    let chats = []; // история разговоров: чаты из настроек плюс найденные на диске
    let hidden = 0; // спрятанных разговоров, чьи файлы ещё на диске
    let chatId = null; // открытый здесь чат — он же адресат следующей реплики
    let histOpen = false; // история свёрнута: место у переписки дороже места у списка

    /* Разговор целиком: его лента, нить, занятость и стриминговый пузырь. Всё
     * это раньше было по одной переменной на окно — отсюда и запрет уходить из
     * чата, пока агент пишет. Строки закрытого чата никуда не деваются: они
     * лежат в rows и ждут, когда на них снова посмотрят. */
    const threads = new Map(); // chatId → разговор
    const HIST = {}; // ключ ленты «вне экрана»: прошлые реплики собираем в неё же
    const thread = (id) => {
      let t = threads.get(id);
      if (!t) {
        t = { id, rows: [], busy: false, session: null, bubble: null, raw: '', tools: null, gen: 0, loaded: false };
        threads.set(id, t);
      }
      return t;
    };
    const here = () => thread(chatId); // разговор на экране
    const shown = (t) => t.id === chatId;
    const busyOf = (id) => !!(threads.get(id) || {}).busy;
    // Тот же разговор под именем, которое ему дало ядро. Занятое имя не трогаем:
    // склеивать две ленты в одну — врать про обе.
    function relabel(t, id) {
      if (t.id === id || threads.has(id)) return;
      const was = t.id;
      threads.delete(was);
      t.id = id;
      threads.set(id, t);
      if (chatId === was) chatId = id;
    }

    const el = (cls, text) => {
      const d = document.createElement('div');
      d.className = cls;
      if (text != null) d.textContent = text;
      return d;
    };
    // Ответ агента — маркдаун тем же renderChat, что и в чате сессии.
    const paint = (bubble, text) => {
      bubble.textContent = '';
      window.JarvisMarkdown.renderChat(bubble, text);
      // текст нарисован целиком: дальше его можно только дописывать
      bubble.mdAt = String(text).length;
      bubble.mdKept = bubble.childElementCount;
      return bubble;
    };
    /* Стрим дописывает хвост, а не пересобирает пузырь. Полная перерисовка на
     * каждой дельте уносила вместе с DOM выделение (скопировать из растущего
     * ответа было нельзя вовсе) и захлопывала раскрытый Insight. Готовую часть —
     * до последней пустой строки вне фенса и вне Insight — рисуем один раз;
     * дальше её разметка уже не изменится, склейки по половинкам не будет. */
    const grow = (bubble, text) => {
      const md = window.JarvisMarkdown;
      const at = bubble.mdAt || 0;
      const kept = bubble.mdKept || 0;
      while (bubble.childElementCount > kept) bubble.lastElementChild.remove();
      const edge = md.chatSplit(text, at);
      if (edge > at) {
        md.renderChat(bubble, text.slice(at, edge));
        bubble.mdAt = edge;
        bubble.mdKept = bubble.childElementCount;
      }
      md.renderChat(bubble, text.slice(bubble.mdAt || 0)); // живой хвост
      return bubble;
    };
    const scroll = () => { msgs.scrollTop = msgs.scrollHeight; };
    /* Гейт «человек внизу?» — тот же, что у ленты сессии (renderer.js,
     * appendChatItems). Без него отлистать вверх и почитать, пока агент пишет,
     * невозможно: каждая дельта дёргала ленту обратно вниз. Скролл есть только
     * у ленты на экране — у закрытого чата гейт всегда закрыт. */
    const atBottom = () => msgs.scrollHeight - msgs.scrollTop - msgs.clientHeight < 60;
    const near = (t) => (shown(t) ? atBottom() : false);
    const keepDown = (was) => { if (was) scroll(); };
    const show = (t, row) => { if (shown(t)) msgs.appendChild(row); };
    // Лента на экране — зеркало открытого разговора, а не место, где он живёт.
    const draw = (t) => {
      if (!shown(t)) return;
      msgs.textContent = '';
      for (const r of t.rows) msgs.appendChild(r);
      scroll();
    };

    /* Лента чата с агентом задумана переживающей закрытие окна: за пару суток
     * переписки в ней накопились бы тысячи узлов, и каждая новая реплика
     * пересчитывала бы вёрстку по всей куче (renderer.js, trimChatlog). Потолок
     * у каждого разговора свой: закрытый иначе рос бы вовсе без предела. */
    const MAX_ROWS = 400;
    function trimLog(t) {
      let extra = t.rows.length - MAX_ROWS;
      if (extra <= 0) return;
      t.rows = t.rows.filter((row) => {
        if (extra <= 0) return true;
        // В этот пузырь ещё пишет стрим, а живая карточка обещает выбор: унести
        // их значило бы молча проглотить ответ и вопрос. Пропускаем, а не
        // останавливаемся: висящая наверху карточка иначе снимала бы потолок.
        if ((t.bubble && row.contains(t.bubble)) || row.querySelector('.cbtns')) return true;
        row.remove();
        extra--;
        return false;
      });
    }
    const clearHint = (t) => {
      t.rows = t.rows.filter((r) => {
        if (!r.classList.contains('hint')) return true;
        r.remove();
        return false;
      });
    };
    // Пустая лента без слов — не «чисто», а непонятно: приглашение из разметки
    // либо названная причина. Лента чистится целиком при смене чата, поэтому
    // подсказку каждый раз ставим заново из шаблона.
    const showHint = (t, text) => {
      const h = hintTpl.cloneNode(true);
      if (text != null) h.textContent = text;
      t.rows.push(h);
      show(t, h);
    };

    function addRow(t, kind, child) {
      const row = el('msg ' + kind);
      row.appendChild(child);
      clearHint(t);
      t.tools = null;
      const was = near(t);
      t.rows.push(row);
      show(t, row);
      trimLog(t);
      keepDown(was);
      return child;
    }
    const addUser = (t, s) => addRow(t, 'user', el('bubble', s));
    const addErr = (t, s) => addRow(t, 'err', el('bubble', s));
    const addNote = (t, s) => addRow(t, 'note', el('bubble', s));
    const startBot = (t) => { t.raw = ''; return (t.bubble = addRow(t, 'assistant', el('bubble', ''))); };

    // Тул-вызов — не реплика: тот же чип и та же группа, что у чата сессии
    // (renderer.js, addToolChip), иначе в одной панели было бы два языка для
    // одного и того же события.
    function addTool(t, name) {
      clearHint(t);
      const was = near(t);
      if (!t.tools) {
        t.tools = el('msg tools');
        t.rows.push(t.tools);
        show(t, t.tools);
        trimLog(t);
      }
      const chip = el('chip');
      chip.appendChild(el('tverb', name));
      t.tools.appendChild(chip);
      t.bubble = null;
      keepDown(was);
    }

    /* Шапка и поле ввода — про ОТКРЫТЫЙ разговор, а не про окно. Занят один
     * чат — в соседний пишут как ни в чём не бывало: ради этого всё и затеяно. */
    function syncHead() {
      const t = here();
      sendBtn.disabled = t.busy;
      sendBtn.textContent = t.busy ? '…' : '⏎';
      sub.textContent = t.busy ? 'думает…' : 'готов';
    }
    function setBusy(t, v) {
      t.busy = v;
      if (shown(t)) syncHead();
      renderChats(); // занятость соседа обязана быть видна: иначе о нём забудут
    }

    // Метка в шапке: разговор тянется из прошлого запуска окна. Держим её до
    // «Нового чата» — иначе после первой реплики человек снова гадает.
    const syncTag = () => { tag.hidden = !here().session; };

    /* ---------- список разговоров: по чату на проект ---------- */

    /* Все команды списка отвечают одинаково: `{ok:true, current, chats, hidden}`
     * либо `{ok:false, error}`. Отказ показываем вслух — молча оставить прежний
     * список значило бы соврать о том, куда уйдёт следующая реплика. */
    async function listCmd(what, run) {
      let res;
      try {
        res = await run();
      } catch (e) {
        addErr(here(), `Не удалось ${what}: ` + errText(e));
        return null;
      }
      if (res && res.ok === false) {
        addErr(here(), `Не удалось ${what}: ` + why(res));
        return null;
      }
      // Не список — не трогаем показанный: пустая полоска хуже устаревшей.
      if (!res || !Array.isArray(res.chats)) return null;
      chats = res.chats;
      hidden = Number(res.hidden) || 0; // скрытых в chats нет — их считает ядро
      const was = chatId;
      chatId = res.current || (chats.find((c) => c.current) || {}).id || null;
      /* Нить у каждого разговора своя. У занятого верим потоку, а не книжке:
       * свежий id приехал в init, а демон запишет его только под конец. */
      for (const c of chats) {
        if (!c.id) continue;
        const t = thread(c.id);
        if (!t.busy) t.session = c.sessionId || null;
      }
      if (chatId !== was) draw(here()); // лента обязана совпасть с открытым сразу
      renderChats();
      syncHead();
      syncTag();
      return chats.find((c) => c.id === chatId) || null;
    }

    /* История, а не полоска ярлыков. Полоска чипов держала два-три безымянных
     * чата, но по «Чат 5» и «Чат 6» разговор на 249 реплик не найти: у строки
     * обязаны быть заголовок, время и размер. Колонку при этом не держим
     * раскрытой — вкладку открывают ради переписки, и четыре строки (а их
     * станет больше) отняли бы у неё треть окна. Отсюда раскрытие кнопкой и
     * сворачивание сразу после выбора. */
    const closeHist = () => { histOpen = false; renderChats(); };

    function renderChats() {
      if (!chatsRow) return;
      chatsRow.textContent = '';
      const head = el('aghead');
      const toggle = el('agtoggle' + (histOpen ? ' on' : ''));
      // Скрытое называем и в свёрнутой шапке: колонку раскрывают не каждый день,
      // а прятать в тишину — тот же способ потерять разговор, только своими руками.
      const label = chats.length ? 'История · ' + chats.length : 'История пуста';
      toggle.appendChild(el('agtlabel', hidden ? label + ' · скрыто ' + hidden : label));
      toggle.appendChild(chevron()); // тот же шеврон, что у свёрнутого Insight
      toggle.title = histOpen ? 'Свернуть историю' : 'Все разговоры: открыть, переименовать, убрать';
      toggle.addEventListener('click', () => { histOpen = !histOpen; renderChats(); });
      head.appendChild(toggle);
      // Открытый чат виден и со свёрнутой историей: без него не понять, куда
      // уйдёт следующая реплика.
      const cur = chats.find((c) => c.id && c.id === chatId);
      if (cur) {
        const n = el('agcur' + (busyOf(cur.id) ? ' busy' : ''), cur.name);
        n.title = 'Открыт: ' + cur.name;
        head.appendChild(n);
      }
      /* Ушёл в соседний разговор — и потерял из виду, что первый ещё пишет.
       * История свёрнута, поэтому занятых соседей называем прямо в шапке. */
      const work = chats.filter((c) => c.id && c.id !== chatId && busyOf(c.id));
      if (work.length) {
        const b = el('agbusy', work.length === 1 ? '«' + cut(work[0].name, 22) + '» отвечает' : 'ещё ' + work.length + ' отвечают');
        b.title = 'Отвечают прямо сейчас: ' + work.map((c) => c.name).join(', ');
        head.appendChild(b);
      }
      head.appendChild(el('spacer'));
      const add = el('agchat add', '+');
      add.title = 'Новый чат';
      add.addEventListener('click', () => createChat());
      head.appendChild(add);
      chatsRow.appendChild(head);

      if (!histOpen) return;
      const list = el('aglist');
      for (const c of chats) list.appendChild(chatRow(c));
      // Ни одного разговора — приглашение, а не пустая полоска.
      if (!chats.length && !hidden) list.appendChild(el('agempty', 'Разговоров пока нет — напиши первую реплику, и чат появится здесь.'));
      /* Скрытие обратимо только пока о нём помнят: без этой строки спрятанный
       * разговор ничем не отличается от потерянного, а искать его негде. */
      if (hidden) {
        const back = el('aghidden', 'Скрыто ' + hidden + ' · вернуть');
        back.title = 'Вернуть скрытые разговоры в список — файлы всё это время на диске';
        back.addEventListener('click', unhideAll);
        list.appendChild(back);
      }
      chatsRow.appendChild(list);
    }

    /* Строка истории: заголовок от демона (своего фолбэка не заводим — имя
     * человека, первую реплику и «Новый чат» он уже сложил), время и размер.
     * Разговор с диска отличаем формой — бейджем, а не второй краской: чата за
     * ним ещё нет, переименовывать нечего, а клик его ПРИВЯЗЫВАЕТ
     * (agent_chat_open), а не переключает. Кнопки у него свои: чат убирают
     * крестиком, а строку с диска — прячут, и это разные вещи. */
    function chatRow(c) {
      const disk = !c.id;
      const open = !!c.id && c.id === chatId;
      // Занят — точкой и весом имени, как у идущего цикла: одна краска, разная
      // форма. Цветной светофор на списке из двадцати разговоров — шум.
      const work = !disk && busyOf(c.id);
      const row = el('agchat' + (open ? ' on' : '') + (disk ? ' disk' : '') + (work ? ' busy' : ''));
      const main = el('agmain');
      const line = el('agline');
      line.appendChild(el('agname', c.name));
      if (disk) line.appendChild(el('agdisk', 'с диска'));
      main.appendChild(line);
      const metaLine = el('agsub');
      metaLine.appendChild(el('agmeta', metaOf(c)));
      main.appendChild(metaLine);
      // Превью — только когда оно добавляет: у безымянного чата заголовок и есть
      // первая реплика, и вторая её копия под ней — просто шум.
      if (c.preview && c.preview !== c.name) main.appendChild(el('agprev', c.preview));
      row.appendChild(main);
      row.title = disk
        ? 'Разговор с диска — открыть и завести под него чат'
        : (work ? 'Отвечает прямо сейчас · ' : '') + (open ? 'Открыт' : 'Открыть «' + c.name + '»');
      row.addEventListener('click', () => (disk ? openThread(c) : open ? closeHist() : switchTo(c.id)));
      if (disk) {
        /* Пока у строки с диска не было кнопок вовсе, убрать её было нечем —
         * ровно с этого вопрос и начался. Крестик здесь ПРЯЧЕТ: файл остаётся,
         * возврат — одним нажатием. У чата тот же знак значит «удалить чат»,
         * поэтому подписи разные, а класс не общий: перепутать нечем. */
        const h = el('aghide', '×');
        h.title = 'Убрать из списка — разговор останется на диске';
        h.addEventListener('click', (e) => { e.stopPropagation(); hideThread(c); });
        /* Забвение необратимо, поэтому оно и не стоит рядом с крестиком: слово в
         * строке слева, крестик — у правого края. Промахнуться из одного в
         * другое нечем, а название действия читается, не наведя мышь: скрытым
         * его знал бы только тот, кто это писал. */
        const f = el('agforget', 'забыть насовсем');
        f.title = 'Удалить транскрипт с диска — спросим перед удалением';
        f.addEventListener('click', (e) => { e.stopPropagation(); askForget(c, row); });
        metaLine.appendChild(f);
        row.appendChild(h);
        return row;
      }

      // Переименование обязано НАХОДИТЬСЯ: клик по открытому чипу знал только
      // тот, кто это писал. Кнопка рядом с именем — на виду.
      const ed = el('agedit', '✎');
      ed.title = 'Переименовать';
      ed.addEventListener('click', (e) => { e.stopPropagation(); startRename(c, row); });
      row.appendChild(ed);
      // Крестик только когда есть куда уйти: последний чат демон удалить не
      // даст, и кнопка обещала бы действие, которое заведомо откажут.
      if (chats.filter((x) => x.id).length > 1) {
        const x = el('agx', '×');
        x.title = 'Убрать чат из списка — разговор останется в истории';
        x.addEventListener('click', (e) => { e.stopPropagation(); removeChat(c); });
        row.appendChild(x);
      }
      return row;
    }

    /* Правка имени на месте строки — так же, как имя чата в списке сессий.
     * Отдельного диалога тут не нужно: имя короткое, а место у него одно. */
    function startRename(c, row) {
      const box = row.parentElement;
      if (!box) return;
      const inp = document.createElement('input');
      inp.className = 'agrename';
      // Автозаголовок — не имя, а первая реплика: править её в поле бессмысленно
      // (да ещё и с многоточием), поэтому подставляем только своё имя, а чужой
      // заголовок оставляем подсказкой.
      inp.value = c.named ? c.name : '';
      inp.placeholder = c.name;
      inp.maxLength = 60; // тот же потолок, что у демона
      let done = false;
      const stop = () => { if (done) return true; done = true; return false; };
      inp.addEventListener('keydown', (e) => {
        e.stopPropagation(); // хоткеи панели не должны мешать печатать
        if (e.key === 'Enter') { e.preventDefault(); if (!stop()) commitRename(c.id, inp.value); }
        else if (e.key === 'Escape') { e.preventDefault(); if (!stop()) renderChats(); }
      });
      inp.addEventListener('blur', () => { if (!stop()) renderChats(); });
      box.replaceChild(inp, row);
      inp.focus();
      inp.select?.();
    }

    const commitRename = (id, name) =>
      listCmd('переименовать чат', () => api.rename(id, name)).then((c) => { if (!c) renderChats(); });

    async function createChat(name) {
      const cur = await listCmd('создать чат', () => api.create(name));
      if (!cur) return;
      closeHist();
      await loadHistory(cur.id); // новый чат пуст — но пустоту тоже надо показать
      input.focus();
    }

    async function removeChat(c) {
      /* Уйти из отвечающего чата теперь можно всегда, а вот убрать его — нет:
       * ответ ещё едет, и уносить ленту у него из-под ног нечестно. */
      if (busyOf(c.id)) {
        addNote(here(), '«' + c.name + '» сейчас отвечает — убрать его выйдет, когда закончит.');
        return;
      }
      const open = c.id === chatId;
      const cur = await listCmd('удалить чат', () => api.remove(c.id));
      if (!cur || !open) return;
      // Удалили тот, что был открыт, — демон уже перевёл нас на соседний.
      await loadHistory(chatId);
    }

    async function switchTo(id) {
      const cur = await listCmd('переключить чат', () => api.switch(id));
      if (!cur) return;
      closeHist();
      await loadHistory(cur.id);
      input.focus();
    }

    /* Разговор с диска: за ним нет чата, и переключать нечего — демон сперва
     * привяжет его (agent_chat_open) и уже привязанный сделает открытым. Ровно
     * этой дороги и не было: файл на 249 реплик лежал на месте, а дотянуться до
     * него из окна было нельзя. Старого демона просим обновиться вслух —
     * молчащий клик выглядел бы как второй потерянный разговор. */
    async function openThread(c) {
      if (!api.open) {
        addErr(here(), 'Разговор лежит на диске, а открыть его нечем: эта сборка Jarvis такого ещё не умеет — обнови.');
        return;
      }
      const cur = await listCmd('открыть разговор', () => api.open(c.sessionId));
      if (!cur) return;
      closeHist();
      await loadHistory(cur.id);
      input.focus();
    }

    /* Спрятать и вернуть — обратимая пара, файл диска обе не трогают. Историю
     * не сворачиваем: прячут обычно подряд несколько строк, и уезжающая из-под
     * рук колонка тут только мешала бы. */
    const hideThread = (c) => listCmd('убрать разговор из истории', () => api.hide(c.sessionId));
    const unhideAll = () => listCmd('вернуть скрытые разговоры', () => api.unhideAll());

    /* Единственное необратимое действие окна — и потому единственное, которое
     * спрашивает. Спрашивает вслух, кнопкой: невидимый модификатор (alt-клик)
     * нельзя обнаружить, а необратимое не должно зависеть от того, знал ли
     * человек про комбинацию. Вопрос называет, ЧТО исчезнет: заголовок и размер
     * разговора — по ним его и узнают в списке. */
    function askForget(c, row) {
      const box = row.parentElement;
      if (!box) return;
      const n = Number(c.turns) || 0;
      const size = n
        ? ' В нём ' + n + ' ' + window.JarvisMarkdown.plural(n, 'реплика', 'реплики', 'реплик') + ', и вернуть их будет нечем.'
        : ' Вернуть его будет нечем.';
      const ask = el('agask');
      ask.appendChild(el('agasktext', 'Удалить разговор «' + cut(c.name, 60) + '» с диска?' + size));
      const btns = el('agaskbtns');
      const yes = el('agbtn danger', 'Удалить');
      const no = el('agbtn', 'Отмена');
      // Второе нажатие по уже отвеченному вопросу вернулось бы отказом «нет на
      // диске» — отказом за то, что человек всё сделал правильно.
      let sent = false;
      yes.addEventListener('click', (e) => { e.stopPropagation(); if (!sent) { sent = true; forgetThread(c); } });
      no.addEventListener('click', (e) => { e.stopPropagation(); renderChats(); });
      btns.append(yes, no);
      ask.appendChild(btns);
      box.replaceChild(ask, row);
    }

    /* Отказы ядра тут не ошибки, а объяснение порядка: «привязан к чату» и «идёт
     * ход» говорят, что сделать сначала. Их печатает listCmd; строку возвращаем
     * на место — файл цел, и вопрос ещё может повториться. */
    async function forgetThread(c) {
      if (!(await listCmd('удалить разговор', () => api.forget(c.sessionId)))) renderChats();
    }

    /* Лента конкретного чата: историю просим по id, а не «текущую». Иначе после
     * переключения окно рисовало бы переписку соседа — ровно та тихая неправда,
     * которую не видно, пока не начнёшь читать. Прочитанную ленту не
     * перечитываем: в ней уже лежит и живой поток, которого у демона ещё нет —
     * транскрипт он допишет только под конец ответа. */
    async function loadHistory(id) {
      chatId = id;
      const t = thread(id);
      draw(t);
      syncHead();
      syncTag();
      if (t.loaded) return;
      const gen = ++t.gen;
      if (!t.rows.length) showHint(t, 'Загружаю переписку…'); // видно, что идёт загрузка
      let res;
      try {
        res = await api.history(id);
      } catch (e) {
        // Прочитанной ленту не считаем: вернётся человек — попробуем ещё раз.
        if (gen !== t.gen) return;
        clearHint(t);
        addErr(t, 'Не удалось прочитать прошлую переписку: ' + errText(e));
        return;
      }
      if (gen !== t.gen) return; // пока ехало, историю этого чата уже прочитали
      clearHint(t);
      if (res && res.ok === false) {
        addErr(t, 'Не удалось прочитать прошлую переписку: ' + why(res));
        return;
      }
      t.loaded = true;
      /* Прошлые реплики собираем вне экрана и ставим ПЕРЕД тем, что натекло из
       * потока, пока история ехала: ответ, начавшийся в закрытом чате, иначе
       * оказался бы выше собственного вопроса. */
      const h = { id: HIST, rows: [], bubble: null, tools: null };
      const items = (res && res.items) || [];
      for (const it of items) {
        if (it.kind === 'tool') addTool(h, it.text);
        else if (it.role === 'user') addUser(h, it.text);
        else paint(addRow(h, 'assistant', el('bubble', '')), it.text);
      }
      const total = (res && res.total) || items.length;
      if (items.length) {
        const n = items.length;
        if (total > n) {
          const word = window.JarvisMarkdown.plural(n, 'реплика', 'реплики', 'реплик');
          addNote(h, `Показаны последние ${n} ${word} из ${total}.`);
        }
      } else if (t.session && res && res.reason) {
        // Пустая лента при живом разговоре — повод объясниться, а не молчать.
        showHint(h, 'Прошлых реплик здесь нет: ' + res.reason);
      } else if (t.session) {
        showHint(h, 'Продолжаю прошлый разговор: агент помнит, о чём шла речь.');
      } else if (!t.rows.length) {
        showHint(h); // разговора ещё не было — приглашение и есть нормальный вид
      }
      t.rows = h.rows.concat(t.rows);
      trimLog(t);
      draw(t);
    }

    // Восстановление: и список чатов, и id разговора, и сами реплики живут у
    // демона, а не в этом окне. Без ленты «продолжение» выглядело потерей
    // переписки. Список — надёжнее state(): он же говорит, какой чат открыт.
    (async () => {
      const cur = await listCmd('прочитать список чатов', () => api.chats());
      if (!cur) {
        // Списка нет (старый демон или отказ) — нить всё равно нужна, иначе
        // окно тихо начнёт новый диалог вместо прошлого.
        try {
          const st = await api.state();
          if (st && st.chatId) chatId = st.chatId;
          if (st && st.sessionId) here().session = st.sessionId;
        } catch (e) {
          addErr(here(), 'Не удалось узнать про прошлый разговор: ' + errText(e));
        }
      }
      await loadHistory(chatId);
    })();

    /* Список мог поехать в соседнем окне (вкладка и трей смотрят в одну книжку).
     * При возвращении на вкладку сверяемся с демоном; ленту перерисовываем,
     * только если открытым стал другой чат — иначе переписка мигала бы на
     * каждом переключении вкладок. Идущему ответу это не мешает: он пишет в
     * ленту своего чата, а не в ту, что на экране. */
    async function refresh() {
      const was = chatId;
      const cur = await listCmd('обновить список чатов', () => api.chats());
      if (cur && cur.id !== was) await loadHistory(cur.id);
    }

    async function newChat() {
      let res;
      try {
        res = await api.reset();
      } catch (e) {
        addErr(here(), 'Не удалось начать новый чат: ' + errText(e)); // старый id остался — так и скажем
        return;
      }
      if (res && res.ok === false) {
        addErr(here(), 'Не удалось начать новый чат: ' + why(res));
        return;
      }
      // reset забывает нить открытого чата, имя и место оставляет: список
      // приезжает тем же ответом, и sessionId в нём уже пуст.
      if (res && Array.isArray(res.chats)) {
        chats = res.chats;
        hidden = Number(res.hidden) || 0;
        chatId = res.current || chatId;
      }
      const t = here();
      t.session = null;
      t.bubble = null;
      t.tools = null;
      t.raw = '';
      t.rows = [];
      t.loaded = true; // пустая лента и есть весь новый разговор — читать нечего
      showHint(t);
      draw(t);
      syncTag();
      setBusy(t, false); // заодно перерисует историю: список приехал этим же ответом
      input.focus();
    }
    newBtn.addEventListener('click', newChat);

    async function send() {
      const t = here(); // занят конкретный разговор, а не окно
      const text = input.value.trim();
      if (!text || t.busy) return;
      input.value = '';
      input.style.height = 'auto';
      addUser(t, text);
      scroll(); // своя реплика — единственное, за чем ленту доводим всегда
      setBusy(t, true);
      t.bubble = null;
      let res;
      try {
        // Адресуем чатом, а не нитью: у свежего чата нити ещё нет, и по пустой
        // реплика уезжала в тот чат, который в этот момент оказался текущим.
        res = await api.send(text, t.id, t.session);
        /* Адресата хода называет ядро: окно могло послать только нить или вовсе
         * ничего, а события придут помеченными. Перевешиваем разговор на эту
         * метку сразу — иначе ответ на свою же реплику уедет в чужую ленту. */
        if (res && res.chatId) relabel(t, res.chatId);
      } catch (e) {
        addErr(t, 'Ошибка запуска агента: ' + errText(e));
        setBusy(t, false);
        return;
      }
      /* Отказ приезжает РАЗРЕШЁННЫМ промисом: нет ни claude, ни codex, не
       * прочитан jarvis-mcp.json, чат удалили в соседнем окне. catch на такое
       * не срабатывает, а событий done/failed уже не будет — «думает…» висело
       * бы вечно, и следующее сообщение отправить было нечем. */
      if (res && res.ok === false) {
        addErr(t, 'Агент не взял сообщение: ' + why(res));
        setBusy(t, false);
      }
    }

    sendBtn.addEventListener('click', send);
    input.addEventListener('keydown', (e) => {
      if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); send(); }
    });
    // авто-рост поля ввода
    input.addEventListener('input', () => {
      input.style.height = 'auto';
      input.style.height = Math.min(120, input.scrollHeight) + 'px';
    });

    /* Поток ответа агента. Событие адресовано ЧАТУ, а не окну: chatId в нагрузке
     * и решает, чья это лента, — потому и можно уйти во второй разговор, пока
     * первый пишет. Пометки нет — так шлёт только старый демон, у которого
     * поток и был один: отдаём открытому. */
    api.onEvent((ev0) => {
      const ev = ev0 || {};
      const t = ev.chatId ? thread(ev.chatId) : here();
      // Пошёл поток — разговор занят, даже если реплику отправили в соседнем
      // окне: иначе там «думает…», а здесь тот же чат выглядит свободным.
      if (!t.busy && ev.type !== 'done' && ev.type !== 'failed') setBusy(t, true);
      switch (ev.type) {
        case 'init':
          // агент инициализирован (ev.tools — гранто-фильтрованный набор)
          if (ev.session_id) t.session = ev.session_id;
          break;
        case 'delta': {
          const was = near(t);
          if (!t.bubble) startBot(t);
          t.raw += ev.text || '';
          grow(t.bubble, t.raw);
          keepDown(was);
          break;
        }
        case 'tool_use':
          addTool(t, ev.name || '?');
          break;
        case 'done':
          if (ev.session_id) t.session = ev.session_id;
          // финальный текст, если дельт не было
          if (ev.result && (!t.bubble || !t.bubble.textContent)) paint(startBot(t), ev.result);
          // Дельты уже нарисованы дописыванием, и хвост в них дорисован тем же
          // разбором, что и целый текст: пересобирать пузырь заново незачем —
          // это только унесло бы выделение ровно в тот момент, когда его делают.
          else if (t.bubble) grow(t.bubble, t.raw);
          setBusy(t, false);
          t.bubble = null;
          break;
        case 'failed':
          // Отказ агента — вслух. Тихо снять «думает…» значило бы соврать, что он ответил.
          setBusy(t, false);
          t.bubble = null;
          addErr(t, ev.message || 'Агент не ответил и причины не назвал.');
          if (ev.lost_session) {
            t.session = null;
            if (shown(t)) syncTag();
            addNote(t, 'Прошлый разговор не открылся — дальше говорим с чистого листа. Он не удалён: транскрипт остался на диске.');
          }
          break;
      }
    });

    /* Открытые карточки: nonce → живой вопрос. Один и тот же вопрос демон
     * рассылает всем окнам, а нажимают в одном — во втором карточка оставалась
     * живой и обещала выбор, которого уже нет. */
    const openCards = new Map(); // nonce → { sent, waiting(), settle(outcome) }
    const settle = (nonce, outcome) => {
      const card = openCards.get(nonce);
      if (!card) return;
      openCards.delete(nonce);
      card.settle(outcome);
    };
    /* Единственный авторитетный голос об исходе — демон. Пока его нет, карточка
     * остаётся в списке открытых: раннее удаление глотало настоящий исход, и на
     * экране висело «разрешено» за действие, которого не произошло. */
    api.onConfirmDone((d) => {
      if (!d || !d.nonce) return;
      const o = outcomeOf(d);
      if (o) settle(d.nonce, o);
      else { const card = openCards.get(d.nonce); if (card) card.waiting(); }
    });

    // карточка подтверждения side-effect (PanelConfirmer)
    api.onConfirm((c0) => {
      const c = c0 || {};
      const cd = c.card || {};
      /* Карточка меткой чата НЕ помечена: демон шлёт её из другого места, куда
       * chat_id не доходит, и при двух ходах в полёте угадать спросившего
       * нельзя. Не угадываем: вопрос ложится в ту ленту, на которую человек
       * сейчас смотрит, — решает он, и решает здесь. */
      const t = here();
      clearHint(t);
      t.tools = null;

      const box = el('cbox');
      box.appendChild(el('ctitle', cmdTitle(c.id)));

      let desc;
      if (cd.kind === 'session') {
        desc = (cd.label || 'сессия');
        if (cd.text) desc += ' · «' + cd.text + '»';
        if (cd.model) desc += ' · модель ' + cd.model;
        if (cd.effort) desc += ' · усилие ' + cd.effort;
      } else if (cd.kind === 'settings') {
        // Дифф демон уже посчитал — показываем, что из чего станет: список
        // ключей не отвечает на вопрос, с чем именно человек соглашается.
        const diff = cd.diff || {};
        const keys = Object.keys(diff);
        desc = keys.length
          ? keys.map((k) => k + ': ' + cut(val(diff[k].from), 60) + ' → ' + cut(val(diff[k].to), 60)).join(' · ')
          : 'в правке нет ни одного ключа';
      } else {
        desc = argsDesc(cd.args || cd);
      }
      box.appendChild(el('cdesc', desc));
      if (c.provenance === 'untrusted') box.appendChild(el('cwarn', '⚠ данные из недоверенного источника'));

      const btns = el('cbtns');
      const yes = el('cbtn yes', 'Разрешить');
      const no = el('cbtn no', 'Отклонить');
      const line = el('cresult');
      const card = {
        sent: false,
        // выбор сделан, исход ещё едет: кнопок больше нет, но и обещать нечего
        waiting: () => {
          card.sent = true;
          btns.remove();
          if (!line.parentElement) box.appendChild(line);
          line.textContent = 'решение отправлено, жду ответа…';
        },
        settle: (outcome) => {
          const was = near(t);
          btns.remove();
          if (!line.parentElement) box.appendChild(line);
          line.textContent = outcomeText(outcome);
          keepDown(was);
        },
      };
      const decide = async (approved) => {
        if (card.sent || !openCards.has(c.nonce)) return; // повтор демону не нужен
        card.waiting();
        api.confirmDone(c.nonce); // соседнему окну: вопрос больше не ждёт нажатия
        let res;
        try {
          res = await api.confirm(c.nonce, approved);
        } catch (e) {
          addErr(t, 'Решение не дошло до Jarvis: ' + errText(e));
          settle(c.nonce, 'expired');
          return;
        }
        /* `{ok:false}` — нонса в реестре уже нет: гейт истёк, пока карточка
         * ждала. Настоящий исход придёт событием; если нет — не выдумываем
         * согласие, которого демон не принял. */
        if (res && res.ok === false) settle(c.nonce, 'expired');
      };
      yes.addEventListener('click', () => decide(true));
      no.addEventListener('click', () => decide(false));
      btns.append(yes, no);
      box.appendChild(btns);

      const row = el('msg confirm');
      row.appendChild(box);
      const was = near(t);
      t.rows.push(row);
      show(t, row);
      openCards.set(c.nonce, card);
      trimLog(t);
      keepDown(was);
    });

    input.focus();
    return { refresh };
  }

  /* Вкладка панели: разметка лежит в index.html, команды идут через мост.
   * Вкладку открывают много раз — монтируем ровно однажды, чтобы не
   * получить два обработчика на одном поле ввода. */
  let tab = null; // смонтированная вкладка: при повторном открытии её обновляем
  window.initAgentChat = (root) => {
    const q = (id) => root.querySelector('#' + id);
    if (!root.dataset.mounted) {
      root.dataset.mounted = '1';
      const j = window.jarvis;
      tab = mount(
        { msgs: q('agLog'), input: q('agInput'), sendBtn: q('agSend'), sub: q('agSub'), tag: q('agTag'), newBtn: q('agNew'), chatsRow: q('agChats') },
        {
          state: () => j.agentChatState(),
          history: (chatId) => j.agentChatHistory(chatId),
          reset: () => j.agentChatReset(),
          chats: () => j.agentChatsList(),
          switch: (chatId) => j.agentChatSwitch(chatId),
          create: (name) => j.agentChatCreate(name),
          rename: (chatId, name) => j.agentChatRename(chatId, name),
          remove: (chatId) => j.agentChatDelete(chatId),
          // привязать разговор, найденный на диске (не «открыть окно чата»:
          // окно поднимает agent_chat_window)
          open: (sessionId) => j.agentChatOpen(sessionId),
          // спрятать строку с диска (файл цел), вернуть все спрятанные и —
          // с подтверждением выше — удалить транскрипт насовсем
          hide: (sessionId) => j.agentHistoryHide(sessionId),
          unhideAll: () => j.agentHistoryUnhideAll(),
          forget: (sessionId) => j.agentHistoryForget(sessionId),
          send: (message, chatId, sessionId) => j.agentSend(message, chatId, sessionId),
          confirm: (nonce, approved) => j.agentConfirm(nonce, approved),
          confirmDone,
          onEvent: (cb) => j.onAgentEvent(cb),
          onConfirm: (cb) => j.onAgentConfirm(cb),
          onConfirmDone,
        },
      );
    } else if (tab) {
      tab.refresh(); // чат могли переключить в окне из трея, пока вкладка ждала
    }
    q('agInput').focus();
  };

  /* Отдельное окно из трея: общего моста в нём нет — зовём Tauri напрямую.
   * Признак окна — его собственная разметка; в панели её нет, и файл остаётся
   * просто библиотекой. */
  if (document.getElementById('msgs')) {
    const { invoke } = window.__TAURI__.core;
    const { listen } = window.__TAURI__.event;

    // тема и краска: окно живёт вне общего моста, поэтому подписывается само
    window.jarvis = Object.assign(window.jarvis || {}, {
      getSettings: () => invoke('settings_get'),
      setSettings: (patch) => invoke('settings_set', { patch }),
      onAppearance: (cb) => { listen('appearance', (e) => cb(e.payload)); },
    });

    const g = (id) => document.getElementById(id);
    mount(
      { msgs: g('msgs'), input: g('input'), sendBtn: g('send'), sub: g('sub'), tag: g('tag'), newBtn: g('newChat'), chatsRow: g('chats') },
      {
        state: () => invoke('agent_chat_state'),
        history: (chatId) => invoke('agent_chat_history', { chatId }),
        reset: () => invoke('agent_chat_reset'),
        // Полный список и здесь: окно из трея — единственный вход, когда панель
        // закрыта, и «только текущий чат» отрезал бы от остальных проектов.
        chats: () => invoke('agent_chats_list'),
        switch: (chatId) => invoke('agent_chat_switch', { chatId }),
        create: (name) => invoke('agent_chat_create', { name }),
        rename: (chatId, name) => invoke('agent_chat_rename', { chatId, name }),
        remove: (chatId) => invoke('agent_chat_delete', { chatId }),
        open: (sessionId) => invoke('agent_chat_open', { sessionId }),
        hide: (sessionId) => invoke('agent_history_hide', { sessionId }),
        unhideAll: () => invoke('agent_history_unhide_all'),
        forget: (sessionId) => invoke('agent_history_forget', { sessionId }),
        send: (message, chatId, sessionId) => invoke('agent_send', { message, chatId, sessionId }),
        confirm: (nonce, approved) => invoke('agent_confirm', { nonce, approved }),
        confirmDone,
        onEvent: (cb) => listen('agent:event', (e) => cb(e.payload)),
        onConfirm: (cb) => listen('agent:confirm', (e) => cb(e.payload)),
        onConfirmDone,
      },
    );

    // Закрытие = hide (перехвачено в main.rs), поэтому размер и позиция окна
    // переживают закрытие, а лента остаётся на месте.
    g('winClose').addEventListener('click', () => window.__TAURI__.window.getCurrentWindow().close());
  }
})();
