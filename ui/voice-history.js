/* ============================================================================
 * voice-history.js — самодостаточный модуль страницы «История голосового ввода».
 *
 * Экспортирует window.initVoiceHistory(rootEl): строит Wispr-подобную ленту
 * ВСЕГО голосового ввода — и диктовки (F8), и разговоров («Hey Jarvis») вместе,
 * с разделением по источнику и фильтром. История читается из IPC (window.jarvis).
 *
 * Дизайн 1:1 по макету docs/superpowers/mockups/jisper.html: тёмное стекло,
 * акцент #6ca0ff, моно для времени, поиск сверху, полоса статистики, липкие
 * НЕПРОЗРАЧНЫЕ заголовки дней, чистые строки (БЕЗ иконок-плиток источника),
 * ховер-действия, меню «Преобразовать ▾», светлая карточка результата
 * «Модифицированный текст», нижняя action-bar.
 *
 * ОТЛИЧИЕ от макета: страница КОМБИНИРОВАННАЯ → добавлен сегментный фильтр
 * источника (Все · Голос · Диктовка) и метка источника в каждой строке.
 *
 * Чистый ванильный JS под WKWebView: никаких импортов, фреймворков, CDN.
 * DOM строится через createElement / createElementNS — без innerHTML (XSS-хук).
 * Каждый вызов IPC в try/catch — наружу не бросаем. Повторный init() полностью
 * перестраивает UI без дублей стилей и без утечек слушателей.
 * ========================================================================== */
