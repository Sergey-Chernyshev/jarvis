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

  /* ============ признаки того, что «Разрешить» нажал человек ============
   *
   * Проверяющий CLI слал синтетический ввод в живое окно, где человек в этот
   * момент печатал. Дыра не в удобстве: подброшенный клик способен нажать
   * «Разрешить» и согласиться за человека — обойти ровно тот гейт, через
   * который агент и спрашивает разрешение.
   *
   * У человеческого нажатия есть то, чего у слепого клика нет: карточка успела
   * пожить на экране, курсор к ней ЕХАЛ (а не возник в точке), окно было
   * поднято не в это же мгновение. Ничего из этого не является доказательством
   * по отдельности — вместе они отсекают именно слепой клик по координатам.
   *
   * Это заслон, а не замок: кто синтезирует ещё и движение курсора, пройдёт.
   * Настоящий запрет стоит у источника — в шимах, которыми запускаются CLI.
   * Здесь второй рубеж: для всего, что мимо шимов.
   *
   * Требуется только для СОГЛАСИЯ. Запертое «Отклонить» оставило бы человека
   * наедине с карточкой, которую нечем закрыть (та же асимметрия — в
   * `gate::decision_allowed`).
   */
  const ARM_MS = 700; // столько карточка живёт до первого принятого согласия
  const ARM_SPOTS = 2; // столько РАЗНЫХ позиций курсора над ней: телепорт даёт одну

  /* Когда окно стало активным. Ноль — неактивно; клик в неактивное окно как раз
   * и есть почерк синтетики: она поднимает окно и жмёт в тот же момент. */
  let focusedAt = typeof document !== 'undefined' && document.hasFocus && document.hasFocus() ? Date.now() : 0;
  if (typeof window !== 'undefined' && window.addEventListener) {
    window.addEventListener('focus', () => { focusedAt = Date.now(); });
    window.addEventListener('blur', () => { focusedAt = 0; });
  }

  /* Чистая функция: почему согласие не принято (null — принято). Отдельно от
   * DOM, чтобы проверялась без окна — синтетические пробы в живом окне
   * запрещены, и правило про них само обязано жить по этому правилу. */
  const armWhy = (p, now, focused) => {
    if (!p) return 'карточка не отслеживалась';
    if (now - p.born < ARM_MS) return 'карточка только появилась';
    if (p.spots.size < ARM_SPOTS) return 'курсор к кнопке не подводили';
    if (!focused) return 'окно не активно';
    if (now - focused < ARM_MS) return 'окно только что подняли';
    return null;
  };
  /* Слежка за одной карточкой: рождение и разные позиции курсора над ней. */
  const watchPresence = (node) => {
    const p = { born: Date.now(), spots: new Set() };
    const track = (e) => {
      if (!e || typeof e.clientX !== 'number') return;
      p.spots.add(Math.round(e.clientX) + ':' + Math.round(e.clientY));
    };
    if (node && node.addEventListener) {
      node.addEventListener('pointermove', track);
      node.addEventListener('mousemove', track);
    }
    return p;
  };
  // Наружу — для headless-прогона интерфейса (окно трогать нельзя).
  if (typeof window !== 'undefined') {
    window.JarvisConfirmArm = { armWhy, ARM_MS, ARM_SPOTS };
  }

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

  /* Насколько разговор большой. «Пока ни одной реплики» честнее прочерка: у
   * свежего чата нити ещё нет, и это не поломка. Время стоит отдельно — у имени,
   * как в любом списке чатов. */
  const sizeOf = (c) => {
    const n = Number(c.turns) || 0;
    if (!n) return 'пока ни одной реплики';
    return n + ' ' + window.JarvisMarkdown.plural(n, 'реплика', 'реплики', 'реплик');
  };

  /* ---------- сколько контекста занял разговор ---------- */

  /* Токены человеку: 300227 → «300k», 1000000 → «1M». Точное число живёт в
   * подробностях — в шапке от него остаётся только порядок величины. */
  const short = (n) => {
    if (!Number.isFinite(n)) return '—';
    if (n >= 1e6) return String(Math.round(n / 1e5) / 10).replace(/\.0$/, '') + 'M';
    if (n >= 1000) return Math.round(n / 1000) + 'k';
    return String(Math.round(n));
  };
  const spaced = (n) => String(Math.round(n)).replace(/\B(?=(\d{3})+(?!\d))/g, ' ');
  const pct = (f) => Math.round(f * 100) + '%';
  /* Деньги автономного чата — до цента и всегда со знаком валюты: «1.2» рядом с
   * токенами читается как что угодно. Не число — прочерк, а не ноль. */
  const usd = (n) => (Number.isFinite(n) ? (Math.round(n * 100) / 100).toFixed(2) + '$' : '—');

  /* Порог предупреждения — тот же, что у ядра (agent/context.rs, NEAR): человек
   * должен узнать про исход контекста заранее, а не по внезапно поглупевшему
   * собеседнику. */
  const NEAR = 0.85;

  /* Свести известное в счётчик. Занятое — всегда факт провайдера; потолок бывает
   * и оценкой, и тогда доля помечается. Оценка НИЖЕ уже занятого — не оценка, а
   * опровергнутая догадка: «занято 150%» не значит ничего, честнее сказать, что
   * окна не знаем. */
  const gauge = (c) => {
    if (!c || !Number.isFinite(c.used)) return null;
    let w = Number.isFinite(c.window) && c.window > 0 ? c.window : null;
    if (w && !c.exact && c.used > w) w = null;
    const frac = w ? c.used / w : null;
    return {
      used: c.used, window: w, exact: !!(c.exact && w), frac,
      left: w ? Math.max(0, w - c.used) : null,
      near: frac != null && frac >= NEAR,
    };
  };

  /** Смонтировать чат на разметку `els` поверх команд `api`. */
  function mount(els, api) {
    const { msgs, input, sendBtn, sub, tag, newBtn, chatsRow, extFind } = els;
    const hintTpl = msgs.querySelector('.hint').cloneNode(true);

    /* «Стоп» для мыши — рядом с отправкой: клавиша не должна быть единственным
     * способом прервать ход. Кнопку ставим кодом, а не разметкой: разметки у
     * вкладки и окна две, и вторая копия разъехалась бы на первой же правке. */
    const stopBtn = document.createElement('button');
    stopBtn.className = 'agstop';
    stopBtn.textContent = '■';
    stopBtn.hidden = true;
    stopBtn.title = 'Остановить ход · Esc';
    stopBtn.addEventListener('click', () => stopTurn(here()));
    if (sendBtn.parentElement) sendBtn.parentElement.insertBefore(stopBtn, sendBtn);

    let chats = []; // история разговоров: чаты из настроек плюс найденные на диске
    let hidden = 0; // спрятанных разговоров, чьи файлы ещё на диске
    let chatId = null; // открытый здесь чат — он же адресат следующей реплики

    /* Разговор целиком: его лента, нить, занятость и стриминговый пузырь. Всё
     * это раньше было по одной переменной на окно — отсюда и запрет уходить из
     * чата, пока агент пишет. Строки закрытого чата никуда не деваются: они
     * лежат в rows и ждут, когда на них снова посмотрят. */
    const threads = new Map(); // chatId → разговор
    const HIST = {}; // ключ ленты «вне экрана»: прошлые реплики собираем в неё же
    const thread = (id) => {
      let t = threads.get(id);
      if (!t) {
        // stopping — латч остановки (Esc жмут подряд, команда идёт одна),
        // stopMark — ход этого разговора уже помечен остановленным.
        t = {
          id, rows: [], busy: false, session: null, bubble: null, raw: '', tools: null,
          gen: 0, loaded: false, stopping: false, stopMark: false,
          // ctx — занятый контекст ЭТОГО разговора (у каждого своя лента, значит
          // и свой счёт); ctxNear — про исход контекста уже сказано.
          ctx: null, ctxNear: false,
        };
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

    /* ---------- недописанные реплики ----------
     *
     * Поле ввода было одно на все чаты: текст, написанный одному Джарвису,
     * оставался в поле и уезжал в тот, который открыли следующим. Здесь чаты
     * раздают промпты в сессии с доступом к файлам, поэтому промах адресатом —
     * не «неловко», а «ушло не туда и там исполнилось».
     *
     * Черновик принадлежит ЧАТУ, а не окну: свой — на месте, чужой не
     * приезжает. На диске он лежит своим файлом (agent/drafts.rs): в
     * settings.json, который переписывается целиком на любой тумблер, потоку
     * записей «через полсекунды после клавиши» делать нечего.
     *
     * Курсор храним вместе с текстом — иначе, вернувшись, человек дописывает в
     * конец, а не туда, куда смотрел. */
    const drafts = new Map(); // chatId → { text, caret }
    const DRAFT_MS = 500; // тишина после последней клавиши, после которой пишем
    let draftTimer = null;
    let draftFor = null; // чат, чей черновик ещё не лёг на диск
    /* Здесь в поле НАБИРАЛИ. Два окна смотрят в одну книжку, и окно, которое
     * чужой черновик только показало, писать его обратно не должно: иначе оно
     * вернуло бы на диск устаревшую копию поверх свежей, набранной в соседнем. */
    let dirty = false;
    let mark = ''; // пометка открытого чата в списке — чтобы не пересобирать его на каждый символ

    const one = (s) => String(s == null ? '' : s).replace(/\s+/g, ' ').trim();
    const draftOf = (id) => (id ? drafts.get(id) || null : null);
    /* Начало черновика для строки списка. Режем ДО схлопывания пробелов: текст
     * бывает на десятки килобайт, а перебирать его целиком приходится на каждое
     * событие потока — список перерисовывается вместе с занятостью соседей. */
    const markOf = (id) => { const d = draftOf(id); return d ? cut(one(d.text.slice(0, 200)), 40) : ''; };
    const caretAt = () => {
      const n = Number(input.selectionStart);
      return Number.isFinite(n) ? n : input.value.length;
    };
    const setCaret = (n) => {
      const at = Math.max(0, Math.min(Number(n) || 0, input.value.length));
      try { input.selectionStart = at; input.selectionEnd = at; } catch { /* поле без выделения */ }
    };
    /* Поле растёт под текст — и при подстановке черновика тоже, иначе
     * восстановленные три абзаца показались бы одной строкой. */
    const fitInput = () => {
      input.style.height = 'auto';
      const h = Number(input.scrollHeight);
      if (h) input.style.height = Math.min(120, h) + 'px';
    };

    /* Унести набранное из поля в черновик чата. */
    const takeDraft = (id) => {
      if (!id) return;
      const text = input.value;
      if (text.trim()) drafts.set(id, { text, caret: caretAt() });
      else drafts.delete(id);
    };
    /* Положить черновик чата в поле — вместе с курсором. */
    const putDraft = (id) => {
      const d = draftOf(id);
      input.value = d ? d.text : '';
      fitInput();
      setCaret(d ? d.caret : 0);
      dirty = false;
    };

    /* На диск — с задержкой: писать файл на каждый символ незачем, а полсекунды
     * тишины человек уже не наберёт заново. Всё, что могло не дожить до неё,
     * добивает saveNow (уход фокуса, скрытие окна, выход, смена чата). */
    function saveSoon(id) {
      draftFor = id;
      if (draftTimer) clearTimeout(draftTimer);
      draftTimer = setTimeout(() => { draftTimer = null; saveNow(); }, DRAFT_MS);
    }
    function saveNow() {
      if (draftTimer) { clearTimeout(draftTimer); draftTimer = null; }
      const id = draftFor;
      draftFor = null;
      if (!id || !api.draftSet) return;
      const d = draftOf(id);
      try {
        const p = api.draftSet(id, d ? d.text : '', d ? d.caret : 0);
        if (p && p.catch) p.catch(() => { /* не легло — черновик жив хотя бы в памяти */ });
      } catch { /* демон постарше: черновики доживут до закрытия окна */ }
    }
    /* Уходя, чат уносит недописанное с собой — и сразу на диск: переключение как
     * раз тот миг, когда ждать полсекунды нечего. */
    const stashDraft = (id) => {
      if (!id || !dirty) return;
      takeDraft(id);
      draftFor = id;
      saveNow();
      dirty = false; // на диске теперь ровно то, что в поле, — расходиться нечему
    };
    /* Единственное, что стирает черновик, кроме самого человека, — удавшаяся
     * отправка (и удаление чата насовсем). */
    const dropDraft = (id) => {
      if (!id) return;
      drafts.delete(id);
      draftFor = id;
      saveNow();
    };
    /* Принудительный сброс: полсекунды не переживут ни ухода фокуса, ни закрытия
     * окна — а теряют черновики именно там. */
    const flushDraft = () => { stashDraft(chatId); saveNow(); };

    /* Черновики с диска. null — прочитать не вышло (старый демон, отказ): тогда
     * они живут только в памяти этого окна, и врать про «переживёт» не надо. */
    async function readDrafts() {
      if (!api.drafts) return null;
      let res;
      try { res = await api.drafts(); } catch { return null; }
      const raw = res && res.drafts;
      if (!raw || typeof raw !== 'object') return null;
      const out = new Map();
      for (const id of Object.keys(raw)) {
        const d = raw[id] || {};
        const text = typeof d.text === 'string' ? d.text : '';
        if (text.trim()) out.set(id, { text, caret: Math.max(0, Number(d.caret) || 0) });
      }
      return out;
    }
    /* Тот же чат бывает открыт в двух окнах сразу (вкладка и трей), а книжка
     * одна. Возвращаясь, перечитываем её — иначе список помечал бы черновиками
     * то, что уже отправлено из соседнего окна. Поле подменяем ТОЛЬКО когда
     * здесь ничего не набирали: свои клавиши чужой копией не затирают. Если
     * набирают в обоих окнах разом, побеждает тот, кто печатал последним, — но
     * адресатом при этом не промахнуться, а ценой вопроса было именно это. */
    async function syncDrafts() {
      if (dirty) return;
      const got = await readDrafts();
      if (!got || dirty) return; // пока книжка ехала, здесь начали печатать — не мешаем
      drafts.clear();
      for (const [id, d] of got) drafts.set(id, d);
      putDraft(chatId);
      renderChats();
    }
    /* Смена открытого чата — единственное место, где поле подменяется. Возврат
     * говорит, что чат правда сменился: ленту тоже перерисовывают только тогда. */
    function goTo(id) {
      if (chatId === id) return false;
      stashDraft(chatId);
      chatId = id;
      putDraft(id);
      return true;
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
      // Кнопки «стоп» нет, пока нечего останавливать: кнопка, которая ничего не
      // прервёт, обещает остановку там, где её не было.
      stopBtn.hidden = !t.busy;
      /* Со свёрнутой колонкой имя открытого чата больше негде прочесть, а знать,
       * куда уйдёт следующая реплика, обязательно. Развёрнутая колонка говорит
       * это подсветкой строки — тогда в шапке остаётся только состояние. */
      const cur = sideOff && chats.find((c) => c.id && c.id === chatId);
      sub.textContent = (cur ? cut(cur.name, 22) + ' · ' : '') + (t.busy ? 'думает…' : 'готов');
      /* Рядом — счётчик контекста. Класс вешаем кодом: разметки у вкладки и окна
       * две, и общий контейнер иначе пришлось бы править в обоих файлах. */
      sub.classList.add('ctxhost');
      const st = t.chain;
      const sw = chainNode(st);
      if (sw) sub.appendChild(sw);
      const wait = waitNode(st);
      if (wait) sub.appendChild(wait);
      const money = spendNode(st);
      if (money) sub.appendChild(money);
      const g = gauge(t.ctx);
      const node = g ? sub.appendChild(ctxNode(g)) : null;
      // Раскрытые подробности пересобираем вместе со счётчиком: числа в них
      // стареют за ход, а застывшая карточка врёт ровно тем, против чего заведена.
      if (popKind === 'ctx' && node) { popOff(); ctxPop(node, g); }
      else if (popKind === 'chain' && money) { popOff(); chainPop(money, st); }
      else if (popKind === 'wait' && wait) { popOff(); waitPop(wait, st.waiting); }
    }

    /* ---------- автономия чата: режим, расход, журнал заходов ---------- */

    /* Срез цепочки один на шапку и на строку списка: два источника про один чат
     * разъехались бы на первом же заходе. Приезжает командой (открыли чат) и
     * своим событием (заход ушёл, цепочка встала) — опрашивать это на каждый ход
     * значило бы узнавать про ночную работу с опозданием. */
    function applyChain(id, st) {
      if (!id || !st) return;
      thread(id).chain = st;
      const row = chats.find((c) => c.id === id);
      if (row) { row.auto = st.mode === 'auto'; if (st.spend) row.spend = st.spend; }
      if (id === chatId) syncHead();
      renderChats();
    }

    async function chainSync(id) {
      if (!id || !api.chainState) return; // сборка без цепочек — показывать нечего
      let res;
      try { res = await api.chainState(id); } catch { return; }
      if (res && res.ok !== false) applyChain(id, res.state);
    }

    /* Переключатель автономии — в ШАПКЕ, а не только в файле настроек: сегодня
     * сам работает один чат, завтра другой, и ради этого не открывают json. С
     * «стоп» он не путается: та кнопка стоит у поля ввода и живёт ровно столько,
     * сколько идёт ход, — а тумблер про режим, который переживает и ход, и
     * перезапуск. И он про СВОЙ чат: соседний остаётся на «спроси». */
    function chainNode(st) {
      // Пока ядро не назвало режим, тумблера нет: «спроси» по умолчанию — наша
      // догадка, а тумблер с догадкой переключают вслепую.
      if (!st || !api.chainMode) return null;
      const auto = st.mode === 'auto';
      const box = el('chainsw' + (auto ? ' on' : ''), auto ? '⟳ сам' : '⏸ спроси');
      box.title = auto
        ? 'Этот чат продолжает работу сам. Нажми — снова будет спрашивать'
        : 'Этот чат спрашивает перед каждым заходом. Нажми — будет работать сам';
      box.addEventListener('click', async () => {
        const id = chatId;
        let r;
        try { r = await api.chainMode(id, !auto); }
        catch (e) { addErr(here(), 'Не удалось сменить режим: ' + errText(e)); return; }
        if (r && r.ok === false) { addErr(here(), 'Не удалось сменить режим: ' + why(r)); return; }
        applyChain(id, r && r.state);
      });
      return box;
    }

    /* Свой счётчик расхода у автономного чата — за ночь и за сутки. Без него
     * «дорого» остаётся ощущением, а решать, оставлять ли режим, будет не на
     * чем. Чисел нет — так и говорим: «0.00$» соврало бы точностью. */
    function spendNode(st) {
      const s = st && st.mode === 'auto' && st.spend;
      if (!s) return null;
      const box = el('chainspend', s.known
        ? usd(s.night) + ' за ночь · ' + usd(s.day) + ' за сутки'
        : 'расход не посчитан');
      box.title = (s.known
        ? 'Потолки этого чата: ' + usd(s.nightCap) + ' за ночь, ' + usd(s.dayCap) + ' за сутки. '
          + 'У всех автономных вместе ' + usd(s.allNight) + ' из ' + usd(s.allNightCap) + ' за ночь.'
        : 'Токены считает usage, и сравнить пока не с чем.') + ' Журнал заходов — по клику';
      box.addEventListener('click', () => chainPop(box, st));
      return box;
    }

    /* Журнал заходов: что решил, что запустил, что изменилось. «Уехал не туда»
     * лечится следом, а не запретом, — и след обязан быть достижим оттуда же,
     * откуда видно расход. Слой тот же, что у подробностей контекста: двух
     * карточек поверх одной шапки быть не должно. */
    function chainPop(anchor, st) {
      if (pop) { popOff(); return; }
      pop = el('ctxpop');
      popKind = 'chain';
      const line = (s) => pop.appendChild(el('ctxline', s));
      const s = (st && st.spend) || {};
      line(s.known
        ? 'Потрачено ' + usd(s.night) + ' за ночь из ' + usd(s.nightCap)
          + ' и ' + usd(s.day) + ' за сутки из ' + usd(s.dayCap) + '. Заходов за сутки: ' + s.visits + '.'
        : 'Расход по этому чату не посчитан: токены считает usage, а сравнить пока не с чем. Числа появятся со вторым заходом.');
      if (s.known) {
        // Днём общий потолок предупреждает и заход пропускает (chain.rs, CapAction):
        // обещать стоп там, где его нет, — обещать защиту, которой не будет.
        line('У всех автономных чатов вместе: ' + usd(s.allNight) + ' из ' + usd(s.allNightCap)
          + ' за ночь. Ночью общий потолок останавливает цепочку даже там, где этот чат в своих'
          + ' рамках; днём он предупреждает, а заход пропускает — решаешь ты.');
      }
      pop.appendChild(el('ctxsep'));
      const log = ((st && st.visits) || []).slice(-6).reverse();
      if (!log.length) line('Заходов ещё не было — журнал пуст.');
      for (const v of log) {
        line('Заход ' + v.step + ' (' + v.kind + (v.night ? ', ночью' : '')
          + (v.usd == null ? '' : ', ' + usd(v.usd)) + '): ' + v.decided);
        line('— ' + v.changed + (v.ran && v.ran.length ? ' · запускал: ' + v.ran.join(', ') : ''));
      }
      const r = anchor.getBoundingClientRect ? anchor.getBoundingClientRect() : null;
      if (r) { pop.style.top = Math.round(r.bottom + 6) + 'px'; pop.style.right = '12px'; }
      document.body.appendChild(pop);
    }

    /* «Ждёт тебя» — состояние, а не событие: отложенное ночью переживает и
     * перезапуск, и уход в соседний чат. Карточка в ленте говорит про минуту,
     * когда это случилось, а шапка — что решение до сих пор за тобой; источник у
     * них один (ChainState.waiting), поэтому разъехаться им нечем. */
    function waitNode(st) {
      const w = (st && st.waiting) || [];
      if (!w.length) return null;
      const box = el('chainwait', '⏳ ждёт тебя · ' + w.length);
      box.title = 'Ночью отложено необратимое: ' + w.map((x) => x.kind).join(', ')
        + ' · заходы готовы, подробности по клику';
      box.addEventListener('click', () => waitPop(box, w));
      return box;
    }

    function waitPop(anchor, w) {
      if (pop) { popOff(); return; }
      pop = el('ctxpop');
      popKind = 'wait';
      pop.appendChild(el('ctxline',
        'Ночью необратимое не делается вовсе — эти заходы готовы и ждут твоего решения.'));
      for (const x of w || []) pop.appendChild(el('ctxline', '• ' + x.kind + ': ' + cut(one(x.prompt), 200)));
      const r = anchor.getBoundingClientRect ? anchor.getBoundingClientRect() : null;
      if (r) { pop.style.top = Math.round(r.bottom + 6) + 'px'; pop.style.right = '12px'; }
      document.body.appendChild(pop);
    }

    /* ---------- цепочка в ленте: заход, отказ, отложенное, утренняя сводка ----------
     *
     * Событие цепочки — не только срез для шапки: в нём kind и текст, и это
     * ЕДИНСТВЕННОЕ место, где человек узнаёт про ночную работу. Обработчик, из
     * которого брали один state, выбрасывал заходы, отказы, отложенное и
     * утреннюю сводку — то есть всю ночь целиком: она честно копилась на диске и
     * не доходила до глаз.
     *
     * Карточка ложится в ленту ЧАТА, которому событие адресовано, а не в ту, что
     * на экране: ночью работали несколько, и чужая работа в своём чате — та же
     * тихая неправда, что и чужой ответ. */
    const CHAIN_CARD = {
      sent: 'заход ушёл',
      proposed: 'заход предложен',
      failed: 'заход не ушёл',
      stopped: 'цепочка остановлена',
      cap: 'потолок расхода',
      deferred: 'ждёт тебя',
      morning: 'ночная сводка',
      paused: 'цепочка уступила',
      // Нормальный конец, а не сбой: тона нет намеренно — цвет тревоги здесь
      // сказал бы «сломалось» про успешно законченную работу.
      finished: 'сделано, продолжать нечего',
    };
    /* Тон карточки. Оборванная цепочка, предупреждение и «решать тебе» — разные
     * вещи, и одна краска на все три сказала бы «сломалось» про ожидание. */
    const CHAIN_TONE = { failed: 'warn', cap: 'warn', stopped: 'stop', deferred: 'wait', proposed: 'wait', paused: 'wait' };

    /* Не задвоить. Событие приходит обоим окнам, а лента у чата одна: второй его
     * приход нарисовал бы вторую карточку про один и тот же заход. Узнаём по
     * метке времени ядра; метки нет (демон постарше) — рисуем: потерять карточку
     * хуже, чем показать её дважды. */
    const chainSeen = new Set();
    function chainOnce(ev) {
      if (!ev.at) return true;
      const key = ev.chatId + '|' + ev.kind + '|' + ev.at;
      if (chainSeen.has(key)) return false;
      chainSeen.add(key);
      if (chainSeen.size > 500) chainSeen.delete(chainSeen.values().next().value);
      return true;
    }

    /* Текст карточки. Имя поля у каждого вида своё — ядро называет вещи своими
     * именами (`prompt` у захода, `digest` у сводки), и сводить их к одному ключу
     * значило бы соврать, что это одно и то же. */
    function chainText(ev) {
      if (ev.kind === 'sent') return 'Заход ' + (ev.step || 0) + ': ' + (ev.prompt || '');
      if (ev.kind === 'proposed') return ev.prompt || '';
      // У отложенного текст объясняет, ПОЧЕМУ ночь его не тронула, а сам заход
      // лежит рядом: утром человек решает по нему, а не по пересказу.
      const w = ev.waiting || {};
      return (ev.text || '') + (w.prompt ? '\n\n' + w.prompt : '');
    }

    function chainCard(ev) {
      const label = CHAIN_CARD[ev.kind];
      if (!label || !ev.chatId || !chainOnce(ev)) return;
      const t = thread(ev.chatId);
      const box = el('chbox ' + (CHAIN_TONE[ev.kind] || ev.kind));
      if (ev.kind === 'morning') morningBox(box, ev);
      else {
        box.appendChild(el('chkind', label));
        window.JarvisMarkdown.renderChat(box.appendChild(el('bubble')), chainText(ev));
        if (ev.kind === 'proposed') proposeRow(t, box, ev);
        if (ev.kind === 'paused') pauseRow(t, box);
      }
      addRow(t, 'chain ' + ev.kind, box);
    }

    /* Цепочка уступила сессию человеку и ждёт решения.
     *
     * Два выхода рядом с фактом, потому что оба нормальны: человек мог зайти
     * на минуту и хотеть продолжения, а мог перехватить сессию насовсем.
     * Карточка без выхода — это «почему-то больше не идёт», и разбираться в
     * этом человеку пришлось бы самому. */
    function pauseRow(t, box) {
      const row = el('stoprow');
      row.appendChild(el('stopname', 'Заход подождёт.'));
      const go = el('agbtn', 'Продолжить цепочку');
      go.title = 'Цепочка снова пойдёт сама после этого хода';
      go.addEventListener('click', async () => {
        if (!api.chainResume) {
          addErr(t, 'Продолжить нечем: эта сборка Jarvis такого ещё не умеет — обнови.');
          return;
        }
        let r;
        try { r = await api.chainResume(t.id); }
        catch (e) { addErr(t, 'Не удалось продолжить: ' + errText(e)); return; }
        if (r && r.ok === false) { addErr(t, 'Не удалось продолжить: ' + why(r)); return; }
        row.replaceChildren(el('stopname', 'Цепочка продолжена.'));
      });
      const off = el('agbtn', 'Отменить цепочку');
      off.title = 'Сессия останется за тобой, сама цепочка больше не пойдёт';
      off.addEventListener('click', async () => {
        if (!api.chainStop) {
          addErr(t, 'Отменить нечем: эта сборка Jarvis такого ещё не умеет — обнови.');
          return;
        }
        let r;
        try { r = await api.chainStop(t.id); }
        catch (e) { addErr(t, 'Не удалось отменить: ' + errText(e)); return; }
        if (r && r.ok === false) { addErr(t, 'Не удалось отменить: ' + why(r)); return; }
        row.replaceChildren(el('stopname', 'Цепочка отменена.'));
      });
      row.append(go, off);
      box.appendChild(row);
    }

    /* Предложенный заход ждёт кнопки — в этом весь ручной режим. Текст правится
     * на месте: ядро принимает поправку тем же вызовом (`text`), а гонять её
     * через поле ввода значило бы отправить заход обычной репликой не туда. */
    function proposeRow(t, box, ev) {
      const area = document.createElement('textarea');
      area.className = 'chedit';
      area.value = ev.prompt || '';
      area.addEventListener('keydown', (e) => e.stopPropagation()); // хоткеи панели не мешают править
      box.appendChild(area);
      actRow(box, 'Пойдёт, когда скажешь.', 'Отправить заход', 'Отправить этот заход в сессию',
        async (btn, name) => {
          if (!api.chainSend) {
            addErr(t, 'Отправить заход нечем: эта сборка Jarvis такого ещё не умеет — обнови.');
            return;
          }
          const text = area.value.trim();
          if (!text) { addErr(t, 'Пустой заход отправлять некуда — впиши текст или останови цепочку.'); return; }
          let r;
          // Поправку шлём, только если правили: без неё уходит то, что ядро уже
          // держит предложенным, — и подменить его своей копией незачем.
          try { r = await api.chainSend(t.id, text === String(ev.prompt || '').trim() ? null : text); }
          catch (e) { addErr(t, 'Заход не ушёл: ' + errText(e)); return; }
          if (r && r.ok === false) { addErr(t, 'Заход не ушёл: ' + why(r)); return; }
          btn.remove();
          area.disabled = true;
          name.textContent = 'Заход отправлен.';
        });
    }

    /* Утренняя сводка — отчёт, а не реплика: пять разделов с фиксированными
     * заголовками и полтора экрана текста. Вопрос у человека при этом один и
     * дословный — «куда он ушёл, пока я спал», и ответ на него обязан читаться
     * за десять секунд. Поэтому сверху строка чисел, а разделы сложены: раскрыты
     * те два, ради которых сводку и открывают. Разметку внутри раздела рисует
     * общий renderChat — своей у сводки нет и заводить её незачем. */
    const DIGEST_HEAD = /^([А-ЯЁ][А-ЯЁ ,]*[А-ЯЁ])(\s*\([^)]*\))?:\s*(.*)$/;
    const DIGEST_OPEN = /^(КУДА|ЧТО ЖДЁТ)/;

    /* Разбор на разделы: заголовок — строка прописными до двоеточия. Незнакомый
     * раздел станет таким же своим: ядро их дописывает (хвост «ещё за ночь» и
     * есть такой), и пропасть новому нельзя. */
    function digestParts(text) {
      const out = { intro: '', foot: '', secs: [] };
      let body = String(text == null ? '' : text).replace(/\s+$/, '');
      /* Подвал — абзац после последней пустой строки, без пунктов: ядро ставит
       * туда счёт непоказанного, и разделу «что ждёт тебя» он не принадлежит. */
      const at = body.lastIndexOf('\n\n');
      const tail = at < 0 ? '' : body.slice(at + 2).trim();
      if (tail && !/^[•\-*]/.test(tail) && !DIGEST_HEAD.test(tail)) {
        out.foot = tail;
        body = body.slice(0, at);
      }
      const intro = [];
      let cur = null;
      for (const line of body.split('\n')) {
        const h = line.match(DIGEST_HEAD);
        if (h) { cur = { head: h[1] + (h[2] || ''), body: h[3] ? [h[3]] : [] }; out.secs.push(cur); }
        else if (cur) cur.body.push(line);
        else if (line.trim()) intro.push(line.trim());
      }
      out.intro = intro.join(' ');
      return out;
    }

    /* Ответ за десять секунд: сколько заходов и в скольких чатах, во сколько
     * обошлось, сколько встало и сколько ждёт решения. Числа берём из ночного
     * среза, а не вычитываем из прозы: две правды об одной ночи разъедутся. */
    function morningSum(night, secs) {
      const md = window.JarvisMarkdown;
      const notes = night.notices || [];
      const count = (kinds) => notes.filter((n) => kinds.includes(n.kind)).length;
      const sent = count(['sent']);
      const stuck = count(['failed', 'stopped']);
      const wait = (night.waiting || []).length;
      const chats = new Set(notes.map((n) => n.chatId)).size;
      const money = (secs.find((s) => /ПОТРАЧЕНО/.test(s.head)) || { body: [] }).body.join(' ').trim();
      const row = el('chsum');
      const chip = (s, cls) => { if (s) row.appendChild(el('chnum' + (cls ? ' ' + cls : ''), s)); };
      chip(sent + ' ' + md.plural(sent, 'заход', 'захода', 'заходов'));
      chip(chats ? 'в ' + chats + ' ' + md.plural(chats, 'чате', 'чатах', 'чатах') : '');
      chip(money);
      chip(stuck ? stuck + ' встало' : '', 'bad');
      chip(wait ? wait + ' ждёт тебя' : '', 'wait');
      return row;
    }

    function morningBox(box, ev) {
      const md = window.JarvisMarkdown;
      const d = digestParts(ev.digest == null ? ev.text : ev.digest);
      if (d.intro) box.appendChild(el('chtitle', d.intro));
      box.appendChild(morningSum(ev.night || {}, d.secs));
      for (const s of d.secs) {
        const sec = el('chsec' + (DIGEST_OPEN.test(s.head) ? ' open' : ''));
        const head = el('chsechead');
        head.appendChild(chevron()); // тот же шеврон, что у свёрнутой группы и Insight
        head.appendChild(el('chsecname', s.head));
        head.addEventListener('click', () => sec.classList.toggle('open'));
        sec.appendChild(head);
        md.renderChat(sec.appendChild(el('bubble')), s.body.join('\n').trim());
        box.appendChild(sec);
      }
      if (d.foot) box.appendChild(el('chfoot', d.foot));
    }

    /* ---------- счётчик контекста в шапке ---------- */

    /* Мелким и тихим: полоска, доля и «занято / потолок». Точные числа и то,
     * факт это или оценка, — по клику: в шапке им места нет, а врать
     * сокращением «300k» без пометки нельзя. */
    function ctxNode(g) {
      const box = el('ctx' + (g.near ? ' near' : ''));
      const bar = el('ctxbar');
      const fill = el('ctxfill');
      if (g.frac != null) fill.style.width = Math.min(100, Math.round(g.frac * 100)) + '%';
      bar.appendChild(fill);
      box.appendChild(bar);
      // «≈» — потолок оценён нами, а не назван провайдером. Врать точностью нельзя.
      const num = g.frac == null
        ? short(g.used) + ' · окно неизвестно'
        : pct(g.frac) + ' · ' + short(g.used) + ' / ' + short(g.window) + (g.exact ? '' : ' ≈');
      box.appendChild(el('ctxnum', num));
      // Точные токены — под курсором и по клику: в шапке им места нет, а
      // «300k» без них было бы сокращением без права на проверку.
      box.title = 'Контекст разговора: занято ' + spaced(g.used)
        + (g.left == null ? ', потолок неизвестен' : ', осталось ' + spaced(g.left) + ' из ' + spaced(g.window))
        + ' · подробности по клику';
      box.addEventListener('click', () => ctxPop(box, g));
      return box;
    }

    let pop = null; // подробности счётчика: слой поверх шапки
    // Чья карточка раскрыта: пересобирая шапку, надо вернуть ту же, а не соседнюю.
    let popKind = '';
    const popOff = () => { if (pop) { pop.remove(); pop = null; popKind = ''; } };
    /* Клик мимо — закрыть. Свой клик узнаём по цели, а не глушим всплытие:
     * глушение стоило бы закрытия там, где до document слушает кто-то ещё. */
    document.addEventListener('click', (e) => {
      const el0 = e && e.target;
      if (el0 && el0.closest && (el0.closest('.ctx') || el0.closest('.ctxpop')
        || el0.closest('.chainspend') || el0.closest('.chainwait'))) return;
      popOff();
    });

    /* Подробности. Лимит стоит РЯДОМ, но отдельной строкой за отбивкой: слить
     * его со счётчиком в одно число нельзя — контекст про то, сколько помнит
     * разговор, а лимит про то, что мы можем себе позволить до сброса. */
    function ctxPop(anchor, g) {
      if (pop) { popOff(); return; }
      pop = el('ctxpop');
      popKind = 'ctx';
      const line = (s) => pop.appendChild(el('ctxline', s));
      line('Контекст разговора: занято ' + spaced(g.used) + ' токенов'
        + (g.frac == null ? '' : ' из ' + spaced(g.window) + ' — это ' + pct(g.frac)));
      if (g.left != null) line('Осталось ' + spaced(g.left) + ' токенов.');
      line(g.window == null
        ? 'Потолок окна неизвестен: модель его не назвала, а гадать числом нельзя. Доля появится после первого ответа агента.'
        : g.exact
          ? 'Потолок назвал сам CLI — это факт, а не наш подсчёт.'
          : 'Потолок ОЦЕНЁН по модели: в транскрипте пометки про размер окна нет. Точное число придёт с ответом агента.');
      if (g.near) line('Контекст на исходе: скоро часть разговора уедет или будет сжата.');
      pop.appendChild(el('ctxsep'));
      line('Лимит провайдера — это другое: контекст про память разговора, лимит про то, сколько мы можем себе позволить до сброса.');
      limitLine(pop);
      // Позиция от счётчика: шапка узкая, а подробностям нужна ширина.
      const r = anchor.getBoundingClientRect ? anchor.getBoundingClientRect() : null;
      if (r) { pop.style.top = Math.round(r.bottom + 6) + 'px'; pop.style.right = '12px'; }
      document.body.appendChild(pop);
    }

    /* Остаток лимита в днях — если демон умеет его назвать. Не умеет (старая
     * сборка, чисел ещё нет) — молчим: выдуманный запас хода хуже пустоты. */
    async function limitLine(box) {
      if (!api.limit) return;
      let res;
      try { res = await api.limit(); } catch { return; }
      const p = (res && res.providers && res.providers.claude) || null;
      if (!p || !Number.isFinite(p.runwayDays)) return;
      if (box.isConnected === false) return;
      const days = Math.round(p.runwayDays * 10) / 10;
      box.appendChild(el('ctxline', 'Лимит claude: запас хода ' + days + ' дн'
        + (Number.isFinite(p.weekLeftPct) ? ', недели осталось ' + Math.round(p.weekLeftPct) + '%' : '') + '.'));
    }

    /* Предупреждение о границе — строкой в ленте, один раз на подход: человек
     * должен узнать заранее, а не по внезапно поглупевшему собеседнику. */
    function ctxWatch(t) {
      const g = gauge(t.ctx);
      if (!g || !g.window) return;
      if (!g.near) { t.ctxNear = false; return; }
      if (t.ctxNear) return;
      t.ctxNear = true;
      addNote(t, 'Контекст на исходе: занято ' + pct(g.frac) + ' (' + spaced(g.used)
        + ' из ' + spaced(g.window) + ' токенов). Скоро часть разговора уедет или будет сжата —'
        + ' важное лучше повторить, а длинную тему увести в новый чат.');
    }

    /* Момент сжатия — отдельной строкой. Без неё разрыв в памяти агента человек
     * читает как его ошибку. */
    function addSqueeze(t, m) {
      let s = 'здесь контекст был сжат';
      if (m && Number.isFinite(m.pre) && Number.isFinite(m.post)) s += ': ' + short(m.pre) + ' → ' + short(m.post);
      if (m && m.trigger === 'manual') s += ' (по просьбе человека)';
      return addRow(t, 'squeeze', el('bubble', s + ' — сказанное выше агент помнит только в пересказе.'));
    }
    function setBusy(t, v) {
      t.busy = v;
      if (v) t.stopMark = false; // новый ход — и пометка об остановке снова возможна
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
      // Открытый чат меняет ядро, а не окно, — и поле обязано смениться вместе
      // с ним: черновик покидаемого унесёт goTo, чужой сюда не приедет.
      const moved = goTo(res.current || (chats.find((c) => c.current) || {}).id || null);
      /* Нить у каждого разговора своя. У занятого верим потоку, а не книжке:
       * свежий id приехал в init, а демон запишет его только под конец. */
      for (const c of chats) {
        if (!c.id) continue;
        const t = thread(c.id);
        if (!t.busy) t.session = c.sessionId || null;
        /* Счётчик контекста едет вместе со списком — у открытого чата. Идущий
         * ход знает про контекст больше диска (транскрипт демон допишет только
         * под конец), поэтому его чисел не трогаем. */
        if (!t.busy && c.ctx) t.ctx = { used: c.ctx.used, window: c.ctx.window, exact: !!c.ctx.exact };
        if (Array.isArray(c.squeezes)) t.marks = c.squeezes;
      }
      if (moved) draw(here()); // лента обязана совпасть с открытым сразу
      renderChats();
      syncHead();
      syncTag();
      // Список приехал — режим и расход открытого чата обязаны совпасть с ним:
      // тумблер, показывающий вчерашнее, хуже отсутствующего.
      chainSync(chatId);
      // Открыли чат, у которого контекст уже на исходе, — сказать надо сразу, а
      // не ждать следующего хода: латч в разговоре не даст повториться.
      ctxWatch(here());
      return chats.find((c) => c.id === chatId) || null;
    }

    /* Колонка, а не выпадающая шапка. «История · N» была меню: чтобы попасть в
     * соседний разговор, его надо было сперва вспомнить, а потом раскрыть. Так
     * не устроен ни один мессенджер, и правильно — чаты видны всегда, переход в
     * один клик. Прежний довод («треть окна отнята у переписки») закрыт не
     * выпадашкой, а границей: колонку тянут за край и сворачивают целиком (⌘\),
     * а ширина и свёрнутость лежат в settings.json рядом с темой — своего
     * механизма памяти тут не заводим. */
    const SIDE_MIN = 180;
    /* Встроенная колонка несёт не только чаты: над ней стоят поиск и вкладки
     * окна. На 180px вкладки 2×2 уже не читаются, а это дорога назад. */
    const SIDE_MIN_DOCK = 240;
    const SIDE_MAX = 420;
    /* Ниже этого колонка не теснит переписку, а кроет её и уходит после выбора:
     * в окне из трея (~460px) 260 колонки и 200 переписки — две нечитаемые
     * полосы вместо одного разговора. */
    const SIDE_NARROW = 560;
    let sideW = 260; // ширина колонки
    let sideOff = false; // колонка свёрнута до рейки
    let query = ''; // поиск в шапке колонки
    let menu = null; // меню строки: живёт ВНЕ списка, см. renderPop
    let dragId = null; // чат, который тащат прямо сейчас
    let dragOver = null; // строка под курсором во время жеста
    let dropped = false; // жест закончился перестановкой — клик после него глушим
    /* Группа истории открыта по умолчанию: свернуть её человек может сам, а вот
     * пропавший с глаз разговор он уже однажды искал. Прячем по желанию, не молча. */
    let diskOpen = true;
    let shell = null; // собранная колонка: список, слой меню, рейка

    /* Колонка может стоять не внутри переписки, а в ЛЕВОЙ колонке окна: во
     * вкладке панели место списка сессий на время отдано чатам Джарвиса
     * (renderer.js, setView) — иначе два списка встали бы рядом третьей
     * колонкой. Встроенная колонка живёт по чужим правилам: ширину держит
     * сетка, а сворачивать её нечем — вместе с ней ушли бы вкладки, то есть
     * дорога назад. */
    const docked = () => !!chatsRow && chatsRow.dataset.dock === '1';
    const num = (v) => (Number.isFinite(v) ? v : 0);
    const clampW = (n) => Math.max(docked() ? SIDE_MIN_DOCK : SIDE_MIN, Math.min(SIDE_MAX, Math.round(num(n))));
    /* Ширину окна знает не всякая среда (скрытая вкладка, тест). «Не знаю»
     * значит «широко»: молча свернуть колонку хуже, чем показать её. */
    const narrow = () => {
      if (docked()) return false; // колонка сетки переписку не кроет
      const host = chatsRow && chatsRow.parentElement;
      const w = num(host && host.clientWidth) || num(window.innerWidth);
      return w > 0 && w < SIDE_NARROW;
    };
    const hotLabel = () => (window.jarvisKeys ? window.jarvisKeys.k('\\') : '⌘\\');

    /* Ширину и свёрнутость помнит settings.json — тем же путём, что тему (см.
     * theme.js). В узком окне колонка выдвижная, и её состояние не выбор
     * человека, а теснота: такое не сохраняем. */
    const bag = () => window.jarvis || {};
    const saveSide = () => {
      if (narrow()) return;
      const patch = { agentSideWidth: Math.round(sideW) };
      // Свёрнутость встроенной колонки никто не выбирал (сворачивать её нечем) —
      // и затирать ею выбор, сделанный в окне из трея, не за что.
      if (!docked()) patch.agentSideOff = sideOff;
      try { bag().setSettings?.(patch); }
      catch { /* окно без моста — колонка доживёт до перезапуска */ }
    };
    function toggleSide() {
      sideOff = !sideOff;
      saveSide();
      renderChats();
      syncHead(); // со свёрнутой колонкой имя открытого чата уходит в шапку
    }
    // Выбрали чат — выдвижная колонка своё отработала и не заслоняет переписку.
    const afterPick = () => { if (narrow() && !sideOff) { sideOff = true; renderChats(); syncHead(); } };

    (async () => {
      let s = null;
      try { s = await bag().getSettings?.(); } catch { /* настроек нет — заводские */ }
      if (s && Number(s.agentSideWidth)) sideW = clampW(s.agentSideWidth);
      if (s && s.agentSideOff != null) sideOff = !!s.agentSideOff;
      if (narrow()) sideOff = true;
      shell = null; // числа приехали после первой отрисовки — пересобираем колонку
      renderChats();
      syncHead();
    })();
    // Окно тянут мышью, и «узко» меняется на лету: колонка обязана переехать
    // вместе с ним, а не остаться полосой на пол-экрана.
    window.addEventListener('resize', () => { shell = null; renderChats(); });

    /* Ширину держит либо сама колонка, либо колонка сетки, в которую её
     * встроили: у grid-элемента своя width границу колонки не двигает. */
    function applyWidth() {
      if (docked()) chatsRow.parentElement?.style.setProperty('--side-w', sideW + 'px');
      else chatsRow.style.width = sideW + 'px';
    }

    function renderChats() {
      if (!chatsRow) return;
      mark = markOf(chatId); // список нарисован — значит пометка в нём уже верна
      if (docked()) sideOff = false; // встроенная колонка не сворачивается
      if (!shell || shell.off !== sideOff || shell.dock !== docked()) buildSide();
      if (sideOff) { renderRail(); return; }
      applyWidth();
      renderList();
      // Строка, на которую показывало меню, могла уйти из списка — тогда и
      // показывать его не на что.
      if (menu && !chats.some((c) => keyOf(c) === menu.key)) closeMenu();
    }

    function buildSide() {
      // Колонку пересобирают редко (свернули, окно поехало) — и открытое меню
      // после этого показывать уже не на что.
      menu = null;
      chatsRow.textContent = '';
      chatsRow.className = 'agside' + (docked() ? ' docked' : '') + (sideOff ? ' off' : '');
      chatsRow.style.width = '';
      const wrap = chatsRow.parentElement;
      if (wrap && !docked()) wrap.classList.toggle('narrow', narrow());
      if (sideOff) {
        const rail = el('agrail');
        chatsRow.appendChild(rail);
        shell = { off: true, dock: docked(), rail };
        return;
      }
      const top = el('agtop');
      const add = el('agnew', '+  Новый чат');
      add.title = 'Новый чат';
      add.addEventListener('click', () => createChat());
      top.appendChild(add);
      if (!docked()) {
        const fold = el('agfold');
        fold.title = 'Свернуть список чатов · ' + hotLabel();
        fold.appendChild(chevron()); // тот же шеврон, что у свёрнутого Insight
        fold.addEventListener('click', toggleSide);
        top.appendChild(fold);
      }
      chatsRow.appendChild(top);

      /* Поиск в шапке колонки, а не отдельным экраном: двадцать разговоров
       * листают глазами, а сотню — уже нет. Поле переживает перерисовку списка:
       * оно вне .aglist, иначе набранное слово стирал бы чужой ответ.
       * Там, где у окна уже есть строка поиска (панель), своего поля не
       * заводим: два одинаковых поля рядом не говорят, что где ищут. */
      let find = null;
      if (!extFind) {
        find = document.createElement('input');
        find.className = 'agfind';
        find.placeholder = 'Поиск по чатам';
        find.value = query;
        find.addEventListener('input', () => { query = find.value; renderList(); });
        find.addEventListener('keydown', (e) => {
          e.stopPropagation(); // хоткеи панели не должны мешать печатать
          if (e.key === 'Escape') { find.value = ''; query = ''; renderList(); }
        });
        chatsRow.appendChild(find);
      }

      const list = el('aglist');
      chatsRow.appendChild(list);
      const pop = el('agpop');
      pop.hidden = true;
      chatsRow.appendChild(pop);
      const grip = el('aggrip');
      grip.title = 'Потянуть — ширина колонки';
      grip.addEventListener('mousedown', startDrag);
      chatsRow.appendChild(grip);
      shell = { off: false, dock: docked(), list, pop, find };
    }

    /* Свёрнутая колонка — рейка с двумя знаками. В ноль не сворачиваем: вместе
     * со списком исчезла бы и дорога обратно, а искать её человеку негде. */
    function renderRail() {
      const rail = shell.rail;
      rail.textContent = '';
      const open = el('agfold');
      open.title = 'Показать чаты · ' + hotLabel();
      open.appendChild(chevron());
      open.addEventListener('click', toggleSide);
      rail.appendChild(open);
      const add = el('agnew rail', '+');
      add.title = 'Новый чат';
      add.addEventListener('click', () => createChat());
      rail.appendChild(add);
      /* Занятость соседа видна и со свёрнутой колонкой: иначе, уйдя во второй
       * разговор, о первом забывают ровно до того, как он допишет. */
      const work = chats.filter((c) => c.id && c.id !== chatId && busyOf(c.id));
      if (work.length) {
        const b = el('agbusy');
        b.title = 'Отвечают прямо сейчас: ' + work.map((c) => c.name).join(', ');
        rail.appendChild(b);
      }
    }

    /* Порядок статичный — его задаёт человек и меняет только перетаскиванием.
     * Раньше строки стояли по последней активности, и отвечающий чат уползал
     * наверх: место переставало быть местом, а список — полкой, где помнишь,
     * где что лежит. Демон отдаёт чаты в порядке массива настроек; мы его НЕ
     * трогаем. Разговоры с диска (id: null) человек не расставлял — они идут
     * отдельной группой ниже, там сортировка по времени осмысленна. */
    const match = (c) => {
      const q = query.trim().toLowerCase();
      if (!q) return true;
      return ((c.name || '') + ' ' + (c.preview || '')).toLowerCase().includes(q);
    };
    const visible = () => chats.filter(match);
    const mine = () => visible().filter((c) => c.id);
    const fromDisk = () => visible().filter((c) => !c.id);

    function renderList() {
      const list = shell.list;
      /* Отпустили над пустотой списка — жест отменяем, а не роняем «никуда».
       * Вешаем один раз: renderList зовётся на каждое событие потока. */
      if (!list.dataset.dragBound) {
        list.dataset.dragBound = '1';
        list.addEventListener('mouseup', () => chatDragCancel());
        list.addEventListener('mouseleave', () => chatDragCancel());
      }
      list.textContent = '';
      const rows = visible();
      for (const c of mine()) list.appendChild(chatRow(c));
      /* История с диска — отдельной сворачиваемой группой ниже. В общем списке
       * она забивала бы сетку, где человек помнит места: разговоров с диска
       * много, и порядок в них он не задавал. */
      const disk = fromDisk();
      if (disk.length) {
        const head = el('aggroup' + (diskOpen ? ' on' : ''));
        head.appendChild(chevron());
        head.appendChild(el('aggroupname', 'История с диска · ' + disk.length));
        head.title = diskOpen ? 'Свернуть историю' : 'Показать разговоры, найденные на диске';
        head.addEventListener('click', () => { diskOpen = !diskOpen; renderList(); });
        list.appendChild(head);
        if (diskOpen) for (const c of disk) list.appendChild(chatRow(c));
      }
      // Пустой список без слов — не «чисто», а непонятно: ищущему скажем, что
      // не нашлось, новому — с чего начать.
      if (!rows.length && (query.trim() || !hidden)) {
        list.appendChild(el('agempty', query.trim()
          ? 'Ничего не нашлось. Попробуй другое слово или очисти поиск.'
          : 'Разговоров пока нет — напиши первую реплику, и чат появится здесь.'));
      }
      /* Скрытие обратимо только пока о нём помнят: без этой строки спрятанный
       * разговор ничем не отличается от потерянного, а искать его негде. */
      if (hidden && !query.trim()) {
        const back = el('aghidden', 'Скрыто ' + hidden + ' · вернуть');
        back.title = 'Вернуть скрытые разговоры в список — файлы всё это время на диске';
        back.addEventListener('click', unhideAll);
        list.appendChild(back);
      }
    }

    /* Строка списка: имя, время, превью последней реплики и размер. Заголовок
     * берём у демона (имя человека, первую реплику и «Новый чат» он уже сложил).
     * Разговор с диска — та же строка с бейджем, а не отдельный раздел: чата за
     * ним ещё нет, поэтому клик его ПРИВЯЗЫВАЕТ (agent_chat_open), а не
     * переключает. Редкие действия — под «…», а не тремя иконками в ряд. */
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
      /* Автономия читается СО СТРОКИ: пять чатов на «спроси» и один, который
       * работает сам, должны различаться мгновенно, а не выясняться в настройках. */
      if (c.auto) {
        const a = el('agauto', '⟳');
        a.title = 'Работает сам: продолжает заходы без твоей кнопки';
        line.appendChild(a);
      }
      if (disk) line.appendChild(el('agdisk', 'с диска'));
      line.appendChild(el('spacer'));
      const when = whenLabel(c.at);
      if (when) line.appendChild(el('agtime', when));
      main.appendChild(line);
      const under = el('agsub');
      /* Недописанное вытесняет превью: строка отвечает на вопрос «где я
       * остановился», а не «чем кончилось». Без пометки человек не знает, что в
       * соседнем чате его ждёт неотправленный текст, — и пишет то же самое заново
       * либо, хуже, отправляет не туда. */
      const d = markOf(c.id);
      if (d) under.appendChild(el('agdraft', 'Черновик: ' + d));
      // Превью — только когда оно добавляет: у безымянного чата заголовок и есть
      // первая реплика, и вторая её копия под ней — просто шум.
      else under.appendChild(el('agprev', c.preview && c.preview !== c.name ? c.preview : ''));
      under.appendChild(el('agmeta', sizeOf(c)));
      // Расход автономного — там же, где он сам: иначе «дорого» так и останется
      // ощущением. Чисел нет — строки нет: прочерк тут ничего не добавляет.
      if (c.auto && c.spend && c.spend.known) {
        const sp = el('agspend', usd(c.spend.night) + ' за ночь');
        sp.title = 'За сутки ' + usd(c.spend.day) + ' из ' + usd(c.spend.dayCap)
          + '; ночной потолок ' + usd(c.spend.nightCap);
        under.appendChild(sp);
      }
      main.appendChild(under);
      row.appendChild(main);
      row.title = disk
        ? 'Разговор с диска — открыть и завести под него чат'
        : (work ? 'Отвечает прямо сейчас · ' : '') + (open ? 'Открыт' : 'Открыть «' + c.name + '»');
      row.addEventListener('click', () => {
        // Клик прилетает следом за отпусканием — без этого перестановка
        // заодно открывала бы чат, на который бросили.
        if (dropped) { dropped = false; return; }
        return disk ? openThread(c) : open ? afterPick() : switchTo(c.id);
      });
      // Правый клик — там же, где он и ожидается; «…» — для тех, кто мышью не
      // правой. Оба ведут в одно меню: два разных набора действий разъехались бы.
      row.addEventListener('contextmenu', (e) => { e.preventDefault(); e.stopPropagation(); openMenu(c, row); });
      const dots = el('agdots', '···');
      dots.title = 'Переименовать, скрыть, забыть';
      dots.addEventListener('click', (e) => { e.stopPropagation(); openMenu(c, row); });
      row.appendChild(dots);
      /* Тянуть — только за свою зону. Если тащить можно всю строку, список
       * начинает ездить от любого движения мышью с зажатой кнопкой, и обычный
       * клик «открыть чат» становится лотереей. У разговора с диска ручки нет:
       * его место человек не задавал.
       *
       * Мышь, а не HTML5 drag&drop: у Tauri на macOS включён свой перехватчик
       * перетаскивания на уровне вебвью (он ловит файлы, брошенные в окно), и
       * события dragstart/drop до страницы не доходят вовсе — код был рабочим,
       * а платформа его глушила. Обычные mousedown/mousemove/mouseup от этого
       * не зависят и проверяются синтетическими событиями. */
      if (!disk) {
        const grip = el('aggrab');
        grip.textContent = '⠿';
        grip.title = 'Перетащить — порядок задаёшь ты';
        grip.addEventListener('click', (e) => e.stopPropagation());
        grip.addEventListener('mousedown', (e) => {
          e.preventDefault();
          e.stopPropagation();
          chatDragStart(c.id, row);
        });
        row.insertBefore(grip, main);
        // Наведение считаем строкой, а не координатами: elementFromPoint в
        // тестовом DOM недоступен, а поведение должно быть одно и то же.
        row.addEventListener('mousemove', () => chatDragOver(c.id, row));
        row.addEventListener('mouseup', () => chatDragEnd(c.id));
      }
      return row;
    }

    /* Перетаскивание на обычных событиях мыши. Держим только id: строки
     * перерисовываются, и ссылка на узел протухла бы посреди жеста. */
    function chatDragStart(id, row) {
      dragId = id;
      dragOver = null;
      dropped = false;
      if (row) row.classList.add('drag');
    }
    function chatDragOver(id, row) {
      if (!dragId || id === dragId) return;
      dragOver = id;
      const list = shell && shell.list;
      if (list) for (const r of list.querySelectorAll('.agchat.over')) r.classList.remove('over');
      if (row) row.classList.add('over');
    }
    function chatDragEnd(id) {
      if (!dragId) return;
      const moved = dragId;
      const target = id && id !== moved ? id : dragOver;
      dragId = null;
      dragOver = null;
      if (!target || target === moved) { renderList(); return; }
      dropped = true; // подавляем клик, который прилетит следом за отпусканием
      dropOn(moved, target);
    }
    /* Отпустили мимо строк — жест отменён, а не «упало никуда». */
    function chatDragCancel() {
      if (!dragId) return;
      dragId = null;
      dragOver = null;
      renderList();
    }

    /* Куда упало — туда и встало: позиция цели в СПИСКЕ ЧЕЛОВЕКА (не в общем,
     * где ниже идут разговоры с диска). Порядок пишет демон в agentChat.chats,
     * поэтому он переживает перезапуск. */
    async function dropOn(movedId, targetId) {
      const order = mine().map((c) => c.id);
      const to = order.indexOf(targetId);
      if (to < 0) return;
      await listCmd('переставить чат', () => api.reorder(movedId, to));
    }

    /* ---------- меню строки: слой поверх колонки ----------
     *
     * Меню, правка имени и вопрос про удаление живут НЕ в строке. Список
     * перерисовывается на каждом событии потока — и открытый вопрос про
     * необратимое удаление сносило ответом соседнего чата прямо из-под руки.
     * Свой слой перерисовку списка переживает: renderList его не трогает. */
    const keyOf = (c) => (c.id ? 'c:' + c.id : 's:' + c.sessionId);
    const anchorTop = (row) => {
      const list = shell && shell.list;
      if (!row || !list) return 0;
      return Math.max(0, num(list.offsetTop) + num(row.offsetTop) - num(list.scrollTop));
    };
    function openMenu(c, row) {
      menu = { key: keyOf(c), c, mode: 'menu', top: anchorTop(row), sent: false };
      renderPop();
    }
    const closeMenu = () => { menu = null; renderPop(); };
    function renderPop() {
      const pop = shell && shell.pop;
      if (!pop) return;
      pop.textContent = '';
      pop.hidden = !menu;
      if (!menu) return;
      pop.style.top = menu.top + 'px';
      pop.appendChild(menu.mode === 'ask' ? askBox(menu.c) : menu.mode === 'rename' ? renameBox(menu.c) : menuBox(menu.c));
      const inp = pop.querySelector('input');
      if (inp) { inp.focus?.(); inp.select?.(); }
    }

    function menuBox(c) {
      const box = el('agmenu');
      const item = (label, title, run, cls) => {
        const it = el('agmi' + (cls ? ' ' + cls : ''), label);
        it.title = title;
        it.addEventListener('click', (e) => { e.stopPropagation(); run(); });
        box.appendChild(it);
      };
      // Последний чат демон не отдаст — ни убрать, ни стереть за ним файл: оба
      // пункта обещали бы заведомый отказ.
      const spare = chats.filter((x) => x.id).length > 1;
      if (c.id) {
        item('Переименовать', 'Дать чату своё имя', () => { menu.mode = 'rename'; renderPop(); });
        if (spare) {
          item('Скрыть', 'Убрать чат из списка — разговор останется на диске и вернётся строкой «с диска»',
            () => { closeMenu(); removeChat(c); });
        }
      } else {
        item('Открыть', 'Завести чат под этот разговор с диска', () => { closeMenu(); openThread(c); });
        item('Скрыть', 'Убрать из списка — разговор останется на диске', () => { closeMenu(); hideThread(c); });
      }
      /* Забвение необратимо — и отличается ровно этим: своей краской и отбивкой,
       * а не третьей кнопкой в ряду. Серым по серому, как метаданные, оно и не
       * читалось действием. Спрашиваем отдельно, следующим шагом. */
      if (c.sessionId && (spare || !c.id)) {
        item('Забыть насовсем', 'Стереть транскрипт с диска — спросим перед удалением',
          () => { menu.mode = 'ask'; renderPop(); }, 'danger');
      }
      return box;
    }

    /* Правка имени на месте — так же коротко, как имя чата в списке сессий:
     * имя короткое, отдельного диалога ему не нужно. */
    function renameBox(c) {
      const box = el('agmenu');
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
        if (e.key === 'Enter') { e.preventDefault(); if (!stop()) { closeMenu(); commitRename(c.id, inp.value); } }
        else if (e.key === 'Escape') { e.preventDefault(); if (!stop()) closeMenu(); }
      });
      inp.addEventListener('blur', () => { if (!stop()) closeMenu(); });
      box.appendChild(inp);
      return box;
    }

    /* Единственное необратимое действие окна — и потому единственное, которое
     * спрашивает. Спрашивает вслух, кнопкой: невидимый модификатор (alt-клик)
     * нельзя обнаружить, а необратимое не должно зависеть от того, знал ли
     * человек про комбинацию. Вопрос называет, ЧТО исчезнет: заголовок и размер
     * разговора — по ним его и узнают в списке. */
    function askBox(c) {
      const n = Number(c.turns) || 0;
      const size = n
        ? ' В нём ' + n + ' ' + window.JarvisMarkdown.plural(n, 'реплика', 'реплики', 'реплик') + ', и вернуть их будет нечем.'
        : ' Вернуть его будет нечем.';
      const ask = el('agask');
      ask.appendChild(el('agasktext',
        'Удалить разговор «' + cut(c.name, 60) + '» с диска?' + size + (c.id ? ' Чат исчезнет вместе с ним.' : '')));
      const btns = el('agaskbtns');
      const yes = el('agbtn danger', 'Удалить');
      const no = el('agbtn', 'Отмена');
      // Второе нажатие по уже отвеченному вопросу вернулось бы отказом «нет на
      // диске» — отказом за то, что человек всё сделал правильно.
      yes.addEventListener('click', (e) => {
        e.stopPropagation();
        if (!menu || menu.sent) return;
        menu.sent = true;
        const target = menu.c;
        closeMenu();
        forgetThread(target);
      });
      no.addEventListener('click', (e) => { e.stopPropagation(); closeMenu(); });
      btns.append(yes, no);
      ask.appendChild(btns);
      return ask;
    }

    // Клик мимо и Escape закрывают меню: открытый слой поверх списка не должен
    // переживать то, ради чего в список и пришли.
    document.addEventListener('click', () => { if (menu) closeMenu(); });
    document.addEventListener('keydown', (e) => {
      if (!chatsRow || chatsRow.closest('[hidden]')) return; // вкладка не на экране
      if (e.key === 'Escape' && menu) { closeMenu(); return; }
      /* Esc посреди хода — стоп, и только для ОТКРЫТОГО разговора: соседний
       * пишет своё. Когда хода нет, клавишу не трогаем вовсе — пусть закрывает
       * поиск и меню, как везде; сделать вид, что что-то остановлено, нельзя.
       * Правка имени и поле поиска глушат событие у себя и сюда не доходят. */
      if (e.key === 'Escape') {
        if (!here().busy) return; // хода нет — Esc чужой, пусть закрывает поиск и вкладку
        e.preventDefault();
        /* Дальше по дереву Esc ловит панель и уводит из вкладки в список
         * сессий (renderer.js). Уйти с экрана вместе с остановкой — значит не
         * увидеть ни пометки, ни оставшихся сессий: одно нажатие делает одно. */
        e.stopPropagation();
        stopTurn(here());
        return;
      }
      // ⌘\ — тот же жест, что сворачивает боковую колонку в редакторах.
      // Встроенную в окно колонку он не трогает: сворачивать её нечем.
      if (e.key === '\\' && (e.metaKey || e.ctrlKey) && !e.altKey && !docked()) { e.preventDefault(); toggleSide(); }
    });

    /* Граница колонки тянется мышью — этим и закрыт довод «список отнял треть
     * окна»: место делит человек, а не автор. */
    function startDrag(e) {
      const x0 = num(e.clientX);
      const w0 = sideW;
      const move = (ev) => { sideW = clampW(w0 + (num(ev.clientX) - x0)); applyWidth(); };
      const up = () => {
        document.removeEventListener('mousemove', move);
        document.removeEventListener('mouseup', up);
        saveSide();
      };
      document.addEventListener('mousemove', move);
      document.addEventListener('mouseup', up);
      e.preventDefault?.();
    }

    const commitRename = (id, name) =>
      listCmd('переименовать чат', () => api.rename(id, name)).then((c) => { if (!c) renderChats(); });

    async function createChat(name) {
      const cur = await listCmd('создать чат', () => api.create(name));
      if (!cur) return;
      afterPick();
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
      afterPick();
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
      afterPick();
      await loadHistory(cur.id);
      input.focus();
    }

    /* Спрятать и вернуть — обратимая пара, файл диска обе не трогают. */
    const hideThread = (c) => listCmd('убрать разговор из истории', () => api.hide(c.sessionId));
    const unhideAll = () => listCmd('вернуть скрытые разговоры', () => api.unhideAll());

    /* Отказы ядра тут не ошибки, а объяснение порядка: «привязан к чату» и «идёт
     * ход» говорят, что сделать сначала. Их печатает listCmd; список рисуем
     * заново — файл цел, и строка обязана вернуться на место.
     *
     * За чатом файл не стереть: ядро сперва требует убрать сам чат. Оба шага
     * делаем сами — согласились именно на это, и вопрос так и был задан. */
    async function forgetThread(c) {
      if (c.id) {
        if (busyOf(c.id)) {
          addNote(here(), '«' + c.name + '» сейчас отвечает — удалить его выйдет, когда закончит.');
          return;
        }
        if (!(await listCmd('убрать чат', () => api.remove(c.id)))) { renderChats(); return; }
      }
      if (!(await listCmd('удалить разговор', () => api.forget(c.sessionId)))) { renderChats(); return; }
      /* Забвение необратимо — и черновик уходит вместе с разговором: он был
       * репликой ИМЕННО в него. «Скрыть» так не делает (removeChat, hideThread):
       * скрытие обратимо, и потерять на нём набранное значило бы обещать
       * обратимость, которой нет. */
      dropDraft(c.id);
      if (c.id) await loadHistory(chatId);
      renderChats();
    }

    /* Лента конкретного чата: историю просим по id, а не «текущую». Иначе после
     * переключения окно рисовало бы переписку соседа — ровно та тихая неправда,
     * которую не видно, пока не начнёшь читать. Прочитанную ленту не
     * перечитываем: в ней уже лежит и живой поток, которого у демона ещё нет —
     * транскрипт он допишет только под конец ответа. */
    async function loadHistory(id) {
      goTo(id); // чаще всего чат уже сменил listCmd — тогда это ничего не делает
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
      /* Отметки сжатия ставим ПО ВРЕМЕНИ, между репликами: сваленные в конец,
       * они говорили бы, что память оборвалась только что. */
      const marks = (t.marks || []).slice().sort((a, b) => (a.at || 0) - (b.at || 0));
      let mi = 0;
      const marksUpto = (ts) => {
        while (mi < marks.length && marks[mi].at && ts && marks[mi].at <= ts) addSqueeze(h, marks[mi++]);
      };
      for (const it of items) {
        marksUpto(it.ts);
        if (it.kind === 'tool') addTool(h, it.text);
        else if (it.role === 'user') addUser(h, it.text);
        else paint(addRow(h, 'assistant', el('bubble', '')), it.text);
      }
      while (mi < marks.length) addSqueeze(h, marks[mi++]);
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
      // Черновики читаем ДО первой ленты: иначе первый же кадр показал бы пустое
      // поле, и человек решил бы, что набранное пропало вместе с окном.
      const got = await readDrafts();
      if (got) for (const [id, d] of got) drafts.set(id, d);
      const cur = await listCmd('прочитать список чатов', () => api.chats());
      if (!cur) {
        // Списка нет (старый демон или отказ) — нить всё равно нужна, иначе
        // окно тихо начнёт новый диалог вместо прошлого.
        try {
          const st = await api.state();
          if (st && st.chatId) goTo(st.chatId);
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
      unpark();
      const was = chatId;
      const cur = await listCmd('обновить список чатов', () => api.chats());
      if (cur && cur.id !== was) await loadHistory(cur.id);
      await syncDrafts(); // черновики тоже могли поехать в соседнем окне
    }

    /* Уход со вкладки ленту не рушит — она остаётся в DOM вместе с набранным и
     * неотправленным. А вот прокрутка display:none не переживает: браузер
     * обнуляет scrollTop у спрятанного узла. Снимаем её на уходе и возвращаем
     * на входе — иначе возврат к Джарвису каждый раз кидал бы в конец ленты. */
    let parked = null;
    // Уходя со вкладки, недописанное дожимаем на диск: ждать полсекунды тут уже
    // некому, а вкладку закрывают вместе с окном.
    const park = () => { parked = msgs.scrollTop; flushDraft(); };
    const unpark = () => { if (parked != null) { msgs.scrollTop = parked; parked = null; } };

    /* Поиск по чатам приезжает извне, когда поле есть у самого окна (панель):
     * колонка своего не рисует, а фильтрует тем, что набрали в общем поиске. */
    const search = (q) => {
      query = String(q == null ? '' : q);
      // Искать в свёрнутой колонке некуда — раскрываем: об этом и просили.
      if (query && sideOff) { sideOff = false; syncHead(); }
      renderChats();
    };

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
        goTo(res.current || chatId); // reset чат не меняет, но ядру виднее
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
      const from = t.id; // черновик у чата, а имя чату может сменить ядро
      const back = input.value; // ровно то, что было в поле, если отправка не состоится
      const caret = caretAt();
      input.value = '';
      dirty = false;
      fitInput();
      addUser(t, text);
      scroll(); // своя реплика — единственное, за чем ленту доводим всегда
      setBusy(t, true);
      t.bubble = null;
      /* Черновик доживает до ответа ядра: «отправлено» — это когда сообщение
       * взяли, а не когда мы очистили поле. Прервали, не дошло, отказали —
       * набранное обязано остаться, переписывать его заново человек не нанимался. */
      const keep = () => {
        if (from) drafts.set(from, { text: back, caret });
        // ...а поле возвращаем, только если человек всё ещё здесь и не начал
        // писать новое: затирать набранное своей копией — та же потеря.
        if (chatId === from && !input.value.trim()) {
          input.value = back;
          fitInput();
          setCaret(caret);
          dirty = !!from;
        }
        if (from) { draftFor = from; saveNow(); }
        renderChats();
      };
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
        keep();
        return;
      }
      /* Отказ приезжает РАЗРЕШЁННЫМ промисом: нет ни claude, ни codex, не
       * прочитан jarvis-mcp.json, чат удалили в соседнем окне. catch на такое
       * не срабатывает, а событий done/failed уже не будет — «думает…» висело
       * бы вечно, и следующее сообщение отправить было нечем. */
      if (res && res.ok === false) {
        addErr(t, 'Агент не взял сообщение: ' + why(res));
        setBusy(t, false);
        keep();
        return;
      }
      // Взяли — только теперь черновик исчезает. Имя чата ядро могло сменить
      // прямо в этом ходе (relabel), поэтому стираем оба.
      dropDraft(from);
      if (t.id !== from) dropDraft(t.id);
      renderChats();
    }

    sendBtn.addEventListener('click', send);
    input.addEventListener('keydown', (e) => {
      if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); send(); }
    });
    // авто-рост поля ввода — и черновик открытого чата вместе с ним
    input.addEventListener('input', () => {
      fitInput();
      if (!chatId) return;
      dirty = true;
      takeDraft(chatId);
      saveSoon(chatId);
      // Список пересобираем, только когда пометка чата правда сменилась: на
      // каждый символ перебирать двадцать строк незачем.
      if (markOf(chatId) !== mark) renderChats();
    });

    /* Дожать на диск раньше срока: уход фокуса, скрытие окна, выход. Именно там
     * черновик и теряют — полсекунды тишины до них не доживают. */
    input.addEventListener('blur', flushDraft);
    window.addEventListener('blur', flushDraft);
    window.addEventListener('pagehide', flushDraft);
    window.addEventListener('beforeunload', flushDraft);
    document.addEventListener('visibilitychange', () => {
      if (document.visibilityState === 'hidden') flushDraft();
    });
    // Вернулись в окно — сверяемся с книжкой: в соседнем окне мог быть открыт
    // тот же чат.
    window.addEventListener('focus', () => { syncDrafts(); });

    /* ---------- остановка хода ----------
     *
     * «Esc останавливает работу Джарвиса». Рвётся ХОД одного разговора: поток
     * помечен chatId, и соседний Джарвис пишет дальше как ни в чём не бывало.
     *
     * Пришедшее из ленты НЕ стираем: недописанный ответ — это работа, ради
     * которой ход и останавливают; стереть её вместе с ходом значит наказать за
     * нажатие. Оставляем и помечаем — иначе оборванный ответ не отличить от
     * полного. Дочерние CLI не убиваем по той же причине: там своя работа, и
     * закрывает их человек, кнопкой рядом.
     *
     * (Не путать с stop() в renameBox — та защёлка про двойное применение имени.) */
    const STOPPED = 'остановлено вами';
    const stopWord = (ev) => (ev && ev.by && ev.by !== 'user' ? 'остановлено: ' + ev.by : STOPPED);

    /* Строка «что-то · действие»: ею говорим и про живую дочернюю сессию, и про
     * остановленную цепочку — оба раза рядом с фактом стоит выход. */
    function actRow(box, text, label, title, run) {
      const row = el('stoprow');
      const name = el('stopname', text);
      row.appendChild(name);
      const btn = el('agbtn', label);
      btn.title = title;
      btn.addEventListener('click', () => run(btn, name));
      row.appendChild(btn);
      box.appendChild(row);
      return row;
    }

    /* Дочерние сессии живут дальше — так и говорим, называя их поимённо.
     * Закрытие в два нажатия, как завершение сессии в списке (renderer.js,
     * killSession): за кнопкой чужая работа, и промахнуться по ней нельзя. */
    function showKids(t, kids) {
      const box = el('bubble');
      box.appendChild(el('stophead', 'Ход остановлен, а эти сессии продолжают работу:'));
      for (const k of kids) {
        const label = (k.name || k.id) + (k.agent ? ' · ' + k.agent : '');
        actRow(box, label, 'Закрыть', 'Завершить сессию — работа в ней прервётся', async (btn, name) => {
          if (btn.dataset.armed !== '1') {
            btn.dataset.armed = '1';
            btn.textContent = 'Точно закрыть?';
            btn.classList.add('danger');
            return;
          }
          let res;
          try {
            res = await api.kill(k.id);
          } catch (e) {
            addErr(t, 'Не удалось закрыть сессию: ' + errText(e));
            return;
          }
          if (res && res.ok === false) { addErr(t, 'Не удалось закрыть сессию: ' + why(res)); return; }
          btn.remove();
          name.textContent = label + ' · закрыта';
        });
      }
      addRow(t, 'note', box);
    }

    /* Цепочку гасит само ядро — тем же agent_stop: остановленный ход иначе через
     * минуту сменился бы следующим, и Esc выглядел бы сломанным. Но молчать об
     * этом нельзя — авто-продолжение человек включал сам. Спрашиваем ДО
     * остановки: после неё срез уже пустой, и живую цепочку не отличить от той,
     * которой не было. Своего мнения не сочиняем: не ответило ядро — молчим. */
    async function chainAlive(t) {
      if (!api.chainState) return false; // сборка без цепочек — говорить не о чем
      let st;
      try { st = await api.chainState(t.id); } catch { return false; }
      const c = st && st.ok !== false ? st.state : null;
      return !!c && (c.active || c.mode === 'auto');
    }

    // Цепочка встала — и рядом дорога назад: без неё «остановлено» читается как
    // «выключено навсегда», а включал его человек одним нажатием.
    function chainRow(t) {
      const box = el('bubble');
      actRow(box, 'Цепочка остановлена — сам продолжать не буду.', 'Продолжить цепочку',
        'Вернуть авто-продолжение этому чату', async (btn, name) => {
          let r;
          try {
            r = await api.chainMode(t.id, true);
          } catch (e) {
            addErr(t, 'Не удалось вернуть авто-продолжение: ' + errText(e));
            return;
          }
          if (r && r.ok === false) { addErr(t, 'Не удалось вернуть авто-продолжение: ' + why(r)); return; }
          btn.remove();
          name.textContent = 'Цепочка снова продолжает сама.';
        });
      addRow(t, 'note', box);
    }

    /* Пометка на ленте. Зовут её двое — ответ команды и событие потока (ход
     * могли остановить из соседнего окна), а ход один: латч не даёт написать об
     * одной остановке дважды. */
    function markStopped(t, ev) {
      if (t.stopMark) return;
      t.stopMark = true;
      const was = near(t);
      setBusy(t, false);
      const row = t.bubble && t.bubble.parentElement;
      if (row) {
        grow(t.bubble, t.raw); // хвост дорисован тем же разбором, что и целый текст
        row.appendChild(el('stopmark', stopWord(ev)));
      } else {
        // Сказать нечего — тогда словами ядра: это и есть та короткая строка,
        // ради которой оно шлёт note. Над ответом она была бы второй пометкой.
        addNote(t, (ev && ev.note) || 'Остановлено вами — сказать агент ничего не успел.');
      }
      t.bubble = null;
      t.tools = null;
      const kids = (ev && ev.children) || [];
      if (kids.length) showKids(t, kids);
      keepDown(was);
    }

    /* Esc и кнопка ведут в одну дверь: два способа не должны расходиться в
     * поведении. Латч на разговор — Esc жмут подряд, а ход рвут один раз. */
    async function stopTurn(t) {
      if (!t.busy || t.stopping) return;
      if (!api.stop) {
        addErr(t, 'Остановить ход нечем: эта сборка Jarvis такого ещё не умеет — обнови.');
        return;
      }
      t.stopping = true;
      const chain = await chainAlive(t); // до остановки: ядро гасит цепочку вместе с ходом
      let res;
      try {
        res = await api.stop(t.id);
      } catch (e) {
        addErr(t, 'Не удалось остановить ход: ' + errText(e));
        t.stopping = false;
        return;
      }
      t.stopping = false;
      if (res && res.ok === false) { addErr(t, 'Не удалось остановить ход: ' + why(res)); return; }
      /* `stopped:false` — хода не было. Это не отказ: показывать нечего, а
       * «остановлено» за неслучившееся и есть та ложь, которой тут не место.
       * Занятость снимаем: раз ядро говорит, что никто не пишет, — не пишет. */
      if (res && res.stopped === false) setBusy(t, false);
      else markStopped(t, res || {});
      // Цепочку ядро оборвало в любом случае — и об этом говорим даже там, где
      // хода не было: карусель заходов остановлена, а это видимая перемена.
      if (chain) chainRow(t);
    }

    /* Поток ответа агента. Событие адресовано ЧАТУ, а не окну: chatId в нагрузке
     * и решает, чья это лента, — потому и можно уйти во второй разговор, пока
     * первый пишет. Пометки нет — так шлёт только старый демон, у которого
     * поток и был один: отдаём открытому. */
    /* Свой канал цепочки: заход ушёл, режим сменили в соседнем окне, цепочка
     * встала о потолок, утро принесло сводку. Срез идёт в шапку и строку списка,
     * а kind с текстом — карточкой в ленту: без второго ночная работа видна
     * только в файле журнала. */
    if (api.onChain) api.onChain((ev0) => {
      const ev = ev0 || {};
      applyChain(ev.chatId, ev.state);
      chainCard(ev);
    });

    const END = { done: 1, failed: 1, stopped: 1 }; // события конца хода
    api.onEvent((ev0) => {
      const ev = ev0 || {};
      const t = ev.chatId ? thread(ev.chatId) : here();
      // Пошёл поток — разговор занят, даже если реплику отправили в соседнем
      // окне: иначе там «думает…», а здесь тот же чат выглядит свободным.
      // Концы хода (в том числе остановка) занятость, наоборот, не включают.
      if (!t.busy && !END[ev.type]) setBusy(t, true);
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
        /* Счётчик контекста. Части приезжают из разных событий — занятое с
         * каждой репликой, потолок один раз с итогом, — поэтому копим, а не
         * перезаписываем: иначе итог обнулял бы занятое. */
        case 'context': {
          const c = t.ctx || (t.ctx = { used: null, window: null, exact: false });
          if (Number.isFinite(ev.used)) c.used = ev.used;
          if (Number.isFinite(ev.window)) { c.window = ev.window; c.exact = !!ev.window_exact; }
          if (shown(t)) syncHead();
          ctxWatch(t);
          break;
        }
        case 'squeezed':
          addSqueeze(t, ev);
          break;
        /* Ход остановлен — в том числе из соседнего окна или кнопкой: лента
         * обязана сказать это сама, а не ждать, пока человек догадается по
         * замолчавшему пузырю. */
        case 'stopped':
          markStopped(t, ev);
          break;
        case 'done':
          if (ev.session_id) t.session = ev.session_id;
          /* Хвост остановленного хода. Пометка уже стоит, а финальный текст
           * дорисовал бы ВТОРОЙ пузырь — копию того же оборванного ответа. */
          if (t.stopMark) { setBusy(t, false); break; }
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
          // «Прервано» вслед за нашей же остановкой — не новость, а её эхо.
          if (t.stopMark) break;
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
      /* Один вопрос — одна карточка. Событие приходит и во вкладку, и в окно, а
       * при переподписке (редок, но бывает) и дважды в одно окно: вторая
       * карточка выглядела бы как второй вопрос, и человек соглашался бы дважды
       * на одно действие. Ключ — nonce: он и так одноразовый в демоне. */
      if (c.nonce && openCards.has(c.nonce)) return;
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
      const presence = watchPresence(box);
      const decide = async (approved) => {
        if (card.sent || !openCards.has(c.nonce)) return; // повтор демону не нужен
        /* Согласие — только с признаками человека за клавишами. Не принятое
         * нажатие НЕ съедает карточку: она ждёт дальше, и человеку сказано,
         * чего не хватило, — иначе это выглядит как «нажал, и ничего». */
        const armed = approved ? armWhy(presence, Date.now(), focusedAt) === null : true;
        if (approved && !armed) {
          if (!line.parentElement) box.appendChild(line);
          line.textContent =
            'нажатие не принято: ' + armWhy(presence, Date.now(), focusedAt) + ' — нажмите ещё раз';
          return;
        }
        card.waiting();
        api.confirmDone(c.nonce); // соседнему окну: вопрос больше не ждёт нажатия
        let res;
        try {
          res = await api.confirm(c.nonce, approved, armed);
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
    return { refresh, park, search, redock: () => { shell = null; renderChats(); } };
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
        {
          msgs: q('agLog'), input: q('agInput'), sendBtn: q('agSend'), sub: q('agSub'), tag: q('agTag'), newBtn: q('agNew'),
          // Колонка чатов в оконном режиме переезжает в сетку окна (renderer.js,
          // dockAgentSide) — под своим корнем её там уже не найти.
          chatsRow: q('agChats') || document.getElementById('agChats'),
          extFind: true, // по чатам ищет строка поиска панели, а не своё поле
        },
        {
          state: () => j.agentChatState(),
          history: (chatId) => j.agentChatHistory(chatId),
          // остаток лимита провайдера — число ДРУГОЕ, чем счётчик контекста, и
          // приезжает своей командой: сложить их в одно нельзя
          limit: () => j.getLimit(),
          reset: () => j.agentChatReset(),
          chats: () => j.agentChatsList(),
          switch: (chatId) => j.agentChatSwitch(chatId),
          create: (name) => j.agentChatCreate(name),
          rename: (chatId, name) => j.agentChatRename(chatId, name),
          remove: (chatId) => j.agentChatDelete(chatId),
          reorder: (chatId, toIndex) => j.agentChatReorder(chatId, toIndex),
          // привязать разговор, найденный на диске (не «открыть окно чата»:
          // окно поднимает agent_chat_window)
          open: (sessionId) => j.agentChatOpen(sessionId),
          // недописанное: своё у каждого чата, своим же файлом на диске
          drafts: () => j.agentDraftsGet(),
          draftSet: (chatId, text, caret) => j.agentDraftSet(chatId, text, caret),
          // спрятать строку с диска (файл цел), вернуть все спрятанные и —
          // с подтверждением выше — удалить транскрипт насовсем
          hide: (sessionId) => j.agentHistoryHide(sessionId),
          unhideAll: () => j.agentHistoryUnhideAll(),
          forget: (sessionId) => j.agentHistoryForget(sessionId),
          send: (message, chatId, sessionId) => j.agentSend(message, chatId, sessionId),
          // остановка хода и пауза авто-продолжения — по одному Esc
          stop: (chatId) => j.agentStop(chatId),
          chainState: (chatId) => j.agentChainState(chatId),
          chainMode: (chatId, auto) => j.agentChainMode(chatId, auto),
          /* Предложенный заход по кнопке; text — только если человек поправил.
           * Моста постарше может и не быть: тогда кнопка честно скажет, что
           * отправлять нечем, вместо тихой ошибки в консоли. */
          chainSend: j.agentChainSend && ((chatId, text) => j.agentChainSend(chatId, text)),
          /* Выходы из паузы, которую поставила задача от человека. Моста
           * постарше может не быть — кнопка тогда честно скажет об этом. */
          chainResume: j.agentChainResume && ((chatId) => j.agentChainResume(chatId)),
          chainStop: j.agentChainStop && ((chatId) => j.agentChainStop(chatId)),
          // Канала цепочки может не быть (старый мост) — тогда шапка живёт на
          // одних ответах команд, а не падает вместе со вкладкой.
          onChain: (cb) => j.onAgentChain && j.onAgentChain(cb),
          // закрыть дочернюю сессию — той же командой, что и список сессий
          kill: (sessionId) => j.killSession(sessionId),
          confirm: (nonce, approved, armed) => j.agentConfirm(nonce, approved, armed),
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
    return tab; // рукоятка вкладки: панель через неё паркует ленту и ищет по чатам
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
        limit: () => invoke('limit_get'),
        reset: () => invoke('agent_chat_reset'),
        // Полный список и здесь: окно из трея — единственный вход, когда панель
        // закрыта, и «только текущий чат» отрезал бы от остальных проектов.
        chats: () => invoke('agent_chats_list'),
        switch: (chatId) => invoke('agent_chat_switch', { chatId }),
        create: (name) => invoke('agent_chat_create', { name }),
        rename: (chatId, name) => invoke('agent_chat_rename', { chatId, name }),
        remove: (chatId) => invoke('agent_chat_delete', { chatId }),
        reorder: (chatId, toIndex) => invoke('agent_chat_reorder', { chatId, toIndex }),
        open: (sessionId) => invoke('agent_chat_open', { sessionId }),
        drafts: () => invoke('agent_drafts_get'),
        draftSet: (chatId, text, caret) => invoke('agent_draft_set', { chatId, text, caret }),
        hide: (sessionId) => invoke('agent_history_hide', { sessionId }),
        unhideAll: () => invoke('agent_history_unhide_all'),
        forget: (sessionId) => invoke('agent_history_forget', { sessionId }),
        send: (message, chatId, sessionId) => invoke('agent_send', { message, chatId, sessionId }),
        stop: (chatId) => invoke('agent_stop', { chatId }),
        chainState: (chatId) => invoke('agent_chain_state', { chatId }),
        chainMode: (chatId, auto) => invoke('agent_chain_mode', { chatId, auto }),
        chainSend: (chatId, text) => invoke('agent_chain_send', { chatId, text }),
        chainResume: (chatId) => invoke('agent_chain_resume', { chatId }),
        chainStop: (chatId) => invoke('agent_chain_stop', { chatId }),
        onChain: (cb) => listen('agent:chain', (e) => cb(e.payload)),
        kill: (sessionId) => invoke('session_kill', { sessionId }),
        confirm: (nonce, approved, armed) => invoke('agent_confirm', { nonce, approved, armed }),
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
