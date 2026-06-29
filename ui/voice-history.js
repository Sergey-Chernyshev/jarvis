/* ============================================================================
 * voice-history.js — самодостаточный модуль раздела «Whisper».
 *
 * Экспортирует window.initVoiceHistory(rootEl): строит полноценный раздел
 * Whisper в стиле тёмного Wispr-Flow — боковое меню + переключаемые под-разделы:
 *   История · Статистика · Словарь · Преобразования · Черновик.
 *
 * Дизайн 1:1 по макету docs/superpowers/mockups/whisper-section.html:
 * тёмное стекло, акцент #6ca0ff, моно для чисел/времени, сайдкарта «Умные
 * промпты» с тумблером, лента истории с авто-тегом ПОД временем (левая
 * колонка), карточки статистики, тепловая карта по реальным дням, ряды
 * «Преобразований» с триггер-чипами и тумблерами, большой тёмный черновик.
 *
 * Данные — только реальные: история читается из IPC (window.jarvis),
 * статистика выводится из истории (+usage при наличии). Опциональные IPC
 * (словарь / промпты / черновик) деградируют до интерактивного локального
 * состояния, если методов в мосте ещё нет.
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

  const SVG_NS = 'http://www.w3.org/2000/svg';

  // ── Библиотека преобразований для меню строки истории (стили enhance) ────
  const TRANSFORMS = [
    { style: 'prompt',    name: 'Промпт для агента',     hint: '⌘1' },
    { style: 'commit',    name: 'Коммит-сообщение',      hint: '⌘2' },
    { style: 'clean',     name: 'Чистовик · грамматика', hint: '⌘3' },
    { style: 'translate', name: 'Перевод на English',    hint: '⌘4' },
  ];

  // ── Встроенные преобразования для раздела «Преобразования» (как в макете) ─
  const BUILTIN_PROMPTS = [
    { id: 'prompt',    icon: 'sparkle', name: 'Промпт для агента',
      desc: 'Превращает надиктованное в чёткий промпт для Claude Code.',
      trigger: 'когда диктуешь в терминал / IDE', auto: true, enabled: true },
    { id: 'commit',    icon: 'git', name: 'Коммит-сообщение',
      desc: 'Делает аккуратный git-commit: заголовок + тело.',
      trigger: 'текст похож на описание изменения', auto: true, enabled: true },
    { id: 'clean',     icon: 'text', name: 'Чистовик',
      desc: 'Убирает оговорки и повторы, чинит пунктуацию.',
      trigger: 'личные сообщения и заметки', auto: true, enabled: true },
    { id: 'translate', icon: 'lang', name: 'Перевод на English',
      desc: 'Естественный перевод реплики на английский.',
      trigger: null, auto: false, enabled: false },
  ];

  /* ════════════════════════════════════════════════════════════════════════
   * IPC-обёртки: никогда не бросают наружу
   * ══════════════════════════════════════════════════════════════════════ */

  // История диктовки — основной существующий IPC
  async function ipcGetTranscripts() {
    try {
      if (!window.jarvis || typeof window.jarvis.transcriptsGet !== 'function') return [];
      const r = await window.jarvis.transcriptsGet();
      const items = (r && Array.isArray(r.items)) ? r.items : [];
      return items.map((it, i) => ({
        id: (it && it.id != null) ? it.id : null,
        idx: i,
        text: (it && typeof it.text === 'string') ? it.text : String((it && it.text) || ''),
        ts: (it && Number(it.ts)) || 0,
        source: (it && it.source === 'wake') ? 'wake' : 'dictation',
        // авто-стиль, если когда-нибудь придёт из движка (сейчас отсутствует)
        appliedStyle: (it && typeof it.appliedStyle === 'string' && it.appliedStyle) || null,
      }))
        // ТОЛЬКО диктовка (Whisper / F8): разговоры «Hey Jarvis» здесь НЕ показываем.
        .filter((o) => o.source === 'dictation');
    } catch (e) { return []; }
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

  // Usage — опционально (есть в мосте), но обёрнуто на случай отсутствия
  function hasUsage() {
    return !!(window.jarvis && typeof window.jarvis.getUsage === 'function');
  }
  async function ipcUsage(period) {
    try {
      if (!hasUsage()) return null;
      const r = await window.jarvis.getUsage(period);
      return (r && typeof r === 'object') ? r : null;
    } catch (e) { return null; }
  }

  // ── Словарь (опционально-отсутствующий IPC) ──
  async function ipcDictGet() {
    try {
      if (!window.jarvis || typeof window.jarvis.dictionaryGet !== 'function') return null;
      const r = await window.jarvis.dictionaryGet();
      const words = (r && Array.isArray(r.words)) ? r.words : [];
      return words.map((w) => ({
        word: String((w && w.word) || ''),
        note: (w && typeof w.note === 'string') ? w.note : '',
        count: (w && Number(w.count)) || 0,
      })).filter((w) => w.word);
    } catch (e) { return null; }
  }
  async function ipcDictAdd(word, note) {
    try {
      if (window.jarvis && typeof window.jarvis.dictionaryAdd === 'function') {
        await window.jarvis.dictionaryAdd(word, note);
      }
    } catch (e) { /* проглатываем */ }
  }
  async function ipcDictRemove(word) {
    try {
      if (window.jarvis && typeof window.jarvis.dictionaryRemove === 'function') {
        await window.jarvis.dictionaryRemove(word);
      }
    } catch (e) { /* проглатываем */ }
  }

  // ── Промпты / умный режим (опционально-отсутствующий IPC) ──
  async function ipcPromptsGet() {
    try {
      if (!window.jarvis || typeof window.jarvis.promptsGet !== 'function') return null;
      const r = await window.jarvis.promptsGet();
      const prompts = (r && Array.isArray(r.prompts)) ? r.prompts : [];
      return prompts.map((p) => ({
        id: String((p && p.id) || ''),
        icon: (p && typeof p.icon === 'string') ? p.icon : 'sparkle',
        name: String((p && p.name) || ''),
        desc: String((p && p.desc) || ''),
        trigger: (p && typeof p.trigger === 'string' && p.trigger) ? p.trigger : null,
        auto: !!(p && p.auto),
        enabled: (p && p.enabled !== false),
      })).filter((p) => p.name);
    } catch (e) { return null; }
  }
  async function ipcPromptsSettings() {
    try {
      if (!window.jarvis || typeof window.jarvis.promptsGetSettings !== 'function') return null;
      const r = await window.jarvis.promptsGetSettings();
      return (r && typeof r === 'object') ? r : null;
    } catch (e) { return null; }
  }
  async function ipcPromptsSetSmart(on) {
    try {
      if (window.jarvis && typeof window.jarvis.promptsSetSmart === 'function') {
        await window.jarvis.promptsSetSmart(!!on);
      }
    } catch (e) { /* проглатываем */ }
  }

  // ── Черновик (опционально-отсутствующий IPC → localStorage фолбэк) ──
  const SCRATCH_KEY = 'jarvis-scratch';
  async function ipcScratchGet() {
    try {
      if (window.jarvis && typeof window.jarvis.scratchpadGet === 'function') {
        const r = await window.jarvis.scratchpadGet();
        if (typeof r === 'string') return r;
        if (r && typeof r.text === 'string') return r.text;
      }
    } catch (e) { /* падаем на localStorage */ }
    try {
      const v = window.localStorage ? window.localStorage.getItem(SCRATCH_KEY) : null;
      return typeof v === 'string' ? v : '';
    } catch (e) { return ''; }
  }
  async function ipcScratchSet(text) {
    try {
      if (window.jarvis && typeof window.jarvis.scratchpadSet === 'function') {
        await window.jarvis.scratchpadSet(String(text));
        return;
      }
    } catch (e) { /* падаем на localStorage */ }
    try {
      if (window.localStorage) window.localStorage.setItem(SCRATCH_KEY, String(text));
    } catch (e) { /* нет хранилища — в памяти */ }
  }

  async function copyText(text) {
    try {
      await navigator.clipboard.writeText(String(text));
      return true;
    } catch (e) { return false; }
  }

  /* ── Тосты: глобальный window.showToast, иначе свой минимальный ──────────── */
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

  /* ════════════════════════════════════════════════════════════════════════
   * Утилиты времени / слов / чисел
   * ══════════════════════════════════════════════════════════════════════ */
  function tsToDate(ts) { return new Date(ts * 1000); } // ts — unix-секунды
  function pad2(n) { return n < 10 ? '0' + n : '' + n; }
  function fmtTime(ts) {
    const d = tsToDate(ts);
    return pad2(d.getHours()) + ':' + pad2(d.getMinutes());
  }
  // ключ локального дня (YYYY-M-D) для группировки/тепловой карты
  function dayKey(ts) {
    const d = tsToDate(ts);
    return d.getFullYear() + '-' + d.getMonth() + '-' + d.getDate();
  }
  const MONTHS = ['января', 'февраля', 'марта', 'апреля', 'мая', 'июня',
    'июля', 'августа', 'сентября', 'октября', 'ноября', 'декабря'];
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
  function pluralReplicas(n) {
    const m100 = n % 100, m10 = n % 10;
    if (m100 >= 11 && m100 <= 14) return 'реплик';
    if (m10 === 1) return 'реплика';
    if (m10 >= 2 && m10 <= 4) return 'реплики';
    return 'реплик';
  }
  function pluralDays(n) {
    const m100 = n % 100, m10 = n % 10;
    if (m100 >= 11 && m100 <= 14) return 'дней';
    if (m10 === 1) return 'день';
    if (m10 >= 2 && m10 <= 4) return 'дня';
    return 'дней';
  }
  // Разделитель тысяч (узкий пробел) — как «37 065» в макете
  function fmtNum(n) {
    return String(n).replace(/\B(?=(\d{3})+(?!\d))/g, ' ');
  }
  // подпись к авто-стилю (если когда-нибудь придёт)
  function styleTag(style) {
    const map = { prompt: 'Промпт', commit: 'Коммит', clean: 'Чистовик', translate: 'English' };
    return map[style] || style;
  }

  /* ════════════════════════════════════════════════════════════════════════
   * Инъекция стилей (один раз, guard по id)
   * ══════════════════════════════════════════════════════════════════════ */
  function injectStyle() {
    if (document.getElementById('voice-history-style')) return;
    const style = document.createElement('style');
    style.id = 'voice-history-style';
    style.textContent = CSS;
    document.head.appendChild(style);
  }

  /* ════════════════════════════════════════════════════════════════════════
   * Хелперы DOM + SVG-иконки (createElementNS, без innerHTML)
   * ══════════════════════════════════════════════════════════════════════ */
  function el(tag, cls, text) {
    const n = document.createElement(tag);
    if (cls) n.className = cls;
    if (text != null) n.textContent = text;
    return n;
  }

  // Универсальный конструктор lucide-style иконки: набор path-данных
  // (d / тег + атрибуты). Каждая иконка — массив дескрипторов.
  function svgIcon(parts, opts) {
    const o = opts || {};
    const svg = document.createElementNS(SVG_NS, 'svg');
    svg.setAttribute('viewBox', '0 0 24 24');
    svg.setAttribute('fill', 'none');
    svg.setAttribute('stroke', 'currentColor');
    svg.setAttribute('stroke-width', o.sw || '2');
    svg.setAttribute('stroke-linecap', 'round');
    svg.setAttribute('stroke-linejoin', 'round');
    if (o.w) svg.setAttribute('width', o.w);
    if (o.h) svg.setAttribute('height', o.h);
    for (const p of parts) {
      const node = document.createElementNS(SVG_NS, p.t);
      for (const k in p) {
        if (k === 't') continue;
        node.setAttribute(k, p[k]);
      }
      svg.appendChild(node);
    }
    return svg;
  }

  // Иконки навигации / разделов / триггеров — пути взяты из макета 1:1
  const ICONS = {
    history: () => svgIcon([
      { t: 'rect', x: '3', y: '3', width: '7', height: '7', rx: '1' },
      { t: 'rect', x: '14', y: '3', width: '7', height: '7', rx: '1' },
      { t: 'rect', x: '14', y: '14', width: '7', height: '7', rx: '1' },
      { t: 'rect', x: '3', y: '14', width: '7', height: '7', rx: '1' },
    ]),
    insights: () => svgIcon([
      { t: 'line', x1: '18', y1: '20', x2: '18', y2: '10' },
      { t: 'line', x1: '12', y1: '20', x2: '12', y2: '4' },
      { t: 'line', x1: '6', y1: '20', x2: '6', y2: '14' },
    ]),
    dict: () => svgIcon([
      { t: 'path', d: 'M4 19.5A2.5 2.5 0 0 1 6.5 17H20' },
      { t: 'path', d: 'M6.5 2H20v20H6.5A2.5 2.5 0 0 1 4 19.5v-15A2.5 2.5 0 0 1 6.5 2z' },
    ]),
    sparkle: () => svgIcon([
      { t: 'path', d: 'm12 3-1.9 5.8a2 2 0 0 1-1.3 1.3L3 12l5.8 1.9a2 2 0 0 1 1.3 1.3L12 21l1.9-5.8a2 2 0 0 1 1.3-1.3L21 12l-5.8-1.9a2 2 0 0 1-1.3-1.3z' },
    ]),
    scratch: () => svgIcon([
      { t: 'path', d: 'M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z' },
      { t: 'polyline', points: '14 2 14 8 20 8' },
    ]),
    settings: () => svgIcon([
      { t: 'circle', cx: '12', cy: '12', r: '3' },
      { t: 'path', d: 'M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z' },
    ]),
    check: () => svgIcon([{ t: 'path', d: 'm5 12 5 5L20 7' }], { sw: '2.5' }),
    git: () => svgIcon([
      { t: 'path', d: 'M6 3v12' },
      { t: 'circle', cx: '6', cy: '18', r: '3' },
      { t: 'circle', cx: '18', cy: '6', r: '3' },
      { t: 'path', d: 'M18 9a9 9 0 0 1-9 9' },
    ]),
    text: () => svgIcon([
      { t: 'path', d: 'M4 7V4h16v3' },
      { t: 'path', d: 'M9 20h6' },
      { t: 'path', d: 'M12 4v16' },
    ]),
    lang: () => svgIcon([
      { t: 'path', d: 'm5 8 6 6' },
      { t: 'path', d: 'm4 14 6-6 2-3' },
      { t: 'path', d: 'M2 5h12' },
      { t: 'path', d: 'M7 2h1' },
      { t: 'path', d: 'm22 22-5-10-5 10' },
      { t: 'path', d: 'M14 18h6' },
    ]),
    mic: () => svgIcon([
      { t: 'path', d: 'M12 2a3 3 0 0 0-3 3v7a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3z' },
      { t: 'path', d: 'M19 10v2a7 7 0 0 1-14 0v-2' },
      { t: 'line', x1: '12', y1: '19', x2: '12', y2: '22' },
    ]),
    search: () => svgIcon([
      { t: 'circle', cx: '11', cy: '11', r: '7' },
      { t: 'line', x1: '21', y1: '21', x2: '16.65', y2: '16.65' },
    ], { w: '15', h: '15' }),
  };
  function promptIcon(name) {
    const fn = ICONS[name] || ICONS.sparkle;
    return fn();
  }

  /* ════════════════════════════════════════════════════════════════════════
   * РАЗДЕЛ: История
   * ══════════════════════════════════════════════════════════════════════ */
  function visibleItems() {
    const q = state.query.trim().toLowerCase();
    return state.items.filter((it) => {
      if (q && !it.text.toLowerCase().includes(q)) return false;
      return true;
    });
  }

  // ── меню «Преобразовать» (как в текущем модуле) ──
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
      switchSection('transforms');
    });
    menu.appendChild(add);
    menu.addEventListener('click', (e) => e.stopPropagation());
    entryNode.appendChild(menu);
    openMenu = menu;
  }

  async function runEnhance(item, bodyNode, tr) {
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
      enh.remove();
      showToast('не поддерживается');
      return;
    }
    const result = String(res.result || '');
    etext.textContent = result;

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
      item.text = result;
      const textNode = bodyNode.querySelector('.vh-text');
      if (textNode) textNode.textContent = result;
      enhNode.remove();
    }));
    menu.appendChild(mk('Скрыть', () => { enhNode.remove(); }));
    menu.addEventListener('click', (e) => e.stopPropagation());
    enhNode.style.position = 'relative';
    enhNode.appendChild(menu);
    openMenu = menu;
  }

  // ── строка ленты (макет: левая колонка = время + авто-тег под ним) ──
  function buildEntry(item) {
    const entry = el('div', 'ent');

    const lc = el('div', 'lc');
    lc.appendChild(el('span', 'tm', fmtTime(item.ts)));
    if (item.appliedStyle) {
      const tag = el('span', 'autotag');
      tag.appendChild(ICONS.check());
      tag.appendChild(document.createTextNode(styleTag(item.appliedStyle)));
      lc.appendChild(tag);
    }
    entry.appendChild(lc);

    const body = el('div', 'vh-body');
    body.appendChild(el('div', 'tx vh-text', item.text));
    entry.appendChild(body);

    // ── ховер-действия ──
    const acts = el('div', 'vh-acts');
    const transformBtn = el('button', 'vh-primary', 'Преобразовать ▾');
    transformBtn.addEventListener('click', (e) => {
      e.stopPropagation();
      if (openMenu && openMenu.parentNode === entry &&
          openMenu.classList.contains('vh-tmenu') && !openMenu.classList.contains('vh-ovf')) {
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

    // Перегенерировать распознавание из сохранённого аудио (если оно есть) —
    // на случай ошибки/мусора анализа. Кнопка скрыта, если аудио не сохранялось
    // или мост ещё без метода (старый бинарь).
    if (item.hasAudio && window.jarvis && typeof window.jarvis.transcriptRetranscribe === 'function') {
      const regenBtn = el('button', null, 'Перегенерировать');
      regenBtn.title = 'Заново распознать из сохранённого аудио';
      regenBtn.addEventListener('click', async (e) => {
        e.stopPropagation();
        regenBtn.disabled = true;
        const prev = regenBtn.textContent;
        regenBtn.textContent = 'Распознаю…';
        let r = null;
        try { r = await window.jarvis.transcriptRetranscribe(item.id); } catch {}
        regenBtn.disabled = false;
        regenBtn.textContent = prev;
        if (r && r.ok && r.text) {
          item.text = r.text;
          item.appliedStyle = null;
          renderHistory();
        } else {
          showToast((r && r.error) || 'Не удалось перегенерировать');
        }
      });
      acts.appendChild(regenBtn);
    }

    if (hasDelete()) {
      const delBtn = el('button', 'vh-icon vh-danger', '✕');
      delBtn.title = 'Удалить';
      delBtn.addEventListener('click', async (e) => {
        e.stopPropagation();
        const r = await ipcDelete(item.id);
        if (r && r.ok) {
          state.items = state.items.filter((x) => x !== item);
          renderHistory();
          renderRail();
        } else {
          showToast('Не удалось удалить');
        }
      });
      acts.appendChild(delBtn);
    }
    body.appendChild(acts); // действия ВНУТРИ тела, под текстом (не оверлеем)
    return entry;
  }

  // ── лента истории (группировка по дням) ──
  function renderHistory() {
    const feed = state.feed;
    if (!feed) return;
    feed.textContent = '';

    const items = visibleItems();
    if (!items.length) {
      const empty = el('div', 'vh-empty');
      if (state.items.length && state.query) {
        empty.textContent = 'Ничего не найдено.';
      } else {
        empty.textContent = 'Пока пусто. Надиктуй что-нибудь через диктовку (Whisper · F8).';
      }
      feed.appendChild(empty);
      return;
    }

    let curKey = null, group = null;
    for (const it of items) {
      const k = dayKey(it.ts);
      if (k !== curKey) {
        curKey = k;
        const head = el('div', 'dayhead', dayLabel(it.ts));
        const cntN = items.filter((x) => dayKey(x.ts) === k).length;
        head.appendChild(el('span', 'vh-cnt', cntN + ' ' + pluralReplicas(cntN)));
        feed.appendChild(head);
        group = el('div', 'vh-daygroup');
        feed.appendChild(group);
      }
      group.appendChild(buildEntry(it));
    }
  }

  // ── правый рейл со стат-карточками ──
  function makeStatCard(value, label, opts) {
    const o = opts || {};
    const card = el('div', o.streak ? 'scard streak' : 'scard');
    card.appendChild(el('div', o.accent ? 'n acc' : 'n', value));
    card.appendChild(el('div', 'l', label));
    if (o.sub) card.appendChild(el('div', 'sub', o.sub));
    return card;
  }

  function renderRail() {
    const rail = state.rail;
    if (!rail) return;
    rail.textContent = '';

    const all = state.items;
    const todayKey = dayKey(Math.floor(Date.now() / 1000));
    let words = 0, today = 0;
    for (const it of all) {
      words += wordCount(it.text);
      if (dayKey(it.ts) === todayKey) today++;
    }

    rail.appendChild(makeStatCard(fmtNum(words), 'всего слов', { accent: true }));
    rail.appendChild(makeStatCard(fmtNum(today), 'сегодня', {}));

    // карточка токенов/стоимости — только если getUsage есть и вернул данные
    if (state.usage) {
      const u = state.usage;
      const tok = (u && (u.tokens != null ? u.tokens : u.total_tokens));
      const cost = (u && (u.cost != null ? u.cost : u.total_cost));
      if (tok != null) {
        const card = makeStatCard(fmtNum(Math.round(Number(tok))), 'токенов сегодня', { accent: false });
        if (cost != null) {
          const sub = el('div', 'sub', '$' + Number(cost).toFixed(2));
          card.appendChild(sub);
        }
        rail.appendChild(card);
      } else {
        rail.appendChild(makeStatCard(fmtNum(all.length), 'всего записей', {}));
      }
    } else {
      rail.appendChild(makeStatCard(fmtNum(all.length), 'всего записей', {}));
    }
  }

  function buildHistoryPane() {
    const pane = el('div', 'pane on');
    pane.dataset.k = 'history';
    const home = el('div', 'home');

    const feed = el('div', 'feed');
    // строка поиска сверху ленты
    const searchWrap = el('div', 'vh-searchwrap');
    const si = el('span', 'vh-si');
    si.appendChild(ICONS.search());
    const input = el('input');
    input.type = 'text';
    input.placeholder = 'Поиск по надиктованному…';
    input.addEventListener('input', () => { state.query = input.value; renderHistory(); });
    searchWrap.appendChild(si);
    searchWrap.appendChild(input);
    feed.appendChild(searchWrap);

    const feedBody = el('div', 'vh-feedbody');
    feed.appendChild(feedBody);
    state.feed = feedBody;

    const rail = el('div', 'rail');
    state.rail = rail;

    home.appendChild(feed);
    home.appendChild(rail);
    pane.appendChild(home);
    return pane;
  }

  /* ════════════════════════════════════════════════════════════════════════
   * РАЗДЕЛ: Статистика
   * ══════════════════════════════════════════════════════════════════════ */
  function buildBigCard(num, label, rows) {
    const card = el('div', 'bigcard');
    card.appendChild(el('div', 'bn', num));
    card.appendChild(el('div', 'bl', label));
    if (rows && rows.length) {
      card.appendChild(el('div', 'line'));
      for (const r of rows) {
        const row = el('div', 'r');
        row.appendChild(el('span', null, r.k));
        row.appendChild(el('b', null, r.v));
        card.appendChild(row);
      }
    }
    return card;
  }

  function buildInsightsPane() {
    const pane = el('div', 'pane');
    pane.dataset.k = 'insights';

    const all = state.items;
    const todayKey = dayKey(Math.floor(Date.now() / 1000));
    let words = 0, today = 0;
    const days = new Set();
    for (const it of all) {
      words += wordCount(it.text);
      days.add(dayKey(it.ts));
      if (dayKey(it.ts) === todayKey) today++;
    }

    // ── большие карточки (только реальные числа) ──
    const grid = el('div', 'grid');
    grid.appendChild(buildBigCard(fmtNum(words), 'всего надиктовано слов', []));
    grid.appendChild(buildBigCard(fmtNum(all.length), 'записей в истории', [
      { k: 'разных дней', v: fmtNum(days.size) },
    ]));
    grid.appendChild(buildBigCard(fmtNum(today), 'диктовок сегодня', []));
    // карточка usage — только если данные реально пришли
    if (state.usageAll) {
      const u = state.usageAll;
      const tok = (u.tokens != null ? u.tokens : u.total_tokens);
      const cost = (u.cost != null ? u.cost : u.total_cost);
      if (tok != null) {
        const rows = [];
        if (cost != null) rows.push({ k: 'стоимость', v: '$' + Number(cost).toFixed(2) });
        grid.appendChild(buildBigCard(fmtNum(Math.round(Number(tok))), 'токенов всего', rows));
      }
    }
    pane.appendChild(grid);

    // ── тепловая карта по реальным дням (~16 недель) ──
    const sect1 = el('div', 'sect');
    const h1 = el('div', 'secth');
    h1.appendChild(document.createTextNode('Серия'));
    h1.appendChild(el('span', 'cap', days.size + ' ' + pluralDays(days.size) + ' с диктовкой'));
    sect1.appendChild(h1);
    sect1.appendChild(buildHeatmap(all, days));
    pane.appendChild(sect1);

    // ── «Куда диктуешь»: реальных источников нет (только диктовка) → один бар ──
    const sect2 = el('div', 'sect');
    const h2 = el('div', 'secth');
    h2.appendChild(document.createTextNode('Куда диктуешь'));
    h2.appendChild(el('span', 'cap', 'источник один'));
    sect2.appendChild(h2);
    const bar = el('div', 'bar');
    bar.appendChild(el('span', 'bl', 'Диктовка · Whisper'));
    const bt = el('span', 'bt');
    const i = el('i');
    i.style.width = '100%';
    bt.appendChild(i);
    bar.appendChild(bt);
    bar.appendChild(el('span', 'bv', '100%'));
    sect2.appendChild(bar);
    pane.appendChild(sect2);

    return pane;
  }

  // тепловая карта: считаем число диктовок по дню, раскрашиваем последние 16 недель
  function buildHeatmap(items, daysSet) {
    const heat = el('div', 'heat');
    // счётчик по дню
    const counts = Object.create(null);
    for (const it of items) {
      const k = dayKey(it.ts);
      counts[k] = (counts[k] || 0) + 1;
    }
    let maxC = 1;
    for (const k in counts) if (counts[k] > maxC) maxC = counts[k];

    const WEEKS = 16, CELLS = WEEKS * 7;
    const now = new Date();
    const start = new Date(now.getFullYear(), now.getMonth(), now.getDate());
    // самая старая ячейка — (CELLS-1) дней назад
    for (let i = CELLS - 1; i >= 0; i--) {
      const d = new Date(start.getTime() - i * 86400000);
      const k = d.getFullYear() + '-' + d.getMonth() + '-' + d.getDate();
      const cell = el('i');
      const c = counts[k] || 0;
      if (c > 0) {
        const ratio = c / maxC;
        if (ratio > 0.66) cell.className = 'l3';
        else if (ratio > 0.33) cell.className = 'l2';
        else cell.className = 'l1';
        cell.title = k + ' · ' + c + ' ' + pluralReplicas(c);
      }
      heat.appendChild(cell);
    }
    return heat;
  }

  /* ════════════════════════════════════════════════════════════════════════
   * РАЗДЕЛ: Словарь
   * ══════════════════════════════════════════════════════════════════════ */
  function buildDictRow(w) {
    const row = el('div', 'lrow');
    row.appendChild(el('span', 'key', w.word));
    row.appendChild(el('span', 'val', w.note || ''));
    if (w.count) row.appendChild(el('span', 'meta', '×' + w.count));
    const x = el('span', 'x', '✕');
    x.addEventListener('click', async (e) => {
      e.stopPropagation();
      await ipcDictRemove(w.word);
      state.dict = state.dict.filter((d) => d !== w);
      renderDictList();
    });
    row.appendChild(x);
    return row;
  }

  function renderDictList() {
    const list = state.dictList;
    if (!list) return;
    list.textContent = '';
    if (!state.dict.length) {
      list.appendChild(el('div', 'vh-empty', 'Пока нет своих слов — добавь термины, имена, названия.'));
      return;
    }
    for (const w of state.dict) list.appendChild(buildDictRow(w));
  }

  function buildDictPane() {
    const pane = el('div', 'pane');
    pane.dataset.k = 'dict';
    const sect = el('div', 'sect');

    const h = el('div', 'secth');
    h.style.marginTop = '18px';
    h.appendChild(document.createTextNode('Свои слова и термины'));
    h.appendChild(el('span', 'cap', 'распознаются точнее'));
    sect.appendChild(h);

    // примечание, если IPC ещё нет
    if (!state.dictLive) {
      sect.appendChild(el('div', 'vh-note', 'Скоро — словарь свяжется с движком распознавания. Пока список живёт в этой сессии.'));
    }

    const addrow = el('div', 'addrow');
    const input = el('input');
    input.placeholder = 'Добавить слово (напр. Tauri, Haiku, openWakeWord)…';
    const btn = el('button', 'btn', 'Добавить');
    const doAdd = async () => {
      const word = input.value.trim();
      if (!word) return;
      if (state.dict.some((d) => d.word.toLowerCase() === word.toLowerCase())) {
        showToast('Уже в словаре');
        input.value = '';
        return;
      }
      const entry = { word, note: '', count: 0 };
      state.dict.unshift(entry);
      await ipcDictAdd(word, '');
      input.value = '';
      renderDictList();
    };
    btn.addEventListener('click', (e) => { e.stopPropagation(); doAdd(); });
    input.addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); doAdd(); } });
    addrow.appendChild(input);
    addrow.appendChild(btn);
    sect.appendChild(addrow);

    const list = el('div', 'vh-dictlist');
    state.dictList = list;
    sect.appendChild(list);

    pane.appendChild(sect);
    return pane;
  }

  /* ════════════════════════════════════════════════════════════════════════
   * РАЗДЕЛ: Преобразования (библиотека умных промптов)
   * ══════════════════════════════════════════════════════════════════════ */
  function buildTransformRow(p) {
    const tr = el('div', 'tr');

    const ti = el('span', 'ti');
    ti.appendChild(promptIcon(p.icon));
    tr.appendChild(ti);

    const mid = el('div', 'vh-trmid');
    mid.appendChild(el('div', 'tn', p.name));
    mid.appendChild(el('div', 'tdesc', p.desc));
    if (p.auto && p.trigger) {
      const trig = el('span', 'trig');
      trig.appendChild(ICONS.check());
      trig.appendChild(document.createTextNode('авто: ' + p.trigger));
      mid.appendChild(trig);
    } else {
      mid.appendChild(el('span', 'trig manual', 'вручную'));
    }
    tr.appendChild(mid);

    const tg = el('span', p.enabled ? 'tg' : 'tg off');
    tg.addEventListener('click', (e) => {
      e.stopPropagation();
      p.enabled = !p.enabled;
      tg.classList.toggle('off', !p.enabled);
    });
    tr.appendChild(tg);
    return tr;
  }

  function renderTransformList() {
    const list = state.trList;
    if (!list) return;
    list.textContent = '';
    for (const p of state.prompts) list.appendChild(buildTransformRow(p));
  }

  function buildAddPromptForm() {
    // простая инлайн-форма добавления локального преобразования
    const form = el('div', 'vh-addform');
    const nameIn = el('input');
    nameIn.placeholder = 'Название преобразования';
    const descIn = el('input');
    descIn.placeholder = 'Короткое описание';
    const btn = el('button', 'btn', 'Создать');
    btn.addEventListener('click', (e) => {
      e.stopPropagation();
      const name = nameIn.value.trim();
      if (!name) return;
      state.prompts.push({
        id: 'custom-' + Date.now(),
        icon: 'sparkle',
        name,
        desc: descIn.value.trim() || 'Своё преобразование.',
        trigger: null, auto: false, enabled: true,
      });
      nameIn.value = ''; descIn.value = '';
      form.classList.remove('on');
      renderTransformList();
    });
    form.appendChild(nameIn);
    form.appendChild(descIn);
    form.appendChild(btn);
    return form;
  }

  function buildTransformsPane() {
    const pane = el('div', 'pane');
    pane.dataset.k = 'transforms';
    const sect = el('div', 'sect');

    // ── строка «Умный режим» (auto-apply) с тумблером ──
    const smartRow = el('div', 'vh-smartrow');
    const sIcon = el('span', 'ti');
    sIcon.appendChild(ICONS.sparkle());
    smartRow.appendChild(sIcon);
    const sMid = el('div', 'vh-trmid');
    sMid.appendChild(el('div', 'tn', 'Умный режим'));
    sMid.appendChild(el('div', 'tdesc', 'Авто-подбор и применение преобразования по тексту и контексту.'));
    smartRow.appendChild(sMid);
    const smartTg = el('span', state.smart ? 'tg' : 'tg off');
    smartTg.addEventListener('click', async (e) => {
      e.stopPropagation();
      state.smart = !state.smart;
      smartTg.classList.toggle('off', !state.smart);
      // «Умный режим» и сайдкарта «Умные промпты» — ОДНА функция: синхронизируем.
      if (state.smartTg && state.smartTg !== smartTg) state.smartTg.classList.toggle('off', !state.smart);
      await ipcPromptsSetSmart(state.smart);
    });
    smartRow.appendChild(smartTg);
    sect.appendChild(smartRow);

    // ── заголовок секции + «+ добавить» ──
    const h = el('div', 'secth');
    h.style.marginTop = '18px';
    h.appendChild(document.createTextNode('Преобразования'));
    const addCap = el('span', 'cap vh-addcap', '+ добавить');
    const form = buildAddPromptForm();
    addCap.addEventListener('click', (e) => {
      e.stopPropagation();
      form.classList.toggle('on');
    });
    h.appendChild(addCap);
    sect.appendChild(h);
    sect.appendChild(form);

    const list = el('div', 'vh-trlist');
    state.trList = list;
    sect.appendChild(list);

    pane.appendChild(sect);
    return pane;
  }

  /* ════════════════════════════════════════════════════════════════════════
   * РАЗДЕЛ: Черновик
   * ══════════════════════════════════════════════════════════════════════ */
  function buildScratchPane() {
    const pane = el('div', 'pane');
    pane.dataset.k = 'scratch';
    const wrap = el('div', 'scratch');

    const ta = el('textarea');
    ta.placeholder = 'Надиктуй сюда что угодно — свободное поле для мыслей, заметок, длинных промптов…';
    ta.value = state.scratch || '';
    let saveTimer = null;
    ta.addEventListener('input', () => {
      state.scratch = ta.value;
      clearTimeout(saveTimer);
      saveTimer = setTimeout(() => { ipcScratchSet(ta.value); }, 400);
    });
    wrap.appendChild(ta);

    const hint = el('div', 'hint');
    hint.appendChild(ICONS.mic());
    hint.appendChild(document.createTextNode(' Зажми F8 и говори — текст добавится сюда'));
    wrap.appendChild(hint);

    pane.appendChild(wrap);
    return pane;
  }

  /* ════════════════════════════════════════════════════════════════════════
   * Навигация / переключение разделов
   * ══════════════════════════════════════════════════════════════════════ */
  const TITLES = {
    history: 'С возвращением',
    insights: 'Статистика',
    dict: 'Словарь',
    transforms: 'Преобразования',
    scratch: 'Черновик',
  };

  function switchSection(k) {
    if (!state) return;
    state.section = k;
    // навигация
    if (state.nav) {
      const items = state.nav.querySelectorAll('.it');
      items.forEach((x) => x.classList.toggle('on', x.dataset.k === k));
    }
    // панели
    if (state.panes) {
      for (const key in state.panes) {
        state.panes[key].classList.toggle('on', key === k);
      }
    }
    if (state.title) state.title.textContent = TITLES[k] || '';
    closeMenu();

    // ленивая дорисовка содержимого, зависящего от данных
    if (k === 'history') { renderHistory(); renderRail(); }
    else if (k === 'transforms') renderTransformList();
    else if (k === 'dict') renderDictList();
    else if (k === 'insights') rebuildInsights();
  }

  // Статистика и тепловая карта зависят от данных → пересобираем пейн при заходе
  function rebuildInsights() {
    if (!state.panes || !state.panes.insights) return;
    const old = state.panes.insights;
    const fresh = buildInsightsPane();
    fresh.classList.add('on');
    old.replaceWith(fresh);
    state.panes.insights = fresh;
  }

  /* ════════════════════════════════════════════════════════════════════════
   * Сборка каркаса (сайдбар + main)
   * ══════════════════════════════════════════════════════════════════════ */
  function buildSidebar() {
    const side = el('aside', 'side');

    // бренд
    const brand = el('div', 'brand');
    brand.appendChild(el('span', 'mk', 'J'));
    brand.appendChild(el('span', 'nm', 'Whisper'));
    brand.appendChild(el('span', 'tag', 'локально'));
    side.appendChild(brand);

    // навигация
    const nav = el('nav', 'nav');
    state.nav = nav;
    const NAV = [
      { k: 'history', icon: 'history', label: 'История' },
      { k: 'insights', icon: 'insights', label: 'Статистика' },
      { k: 'dict', icon: 'dict', label: 'Словарь' },
      { k: 'transforms', icon: 'sparkle', label: 'Преобразования' },
      { k: 'scratch', icon: 'scratch', label: 'Черновик' },
    ];
    for (const n of NAV) {
      const it = el('div', n.k === 'history' ? 'it on' : 'it');
      it.dataset.k = n.k;
      it.appendChild(ICONS[n.icon]());
      it.appendChild(document.createTextNode(n.label));
      it.addEventListener('click', (e) => { e.stopPropagation(); switchSection(n.k); });
      nav.appendChild(it);
    }
    side.appendChild(nav);

    side.appendChild(el('div', 'sp'));

    // сайдкарта «Умные промпты» с тумблером
    const smart = el('div', 'smartcard');
    const ic = el('span', 'ic');
    ic.appendChild(ICONS.sparkle());
    smart.appendChild(ic);
    const txt = el('div');
    txt.appendChild(el('div', 't', 'Умные промпты'));
    txt.appendChild(el('div', 'd', 'авто-подбор по тексту'));
    smart.appendChild(txt);
    const tg = el('span', state.smart ? 'tg' : 'tg off');
    tg.addEventListener('click', async (e) => {
      e.stopPropagation();
      state.smart = !state.smart;
      tg.classList.toggle('off', !state.smart);
      await ipcPromptsSetSmart(state.smart);
      // синхронизируем тумблер в разделе «Преобразования», если он отрисован
      renderTransformList();
    });
    state.smartTg = tg;
    smart.appendChild(tg);
    side.appendChild(smart);

    // подвал — настройки
    const foot = el('div', 'foot');
    const fit = el('div', 'it');
    fit.appendChild(ICONS.settings());
    fit.appendChild(document.createTextNode('Настройки'));
    fit.addEventListener('click', (e) => {
      e.stopPropagation();
      try {
        if (window.jarvis && typeof window.jarvis.onboardingOpen === 'function') {
          // мягкий маршрут к настройкам, если приложение его слушает
        }
      } catch (er) { /* игнор */ }
      showToast('Настройки — в основном окне');
    });
    foot.appendChild(fit);
    side.appendChild(foot);

    return side;
  }

  function buildShell(rootEl) {
    rootEl.textContent = '';

    const root = el('div', null);
    root.id = 'voicehist';

    root.appendChild(buildSidebar());

    const main = el('main', 'main');
    const mhead = el('div', 'mhead');
    const title = el('div', 'h', TITLES[state.section] || 'С возвращением');
    state.title = title;
    mhead.appendChild(title);
    main.appendChild(mhead);

    // панели разделов
    state.panes = {
      history: buildHistoryPane(),
      insights: buildInsightsPane(),
      dict: buildDictPane(),
      transforms: buildTransformsPane(),
      scratch: buildScratchPane(),
    };
    // изначально активна только история
    for (const key in state.panes) {
      state.panes[key].classList.toggle('on', key === state.section);
      main.appendChild(state.panes[key]);
    }

    root.appendChild(main);
    rootEl.appendChild(root);
  }

  /* ── Глобальный «клик мимо»: закрывает любое открытое меню ───────────────── */
  function onDocClick() {
    closeMenu();
  }

  /* ════════════════════════════════════════════════════════════════════════
   * Публичный вход: window.initVoiceHistory(rootEl)
   * ══════════════════════════════════════════════════════════════════════ */
  async function initVoiceHistory(rootEl) {
    if (!rootEl) return;
    injectStyle();

    closeMenu();
    state = {
      section: 'history',
      query: '',
      items: [],
      usage: null,      // getUsage('today')
      usageAll: null,   // getUsage('all')
      dict: [],
      dictLive: false,  // true → IPC словаря реально отвечает
      prompts: [],
      smart: false,
      scratch: '',
      // DOM-ссылки
      nav: null, title: null, panes: null,
      feed: null, rail: null,
      dictList: null, trList: null, smartTg: null,
    };

    // ── загрузка данных (параллельно, всё в try/catch внутри обёрток) ──
    const [items, usage, usageAll, dict, prompts, settings, scratch] = await Promise.all([
      ipcGetTranscripts(),
      ipcUsage('today'),
      ipcUsage('all'),
      ipcDictGet(),
      ipcPromptsGet(),
      ipcPromptsSettings(),
      ipcScratchGet(),
    ]);

    state.items = items;
    state.usage = usage;
    state.usageAll = usageAll;

    if (dict) { state.dict = dict; state.dictLive = true; }
    else { state.dict = []; state.dictLive = false; }

    state.prompts = prompts || BUILTIN_PROMPTS.map((p) => Object.assign({}, p));

    if (settings && typeof settings.smart === 'boolean') state.smart = settings.smart;
    else if (settings && typeof settings.enabled === 'boolean') state.smart = settings.enabled;

    state.scratch = scratch || '';

    buildShell(rootEl);

    // первичная отрисовка активного раздела
    renderHistory();
    renderRail();
    renderTransformList();
    renderDictList();

    // глобальный «клик мимо» — единожды на весь жизненный цикл модуля
    if (!docClickBound) {
      document.addEventListener('click', onDocClick);
      docClickBound = true;
    }
  }

  window.initVoiceHistory = initVoiceHistory;

  /* ════════════════════════════════════════════════════════════════════════
   * Стили: всё под #voicehist. Глобальные токены берём из index.html,
   * локально доопределяем акцент/карты/сайдбар.
   * ══════════════════════════════════════════════════════════════════════ */
  const CSS = `
#voicehist {
  --accent: #6ca0ff;
  --accent-soft: rgba(108,160,255,.14);
  --accent-line: rgba(108,160,255,.3);
  --done-soft: rgba(65,201,142,.12);
  --done-line: rgba(65,201,142,.32);
  --sidebar: #121216;
  --card: rgba(255,255,255,0.03);
  position: relative;
  width: 100%; height: 100%;
  display: flex; min-width: 0;
  overflow: hidden;
  font-family: -apple-system, BlinkMacSystemFont, "SF Pro Text", "Segoe UI", system-ui, sans-serif;
  font-size: 13px;
  color: var(--text);
}

/* ════ Сайдбар ════ */
#voicehist .side {
  width: 210px; flex: none; background: var(--sidebar);
  border-right: 1px solid var(--hairline);
  display: flex; flex-direction: column; padding: 16px 12px 12px;
}
#voicehist .brand { display: flex; align-items: center; gap: 9px; padding: 2px 8px 16px; }
#voicehist .brand .mk {
  width: 22px; height: 22px; border-radius: 6px;
  background: linear-gradient(135deg,#6ca0ff,#8b7ec8);
  display: grid; place-items: center; color: #0a0a0c; font: 800 12px/1 var(--mono);
}
#voicehist .brand .nm { font-size: 15px; font-weight: 600; }
#voicehist .brand .tag {
  margin-left: auto; font-size: 10px; color: var(--muted);
  border: 1px solid var(--border); border-radius: 6px; padding: 2px 7px;
}
#voicehist .nav { display: flex; flex-direction: column; gap: 2px; }
#voicehist .nav .it {
  display: flex; align-items: center; gap: 11px; padding: 8px 9px; border-radius: 8px;
  color: var(--text-body); font-size: 13.5px; cursor: default; user-select: none;
}
#voicehist .nav .it:hover { background: var(--row-hover); }
#voicehist .nav .it.on { background: rgba(255,255,255,.07); color: var(--text); }
#voicehist .nav .it svg { width: 17px; height: 17px; flex: none; opacity: .8; }
#voicehist .nav .it.on svg { color: var(--accent); opacity: 1; }
#voicehist .side .sp { flex: 1; }

#voicehist .smartcard {
  display: flex; align-items: center; gap: 10px; background: var(--accent-soft);
  border: 1px solid var(--accent-line); border-radius: 11px; padding: 10px 11px; margin-bottom: 10px;
}
#voicehist .smartcard .ic {
  width: 28px; height: 28px; border-radius: 8px; background: rgba(108,160,255,.2);
  display: grid; place-items: center; color: var(--accent); flex: none;
}
#voicehist .smartcard .ic svg { width: 16px; height: 16px; }
#voicehist .smartcard .t { font-size: 12px; color: var(--text); font-weight: 500; }
#voicehist .smartcard .d { font-size: 10.5px; color: var(--muted); margin-top: 2px; }

#voicehist .foot {
  border-top: 1px solid var(--hairline); padding-top: 11px;
  display: flex; flex-direction: column; gap: 2px;
}
#voicehist .foot .it {
  display: flex; align-items: center; gap: 11px; padding: 7px 9px; border-radius: 8px;
  color: var(--muted); font-size: 13px; cursor: default;
}
#voicehist .foot .it:hover { background: var(--row-hover); color: var(--text); }
#voicehist .foot .it svg { width: 16px; height: 16px; opacity: .8; }

/* тумблеры (сайдкарта + ряды + умный режим) */
#voicehist .tg {
  width: 32px; height: 19px; border-radius: 10px; background: var(--accent);
  position: relative; flex: none;
}
#voicehist .tg::after {
  content: ""; position: absolute; top: 2px; right: 2px;
  width: 15px; height: 15px; border-radius: 50%; background: #fff; transition: .15s;
}
#voicehist .tg.off { background: rgba(255,255,255,.13); }
#voicehist .tg.off::after { right: auto; left: 2px; }
#voicehist .smartcard .tg { margin-left: auto; }

/* ════ Main ════ */
#voicehist .main { flex: 1; min-width: 0; display: flex; flex-direction: column; overflow: hidden; }
#voicehist .mhead {
  padding: 20px 24px 12px; display: flex; align-items: center;
  justify-content: space-between; flex: none;
}
#voicehist .mhead .h { font-size: 21px; font-weight: 600; }
#voicehist .pane { flex: 1; min-height: 0; overflow-y: auto; display: none; padding-bottom: 72px; }
#voicehist .pane.on { display: block; }
#voicehist .pane::-webkit-scrollbar { width: 0; }

/* ════ История: лента + правый рейл ════ */
#voicehist .home { display: flex; height: 100%; }
#voicehist .feed {
  flex: 1; min-width: 0; overflow-y: auto; padding: 6px 8px 80px 24px;
  display: flex; flex-direction: column;
}
#voicehist .feed::-webkit-scrollbar { width: 0; }
#voicehist .vh-searchwrap {
  display: flex; align-items: center; gap: 9px; padding: 8px 10px; margin: 0 4px 4px;
  background: var(--card); border: 1px solid var(--hairline); border-radius: 10px; flex: none;
}
#voicehist .vh-si { color: var(--faint); display: flex; align-items: center; }
#voicehist .vh-searchwrap input {
  flex: 1; background: transparent; border: 0; outline: 0;
  color: var(--text); font: 400 13.5px/1 inherit;
}
#voicehist .vh-searchwrap input::placeholder { color: var(--faint); }
#voicehist .vh-feedbody { flex: 1; min-height: 0; }

#voicehist .rail {
  width: 208px; flex: none; border-left: 1px solid var(--hairline);
  padding: 18px; display: flex; flex-direction: column; gap: 12px;
}
#voicehist .scard {
  background: var(--card); border: 1px solid var(--hairline); border-radius: 13px; padding: 15px 16px;
}
#voicehist .scard .n {
  font-family: var(--mono); font-size: 26px; font-weight: 600;
  font-variant-numeric: tabular-nums; line-height: 1;
}
#voicehist .scard .n.acc { color: var(--accent); }
#voicehist .scard .l { font-size: 11.5px; color: var(--muted); margin-top: 5px; }
#voicehist .scard.streak .n { font-size: 22px; }
#voicehist .scard .sub { font-size: 10.5px; color: var(--faint); margin-top: 3px; }

#voicehist .dayhead {
  position: sticky; top: 0; background: var(--bg); padding: 13px 4px 7px;
  font: 600 10.5px/1 inherit; letter-spacing: .07em; text-transform: uppercase;
  color: var(--faint); z-index: 1; display: flex; align-items: center;
}
#voicehist .dayhead .vh-cnt {
  margin-left: auto; font-family: var(--mono); font-size: 10px;
  color: var(--faint); text-transform: none; letter-spacing: 0;
}
#voicehist .ent {
  display: flex; gap: 18px; padding: 13px 8px 13px 4px;
  border-top: 1px solid var(--hairline); position: relative;
}
#voicehist .ent:hover { background: var(--row-hover); border-radius: 10px; }
#voicehist .ent .lc {
  flex: none; width: 58px; display: flex; flex-direction: column; gap: 7px; padding-top: 1px;
}
#voicehist .ent .tm {
  font-family: var(--mono); font-size: 12px; color: var(--faint); font-variant-numeric: tabular-nums;
}
#voicehist .ent .vh-body { flex: 1; min-width: 0; }
#voicehist .ent .tx {
  font-size: 13.5px; line-height: 1.55; color: var(--text-body);
  word-wrap: break-word; overflow-wrap: anywhere;
}
#voicehist .autotag {
  display: inline-flex; align-items: center; gap: 4px; font-size: 10px; color: var(--done);
  background: var(--done-soft); border: 1px solid var(--done-line); border-radius: 6px;
  padding: 2px 6px; white-space: nowrap; align-self: flex-start;
}
#voicehist .autotag svg { width: 10px; height: 10px; }

/* ховер-действия — отдельной строкой ПОД текстом (в потоке, не оверлеем):
   текст всегда на всю ширину, кнопки появляются под ним при наведении. */
#voicehist .ent .vh-acts {
  display: none; align-items: center; gap: 6px; margin-top: 10px;
}
#voicehist .ent:hover .vh-acts { display: flex; }
#voicehist .vh-acts button {
  appearance: none; border: 1px solid var(--hairline); background: rgba(255,255,255,.04);
  color: var(--text-body); font: 500 11px/1 inherit; padding: 5px 9px; border-radius: 6px;
  cursor: default; display: flex; align-items: center; gap: 5px; white-space: nowrap;
}
#voicehist .vh-acts button:hover { background: rgba(255,255,255,.09); color: var(--text); }
#voicehist .vh-acts button.vh-primary {
  border-color: var(--accent-line); background: var(--accent-soft); color: var(--accent);
}
#voicehist .vh-acts button.vh-icon { padding: 5px 7px; }
#voicehist .vh-acts button.vh-danger:hover { border-color: rgba(242,99,99,.4); color: #f26363; }

/* инлайн-результат преобразования */
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
  color: var(--text-body); font: 500 11px/1 inherit; padding: 5px 9px; border-radius: 6px; cursor: default;
}
#voicehist .vh-eh button:hover { color: var(--text); background: rgba(255,255,255,.09); }
#voicehist .vh-eh button.vh-acc {
  border-color: var(--accent-line); background: var(--accent-soft); color: var(--accent);
}
#voicehist .vh-eh button.vh-eh-icon { padding: 4px 9px; font-size: 16px; line-height: .5; letter-spacing: 1px; }
#voicehist .vh-etext { padding: 11px 12px; font-size: 13px; line-height: 1.5; color: var(--text); }

/* меню преобразований (поповер) */
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

/* ════ Статистика ════ */
#voicehist .grid { display: grid; grid-template-columns: 1fr 1fr 1fr; gap: 14px; padding: 18px 24px; }
#voicehist .bigcard { background: var(--card); border: 1px solid var(--hairline); border-radius: 14px; padding: 18px; }
#voicehist .bigcard .bn {
  font-family: var(--mono); font-size: 30px; font-weight: 600; font-variant-numeric: tabular-nums;
}
#voicehist .bigcard .bl {
  font-size: 11px; color: var(--muted); text-transform: uppercase; letter-spacing: .05em; margin-top: 6px;
}
#voicehist .bigcard .line { height: 1px; background: var(--hairline); margin: 13px 0; }
#voicehist .bigcard .r {
  display: flex; justify-content: space-between; font-size: 12.5px; color: var(--text-body); margin-top: 7px;
}
#voicehist .bigcard .r b { font-family: var(--mono); font-weight: 600; }

#voicehist .sect { padding: 0 24px 22px; }
#voicehist .secth {
  font-size: 14px; font-weight: 600; margin: 18px 0 12px;
  display: flex; align-items: center; gap: 10px;
}
#voicehist .secth .cap { margin-left: auto; font-size: 11px; color: var(--faint); font-family: var(--mono); font-weight: 400; }

#voicehist .heat { display: grid; grid-template-columns: repeat(16,1fr); gap: 4px; margin-top: 14px; }
#voicehist .heat i { aspect-ratio: 1; border-radius: 3px; background: rgba(255,255,255,.05); }
#voicehist .heat i.l1 { background: rgba(108,160,255,.3); }
#voicehist .heat i.l2 { background: rgba(108,160,255,.55); }
#voicehist .heat i.l3 { background: rgba(108,160,255,.85); }

#voicehist .bar { display: flex; align-items: center; gap: 12px; margin: 9px 0; }
#voicehist .bar .bl { width: 150px; font-size: 12.5px; color: var(--text-body); }
#voicehist .bar .bt { flex: 1; height: 8px; border-radius: 5px; background: rgba(255,255,255,.05); overflow: hidden; }
#voicehist .bar .bt i { display: block; height: 100%; background: var(--accent); border-radius: 5px; }
#voicehist .bar .bv { font-family: var(--mono); font-size: 11.5px; color: var(--muted); width: 44px; text-align: right; }

/* ════ Словарь / общие ряды ════ */
#voicehist .addrow { display: flex; gap: 9px; margin: 0 0 14px; }
#voicehist .addrow input {
  flex: 1; background: var(--card); border: 1px solid var(--border); border-radius: 9px;
  padding: 9px 12px; color: var(--text); font: 400 13px/1 inherit; outline: 0;
}
#voicehist .addrow input::placeholder { color: var(--faint); }
#voicehist .btn {
  background: var(--accent-soft); border: 1px solid var(--accent-line); color: var(--accent);
  border-radius: 9px; padding: 9px 14px; font: 500 13px/1 inherit; cursor: default;
}
#voicehist .btn:hover { background: rgba(108,160,255,.2); }
#voicehist .lrow {
  display: flex; align-items: center; gap: 12px; padding: 12px 14px;
  border: 1px solid var(--hairline); border-radius: 11px; margin-bottom: 8px; background: var(--card);
}
#voicehist .lrow .key { font-family: var(--mono); font-size: 13px; color: var(--accent); }
#voicehist .lrow .val { font-size: 13px; color: var(--text-body); flex: 1; }
#voicehist .lrow .meta { font-size: 11px; color: var(--faint); font-family: var(--mono); }
#voicehist .lrow .x { color: var(--faint); cursor: default; padding: 2px 6px; border-radius: 6px; }
#voicehist .lrow .x:hover { color: #f26363; background: rgba(242,99,99,.1); }
#voicehist .vh-note {
  font-size: 12px; color: var(--muted); background: var(--accent-soft);
  border: 1px solid var(--accent-line); border-radius: 10px; padding: 10px 12px; margin-bottom: 14px; line-height: 1.5;
}

/* ════ Преобразования ════ */
#voicehist .vh-smartrow,
#voicehist .tr {
  display: flex; align-items: flex-start; gap: 14px; padding: 15px 16px;
  border: 1px solid var(--hairline); border-radius: 13px; margin-bottom: 10px; background: var(--card);
}
#voicehist .vh-smartrow {
  border-color: var(--accent-line); background: var(--accent-soft); margin: 18px 24px 4px;
}
#voicehist .tr .ti, #voicehist .vh-smartrow .ti {
  width: 30px; height: 30px; border-radius: 8px; background: rgba(108,160,255,.2);
  color: var(--accent); display: grid; place-items: center; flex: none;
}
#voicehist .tr .ti svg, #voicehist .vh-smartrow .ti svg { width: 16px; height: 16px; }
#voicehist .vh-trmid { flex: 1; min-width: 0; }
#voicehist .tn { font-size: 14px; font-weight: 500; }
#voicehist .tdesc { font-size: 12px; color: var(--muted); margin-top: 4px; line-height: 1.5; }
#voicehist .trig {
  display: inline-flex; align-items: center; gap: 5px; margin-top: 8px; font-size: 11px;
  color: var(--done); background: var(--done-soft); border: 1px solid var(--done-line);
  border-radius: 6px; padding: 2px 8px;
}
#voicehist .trig svg { width: 11px; height: 11px; }
#voicehist .trig.manual {
  color: var(--muted); background: rgba(255,255,255,.05); border-color: var(--border);
}
#voicehist .tr .tg, #voicehist .vh-smartrow .tg {
  margin-left: auto; width: 34px; height: 20px; border-radius: 11px; margin-top: 3px;
}
#voicehist .tr .tg::after, #voicehist .vh-smartrow .tg::after { width: 16px; height: 16px; }
#voicehist .vh-addcap { cursor: default; }
#voicehist .vh-addcap:hover { color: var(--accent); }
#voicehist .vh-addform { display: none; gap: 9px; margin: 0 0 12px; flex-wrap: wrap; }
#voicehist .vh-addform.on { display: flex; }
#voicehist .vh-addform input {
  flex: 1; min-width: 140px; background: var(--card); border: 1px solid var(--border);
  border-radius: 9px; padding: 9px 12px; color: var(--text); font: 400 13px/1 inherit; outline: 0;
}
#voicehist .vh-addform input::placeholder { color: var(--faint); }

/* ════ Черновик ════ */
#voicehist .scratch { padding: 18px 24px; height: 100%; display: flex; flex-direction: column; }
#voicehist .scratch textarea {
  width: 100%; flex: 1; min-height: 0; background: var(--card); border: 1px solid var(--hairline);
  border-radius: 14px; padding: 16px; color: var(--text-body); font: 400 14px/1.6 inherit;
  outline: 0; resize: none;
}
#voicehist .scratch textarea::placeholder { color: var(--faint); }
#voicehist .scratch .hint {
  margin-top: 10px; font-size: 12px; color: var(--faint); display: flex; align-items: center; gap: 7px; flex: none;
}
#voicehist .scratch .hint svg { width: 14px; height: 14px; }

/* ════ Пусто ════ */
#voicehist .vh-empty {
  padding: 48px 24px; text-align: center; color: var(--faint); font-size: 13px; line-height: 1.5;
}
`;
})();