(function () {
  'use strict';

  // ── Модульные флаги (живут между ре-init) ───────────────────────────────
  let docClickBound = false;   // глобальный «клик мимо» для закрытия меню — ставим раз
  let openMenu = null;         // текущее открытое меню «Преобразовать» (DOM-узел)

  // Состояние активной страницы (пересоздаётся на каждый init)
  let state = null;

  // ── Библиотека преобразований (стили для transcriptEnhance) ─────────────
  const TRANSFORMS = [
    { style: 'prompt',    name: 'Промпт для агента',     hint: '⌘1' },
    { style: 'commit',    name: 'Коммит-сообщение',      hint: '⌘2' },
    { style: 'clean',     name: 'Чистовик · грамматика', hint: '⌘3' },
    { style: 'translate', name: 'Перевод на English',    hint: '⌘4' },
  ];

  /* ── IPC-обёртки: никогда не бросают наружу ─────────────────────────────── */
  async function ipcGet() {
    try {
      if (!window.jarvis || typeof window.jarvis.transcriptsGet !== 'function') return [];
      const r = await window.jarvis.transcriptsGet();
      const items = (r && Array.isArray(r.items)) ? r.items : [];
      // нормализуем форму: id (может отсутствовать → фолбэк по индексу), text, ts, source
      return items.map((it, i) => ({
        id: (it && it.id != null) ? it.id : null,
        idx: i,
        text: (it && typeof it.text === 'string') ? it.text : String((it && it.text) || ''),
        ts: (it && Number(it.ts)) || 0,
        source: (it && it.source === 'wake') ? 'wake' : 'dictation',
      }));
    } catch (e) { return []; }
  }

  async function ipcClear() {
    try {
      if (window.jarvis && typeof window.jarvis.transcriptsClear === 'function') {
        await window.jarvis.transcriptsClear();
      }
    } catch (e) { /* проглатываем */ }
  }

  async function ipcEnhance(text, style) {
    try {
      if (!window.jarvis || typeof window.jarvis.transcriptEnhance !== 'function') {
        return { ok: false, error: 'недоступно' };
      }
      const r = await window.jarvis.transcriptEnhance(text, style);
      return (r && typeof r === 'object') ? r : { ok: false, error: 'нет ответа' };
    } catch (e) { return { ok: false, error: 'ошибка' }; }
  }

  // transcriptDelete может отсутствовать в мосте → определяем доступность
  function hasDelete() {
    return !!(window.jarvis && typeof window.jarvis.transcriptDelete === 'function');
  }
  async function ipcDelete(id) {
    try {
      if (!hasDelete() || id == null) return { ok: false };
      const r = await window.jarvis.transcriptDelete(id);
      return (r && typeof r === 'object') ? r : { ok: true };
    } catch (e) { return { ok: false }; }
  }

  async function copyText(text) {
    try {
      await navigator.clipboard.writeText(String(text));
      return true;
    } catch (e) { return false; }
  }

  /* ── Тосты: используем глобальный window.showToast, иначе свой минимальный
   *    (тот же класс .toast, что описан в index.html) ──────────────────────── */
  let toastTimer = null;
  function showToast(msg) {
    if (typeof window.showToast === 'function' && window.showToast !== showToast) {
      try { window.showToast(msg); return; } catch (e) { /* падаем на локальный */ }
    }
    try {
      document.querySelector('.toast')?.remove();
      clearTimeout(toastTimer);
      const t = document.createElement('div');
      t.className = 'toast';
      t.textContent = String(msg);
      document.body.appendChild(t);
      toastTimer = setTimeout(() => t.remove(), 2200);
    } catch (e) { /* нет document.body — молча */ }
  }

  /* ── Утилиты времени/слов ────────────────────────────────────────────────── */
  function tsToDate(ts) { return new Date(ts * 1000); } // ts — unix-секунды

  function pad2(n) { return n < 10 ? '0' + n : '' + n; }

  function fmtTime(ts) {
    const d = tsToDate(ts);
    return pad2(d.getHours()) + ':' + pad2(d.getMinutes());
  }

  // ключ локального дня (YYYY-M-D) для группировки
  function dayKey(ts) {
    const d = tsToDate(ts);
    return d.getFullYear() + '-' + d.getMonth() + '-' + d.getDate();
  }

  const MONTHS = ['января', 'февраля', 'марта', 'апреля', 'мая', 'июня',
    'июля', 'августа', 'сентября', 'октября', 'ноября', 'декабря'];

  // Человеческая метка дня: Сегодня / Вчера / DD месяц
  function dayLabel(ts) {
    const d = tsToDate(ts);
    const now = new Date();
    const today = new Date(now.getFullYear(), now.getMonth(), now.getDate());
    const that = new Date(d.getFullYear(), d.getMonth(), d.getDate());
    const diffDays = Math.round((today - that) / 86400000);
    if (diffDays === 0) return 'Сегодня';
    if (diffDays === 1) return 'Вчера';
    return d.getDate() + ' ' + MONTHS[d.getMonth()];
  }

  function wordCount(text) {
    const t = String(text || '').trim();
    if (!t) return 0;
    return t.split(/\s+/).filter(Boolean).length;
  }

  // Русское склонение «слово»
  function pluralWords(n) {
    const m100 = n % 100, m10 = n % 10;
    if (m100 >= 11 && m100 <= 14) return 'слов';
    if (m10 === 1) return 'слово';
    if (m10 >= 2 && m10 <= 4) return 'слова';
    return 'слов';
  }
  function pluralReplicas(n) {
    const m100 = n % 100, m10 = n % 10;
    if (m100 >= 11 && m100 <= 14) return 'реплик';
    if (m10 === 1) return 'реплика';
    if (m10 >= 2 && m10 <= 4) return 'реплики';
    return 'реплик';
  }

  function sourceLabel(src) {
    return src === 'wake' ? 'Hey Jarvis' : 'диктовка F8';
  }

  // Разделитель тысяч (узкий пробел) — как «1 240» в макете
  function fmtNum(n) {
    return String(n).replace(/\B(?=(\d{3})+(?!\d))/g, ' ');
  }

  /* ── Инъекция стилей (один раз, guard по id) ─────────────────────────────── */
  function injectStyle() {
    if (document.getElementById('voice-history-style')) return;
    const style = document.createElement('style');
    style.id = 'voice-history-style';
    style.textContent = CSS;
    document.head.appendChild(style);
  }

  /* ── Хелперы DOM ─────────────────────────────────────────────────────────── */
  function el(tag, cls, text) {
    const n = document.createElement(tag);
    if (cls) n.className = cls;
    if (text != null) n.textContent = text;
    return n;
  }
  // лупа/иконки — инлайновый SVG через createElementNS (без innerHTML)
  const SVG_NS = 'http://www.w3.org/2000/svg';
  function svgSearch() {
    const svg = document.createElementNS(SVG_NS, 'svg');
    svg.setAttribute('viewBox', '0 0 24 24');
    svg.setAttribute('width', '15'); svg.setAttribute('height', '15');
    svg.setAttribute('fill', 'none');
    svg.setAttribute('stroke', 'currentColor');
    svg.setAttribute('stroke-width', '2');
    svg.setAttribute('stroke-linecap', 'round');
    const c = document.createElementNS(SVG_NS, 'circle');
    c.setAttribute('cx', '11'); c.setAttribute('cy', '11'); c.setAttribute('r', '7');
    const l = document.createElementNS(SVG_NS, 'line');
    l.setAttribute('x1', '21'); l.setAttribute('y1', '21');
    l.setAttribute('x2', '16.65'); l.setAttribute('y2', '16.65');
    svg.appendChild(c); svg.appendChild(l);
    return svg;
  }

  /* ── Фильтрация по поиску + источнику ────────────────────────────────────── */
  function visibleItems() {
    const q = state.query.trim().toLowerCase();
    const f = state.filter; // 'all' | 'wake' | 'dictation'
    return state.items.filter((it) => {
      if (f !== 'all' && it.source !== f) return false;
      if (q && !it.text.toLowerCase().includes(q)) return false;
      return true;
    });
  }

  /* ── Перерасчёт и отрисовка статистики ───────────────────────────────────── */
  function renderStats() {
    const all = state.items;
    const todayKey = dayKey(Math.floor(Date.now() / 1000));
    let today = 0, words = 0;
    for (const it of all) {
      if (dayKey(it.ts) === todayKey) today++;
      words += wordCount(it.text);
    }
    state.statToday.textContent = fmtNum(today);
    state.statTotal.textContent = fmtNum(all.length);
    state.statWords.textContent = fmtNum(words);
  }

  /* ── Меню «Преобразовать» ────────────────────────────────────────────────── */
  function closeMenu() {
    if (openMenu) { openMenu.remove(); openMenu = null; }
  }

  function buildTransformMenu(item, entryNode, bodyNode) {
    closeMenu();
    const menu = el('div', 'vh-tmenu');
    menu.appendChild(el('div', 'vh-tmh', 'Преобразовать'));
    for (const tr of TRANSFORMS) {
      const ti = el('div', 'vh-ti');
      ti.appendChild(el('span', 'vh-tn', tr.name));
      ti.appendChild(el('span', 'vh-th', tr.hint));
      ti.addEventListener('click', (e) => {
        e.stopPropagation();
        closeMenu();
        runEnhance(item, bodyNode, tr);
      });
      menu.appendChild(ti);
    }
    menu.appendChild(el('div', 'vh-tdiv'));
    const add = el('div', 'vh-ti vh-add');
    add.appendChild(el('span', 'vh-tn', '＋ Настроить промпты…'));
    add.addEventListener('click', (e) => {
      e.stopPropagation();
      closeMenu();
      showToast('скоро');
    });
    menu.appendChild(add);
    // клик внутри меню не должен закрывать его через docClick
    menu.addEventListener('click', (e) => e.stopPropagation());
    entryNode.appendChild(menu);
    openMenu = menu;
  }

  /* ── Запуск преобразования: «Думаю…» → светлая карточка результата ───────── */
  async function runEnhance(item, bodyNode, tr) {
    // убираем прежнюю карточку, если была
    const prev = bodyNode.querySelector('.vh-enh');
    if (prev) prev.remove();

    const enh = el('div', 'vh-enh');
    const head = el('div', 'vh-eh');
    const chip = el('span', 'vh-chip', 'Модифицированный текст');
    head.appendChild(chip);
    head.appendChild(el('span', 'vh-sp'));
    const etext = el('div', 'vh-etext', 'Думаю…');
    enh.appendChild(head);
    enh.appendChild(etext);
    bodyNode.appendChild(enh);

    const res = await ipcEnhance(item.text, tr.style);
    if (!res || !res.ok) {
      // по контракту: стиль не поддержан → ok:false. Показываем мягкий тост.
      enh.remove();
      showToast('не поддерживается');
      return;
    }
    const result = String(res.result || '');
    etext.textContent = result;

    // кнопки в заголовке: Копировать (primary) + ⋯ (overflow → Заменить / Скрыть)
    const copyBtn = el('button', 'vh-acc', 'Копировать');
    copyBtn.addEventListener('click', async (e) => {
      e.stopPropagation();
      const ok = await copyText(result);
      showToast(ok ? 'Скопировано' : 'Не удалось скопировать');
    });
    const more = el('button', 'vh-eh-icon', '⋯');
    more.title = 'Заменить · Скрыть';
    more.addEventListener('click', (e) => {
      e.stopPropagation();
      buildOverflowMenu(more, enh, item, bodyNode, result);
    });
    head.appendChild(copyBtn);
    head.appendChild(more);
  }

  // мини-меню overflow карточки результата: Заменить / Скрыть
  function buildOverflowMenu(anchor, enhNode, item, bodyNode, result) {
    closeMenu();
    const menu = el('div', 'vh-tmenu vh-ovf');
    const mk = (label, fn) => {
      const ti = el('div', 'vh-ti');
      ti.appendChild(el('span', 'vh-tn', label));
      ti.addEventListener('click', (e) => { e.stopPropagation(); closeMenu(); fn(); });
      return ti;
    };
    menu.appendChild(mk('Заменить', () => {
      // визуально заменяем текст строки результатом (на сессию)
      item.text = result;
      const textNode = bodyNode.querySelector('.vh-text');
      if (textNode) textNode.textContent = result;
      enhNode.remove();
    }));
    menu.appendChild(mk('Скрыть', () => { enhNode.remove(); }));
    menu.addEventListener('click', (e) => e.stopPropagation());
    // якорим у карточки результата
    enhNode.style.position = 'relative';
    enhNode.appendChild(menu);
    openMenu = menu;
  }

  /* ── Строка-запись ───────────────────────────────────────────────────────── */
  function buildEntry(item) {
    const entry = el('div', 'vh-entry');

    const body = el('div', 'vh-body');
    body.appendChild(el('div', 'vh-text', item.text));

    const meta = el('div', 'vh-meta');
    meta.appendChild(el('span', 'vh-t', fmtTime(item.ts)));
    meta.appendChild(el('span', 'vh-sep', '·'));
    meta.appendChild(el('span', 'vh-lbl', sourceLabel(item.source)));
    meta.appendChild(el('span', 'vh-sep', '·'));
    if (item.source === 'dictation') {
      // диктовка авто-копируется в буфер → «✓ в буфере»
      meta.appendChild(el('span', 'vh-copied', '✓ в буфере'));
    } else {
      const wc = wordCount(item.text);
      meta.appendChild(el('span', 'vh-words', wc + ' ' + pluralWords(wc)));
    }
    body.appendChild(meta);
    entry.appendChild(body);

    // ── ховер-действия ──
    const acts = el('div', 'vh-acts');
    const transformBtn = el('button', 'vh-primary', 'Преобразовать ▾');
    transformBtn.addEventListener('click', (e) => {
      e.stopPropagation();
      if (openMenu && openMenu.parentNode === entry && openMenu.classList.contains('vh-tmenu') && !openMenu.classList.contains('vh-ovf')) {
        closeMenu();
      } else {
        buildTransformMenu(item, entry, body);
      }
    });
    acts.appendChild(transformBtn);

    const copyBtn = el('button', null, 'Копировать');
    copyBtn.addEventListener('click', async (e) => {
      e.stopPropagation();
      const ok = await copyText(item.text);
      showToast(ok ? 'Скопировано' : 'Не удалось скопировать');
    });
    acts.appendChild(copyBtn);

    // удаление — только если IPC доступен
    if (hasDelete()) {
      const delBtn = el('button', 'vh-icon vh-danger', '✕');
      delBtn.title = 'Удалить';
      delBtn.addEventListener('click', async (e) => {
        e.stopPropagation();
        const r = await ipcDelete(item.id);
        if (r && r.ok) {
          // убираем из состояния и перерисовываем (чтобы дни/статы сошлись)
          state.items = state.items.filter((x) => x !== item);
          renderStats();
          renderList();
        } else {
          showToast('Не удалось удалить');
        }
      });
      acts.appendChild(delBtn);
    }

    entry.appendChild(acts);
    return entry;
  }

  /* ── Отрисовка ленты (группировка по дням) ───────────────────────────────── */
  function renderList() {
    const content = state.content;
    content.textContent = ''; // чистим

    const items = visibleItems();
    if (!items.length) {
      const empty = el('div', 'vh-empty');
      if (state.items.length && (state.query || state.filter !== 'all')) {
        empty.textContent = 'Ничего не найдено.';
      } else {
        empty.textContent = 'Пока пусто. Скажи что-нибудь через диктовку (F8) или «Hey Jarvis».';
      }
      content.appendChild(empty);
      return;
    }

    // группируем по дню в порядке items (уже newest-first)
    let curKey = null, group = null;
    for (const it of items) {
      const k = dayKey(it.ts);
      if (k !== curKey) {
        curKey = k;
        group = el('div', 'vh-daygroup');
        const head = el('div', 'vh-dayhead', dayLabel(it.ts));
        // счётчик реплик в дне (по видимым)
        const cntN = items.filter((x) => dayKey(x.ts) === k).length;
        head.appendChild(el('span', 'vh-cnt', cntN + ' ' + pluralReplicas(cntN)));
        group.appendChild(head);
        content.appendChild(group);
      }
      group.appendChild(buildEntry(it));
    }
  }

  /* ── Нижняя action-bar: Копировать всё / Экспорт / Очистить ──────────────── */
  function copyAllVisible() {
    const items = visibleItems();
    if (!items.length) { showToast('Нечего копировать'); return; }
    const joined = items.map((x) => x.text).join('\n\n');
    copyText(joined).then((ok) => showToast(ok ? 'Скопировано всё' : 'Не удалось скопировать'));
  }

  // Сборка .md, сгруппированного по дням (по видимым), и скачивание через Blob
  function exportMd() {
    const items = visibleItems();
    if (!items.length) { showToast('Нечего экспортировать'); return; }
    const lines = ['# История голосового ввода Jarvis', ''];
    let curKey = null;
    for (const it of items) {
      const k = dayKey(it.ts);
      if (k !== curKey) {
        curKey = k;
        lines.push('## ' + dayLabel(it.ts), '');
      }
      lines.push('- `' + fmtTime(it.ts) + '` · ' + sourceLabel(it.source) + ' — ' + it.text);
    }
    lines.push('');
    const md = lines.join('\n');
    try {
      const blob = new Blob([md], { type: 'text/markdown;charset=utf-8' });
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = 'jarvis-voice-history.md';
      document.body.appendChild(a);
      a.click();
      a.remove();
      setTimeout(() => URL.revokeObjectURL(url), 1000);
      showToast('Экспортировано');
    } catch (e) {
      showToast('Не удалось экспортировать');
    }
  }

  // Очистка в два шага: «Точно?» → реальная очистка
  function resetClearBtn() {
    if (!state || !state.clearBtn) return;
    state.clearArmed = false;
    state.clearBtn.classList.remove('vh-armed');
    state.clearLabel.textContent = 'Очистить';
  }
  function onClearClick() {
    if (!state.clearArmed) {
      state.clearArmed = true;
      state.clearBtn.classList.add('vh-armed');
      state.clearLabel.textContent = 'Точно?';
      return;
    }
    ipcClear().then(() => {
      state.items = [];
      resetClearBtn();
      renderStats();
      renderList();
      showToast('История очищена');
    });
  }

  /* ── Глобальный «клик мимо»: закрывает любое открытое меню ───────────────── */
  function onDocClick() {
    closeMenu();
    // первый клик мимо — снимаем взвод «Точно?»
    if (state && state.clearArmed) resetClearBtn();
  }

  /* ── Сборка каркаса страницы ─────────────────────────────────────────────── */
  function buildShell(rootEl) {
    rootEl.textContent = ''; // полностью очищаем хост

    const root = el('div', null);
    root.id = 'voicehist';

    // ── Поиск + сегментный фильтр источника ──
    const head = el('div', 'vh-head');
    const si = el('span', 'vh-si');
    si.appendChild(svgSearch());
    const input = el('input');
    input.type = 'text';
    input.placeholder = 'Поиск по голосовому вводу…';
    input.addEventListener('input', () => {
      state.query = input.value;
      renderList();
    });
    const seg = el('div', 'vh-seg');
    const segDefs = [
      { key: 'all', label: 'Все' },
      { key: 'wake', label: 'Голос' },
      { key: 'dictation', label: 'Диктовка' },
    ];
    state.segBtns = {};
    for (const d of segDefs) {
      const b = el('button', d.key === state.filter ? 'on' : null, d.label);
      b.addEventListener('click', (e) => {
        e.stopPropagation();
        state.filter = d.key;
        for (const k in state.segBtns) {
          state.segBtns[k].classList.toggle('on', k === d.key);
        }
        renderList();
      });
      state.segBtns[d.key] = b;
      seg.appendChild(b);
    }
    head.appendChild(si);
    head.appendChild(input);
    head.appendChild(seg);
    root.appendChild(head);

    // ── Полоса статистики ──
    const stats = el('div', 'vh-stats');
    const mkStat = (accent, label) => {
      const wrap = el('div', 'vh-stat');
      const n = el('span', accent ? 'vh-n vh-accent' : 'vh-n', '0');
      const l = el('span', 'vh-l', label);
      wrap.appendChild(n); wrap.appendChild(l);
      return { wrap, n };
    };
    const sToday = mkStat(true, 'сегодня');
    const sTotal = mkStat(false, 'всего');
    const sWords = mkStat(false, 'слов');
    state.statToday = sToday.n;
    state.statTotal = sTotal.n;
    state.statWords = sWords.n;
    stats.appendChild(sToday.wrap);
    stats.appendChild(sTotal.wrap);
    stats.appendChild(sWords.wrap);
    stats.appendChild(el('span', 'vh-sp'));
    const live = el('div', 'vh-live');
    live.appendChild(el('span', 'vh-pulse'));
    live.appendChild(document.createTextNode('диктовка F8 + «Hey Jarvis»'));
    stats.appendChild(live);
    root.appendChild(stats);

    // ── Лента ──
    const content = el('div', 'vh-content');
    state.content = content;
    root.appendChild(content);

    // ── Нижняя action-bar ──
    const bar = el('div', 'vh-actionbar');
    const lock = el('span', 'vh-lock', '🔒 хранится локально');
    bar.appendChild(lock);
    bar.appendChild(el('span', 'vh-sp'));

    const copyAll = el('span', 'vh-act', 'Копировать всё');
    copyAll.addEventListener('click', (e) => { e.stopPropagation(); copyAllVisible(); });
    bar.appendChild(copyAll);
    bar.appendChild(el('span', 'vh-barsep'));

    const exportBtn = el('span', 'vh-act', 'Экспорт');
    exportBtn.addEventListener('click', (e) => { e.stopPropagation(); exportMd(); });
    bar.appendChild(exportBtn);
    bar.appendChild(el('span', 'vh-barsep'));

    const clearBtn = el('span', 'vh-act vh-danger-act');
    const clearLabel = el('span', null, 'Очистить');
    clearBtn.appendChild(clearLabel);
    clearBtn.addEventListener('click', (e) => { e.stopPropagation(); onClearClick(); });
    state.clearBtn = clearBtn;
    state.clearLabel = clearLabel;
    bar.appendChild(clearBtn);

    root.appendChild(bar);
    rootEl.appendChild(root);
  }

  /* ── Публичный вход: window.initVoiceHistory(rootEl) ─────────────────────── */
  async function initVoiceHistory(rootEl) {
    if (!rootEl) return;
    injectStyle();

    // свежее состояние на каждый init (полная перестройка без утечек)
    closeMenu();
    state = {
      items: [],
      query: '',
      filter: 'all',
      content: null,
      segBtns: null,
      statToday: null, statTotal: null, statWords: null,
      clearBtn: null, clearLabel: null, clearArmed: false,
    };

    buildShell(rootEl);

    // глобальный «клик мимо» — ставим единожды на весь жизненный цикл модуля
    if (!docClickBound) {
      document.addEventListener('click', onDocClick);
      docClickBound = true;
    }

    // загрузка данных и отрисовка
    state.items = await ipcGet();
    renderStats();
    renderList();
  }

  window.initVoiceHistory = initVoiceHistory;

  /* ── Стили: всё под #voicehist. Глобальные токены берём из index.html,
   *    локально доопределяем только акцент. ───────────────────────────────── */
  const CSS = `
#voicehist {
  --accent: #6ca0ff;
  --accent-soft: rgba(108,160,255,.13);
  --accent-line: rgba(108,160,255,.30);
  position: relative;
  width: 100%; height: 100%;
  display: flex; flex-direction: column;
  min-height: 0; overflow: hidden;
  font-family: -apple-system, BlinkMacSystemFont, "SF Pro Text", "Segoe UI", sans-serif;
  color: var(--text);
}

/* ── Поиск + фильтр источника ── */
#voicehist .vh-head {
  display: flex; align-items: center; gap: 11px;
  padding: 12px 16px; border-bottom: 1px solid var(--hairline); flex: none;
}
#voicehist .vh-si { color: var(--faint); display: flex; align-items: center; }
#voicehist .vh-head input {
  flex: 1; background: transparent; border: 0; outline: 0;
  color: var(--text); font: 400 15px/1 inherit;
}
#voicehist .vh-head input::placeholder { color: var(--faint); }
#voicehist .vh-seg {
  display: flex; gap: 2px; background: rgba(255,255,255,.05);
  border: 1px solid var(--hairline); border-radius: 8px; padding: 2px;
}
#voicehist .vh-seg button {
  appearance: none; border: 0; background: transparent; color: var(--muted);
  font: 500 11.5px/1 inherit; padding: 5px 10px; border-radius: 6px; cursor: default;
}
#voicehist .vh-seg button.on { background: rgba(255,255,255,.09); color: var(--text); }

/* ── Полоса статистики ── */
#voicehist .vh-stats {
  display: flex; align-items: center; gap: 22px;
  padding: 11px 18px; border-bottom: 1px solid var(--hairline); flex: none;
}
#voicehist .vh-stat { display: flex; flex-direction: column; gap: 2px; }
#voicehist .vh-n {
  font-family: var(--mono); font-size: 16px; font-weight: 600; color: var(--text);
  font-variant-numeric: tabular-nums; line-height: 1;
}
#voicehist .vh-n.vh-accent { color: var(--accent); }
#voicehist .vh-l {
  font-size: 10px; color: var(--faint); text-transform: uppercase; letter-spacing: .05em;
}
#voicehist .vh-sp { flex: 1; }
#voicehist .vh-live { display: flex; align-items: center; gap: 7px; font-size: 11px; color: var(--muted); }
#voicehist .vh-pulse {
  width: 7px; height: 7px; border-radius: 50%; background: var(--done);
  box-shadow: 0 0 0 0 rgba(65,201,142,.5); animation: vhPulse 2s infinite;
}
@keyframes vhPulse {
  0%,100% { box-shadow: 0 0 0 0 rgba(65,201,142,.4); }
  50% { box-shadow: 0 0 0 5px rgba(65,201,142,0); }
}

/* ── Лента / группы ── */
#voicehist .vh-content { flex: 1; overflow-y: auto; min-height: 0; padding: 0 12px 12px; }
#voicehist .vh-content::-webkit-scrollbar { width: 0; }
#voicehist .vh-daygroup { margin-top: 6px; }
#voicehist .vh-daygroup:first-child { margin-top: 0; }
#voicehist .vh-dayhead {
  position: sticky; top: 0; z-index: 2;
  display: flex; align-items: center; gap: 8px; padding: 9px 8px 7px;
  font: 600 10.5px/1 inherit; letter-spacing: .06em; text-transform: uppercase; color: var(--faint);
  background: rgba(20,20,24,0.97);
}
#voicehist .vh-cnt {
  margin-left: auto; font-family: var(--mono); font-size: 10px;
  color: var(--faint); text-transform: none;
}

/* ── Запись (без иконок-плиток источника) ── */
#voicehist .vh-entry {
  display: flex; gap: 12px; padding: 11px 10px 11px 12px;
  border-radius: 10px; position: relative;
}
#voicehist .vh-entry:hover { background: var(--row-hover); }
#voicehist .vh-body { flex: 1; min-width: 0; }
#voicehist .vh-text {
  font-size: 13.5px; line-height: 1.45; color: var(--text-body);
  word-wrap: break-word; overflow-wrap: anywhere;
}
#voicehist .vh-meta {
  display: flex; align-items: center; gap: 8px; margin-top: 5px;
  font-size: 11px; color: var(--faint); flex-wrap: wrap;
}
#voicehist .vh-meta .vh-t { font-family: var(--mono); font-variant-numeric: tabular-nums; }
#voicehist .vh-meta .vh-lbl { color: var(--muted); }
#voicehist .vh-meta .vh-sep { opacity: .4; }
#voicehist .vh-meta .vh-words { font-family: var(--mono); }
#voicehist .vh-meta .vh-copied { color: var(--done); display: inline-flex; align-items: center; gap: 3px; }

/* действия — на ховер */
#voicehist .vh-acts {
  display: flex; align-items: center; gap: 4px; flex: none;
  opacity: 0; transition: opacity .12s; align-self: flex-start; margin-top: 1px;
}
#voicehist .vh-entry:hover .vh-acts { opacity: 1; }
#voicehist .vh-acts button {
  appearance: none; border: 1px solid var(--hairline); background: rgba(255,255,255,.04);
  color: var(--keycap-text); font: 500 11px/1 inherit; padding: 5px 9px; border-radius: 6px;
  cursor: default; display: flex; align-items: center; gap: 5px; white-space: nowrap;
}
#voicehist .vh-acts button:hover { background: rgba(255,255,255,.09); color: var(--text); }
#voicehist .vh-acts button.vh-primary {
  border-color: var(--accent-line); background: var(--accent-soft); color: var(--accent);
}
#voicehist .vh-acts button.vh-icon { padding: 5px 7px; }
#voicehist .vh-acts button.vh-danger:hover { border-color: rgba(242,99,99,.4); color: #f26363; }

/* инлайн-результат преобразования — светлая карточка */
#voicehist .vh-enh {
  margin: 9px 0 2px 0; border: 1px solid var(--hairline);
  background: rgba(255,255,255,.025); border-radius: 10px; overflow: hidden;
}
#voicehist .vh-eh {
  display: flex; align-items: center; gap: 9px;
  padding: 8px 10px 8px 11px; border-bottom: 1px solid var(--hairline);
}
#voicehist .vh-chip {
  display: inline-flex; align-items: center; gap: 5px; font-size: 11px; font-weight: 500;
  color: var(--accent); background: var(--accent-soft); border: 1px solid var(--accent-line);
  border-radius: 6px; padding: 3px 8px;
}
#voicehist .vh-eh .vh-sp { flex: 1; }
#voicehist .vh-eh button {
  appearance: none; border: 1px solid var(--hairline); background: rgba(255,255,255,.04);
  color: var(--keycap-text); font: 500 11px/1 inherit; padding: 5px 9px; border-radius: 6px; cursor: default;
}
#voicehist .vh-eh button:hover { color: var(--text); background: rgba(255,255,255,.09); }
#voicehist .vh-eh button.vh-acc {
  border-color: var(--accent-line); background: var(--accent-soft); color: var(--accent);
}
#voicehist .vh-eh button.vh-eh-icon { padding: 4px 9px; font-size: 16px; line-height: .5; letter-spacing: 1px; }
#voicehist .vh-etext { padding: 11px 12px; font-size: 13px; line-height: 1.5; color: var(--text); }

/* меню преобразований (библиотека) */
#voicehist .vh-tmenu {
  position: absolute; right: 10px; top: 40px; z-index: 5; width: 288px;
  background: rgba(28,28,32,0.98); border: 1px solid var(--border); border-radius: 11px;
  box-shadow: 0 18px 50px rgba(0,0,0,.6); overflow: hidden;
  backdrop-filter: blur(30px) saturate(160%);
}
#voicehist .vh-tmenu.vh-ovf { width: 168px; top: auto; bottom: 8px; right: 8px; }
#voicehist .vh-tmh {
  padding: 10px 12px 7px; font: 600 10px/1 inherit; letter-spacing: .06em;
  text-transform: uppercase; color: var(--faint);
}
#voicehist .vh-ti { display: flex; align-items: center; gap: 10px; padding: 9px 12px; cursor: default; }
#voicehist .vh-ti:hover { background: var(--row-hover); }
#voicehist .vh-tn { font-size: 13px; color: var(--text); }
#voicehist .vh-th { font-family: var(--mono); font-size: 10px; color: var(--faint); margin-left: auto; }
#voicehist .vh-tdiv { height: 1px; background: var(--hairline); margin: 4px 0; }
#voicehist .vh-ti.vh-add .vh-tn { color: var(--accent); }

/* ── Action-bar ── */
#voicehist .vh-actionbar {
  display: flex; align-items: center; gap: 10px; padding: 9px 14px;
  border-top: 1px solid var(--hairline); background: rgba(0,0,0,.22); flex: none;
}
#voicehist .vh-lock { font-size: 10.5px; color: var(--faint); display: flex; align-items: center; gap: 4px; }
#voicehist .vh-actionbar .vh-sp { flex: 1; }
#voicehist .vh-act { display: inline-flex; align-items: center; gap: 6px; font-size: 12px; color: var(--text-body); cursor: default; }
#voicehist .vh-act:hover { color: var(--text); }
#voicehist .vh-barsep { width: 1px; height: 14px; background: var(--border); }
#voicehist .vh-danger-act { color: #c97c7c; }
#voicehist .vh-danger-act:hover { color: #f26363; }
#voicehist .vh-danger-act.vh-armed { color: #f26363; font-weight: 600; }

/* ── Пусто ── */
#voicehist .vh-empty {
  padding: 48px 24px; text-align: center; color: var(--faint);
  font-size: 13px; line-height: 1.5;
}
`;
})();
