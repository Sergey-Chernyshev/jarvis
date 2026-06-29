/* ============================================================================
 * settings2.js — самодостаточный модуль страницы настроек Jarvis.
 *
 * Экспортирует window.initSettings2(rootEl): строит сайдбар + детальные панели
 * в стиле macOS System Settings / Raycast, грузит значения из IPC (window.jarvis)
 * и подписывается на live-события. Полностью изолирован: все стили под #settings2,
 * иконки — инлайновый SVG (офлайн, без CDN). Повторный вызов перестраивает UI
 * без утечек слушателей и без дублей стилей.
 *
 * ВАЖНО: ничего не импортирует, чистый ванильный JS под WKWebView.
 * Иконки собираются через DOM (createElementNS) — без innerHTML, без XSS.
 * ========================================================================== */
(function () {
  'use strict';

  // ── Модульные флаги (живут между ре-init) ───────────────────────────────
  let subscribed = false;     // подписки на live-события поставлены лишь раз
  let docClickBound = false;  // глобальный «клик мимо» для закрытия селектов
  let currentRoot = null;     // активный rootEl (для live-перерисовок)
  let activePane = 'general'; // выбранная вкладка сайдбара
  const renderingPane = {};   // pane → идёт ли сейчас рендер (анти-гонка)
  const renderPending = {};   // pane → запрошен ли повторный рендер во время текущего
  // Состояние загрузок моделей. `activeDownload` — id модели, что качается СЕЙЧАС
  // (чтобы прогресс шёл только в её строку, а не во все). `dlState[id].error` —
  // текст последней ошибки (показываем в строке + retry), вместо тихого сброса.
  let activeDownload = null;
  const dlState = {};
  // Мультивыбор моделей для «Скачать выбранное» (чекбоксы в строках, id → выбран).
  const selectedModels = new Set();

  // ── Дефолты хоткеев (сверено с HK_DEFAULTS + settings_get в renderer.js) ──
  const HK_DEFAULTS = {
    hotkey: 'Command+J',
    continueHotkey: 'Command+Alt+C',
    repeatHotkey: 'Command+Alt+R',
    muteHotkey: 'Command+Alt+M',
    quietHotkey: 'Command+Alt+J',
  };

  // ── IPC-обёртка: никогда не бросает наружу, возвращает fallback ──────────
  async function safe(fn, fallback) {
    try {
      if (!window.jarvis || typeof fn !== 'function') return fallback;
      const r = await fn();
      return r === undefined || r === null ? fallback : r;
    } catch (e) {
      return fallback;
    }
  }
  // вызвать IPC-action (без ожидания результата), проглотив ошибку
  function fire(fn) {
    try { if (window.jarvis && typeof fn === 'function') return fn(); } catch (e) {}
    return undefined;
  }

  // ── Утилита формата размера на диске (порт fmtBytes из renderer.js) ──────
  function fmtBytes(n) {
    if (!n) return '0 МБ';
    const mb = n / (1024 * 1024);
    if (mb >= 1024) return (mb / 1024).toFixed(mb >= 10240 ? 0 : 1) + ' ГБ';
    return Math.max(1, Math.round(mb)) + ' МБ';
  }

  // ── displayHotkey (порт из renderer.js): акселератор → символы клавиш ────
  function displayHotkey(acc) {
    return String(acc || '')
      .replace('CommandOrControl', '⌘').replace('Command', '⌘')
      .replace('Control', '⌃').replace('Option', '⌥').replace('Alt', '⌥')
      .replace('Shift', '⇧').replaceAll('+', ' ');
  }
  // акселератор → массив отдельных клавиш-капсов
  function hotkeyKeys(acc) {
    return displayHotkey(acc).split(' ').filter(Boolean);
  }

  /* ========================================================================
   * Инлайновые lucide-style иконки (24×24, stroke=currentColor). Данные —
   * настоящий lucide-path, чтобы штрих совпал с остальным приложением.
   * Хранятся как массивы примитивов и собираются через createElementNS —
   * никакого innerHTML (офлайн + безопасно).
   * ====================================================================== */
  const SVG_NS = 'http://www.w3.org/2000/svg';
  // каждый элемент: [tag, {атрибуты}]
  const ICONS = {
    'settings': [
      ['circle', { cx: 12, cy: 12, r: 3 }],
      ['path', { d: 'M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z' }],
    ],
    'mic': [
      ['path', { d: 'M12 2a3 3 0 0 0-3 3v7a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3Z' }],
      ['path', { d: 'M19 10v2a7 7 0 0 1-14 0v-2' }],
      ['line', { x1: 12, x2: 12, y1: 19, y2: 22 }],
    ],
    'volume-2': [
      ['polygon', { points: '11 5 6 9 2 9 2 15 6 15 11 19 11 5' }],
      ['path', { d: 'M15.54 8.46a5 5 0 0 1 0 7.07' }],
      ['path', { d: 'M19.07 4.93a10 10 0 0 1 0 14.14' }],
    ],
    'bell': [
      ['path', { d: 'M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9' }],
      ['path', { d: 'M10.3 21a1.94 1.94 0 0 0 3.4 0' }],
    ],
    'coffee': [
      ['path', { d: 'M10 2v2' }],
      ['path', { d: 'M14 2v2' }],
      ['path', { d: 'M16 8a1 1 0 0 1 1 1v8a4 4 0 0 1-4 4H7a4 4 0 0 1-4-4V9a1 1 0 0 1 1-1h14a4 4 0 1 1 0 8h-1' }],
      ['path', { d: 'M6 2v2' }],
    ],
    'keyboard': [
      ['path', { d: 'M10 8h.01' }],
      ['path', { d: 'M12 12h.01' }],
      ['path', { d: 'M14 8h.01' }],
      ['path', { d: 'M16 12h.01' }],
      ['path', { d: 'M18 8h.01' }],
      ['path', { d: 'M6 8h.01' }],
      ['path', { d: 'M7 16h10' }],
      ['path', { d: 'M8 12h.01' }],
      ['rect', { width: 20, height: 16, x: 2, y: 4, rx: 2 }],
    ],
    'cable': [
      ['path', { d: 'M17 21v-2a1 1 0 0 1-1-1v-1a2 2 0 0 1 2-2h2a2 2 0 0 1 2 2v1a1 1 0 0 1-1 1' }],
      ['path', { d: 'M19 15V6.5a1 1 0 0 0-7 0v11a1 1 0 0 1-7 0V9' }],
      ['path', { d: 'M21 21v-2h-4' }],
      ['path', { d: 'M3 5h4V3' }],
      ['path', { d: 'M7 5a1 1 0 0 1 1 1v1a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V6a1 1 0 0 1 1-1V3' }],
    ],
    'info': [
      ['circle', { cx: 12, cy: 12, r: 10 }],
      ['path', { d: 'M12 16v-4' }],
      ['path', { d: 'M12 8h.01' }],
    ],
    'cpu': [
      ['rect', { width: 16, height: 16, x: 4, y: 4, rx: 2 }],
      ['rect', { width: 6, height: 6, x: 9, y: 9, rx: 1 }],
      ['path', { d: 'M15 2v2' }],
      ['path', { d: 'M15 20v2' }],
      ['path', { d: 'M2 15h2' }],
      ['path', { d: 'M2 9h2' }],
      ['path', { d: 'M20 15h2' }],
      ['path', { d: 'M20 9h2' }],
      ['path', { d: 'M9 2v2' }],
      ['path', { d: 'M9 20v2' }],
    ],
    'chevron-down': [['path', { d: 'm6 9 6 6 6-6' }]],
    'chevron-left': [['path', { d: 'm15 18-6-6 6-6' }]],
    'chevron-right': [['path', { d: 'm9 18 6-6-6-6' }]],
    'rotate-ccw': [
      ['path', { d: 'M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8' }],
      ['path', { d: 'M3 3v5h5' }],
    ],
    'search': [
      ['circle', { cx: 11, cy: 11, r: 8 }],
      ['path', { d: 'm21 21-4.3-4.3' }],
    ],
    'loader-circle': [['path', { d: 'M21 12a9 9 0 1 1-6.219-8.56' }]],
    'check': [['path', { d: 'M20 6 9 17l-5-5' }]],
    'download': [
      ['path', { d: 'M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4' }],
      ['polyline', { points: '7 10 12 15 17 10' }],
      ['line', { x1: 12, x2: 12, y1: 15, y2: 3 }],
    ],
    'trash-2': [
      ['path', { d: 'M3 6h18' }],
      ['path', { d: 'M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2' }],
      ['line', { x1: 10, x2: 10, y1: 11, y2: 17 }],
      ['line', { x1: 14, x2: 14, y1: 11, y2: 17 }],
    ],
  };
  // вернуть DOM-узел <svg> для иконки (стиль lucide, наследует currentColor)
  function icon(name) {
    const svg = document.createElementNS(SVG_NS, 'svg');
    svg.setAttribute('class', 'lucide');
    svg.setAttribute('width', '24');
    svg.setAttribute('height', '24');
    svg.setAttribute('viewBox', '0 0 24 24');
    svg.setAttribute('fill', 'none');
    svg.setAttribute('stroke', 'currentColor');
    svg.setAttribute('stroke-width', '2');
    svg.setAttribute('stroke-linecap', 'round');
    svg.setAttribute('stroke-linejoin', 'round');
    for (const [tag, attrs] of (ICONS[name] || [])) {
      const node = document.createElementNS(SVG_NS, tag);
      for (const k in attrs) node.setAttribute(k, String(attrs[k]));
      svg.appendChild(node);
    }
    return svg;
  }
  // обёртка <span> с иконкой внутри (для inline-вставки)
  function iconSpan(name, cls) {
    const s = document.createElement('span');
    if (cls) s.className = cls;
    s.style.display = 'inline-flex';
    s.appendChild(icon(name));
    return s;
  }

  /* ========================================================================
   * Маленькие DOM-хелперы.
   * ====================================================================== */
  // el('div.cls.cls2', {attr|on*|style|text}, [children|string|node])
  function el(tag, attrs, kids) {
    let name = tag, cls = '', id = '';
    const hashIdx = tag.indexOf('#');
    const dotIdx = tag.indexOf('.');
    // разбор "tag.cls.cls#id" / "tag#id.cls"
    let body = tag;
    if (hashIdx >= 0) {
      const before = tag.slice(0, hashIdx);
      const after = tag.slice(hashIdx + 1);
      id = after.split('.')[0];
      const afterCls = after.split('.').slice(1);
      body = before;
      cls = afterCls.join(' ');
    }
    const parts = body.split('.');
    name = parts[0] || 'div';
    const headCls = parts.slice(1).join(' ');
    cls = [headCls, cls].filter(Boolean).join(' ');

    const node = document.createElement(name);
    if (cls) node.className = cls;
    if (id) node.id = id;
    if (attrs) for (const k in attrs) {
      if (k === 'text') node.textContent = attrs[k];
      else if (k === 'style') node.style.cssText = attrs[k];
      else if (k.startsWith('on') && typeof attrs[k] === 'function') node.addEventListener(k.slice(2), attrs[k]);
      else if (attrs[k] != null) node.setAttribute(k, attrs[k]);
    }
    if (kids != null) {
      const arr = Array.isArray(kids) ? kids : [kids];
      for (const c of arr) {
        if (c == null) continue;
        node.appendChild(typeof c === 'string' ? document.createTextNode(c) : c);
      }
    }
    return node;
  }
  // строка детали: заголовок dt + пояснение dd + контрол(ы) справа
  function drow(title, desc, ctlNodes, opts) {
    const grow = el('div.grow', null, [
      el('div.dt', { text: title }),
      desc ? el('div.dd', { text: desc }) : null,
    ]);
    const dctl = el('div.dctl' + ((opts && opts.ctlClass) ? '.' + opts.ctlClass : ''));
    if (opts && opts.ctlStyle) dctl.style.cssText = opts.ctlStyle;
    const arr = Array.isArray(ctlNodes) ? ctlNodes : [ctlNodes];
    for (const c of arr) if (c) dctl.appendChild(c);
    const leading = opts && opts.dot ? el('span.dot' + (opts.dot === true ? '' : '.' + opts.dot), { style: 'margin-top:5px' }) : null;
    return el('div.drow', null, [leading, grow, dctl]);
  }
  // переключатель (toggle) → IPC
  function toggle(checked, onChange, disabled) {
    const t = el('input.toggle', { type: 'checkbox' });
    t.checked = !!checked;
    if (disabled) t.disabled = true;
    t.addEventListener('change', () => { try { onChange(t.checked); } catch (e) {} });
    return t;
  }
  // кнопка (.btn / .btn.sm / .btn.primary / .btn.danger)
  function button(label, onClick, extra) {
    const b = el('button.btn' + (extra ? '.' + extra.split(' ').join('.') : ''), { text: label });
    b.addEventListener('click', () => { try { onClick(b); } catch (e) {} });
    return b;
  }

  /* ── Скелетоны: показываем мерцающие плейсхолдеры, пока рендерер ждёт IPC
   * (sttGet/voiceGet/modelsGet и т.п.), и убираем по приходу данных — вкладка
   * перестаёт быть пустой в момент переключения. ─────────────────────────*/
  function skelRow() {
    return el('div.skrow', null, [
      el('div.skgrow', null, [
        el('div.skel.skbar', { style: 'width:' + (38 + ((Math.random() * 22) | 0)) + '%' }),
        el('div.skel.skbar', { style: 'width:' + (58 + ((Math.random() * 28) | 0)) + '%;height:11px;opacity:.6' }),
      ]),
      el('div.skel.skctl'),
    ]);
  }
  // группа из n скелетон-строк (в обёртке .dgroup — как настоящие группы)
  function skelGroup(n) {
    const g = el('div.dgroup');
    for (let i = 0; i < (n || 3); i++) g.appendChild(skelRow());
    return g;
  }

  /* ── Кастомный селект (.cselect): триггер + всплывающее меню ─────────────
   * options: [{value, label}], value — текущее, onPick(value) → IPC.
   * Возвращает {node, setBusy(label|false)}. */
  function customSelect(options, value, onPick) {
    const cur = options.find((o) => o.value === value) || options[0] || { value: '', label: '—' };
    const valSpan = el('span.cval', { text: cur ? cur.label : '—' });
    const spin = el('span.spin', null, icon('loader-circle'));
    const chev = el('span.chev', null, icon('chevron-down'));
    const trigger = el('button.cstrigger', null, [valSpan, spin, chev]);
    const menu = el('div.cmenu');
    const root = el('div.cselect', null, [trigger, menu]);

    for (const o of options) {
      const ck = el('span.ck', null, icon('check'));
      const opt = el('div.copt' + (o.value === cur.value ? '.selected' : ''), { 'data-value': o.value }, [
        document.createTextNode(o.label), ck,
      ]);
      opt.addEventListener('click', (e) => {
        e.stopPropagation();
        for (const x of menu.querySelectorAll('.copt')) x.classList.remove('selected');
        opt.classList.add('selected');
        valSpan.textContent = o.label;
        root.classList.remove('open');
        try { onPick(o.value); } catch (err) {}
      });
      menu.appendChild(opt);
    }

    trigger.addEventListener('click', (e) => {
      e.stopPropagation();
      if (root.classList.contains('busy')) return;
      const wasOpen = root.classList.contains('open');
      closeAllSelects(root);
      root.classList.toggle('open', !wasOpen);
    });

    return {
      node: root,
      setBusy(busy) {
        if (busy) { root.classList.add('busy'); root.classList.remove('open'); }
        else root.classList.remove('busy');
      },
    };
  }
  // закрыть все открытые селекты, кроме keep
  function closeAllSelects(keep) {
    if (!currentRoot) return;
    for (const s of currentRoot.querySelectorAll('.cselect.open')) {
      if (s !== keep) s.classList.remove('open');
    }
  }

  /* ── Сегментированный контрол: [{value,label}], onPick(value) ────────────*/
  function segmented(options, value, onPick) {
    const seg = el('div.seg');
    for (const o of options) {
      const b = el('button.segbtn' + (o.value === value ? '.active' : ''), { text: o.label });
      b.addEventListener('click', () => {
        for (const x of seg.querySelectorAll('.segbtn')) x.classList.remove('active');
        b.classList.add('active');
        try { onPick(o.value); } catch (e) {}
      });
      seg.appendChild(b);
    }
    return seg;
  }

  /* ── Хоткей-поле: клавиши-капсы (read-only) + кнопка сброса ──────────────*/
  function hotkeyField(acc, opts) {
    const keys = hotkeyKeys(acc);
    const hkey = el('div.hkey' + (opts && opts.rec ? '.rec' : ''));
    for (const k of keys) hkey.appendChild(el('kbd', { text: k }));
    if (!keys.length) hkey.appendChild(el('kbd', { text: '—' }));
    const nodes = [hkey];
    if (opts && opts.onReset) {
      const rb = el('button.hkreset', { title: 'Сбросить' }, icon('rotate-ccw'));
      rb.addEventListener('click', () => { try { opts.onReset(); } catch (e) {} });
      nodes.push(rb);
    }
    return nodes;
  }

  /* ── Инлайн-заметка ошибки загрузки (красная, под строкой модели) ───────
   * Показывает реальную причину провала скачивания (раньше ошибка молча
   * глоталась и статус «сбрасывался» в «не скачана»). */
  function dlErrorNote(msg) {
    const n = el('div.s2err');
    n.appendChild(el('span.s2err-ic', icon('alert-triangle')));
    n.appendChild(el('span.s2err-txt', { text: msg }));
    return n;
  }

  /* ── KeyboardEvent → tauri-аксельератор ("Command+Shift+D" / "F8") ───────
   * isFn=true для F1..F24 (их можно биндить без модификатора — push-to-talk).
   * Голую букву/цифру без модификатора биндить нельзя: глобальный шорткат
   * перехватит её ввод во всей системе. */
  function eventToAccel(e) {
    const mods = [];
    if (e.metaKey) mods.push('Command');
    if (e.ctrlKey) mods.push('Control');
    if (e.altKey) mods.push('Alt');
    if (e.shiftKey) mods.push('Shift');
    const code = e.code || '';
    const isFn = /^F\d{1,2}$/.test(code);
    let key = null;
    if (isFn) key = code;
    else if (/^Key[A-Z]$/.test(code)) key = code.slice(3);  // KeyD → D
    else if (/^Digit\d$/.test(code)) key = code.slice(5);   // Digit5 → 5
    else if (code === 'Space') key = 'Space';
    return { mods, key, isFn };
  }

  /* ── Поле хоткея диктовки с ЗАПИСЬЮ сочетания ────────────────────────────
   * Клик → режим записи (лови нажатие), Esc — отмена, кнопка сброса → F8.
   * Сохраняет через sttSetHotkey (бэкенд валидирует и перерегистрирует). */
  function dictationHotkeyField(current) {
    let acc = current || 'F8';
    const hkey = el('div.hkey.rec', { title: 'Кликни и нажми новое сочетание' });
    const paint = () => {
      hkey.classList.remove('recording');
      hkey.replaceChildren();
      for (const k of hotkeyKeys(acc)) hkey.appendChild(el('kbd', { text: k }));
    };
    const note = (txt) => { hkey.replaceChildren(el('kbd', { text: txt })); };
    let recording = false, onKey = null;
    function onAway(e) { if (!hkey.contains(e.target)) stop(); }
    function stop() {
      recording = false;
      if (onKey) { document.removeEventListener('keydown', onKey, true); onKey = null; }
      document.removeEventListener('click', onAway, true);
      paint();
    }
    function start() {
      if (recording) return;
      recording = true;
      hkey.classList.add('recording');
      note('Нажмите сочетание…');
      onKey = async (e) => {
        e.preventDefault(); e.stopPropagation();
        if (e.key === 'Escape') { stop(); return; }
        if (['Shift', 'Control', 'Alt', 'Meta'].includes(e.key)) return; // ждём основную клавишу
        const { mods, key, isFn } = eventToAccel(e);
        if (!key) { note('Эта клавиша не поддерживается'); return; }
        if (!isFn && mods.length === 0) { note('Нужен модификатор (⌘/⌥/⌃) или F-клавиша'); return; }
        const next = [...mods, key].join('+');
        const res = await safe(() => window.jarvis.sttSetHotkey(next), null);
        if (res && res.ok) { acc = res.hotkey || next; stop(); }
        else { note((res && res.error) || 'не удалось'); setTimeout(stop, 1500); }
      };
      document.addEventListener('keydown', onKey, true);
      document.addEventListener('click', onAway, true);
    }
    hkey.addEventListener('click', (e) => { e.stopPropagation(); start(); });
    paint();
    const rb = el('button.hkreset', { title: 'Сбросить на F8' }, icon('rotate-ccw'));
    // Применить готовый аксельератор (пресет/сброс) БЕЗ keydown-захвата. Нужно,
    // потому что webview на macOS «съедает» ⌘/⌥-комбинации до JS — записать их
    // нажатием не выходит; зато зарегистрированный глобальный шорткат срабатывает
    // на уровне ОС нормально. Поэтому ⌘K и т.п. ставим кнопкой-пресетом.
    const apply = async (accel) => {
      const res = await safe(() => window.jarvis.sttSetHotkey(accel), null);
      if (res && res.ok) { acc = res.hotkey || accel; paint(); }
      else { note((res && res.error) || 'не удалось'); setTimeout(paint, 1500); }
    };
    rb.addEventListener('click', (e) => { e.stopPropagation(); apply('F8'); });

    const PRESETS = [
      ['F8', 'F8'], ['F9', 'F9'],
      ['⌥Space', 'Alt+Space'], ['⌘⌥D', 'Command+Alt+D'], ['⌘K', 'Command+K'],
    ];
    const chips = el('div.hkpresets');
    for (const [label, accel] of PRESETS) {
      const c = el('button.hkchip', { text: label, title: 'Поставить ' + label });
      c.addEventListener('click', (e) => { e.stopPropagation(); apply(accel); });
      chips.appendChild(c);
    }

    const wrap = el('div', { style: 'display:flex;flex-direction:column;align-items:flex-end;gap:7px' });
    const topRow = el('div', { style: 'display:flex;align-items:center;gap:8px' });
    topRow.appendChild(hkey);
    topRow.appendChild(rb);
    wrap.appendChild(topRow);
    wrap.appendChild(chips);
    return wrap;
  }

  /* ── Полоса загрузки модели (.progress.striped) ─────────────────────────*/
  function progressBar(pct) {
    const bar = el('div.progress.striped', { style: 'margin-top:9px;max-width:280px' });
    bar.appendChild(el('i', { style: 'width:' + Math.max(0, Math.min(100, pct || 0)) + '%' }));
    return bar;
  }

  /* ── Кнопка удаления модели с двойным подтверждением «Точно?» ────────────*/
  function makeDeleteButton(id, after) {
    const del = el('button.btn.sm.danger');
    const setIcon = () => { del.replaceChildren(icon('trash-2')); };
    setIcon();
    let armed = false;
    del.addEventListener('click', async () => {
      if (!armed) { armed = true; del.replaceChildren(document.createTextNode('Точно?')); setTimeout(() => { armed = false; setIcon(); }, 3000); return; }
      del.disabled = true; del.replaceChildren(document.createTextNode('…'));
      await safe(() => window.jarvis.modelDelete(id), null);
      if (after) after();
    });
    return del;
  }

  /* ========================================================================
   * СТИЛИ — единый scoped <style id="settings2-style">, всё под #settings2.
   * Порт компонентов из prototypes/app.css. Инъекция один раз (гард).
   * ====================================================================== */
  function injectStyle() {
    if (document.getElementById('settings2-style')) return;
    const css = `
#settings2 {
  /* локальные токены поверх глобальных (если их нет в приложении) */
  --working-soft: rgba(108,160,255,0.14);
  --limit: #8b7ec8;
  --s2-font: var(--font, -apple-system, BlinkMacSystemFont, "SF Pro Text", "Segoe UI", sans-serif);
  --s2-mono: var(--mono, ui-monospace, "SF Mono", Menlo, monospace);
}
/* организм: окно настроек — заполняет rootEl, две независимо-скроллящихся колонки */
#settings2.swin2 { display:flex; flex-direction:row; height:100%; width:100%;
  color: var(--text, #e7e7ea); font-family: var(--s2-font); overflow:hidden; }
#settings2 *, #settings2 *::before, #settings2 *::after { box-sizing:border-box; }

/* ── Сайдбар ─────────────────────────────────────────────────────────── */
#settings2 .sidebar { width:248px; flex:none; border-right:1px solid var(--hairline, rgba(255,255,255,0.06));
  background: rgba(0,0,0,0.16); display:flex; flex-direction:column; min-height:0; }
#settings2 .ssearch { display:flex; align-items:center; gap:9px; margin:12px 12px 8px; padding:8px 11px;
  border-radius:8px; background: rgba(255,255,255,0.05); border:1px solid var(--border, rgba(255,255,255,0.08)); }
#settings2 .ssearch input { flex:1; background:transparent; border:0; outline:0; color:var(--text,#e7e7ea); font:400 13px/1 var(--s2-font); min-width:0; }
#settings2 .ssearch input::placeholder { color:var(--faint,#55555c); }
#settings2 .ssearch .si { color:var(--faint,#55555c); display:inline-flex; }
#settings2 .saccount { display:flex; align-items:center; gap:10px; padding:8px 14px 12px; border-bottom:1px solid var(--hairline, rgba(255,255,255,0.06)); }
#settings2 .saccount .ava { width:30px; height:30px; border-radius:8px; background:var(--working,#6ca0ff); display:grid; place-items:center; color:#08101e; font:700 13px/1 var(--s2-font); flex:none; }
#settings2 .saccount .nm { font-size:13px; font-weight:500; color:var(--text,#e7e7ea); }
#settings2 .saccount .sub { font-size:11px; color:var(--muted,#76767e); margin-top:2px; }
#settings2 .snav { flex:1; overflow-y:auto; padding:8px 9px; min-height:0; }
#settings2 .snav::-webkit-scrollbar { width:0; }
#settings2 .snav .item { display:flex; align-items:center; gap:10px; padding:7px 9px; border-radius:7px; font-size:13px; color:var(--text-body,#d6d6db); cursor:default; user-select:none; }
#settings2 .snav .item:hover { background: rgba(255,255,255,0.05); }
#settings2 .snav .item.sel { background: rgba(255,255,255,0.10); color:var(--text,#e7e7ea); }
#settings2 .snav .item .ic { width:22px; height:22px; border-radius:6px; display:grid; place-items:center; font-size:12px; flex:none; }
#settings2 .snav .sep { height:1px; background:var(--hairline, rgba(255,255,255,0.06)); margin:9px 9px; }
#settings2 .snav .grp { font:600 9.5px/1 var(--s2-font); letter-spacing:.08em; text-transform:uppercase; color:var(--faint,#55555c); padding:10px 10px 5px; }

/* ── Детальная панель ────────────────────────────────────────────────── */
#settings2 .detail { flex:1; overflow-y:auto; padding:18px 24px 88px; min-height:0; min-width:0; }
#settings2 .detail::-webkit-scrollbar { width:0; }
#settings2 .dnav { display:inline-flex; gap:1px; padding:3px; border-radius:8px; background:rgba(255,255,255,0.05); border:1px solid var(--border, rgba(255,255,255,0.08)); margin-bottom:18px; }
#settings2 .dnav button { appearance:none; border:0; background:transparent; color:var(--muted,#76767e); width:26px; height:22px; border-radius:6px; cursor:default; font-size:13px; display:grid; place-items:center; }
#settings2 .dnav button:hover { background:rgba(255,255,255,0.07); color:var(--text,#e7e7ea); }
#settings2 .dtitle { font-size:17px; font-weight:600; color:var(--text,#e7e7ea); margin:2px 2px 16px; }
#settings2 .dsection { font-size:13px; color:var(--muted,#76767e); font-weight:500; margin:6px 2px 11px; }
#settings2 .dgroup { background:rgba(255,255,255,0.025); border:1px solid var(--hairline, rgba(255,255,255,0.06)); border-radius:11px; margin-bottom:22px; }
#settings2 .drow { display:flex; align-items:flex-start; gap:20px; padding:15px 16px; }
#settings2 .dgroup .drow:not(:first-child) { border-top:1px solid var(--hairline, rgba(255,255,255,0.06)); }
#settings2 .drow .dt { font-size:14px; font-weight:500; color:var(--text,#e7e7ea); }
#settings2 .drow .dd { font-size:12.5px; color:var(--muted,#76767e); margin-top:5px; line-height:1.45; max-width:400px; }
#settings2 .drow .dctl { margin-left:auto; flex:none; display:flex; align-items:center; gap:8px; padding-top:1px; }
#settings2 .dpane { display:none; }
#settings2 .dpane.on { display:block; animation: s2fade .18s ease; }
#settings2 .grow { flex:1; min-width:0; }
#settings2 .mono { font-family: var(--s2-mono); }
@keyframes s2fade { from { opacity:0; transform:translateY(6px); } }

/* ── .ic цветные плитки (тактичные, не эмодзи) ───────────────────────── */
#settings2 .ic.gray{background:rgba(255,255,255,.08);color:var(--text-body,#d6d6db)}
#settings2 .ic.blue{background:rgba(108,160,255,.2);color:var(--working,#6ca0ff)}
#settings2 .ic.green{background:rgba(65,201,142,.2);color:var(--done,#41c98e)}
#settings2 .ic.amber{background:rgba(242,163,60,.2);color:var(--waiting,#f2a33c)}
#settings2 .ic.orange{background:rgba(255,138,76,.2);color:#ff8a4c}
#settings2 .ic.violet{background:rgba(139,126,200,.25);color:var(--limit,#8b7ec8)}
#settings2 .ic.teal{background:rgba(60,200,200,.2);color:#3cc8c8}
#settings2 .ic.purple{background:rgba(180,120,255,.2);color:#b478ff}

/* ── скелетоны: мерцающие плейсхолдеры, пока грузятся данные вкладки ──── */
#settings2 .skel{position:relative;overflow:hidden;background:rgba(255,255,255,.05);border-radius:6px}
#settings2 .skel::after{content:'';position:absolute;inset:0;transform:translateX(-100%);background:linear-gradient(90deg,transparent,rgba(255,255,255,.07),transparent);animation:s2shim 1.15s infinite}
@keyframes s2shim{100%{transform:translateX(100%)}}
#settings2 .skrow{display:flex;align-items:flex-start;gap:20px;padding:15px 16px}
#settings2 .dgroup .skrow:not(:first-child){border-top:1px solid var(--hairline,rgba(255,255,255,0.06))}
#settings2 .skgrow{flex:1;min-width:0;display:flex;flex-direction:column;gap:9px}
#settings2 .skbar{height:13px}
#settings2 .skctl{width:50px;height:22px;border-radius:11px;flex:none;margin-left:auto}

/* ── поле-секрет (API-ключ / токен подписки) ─────────────────────────── */
#settings2 .s2-secret{width:100%;max-width:340px;background:rgba(255,255,255,.04);border:1px solid var(--hairline,rgba(255,255,255,0.12));border-radius:8px;color:var(--text,#e7e7ea);font:12.5px/1.3 var(--s2-mono,ui-monospace,monospace);padding:9px 11px;outline:none;transition:border-color .12s ease}
#settings2 .s2-secret:focus{border-color:var(--working,#6ca0ff)}
#settings2 .s2-secret::placeholder{color:var(--faint,#55555c)}
#settings2 .loadcap.err{color:var(--waiting,#f2a33c)}

/* ── статус-точка ────────────────────────────────────────────────────── */
#settings2 .dot { width:7px; height:7px; border-radius:50%; flex:none; background:var(--idle,#55555c); }
#settings2 .dot.working { background:var(--working,#6ca0ff); animation: s2pulse 2.2s ease-in-out infinite; }
#settings2 .dot.waiting { background:var(--waiting,#f2a33c); }
#settings2 .dot.done { background:var(--done,#41c98e); }
@keyframes s2pulse { 0%,100%{opacity:1;transform:scale(1)} 50%{opacity:.4;transform:scale(.8)} }

/* ── значение справа (есть/нет/активна) ──────────────────────────────── */
#settings2 .sval { font-size:12.5px; color:var(--faint,#55555c); }
#settings2 .sval.on { color:var(--done,#41c98e); }

/* ── Toggle ──────────────────────────────────────────────────────────── */
#settings2 .toggle { appearance:none; -webkit-appearance:none; width:30px; height:18px; border-radius:9px; background:rgba(255,255,255,0.12); position:relative; outline:0; transition:background 120ms ease; flex:none; cursor:default; }
#settings2 .toggle:checked { background:rgba(108,160,255,0.85); }
#settings2 .toggle::after { content:""; position:absolute; top:2px; left:2px; width:14px; height:14px; border-radius:50%; background:#fff; box-shadow:0 1px 2px rgba(0,0,0,0.4); transition:left 120ms ease; }
#settings2 .toggle:checked::after { left:14px; }
#settings2 .toggle:disabled { opacity:.45; }

/* ── Segmented ───────────────────────────────────────────────────────── */
#settings2 .seg { display:flex; border:1px solid var(--border, rgba(255,255,255,0.08)); border-radius:6px; overflow:hidden; }
#settings2 .segbtn { appearance:none; border:0; background:transparent; color:var(--muted,#76767e); font:500 11px/1 var(--s2-font); padding:5px 11px; cursor:default; }
#settings2 .segbtn.active { background:rgba(255,255,255,0.1); color:var(--text,#e7e7ea); }

/* ── Button ──────────────────────────────────────────────────────────── */
#settings2 .btn { font:500 12.5px/1 var(--s2-font); color:var(--text,#e7e7ea); background:rgba(255,255,255,0.06); border:1px solid var(--border, rgba(255,255,255,0.08)); border-radius:7px; padding:8px 13px; cursor:default; display:inline-flex; align-items:center; gap:6px; }
#settings2 .btn:hover { background:rgba(255,255,255,0.09); }
#settings2 .btn:disabled { opacity:.55; }
#settings2 .btn.primary { background:rgba(108,160,255,0.9); color:#08101e; border:0; }
#settings2 .btn.danger { color:#ff8a4c; }
#settings2 .btn.danger svg.lucide { width:14px; height:14px; }
#settings2 .btn.sm { padding:5px 10px; font-size:11.5px; }

/* ── Progress (striped) ──────────────────────────────────────────────── */
#settings2 .progress { height:4px; border-radius:999px; background:rgba(255,255,255,0.07); overflow:hidden; }
#settings2 .progress > i { display:block; height:100%; border-radius:999px; background:var(--working,#6ca0ff); }
#settings2 .progress.striped > i { background-image:linear-gradient(90deg, var(--working,#6ca0ff), #9ec1ff, var(--working,#6ca0ff)); background-size:200% 100%; animation: s2stripe 1.2s linear infinite; }
@keyframes s2stripe { to { background-position:200% 0; } }

/* ── Хоткей-поле (Raycast-style) ─────────────────────────────────────── */
#settings2 .dctl.hk { gap:6px; }
#settings2 .hkey { display:inline-flex; align-items:center; gap:8px; padding:8px 13px; border-radius:8px; background:rgba(255,255,255,0.05); border:1px solid rgba(255,255,255,0.08); cursor:default; transition:background .15s ease; }
#settings2 .hkey:hover { background:rgba(255,255,255,0.08); }
#settings2 .hkey kbd { font:500 13px/1 var(--s2-font); color:var(--text,#e7e7ea); background:transparent; border:0; padding:0; }
#settings2 .hkey.rec { background:rgba(108,160,255,0.1); border-color:rgba(108,160,255,0.25); cursor:pointer; }
#settings2 .hkey.rec:hover { border-color:rgba(108,160,255,0.45); }
#settings2 .hkey.rec kbd { color:var(--working,#6ca0ff); }
#settings2 .hkey.rec.recording { background:rgba(108,160,255,0.18); border-color:var(--working,#6ca0ff); }
#settings2 .hkpresets { display:flex; gap:6px; flex-wrap:wrap; justify-content:flex-end; }
#settings2 .hkchip { font-size:11.5px; padding:3px 8px; border-radius:7px; background:rgba(255,255,255,0.05);
  border:1px solid rgba(255,255,255,0.1); color:var(--muted,#9a9aa2); cursor:pointer; }
#settings2 .hkchip:hover { background:rgba(108,160,255,0.12); border-color:rgba(108,160,255,0.3); color:var(--working,#6ca0ff); }
#settings2 .hkreset { width:32px; height:32px; border-radius:8px; display:grid; place-items:center; background:transparent; border:0; color:var(--faint,#55555c); cursor:default; }
#settings2 .hkreset:hover { color:var(--text-body,#d6d6db); background:rgba(255,255,255,0.06); }
#settings2 .hkreset svg.lucide { width:15px; height:15px; }

/* ── Custom Select ───────────────────────────────────────────────────── */
#settings2 .cselect { position:relative; display:inline-block; }
#settings2 .cstrigger { display:inline-flex; align-items:center; gap:8px; font:500 13px/1 var(--s2-font); color:var(--text,#e7e7ea); background:rgba(255,255,255,0.05); border:1px solid var(--border, rgba(255,255,255,0.08)); border-radius:7px; padding:8px 11px; cursor:default; }
#settings2 .cstrigger:hover { background:rgba(255,255,255,0.08); }
#settings2 .cstrigger .chev { color:var(--muted,#76767e); transition:transform .15s ease; display:inline-flex; }
#settings2 .cselect.open .cstrigger { border-color:rgba(108,160,255,0.5); background:rgba(108,160,255,0.08); }
#settings2 .cselect.open .cstrigger .chev { transform:rotate(180deg); }
#settings2 .cmenu { position:absolute; top:calc(100% + 5px); right:0; min-width:100%; z-index:60; background:#1c1d22; border:1px solid var(--border, rgba(255,255,255,0.08)); border-radius:9px; padding:4px; box-shadow:0 16px 44px rgba(0,0,0,0.55); display:none; }
#settings2 .cselect.open .cmenu { display:block; animation: s2fade .12s ease; }
#settings2 .copt { display:flex; align-items:center; gap:9px; padding:8px 10px; border-radius:6px; font-size:13px; color:var(--text-body,#d6d6db); cursor:default; white-space:nowrap; }
#settings2 .copt:hover { background:var(--working-soft); color:var(--text,#e7e7ea); }
#settings2 .copt .ck { margin-left:auto; color:var(--working,#6ca0ff); opacity:0; display:inline-flex; }
#settings2 .copt .ck svg.lucide { width:13px; height:13px; }
#settings2 .copt.selected { color:var(--text,#e7e7ea); }
#settings2 .copt.selected .ck { opacity:1; }
/* загрузка модели: спиннер в триггере вместо шеврона + подпись loadcap */
#settings2 .cselect .spin { display:none; }
#settings2 .cselect.busy .spin { display:inline-flex; }
#settings2 .cselect.busy .chev { display:none; }
#settings2 .spin svg.lucide { width:14px; height:14px; color:var(--working,#6ca0ff); animation: s2spin .8s linear infinite; }
@keyframes s2spin { to { transform:rotate(360deg); } }
#settings2 .loadcap { font-size:11px; color:var(--muted,#76767e); }

/* ── ошибка загрузки модели (инлайн, красная) ────────────────────────── */
#settings2 .s2err { display:flex; align-items:center; gap:6px; margin-top:6px; font-size:11.5px;
  color:var(--danger,#f2615c); max-width:340px; line-height:1.35; }
#settings2 .s2err .s2err-ic svg.lucide { width:13px; height:13px; }
#settings2 .s2err-txt { word-break:break-word; }

/* ── lucide общая геометрия ──────────────────────────────────────────── */
#settings2 svg.lucide { width:15px; height:15px; stroke-width:2; vertical-align:middle; flex:none; }
#settings2 .snav .item .ic svg.lucide { width:14px; height:14px; }
#settings2 .ssearch .si svg.lucide { width:15px; height:15px; }
#settings2 .dnav button svg.lucide { width:15px; height:15px; }
#settings2 .range { -webkit-appearance:none; appearance:none; height:4px; border-radius:999px; background:rgba(255,255,255,0.12); outline:0; width:140px; }
#settings2 .range::-webkit-slider-thumb { -webkit-appearance:none; width:14px; height:14px; border-radius:50%; background:var(--working,#6ca0ff); cursor:default; }

/* ── Превью уведомления (раздел «Уведомления») ───────────────────────── */
#settings2 .npvbox { display:flex; justify-content:center; padding:26px 20px 22px; border:1px solid var(--hairline, rgba(255,255,255,0.06)); border-radius:14px; background:rgba(255,255,255,0.015); margin-bottom:8px; position:relative; }
#settings2 .npvbox .tag { position:absolute; top:10px; left:14px; font:600 10px/1 var(--s2-font); letter-spacing:.06em; color:var(--faint,#55555c); text-transform:uppercase; }
#settings2 .npvcard { width:344px; padding:13px 16px 14px 18px; border-radius:20px; background:rgba(8,8,10,0.97); border:1px solid rgba(255,255,255,0.09); box-shadow:0 18px 50px rgba(0,0,0,0.5); }
#settings2 .npvcard .row { display:flex; align-items:center; gap:10px; }
#settings2 .npvdot { width:8px; height:8px; border-radius:50%; background:#41c98e; flex:none; }
#settings2 .npvtitle { font-size:14px; font-weight:600; color:#f0f0f2; flex:1; min-width:0; white-space:nowrap; overflow:hidden; text-overflow:ellipsis; }
#settings2 .npvx { width:26px; height:26px; border-radius:50%; flex:none; display:grid; place-items:center; color:#d9d9de; font-size:11px; border:2.5px solid rgba(255,255,255,0.16); }
#settings2 .npvmeta { margin:6px 16px 0 18px; font-size:11.5px; color:#8a8a93; display:flex; gap:7px; flex-wrap:wrap; align-items:center; }
#settings2 .npvmeta:empty { display:none; }
#settings2 .npvmeta .br { color:#7f8da3; font-family:"SF Mono", ui-monospace, Menlo, monospace; font-size:11px; }
#settings2 .npvmeta .md { color:#a9966f; }
#settings2 .npvmeta .ef { font:600 9.5px/1 "SF Mono", ui-monospace, Menlo, monospace; color:#9a9aa2; border:1px solid rgba(255,255,255,0.12); border-radius:5px; padding:3px 5px; }
#settings2 .npvmeta .sp { color:#46464d; }
#settings2 .npvbody { font-size:12.5px; line-height:1.45; color:#9a9aa2; margin:7px 16px 0 18px; }
`;
    const style = document.createElement('style');
    style.id = 'settings2-style';
    style.textContent = css;
    document.head.appendChild(style);
  }

  /* ========================================================================
   * Список вкладок сайдбара.
   * ====================================================================== */
  const NAV = [
    { pane: 'general', label: 'Основное', icon: 'settings', ic: 'gray' },
    { pane: 'stt', label: 'Голосовой ввод', icon: 'mic', ic: 'blue' },
    { pane: 'voice', label: 'Голос', icon: 'volume-2', ic: 'green' },
    { pane: 'wake', label: 'Пробуждение', icon: 'mic', ic: 'blue' },
    { pane: 'notify', label: 'Уведомления', icon: 'bell', ic: 'amber' },
    { pane: 'awake', label: 'Бодрость', icon: 'coffee', ic: 'orange' },
    { pane: 'keys', label: 'Горячие клавиши', icon: 'keyboard', ic: 'violet' },
    { sep: true },
    { pane: 'service', label: 'Под капотом', icon: 'cpu', ic: 'purple' },
    { pane: 'integration', label: 'Интеграция', icon: 'cable', ic: 'teal' },
    { pane: 'about', label: 'О программе', icon: 'info', ic: 'gray' },
  ];

  /* ========================================================================
   * ОТРИСОВКА ОТДЕЛЬНЫХ ПАНЕЛЕЙ. Каждая async, грузит из IPC, заполняет
   * переданный контейнер pane (его внутренность очищается заранее).
   * ====================================================================== */

  // 1. Основное (general) — settings_get
  async function renderGeneral(pane) {
    pane.appendChild(el('div.dtitle', { text: 'Основное' }));
    const _sk = skelGroup(4); pane.appendChild(_sk);
    const s = await safe(() => window.jarvis.getSettings(), {});
    _sk.remove();
    const group = el('div.dgroup');

    // глобальный хоткей (read-only капсы) + сброс
    group.appendChild(drow('Глобальный хоткей', 'Открыть панель Jarvis из любого места.',
      hotkeyField(s.hotkey || HK_DEFAULTS.hotkey, {
        onReset: () => { fire(() => window.jarvis.setSettings({ hotkey: HK_DEFAULTS.hotkey })); reRenderPane('general'); },
      }), { ctlClass: 'hk' }));

    // позиция панели (seg: Центр / Угол)
    group.appendChild(drow('Позиция панели', 'Где появляется панель на экране.',
      segmented([{ value: 'center', label: 'Центр' }, { value: 'corner', label: 'Угол' }],
        s.position || 'center',
        (v) => fire(() => window.jarvis.setSettings({ position: v })))));

    // автозапуск (перечитываем реальное состояние — macOS может отказать)
    group.appendChild(drow('Запускать при старте', 'Автозапуск при входе в систему.',
      toggle(s.openAtLogin, async (on) => {
        await safe(() => window.jarvis.setSettings({ openAtLogin: on }), null);
        reRenderPane('general'); // отразить то, что реально записалось в систему
      })));

    // режим логов / диагностика
    group.appendChild(drow('Режим логов',
      'Тайминги пайплайна, RAM/CPU и события (доставка ответов, уведомления, лимиты) → ~/.jarvis/metrics.jsonl и jarvis.log. ' +
      'Без конф. данных: текст промптов/ответов, тело уведомлений и транскрипты не пишутся — только типы событий, счётчики и усечённые id сессий. Файлы локальные, никуда не отправляются.',
      toggle(!!s.diagnostics, (on) => fire(() => window.jarvis.setSettings({ diagnostics: on })))));

    pane.appendChild(group);
  }

  // 2. Голосовой ввод (stt) — sttGet + modelsGet
  async function renderStt(pane) {
    pane.appendChild(el('div.dtitle', { text: 'Голосовой ввод' }));
    const _sk = skelGroup(3); pane.appendChild(_sk);
    const v = await safe(() => window.jarvis.sttGet(), null);
    _sk.remove();
    const group = el('div.dgroup');

    if (!v) {
      group.appendChild(drow('STT недоступен', 'Данные распознавания речи недоступны.', []));
      pane.appendChild(group);
      return;
    }

    // движок распознавания — кастомный селект с loadcap при переключении
    const engines = v.engines || ['whisper-turbo', 'qwen3-0.6b', 'qwen3-1.7b'];
    const cap = el('span.loadcap', { text: '' });
    cap.style.display = 'none';
    const sel = customSelect(
      engines.map((e) => ({ value: e, label: e })),
      v.engine,
      async (engine) => {
        sel.setBusy(true);
        cap.textContent = 'переключаю модель…';
        cap.style.display = '';
        const r = await safe(() => window.jarvis.sttSetEngine(engine), null);
        sel.setBusy(false);
        cap.style.display = 'none';
        // r.restart === true → нужна перезагрузка; в текущем коде stt_set_engine
        // делает горячую смену (restart:false), но ошибку (ok:false) показываем.
        reRenderPane('stt');
      });
    const engCtl = el('div.dctl', { style: 'flex-direction:column;align-items:flex-end;gap:6px' }, [sel.node, cap]);
    const engRow = el('div.drow', null, [
      el('div.grow', null, [
        el('div.dt', { text: 'Движок распознавания' }),
        el('div.dd', { text: 'Распознаёт речь локально, без облака. Старая модель отвечает, пока грузится новая.' }),
      ]),
      engCtl,
    ]);
    group.appendChild(engRow);

    // устройство ввода (микрофон) — селектор + горячее применение (без перезапуска)
    const dev = await safe(() => window.jarvis.sttInputDevices(), { devices: [], current: null });
    const devOpts = [{ value: '', label: 'Системный по умолчанию' }]
      .concat((dev.devices || []).map((n) => ({ value: n, label: n })));
    const devSel = customSelect(devOpts, dev.current || '', async (name) => {
      if (devSel.setBusy) devSel.setBusy(true);
      await safe(() => window.jarvis.sttSetInputDevice(name || null), null);
      if (devSel.setBusy) devSel.setBusy(false);
    });
    group.appendChild(drow('Микрофон',
      'С какого устройства писать речь. Выбери встроенный микрофон, если гарнитура шумит.',
      devSel.node));

    // клавиша диктовки (read-only капс)
    group.appendChild(drow('Клавиша диктовки', 'Зажми и говори (push-to-talk). Кликни и нажми новое сочетание.',
      dictationHotkeyField(v.hotkey || 'F8'), { ctlClass: 'hk' }));

    // тест микрофона
    group.appendChild(renderMicTestRow());
    pane.appendChild(group);

    // ── Модели на диске (порт renderModelManager + downloadActionFor) ──
    pane.appendChild(el('div.dsection', { text: 'Модели на диске' }));
    const mgroup = el('div.dgroup#s2-models-group');
    pane.appendChild(mgroup);
    await fillModelRows(mgroup);
  }

  // строка теста микрофона (sttTest → показать результат)
  function renderMicTestRow() {
    const result = el('span.dd', { text: '', style: 'margin-top:0;max-width:200px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap' });
    const btn = button('Проверить · 4 с', async (b) => {
      b.disabled = true; b.textContent = 'Запись…'; result.textContent = '';
      const res = await safe(() => window.jarvis.sttTest(), null);
      if (res && res.ok) result.textContent = res.text || '(пусто)';
      else result.textContent = (res && res.error) || 'ошибка';
      b.disabled = false; b.textContent = 'Проверить · 4 с';
    }, 'sm');
    return drow('Микрофон', 'Проверь захват с активного устройства.', [result, btn]);
  }

  // заполнить группу строк моделей (по группам kind), порт modelRow/downloadActionFor
  async function fillModelRows(group) {
    group.textContent = '';
    // инвентарь моделей грузится дольше всего — пока показываем скелетоны
    for (let i = 0; i < 4; i++) group.appendChild(skelRow());
    const r = await safe(() => window.jarvis.modelsGet(), { models: [] });
    group.textContent = '';
    const models = (r && r.models) || [];
    if (!models.length) {
      group.appendChild(drow('Нет моделей', 'Инвентарь моделей пуст.', []));
      return;
    }
    const GROUPS = [
      ['stt', 'Распознавание речи'],
      ['voice', 'Голос'],
      ['wake', 'Wake-word'],
      ['runtime', 'Окружение'],
    ];
    for (const [kind, glabel] of GROUPS) {
      const items = models.filter((m) => m.kind === kind);
      if (!items.length) continue;
      const downloadable = items.filter((m) => !m.present && downloadActionFor(m));
      const header = el('div.grow', null, [el('div.dd', { text: glabel, style: 'margin-top:0' })]);
      const headerCtl = el('div.dctl');
      // «Скачать выбранное» — только если в группе больше одной не-скачанной модели.
      if (downloadable.length > 1) headerCtl.appendChild(bulkDownloadBtn(downloadable.map((m) => m.id)));
      group.appendChild(el('div.drow', null, [header, headerCtl]));
      for (const m of items) group.appendChild(modelRow(m));
    }
  }

  // Кнопка «Скачать выбранное» для группы: качает отмеченные чекбоксами модели.
  function bulkDownloadBtn(idsInGroup) {
    const b = el('button.btn.sm', null, [iconSpan('download'), document.createTextNode('Скачать выбранное')]);
    b.addEventListener('click', async () => {
      const ids = idsInGroup.filter((id) => selectedModels.has(id));
      if (!ids.length) return;
      b.disabled = true; b.replaceChildren(document.createTextNode('Качаю…'));
      await safe(() => window.jarvis.modelsInstall(ids), null);
    });
    return b;
  }

  // выбор download-action по id (порт downloadActionFor из renderer.js)
  function downloadActionFor(m) {
    if (m.present) return null;
    switch (m.id) {
      case 'whisper-turbo': return { label: 'Скачать (~574 МБ)', run: () => window.jarvis.sttInstallWhisper() };
      case 'qwen3-0.6b': return { label: 'Скачать (~1 ГБ)', run: () => window.jarvis.sttInstallQwen('qwen3-0.6b') };
      case 'qwen3-1.7b': return { label: 'Скачать (~1 ГБ)', run: () => window.jarvis.sttInstallQwen('qwen3-1.7b') };
      case 'qwen3-runtime': return { label: 'Установить (~2.6 ГБ)', run: () => window.jarvis.sttInstallSidecar() };
      case 'hey_jarvis': return { label: 'Скачать', run: () => window.jarvis.wakeInstallModels() };
      case 'silero': return { label: 'Установить голос (~1 ГБ)', run: () => window.jarvis.voiceInstallSilero() };
      default: return null;
    }
  }
  // можно ли удалить (скачана и не активный STT-движок)
  function canDeleteModel(m) {
    if (!m.present) return false;
    if (m.kind === 'stt' && m.active) return false;
    return true;
  }

  // одна строка модели (статус-точка + имя + бейдж + контролы)
  function modelRow(m) {
    const dot = el('span.dot' + (m.present ? '.done' : ''), { style: 'margin-top:5px' });
    const grow = el('div.grow');
    const titleRow = el('div.dt');
    titleRow.appendChild(document.createTextNode(m.label));
    if (m.kind === 'stt' && m.active && m.present) {
      titleRow.appendChild(el('span.sval.on', { text: ' · активна', style: 'margin-left:8px;font-size:11.5px' }));
    }
    grow.appendChild(titleRow);
    // Статус: явный успех «✓ размер» (видно, что скачалось) либо «не скачана».
    grow.appendChild(el('div.dd', { text: m.present ? '✓ установлена · ' + fmtBytes(m.bytes) : 'не скачана' }));
    // Ошибка прошлой попытки — прямо в строке (вместо тихого сброса), с подсказкой про retry.
    if (dlState[m.id] && dlState[m.id].error) grow.appendChild(dlErrorNote(dlState[m.id].error));

    const action = downloadActionFor(m);
    if (action) {
      // не скачана: чекбокс (мультивыбор) + кнопка «Скачать»/«Повторить» + место прогресса
      const wrap = el('div.dctl', { style: 'flex-direction:column;align-items:flex-end;gap:6px' });
      const retry = !!(dlState[m.id] && dlState[m.id].error);
      const label = retry ? 'Повторить' : action.label;
      const btn = el('button.btn.sm', null, [iconSpan(retry ? 'rotate-ccw' : 'download'), document.createTextNode(label)]);
      btn.addEventListener('click', async () => {
        delete dlState[m.id];             // сбросить прежнюю ошибку
        btn.disabled = true; btn.replaceChildren(document.createTextNode('Качаю…'));
        // единый путь: оркестратор шлёт прогресс/финал по id модели
        await safe(() => window.jarvis.modelsInstall([m.id]), null);
      });
      const cb = el('input', { type: 'checkbox', style: 'margin-right:6px;vertical-align:middle' });
      cb.checked = selectedModels.has(m.id);
      cb.addEventListener('change', () => {
        if (cb.checked) selectedModels.add(m.id); else selectedModels.delete(m.id);
      });
      const btnRow = el('div', { style: 'display:flex;align-items:center' }, [cb, btn]);
      wrap.appendChild(btnRow);
      wrap.appendChild(el('div', { 'data-model': m.id })); // плейсхолдер прогресса
      return el('div.drow', null, [dot, grow, wrap]);
    }

    const ctl = el('div.dctl');
    if (m.kind === 'stt' && !m.active) {
      ctl.appendChild(button('Сделать активной', async (b) => {
        b.disabled = true; b.textContent = 'Включаю…';
        const res = await safe(() => window.jarvis.sttSetEngine(m.id), null);
        if (res && res.ok === false) { b.disabled = false; b.textContent = 'Сделать активной'; return; }
        reRenderPane('stt');
      }, 'sm'));
    }
    if (canDeleteModel(m)) {
      ctl.appendChild(makeDeleteButton(m.id, () => reRenderPane('stt')));
    }
    if (!ctl.childNodes.length) ctl.appendChild(el('span.sval' + (m.present ? '.on' : ''), { text: m.present ? 'на месте' : '—' }));
    return el('div.drow', null, [dot, grow, ctl]);
  }

  // 3. Голос (voice) — voiceGet
  async function renderVoice(pane) {
    pane.appendChild(el('div.dtitle', { text: 'Голос' }));
    const _sk = skelGroup(3); pane.appendChild(_sk);
    const v = await safe(() => window.jarvis.voiceGet(), null);
    _sk.remove();
    const group = el('div.dgroup');
    if (!v) {
      group.appendChild(drow('Голос недоступен', 'Движок синтеза недоступен.', []));
      pane.appendChild(group);
      return;
    }

    const SPEAKER_LABELS = { aidar: 'Айдар', baya: 'Байя', kseniya: 'Ксения', xenia: 'Ксения (xenia)', eugene: 'Евгений' };
    const speakers = (v.speakers || []).map((s) => ({ value: s, label: SPEAKER_LABELS[s] || s }));
    // диктор — кастомный селект (набор спикеров есть у silero)
    if (speakers.length) {
      const sel = customSelect(speakers, v.speaker, async (sp) => {
        await safe(() => window.jarvis.voiceSetSpeaker(sp), null);
      });
      group.appendChild(drow('Диктор', 'Голос синтеза · движок ' + (v.engine || 'Silero') + ', локально.', sel.node));
    }

    // скорость — segmented
    const RATE_LABELS = { slow: 'медленно', medium: 'норма', fast: 'быстро', 'x-fast': 'очень' };
    const rates = (v.rates || ['slow', 'medium', 'fast', 'x-fast']).map((r) => ({ value: r, label: RATE_LABELS[r] || r }));
    group.appendChild(drow('Скорость', 'Темп речи.',
      segmented(rates, v.rate, (r) => fire(() => window.jarvis.voiceSetRate(r)))));

    // тест
    group.appendChild(drow('Проверить голос', 'Сказать короткий образец вслух.',
      button('Тест', () => fire(() => window.jarvis.voiceTest()), 'sm')));

    // без звука
    group.appendChild(drow('Без звука', 'Временно заглушить озвучку.',
      toggle(v.mute, (on) => fire(() => window.jarvis.voiceSetMute(on)))));

    // пауза чужого звука (duck)
    group.appendChild(drow('Пауза чужого звука', 'Приглушать музыку/видео на время реплики, как Siri.',
      toggle(v.duck !== false, (on) => fire(() => window.jarvis.voiceSetDuck(on)))));

    // озвучка только при Bluetooth-гарнитуре
    group.appendChild(drow('Только через Bluetooth', 'Озвучивать, лишь когда подключена Bluetooth-гарнитура.',
      toggle(v.bluetoothOnly !== false, (on) => fire(() => window.jarvis.voiceSetBluetoothOnly(on)))));

    pane.appendChild(group);
  }

  // 4. Пробуждение (wake) — wakeGet
  async function renderWake(pane) {
    pane.appendChild(el('div.dtitle', { text: 'Пробуждение' }));
    const _sk = skelGroup(4); pane.appendChild(_sk);
    const v = await safe(() => window.jarvis.wakeGet(), null);
    _sk.remove();
    const group = el('div.dgroup');
    if (!v) {
      group.appendChild(drow('Wake-word недоступен', 'Данные активации по фразе недоступны.', []));
      pane.appendChild(group);
      return;
    }

    // вкл/выкл активацию по фразе
    group.appendChild(drow('Активация по фразе',
      v.model_present
        ? 'Скажи «Hey Jarvis», чтобы разбудить ассистента. Работает офлайн.'
        : 'Сначала скачайте модель openWakeWord ниже, чтобы включить.',
      toggle(!!v.enabled, async (on) => { await safe(() => window.jarvis.wakeSetEnabled(on), null); reRenderPane('wake'); }, !v.model_present)));

    // заглушить микрофон (mute у источника)
    group.appendChild(drow('Заглушить микрофон', 'Полностью отключить микрофон у источника.',
      toggle(!!v.muted, async (on) => { await safe(() => window.jarvis.audioSetMute(on), null); reRenderPane('wake'); })));

    // порог срабатывания — слайдер (input → подпись; change → IPC)
    const thVal = el('span.dd', { text: Number(v.threshold != null ? v.threshold : 0.5).toFixed(2), style: 'margin-top:0;margin-right:8px;font-family:var(--s2-mono)' });
    const range = el('input.range', { type: 'range', min: '0', max: '1', step: '0.05' });
    range.value = String(v.threshold != null ? v.threshold : 0.5);
    range.addEventListener('input', () => { thVal.textContent = Number(range.value).toFixed(2); });
    range.addEventListener('change', () => fire(() => window.jarvis.wakeSetThreshold(Number(range.value))));
    group.appendChild(drow('Порог срабатывания', 'Чувствительность детектора фразы.', [thVal, range]));

    // модели openWakeWord
    if (v.model_present) {
      group.appendChild(drow('Модели openWakeWord', 'ONNX-модели «Hey Jarvis» на месте.',
        el('span.sval.on', { text: 'на месте' }), { dot: 'done' }));
    } else {
      const werr = dlState['hey_jarvis'] && dlState['hey_jarvis'].error;
      const wctl = el('div.dctl', { style: 'flex-direction:column;align-items:flex-end;gap:6px' });
      wctl.appendChild(button(werr ? 'Повторить' : 'Скачать (~3.5 МБ)', async (b) => {
        activeDownload = 'hey_jarvis'; delete dlState['hey_jarvis'];
        b.disabled = true; b.textContent = 'Скачиваю…';
        await safe(() => window.jarvis.wakeInstallModels(), null);
      }, 'sm'));
      if (werr) wctl.appendChild(dlErrorNote(werr));
      group.appendChild(drow('Модели openWakeWord', 'Нужно скачать модели (~3.5 МБ), чтобы детектор заработал.',
        wctl, { dot: '' }));
    }

    pane.appendChild(group);
  }

  // 5. Уведомления (notify) — settings_get
  async function renderNotify(pane) {
    pane.appendChild(el('div.dtitle', { text: 'Уведомления' }));
    const _sk = skelGroup(3); pane.appendChild(_sk);
    const s = await safe(() => window.jarvis.getSettings(), {});
    _sk.remove();
    // notify-блок шлём целиком при изменении (бэкенд мержит верхний уровень —
    // полный объект, чтобы не затереть соседние поля).
    const nf = Object.assign({ content: {}, ttlSec: 8 }, s.notify || {});
    const content = Object.assign({ branch: true, model: false, effort: false, time: false }, nf.content || {});

    // ── живое превью карточки (та же вёрстка, что у реального тоста) ──
    const SAMPLE = { br: '⎇ feat/voice-settings', md: 'Opus 4.8', ef: 'low', time: '14:32' };
    const pvMeta = el('div.npvmeta');
    const pvCard = el('div.npvcard', null, [
      el('div.row', null, [
        el('span.npvdot'),
        el('span.npvtitle', { text: 'checkout-flow' }),
        el('span.npvx', { text: '✕' }),
      ]),
      pvMeta,
      el('div.npvbody', { text: 'Готово · изменено 3 файла, тесты прошли.' }),
    ]);
    const renderPreview = () => {
      pvMeta.textContent = '';
      const segs = [];
      if (content.branch) segs.push(['br', SAMPLE.br]);
      if (content.model) segs.push(['md', SAMPLE.md]);
      if (content.effort) segs.push(['ef', SAMPLE.ef]);
      if (content.time) segs.push(['', SAMPLE.time]);
      segs.forEach(([cls, txt], i) => {
        if (i > 0) pvMeta.appendChild(el('span.sp', { text: '·' }));
        pvMeta.appendChild(el('span' + (cls ? '.' + cls : ''), { text: txt }));
      });
    };
    pane.appendChild(el('div.npvbox', null, [el('span.tag', { text: 'превью' }), pvCard]));

    const saveContent = (k, on) => {
      content[k] = on;
      renderPreview();
      fire(() => window.jarvis.setSettings({ notify: Object.assign({}, nf, { content: Object.assign({}, content) }) }));
    };
    renderPreview();

    pane.appendChild(el('div.dsection', { text: 'Содержимое карточки' }));
    const cg = el('div.dgroup');
    cg.appendChild(drow('Текущая ветка', '⎇ рядом с проектом — удобно прыгать между задачами',
      toggle(content.branch, (on) => saveContent('branch', on))));
    cg.appendChild(drow('Модель', 'напр. Opus 4.8 — если несколько агентов',
      toggle(content.model, (on) => saveContent('model', on))));
    cg.appendChild(drow('Уровень усилия', 'reasoning effort: low / high / max',
      toggle(content.effort, (on) => saveContent('effort', on))));
    cg.appendChild(drow('Время', 'когда пришло уведомление',
      toggle(content.time, (on) => saveContent('time', on))));
    pane.appendChild(cg);

    pane.appendChild(el('div.dsection', { text: 'Уведомлять о' }));
    const eg = el('div.dgroup');
    eg.appendChild(drow('Когда агент закончил', 'Уведомлять о завершении ответа.',
      toggle(s.notifyDone, (on) => fire(() => window.jarvis.setSettings({ notifyDone: on })))));
    eg.appendChild(drow('Когда ждёт тебя', 'Уведомлять, когда агенту нужен ответ.',
      toggle(s.notifyWaiting, (on) => fire(() => window.jarvis.setSettings({ notifyWaiting: on })))));
    eg.appendChild(drow('Продолжать после лимита', 'Авто-«продолжай» при сбросе лимита.',
      toggle(s.autoResume !== false, (on) => fire(() => window.jarvis.setSettings({ autoResume: on })))));
    pane.appendChild(eg);

    pane.appendChild(el('div.dsection', { text: 'Вид и поведение' }));
    const vg = el('div.dgroup');
    vg.appendChild(drow('Позиция', 'где появляются карточки',
      segmented([{ value: 'center', label: 'Центр' }, { value: 'corner', label: 'Угол' }],
        s.position || 'center', (v) => fire(() => window.jarvis.setSettings({ position: v })))));
    const ttlNow = (typeof nf.ttlSec === 'number') ? nf.ttlSec : 8;
    vg.appendChild(drow('Автоскрытие', 'через сколько прятать карточку (после озвучки, если она есть)',
      segmented([{ value: 5, label: '5с' }, { value: 8, label: '8с' }, { value: 0, label: 'Не прятать' }],
        ttlNow, (v) => fire(() => window.jarvis.setSettings({ notify: Object.assign({}, nf, { ttlSec: Number(v) }) })))));
    pane.appendChild(vg);
  }

  // 6. Бодрость (awake) — keep-awake через плагины (getPlugins / pluginCmd)
  async function renderAwake(pane) {
    pane.appendChild(el('div.dtitle', { text: 'Бодрость' }));
    const _sk = skelGroup(3); pane.appendChild(_sk);
    const plugins = await safe(() => window.jarvis.getPlugins(), []);
    _sk.remove();
    const byId = (id) => (Array.isArray(plugins) ? plugins.find((p) => p && p.id === id) : null);
    const ka = byId('keep-awake');
    const st = (ka && ka.status) || {};
    const group = el('div.dgroup');

    if (!ka) {
      // keep-awake не в IPC настроек — честный read-only fallback
      group.appendChild(drow('Не спать', 'Плагин keep-awake недоступен в этой сборке.',
        el('span.sval', { text: 'нет данных' })));
      pane.appendChild(group);
      return;
    }

    // активна ли блокировка сна + какой сегмент подсвечен (порт awakeState)
    let seg = 'off';
    const manual = st.manual || {};
    if (st.active) {
      if (manual.kind === 'manual') seg = 'inf';
      else if (manual.kind === 'timer') {
        const leftMin = Math.round(((manual.until || 0) - Date.now()) / 60000);
        seg = leftMin > 120 ? 'inf' : leftMin > 30 ? '4h' : leftMin > 7 ? '1h' : '15m';
      }
    }
    const SEG = [
      { value: 'off', label: 'Выкл' },
      { value: '15m', label: '15м' },
      { value: '1h', label: '1ч' },
      { value: '4h', label: '4ч' },
      { value: 'inf', label: '∞' },
    ];
    const runSeg = (id) => {
      const map = {
        off: () => window.jarvis.pluginCmd('keep-awake', 'stop'),
        '15m': () => window.jarvis.pluginCmd('keep-awake', 'start-timer', { minutes: 15 }),
        '1h': () => window.jarvis.pluginCmd('keep-awake', 'start-timer', { minutes: 60 }),
        '4h': () => window.jarvis.pluginCmd('keep-awake', 'start-timer', { minutes: 240 }),
        inf: () => window.jarvis.pluginCmd('keep-awake', 'start-manual'),
      };
      fire(map[id]);
      setTimeout(() => reRenderPane('awake'), 300);
    };
    group.appendChild(drow('Не спать', 'Не давать маку засыпать, пока работают агенты.',
      segmented(SEG, seg, runSeg)));

    // держать, пока работают агенты + не гасить экран
    group.appendChild(drow('Держать, пока работают агенты', 'Авто-включение при активных сессиях.',
      toggle(!!st.autoEnabled, (on) => fire(() => window.jarvis.pluginCmd('keep-awake', 'set', { auto: on })))));
    group.appendChild(drow('Не гасить заодно и экран', 'Дисплей тоже остаётся активным.',
      toggle(!!st.keepDisplayOn, (on) => fire(() => window.jarvis.pluginCmd('keep-awake', 'set', { keepDisplayOn: on })))));

    // крышка (clamshell) — Спать / Не спать
    const cs = byId('clamshell');
    if (cs) {
      const armed = !!(cs.status && cs.status.armed);
      group.appendChild(drow('Работать с закрытой крышкой', 'Не засыпать при закрытии крышки · требует питания от сети.',
        segmented([{ value: 'sleep', label: 'Спать' }, { value: 'keep', label: 'Не спать' }],
          armed ? 'keep' : 'sleep',
          async (val) => {
            if (val === 'keep') {
              if (cs && cs.enabled === false) await safe(() => window.jarvis.pluginCmd('clamshell', '_enable', { on: true }), null);
              fire(() => window.jarvis.pluginCmd('clamshell', 'arm'));
            } else {
              fire(() => window.jarvis.pluginCmd('clamshell', 'disarm'));
            }
            setTimeout(() => reRenderPane('awake'), 300);
          })));
    }

    pane.appendChild(group);
  }

  // 7. Горячие клавиши (keys) — settings_get
  async function renderKeys(pane) {
    pane.appendChild(el('div.dtitle', { text: 'Горячие клавиши' }));
    const _sk = skelGroup(4); pane.appendChild(_sk);
    const s = await safe(() => window.jarvis.getSettings(), {});
    _sk.remove();
    const group = el('div.dgroup');
    const HKS = [
      ['hotkey', 'Открыть панель', 'Показать или скрыть Jarvis.'],
      ['continueHotkey', 'Продолжить сессию', 'Возобновить последнюю сессию.'],
      ['repeatHotkey', 'Повторить', 'Повторить последнее действие.'],
      ['muteHotkey', 'Без звука', 'Заглушить уведомления и голос.'],
      ['quietHotkey', 'Тихий режим', 'Копить статистику без тостов · ⌘⌥J.'],
    ];
    for (const [key, title, desc] of HKS) {
      const acc = s[key] || HK_DEFAULTS[key] || '';
      group.appendChild(drow(title, desc,
        hotkeyField(acc, {
          onReset: () => { fire(() => window.jarvis.setSettings({ [key]: HK_DEFAULTS[key] })); reRenderPane('keys'); },
        }), { ctlClass: 'hk' }));
    }
    pane.appendChild(group);
  }

  // 8. Интеграция (integration) — integrationGet
  async function renderIntegration(pane) {
    pane.appendChild(el('div.dtitle', { text: 'Интеграция' }));
    const _sk = skelGroup(3); pane.appendChild(_sk);
    const info = await safe(() => window.jarvis.integrationGet(), null);
    _sk.remove();
    if (!info) {
      pane.appendChild(el('div.dgroup', null, [drow('Данные недоступны', 'Не удалось получить статус интеграции.', [])]));
      return;
    }
    const st = info.status || {};
    const integrated = st.hooks && st.shim;

    pane.appendChild(el('div.dsection', { text: 'Claude Code · ' + (integrated ? 'подключено' : 'не подключено') }));
    const statusGroup = el('div.dgroup');
    const rows = [
      ['Хуки событий', 'Уведомляют Jarvis о действиях агента.', st.hooks],
      ['Шим запуска claude', 'Обёртка команды claude для перехвата.', st.shim],
      ['tmux-транспорт', 'Канал событий через tmux.', st.tmux_conf],
      ['PATH-блок в shell', 'Добавляет команды Jarvis в PATH.', st.path_block],
    ];
    for (const [label, desc, ok] of rows) {
      statusGroup.appendChild(drow(label, desc,
        el('span.sval' + (ok ? '.on' : ''), { text: ok ? 'есть' : '—' }), { dot: ok ? 'done' : '' }));
    }
    pane.appendChild(statusGroup);

    if (info.foreign_hooks > 0) {
      pane.appendChild(el('div.dd', { text: 'При удалении сохранятся ' + info.foreign_hooks + ' чужих хук(ов) — трогаем только свои.', style: 'margin:-12px 2px 18px' }));
    }

    // разработчик: тихий режим
    pane.appendChild(el('div.dsection', { text: 'Разработчик' }));
    const devGroup = el('div.dgroup');
    devGroup.appendChild(drow('Тихий режим', 'Копить статистику без тостов, голоса и показа панели · ⌘⌥J.',
      toggle(!!info.quiet, (on) => fire(() => window.jarvis.quietSet(on)))));
    pane.appendChild(devGroup);

    // управление и диск
    pane.appendChild(el('div.dsection', { text: 'Управление и диск' }));
    const manGroup = el('div.dgroup');
    manGroup.appendChild(drow('Переустановить интеграцию', 'Обновить хуки, шим и транспорт.',
      button(integrated ? 'Переустановить' : 'Настроить', () => fire(() => window.jarvis.onboardingOpen()), 'sm')));
    if (integrated) {
      const rm = el('button.btn.sm.danger', { text: 'Удалить' });
      let armed = false;
      rm.addEventListener('click', async () => {
        if (!armed) { armed = true; rm.textContent = 'Точно удалить?'; setTimeout(() => { armed = false; rm.textContent = 'Удалить'; }, 3000); return; }
        rm.disabled = true; rm.textContent = 'Удаляю…';
        await safe(() => window.jarvis.integrationRemove(), null);
        reRenderPane('integration');
      });
      manGroup.appendChild(drow('Удалить интеграцию', 'Отключить Jarvis от Claude Code (чужие хуки сохранятся).', rm));
    }
    // модели голоса/диктовки на диске (из info.models — Artifact[]: {id,label,bytes})
    for (const m of (info.models || [])) {
      manGroup.appendChild(drow(m.label || m.id, fmtBytes(m.bytes) + ' на диске.',
        makeDeleteButton(m.id, () => reRenderPane('integration'))));
    }
    pane.appendChild(manGroup);
  }

  // 9. О программе (about) — getMeta
  async function renderAbout(pane) {
    pane.appendChild(el('div.dtitle', { text: 'О программе' }));
    const _sk = skelGroup(2); pane.appendChild(_sk);
    const meta = await safe(() => window.jarvis.getMeta(), {});
    _sk.remove();
    const group = el('div.dgroup');
    const ver = (meta && meta.version) ? ('v' + meta.version) : 'локально';
    group.appendChild(drow('Версия', 'Jarvis · локальный ассистент.',
      el('span.sval', { text: ver })));

    // Обновления: ручная проверка + установка (авто-проверка и так на старте).
    const status = el('div.dd', { text: 'Обновляется автоматически при запуске.', style: 'margin-top:0' });
    const ctl = el('div.dctl');
    const checkBtn = button('Проверить', async () => {
      checkBtn.disabled = true;
      status.textContent = 'Проверяю…';
      const r = await safe(() => window.jarvis.updateCheckInstall(), { ok: false, error: 'нет связи с апдейтером' });
      if (r && r.ok && r.updated) {
        status.textContent = 'Установлена v' + (r.version || '') + ' — перезапустите.';
        ctl.textContent = '';
        ctl.appendChild(button('Перезапустить', () => window.jarvis.relaunch(), 'primary'));
      } else if (r && r.ok) {
        status.textContent = 'У вас последняя версия.';
        checkBtn.disabled = false;
      } else {
        status.textContent = 'Ошибка: ' + ((r && r.error) || 'не удалось проверить');
        checkBtn.disabled = false;
      }
    }, 'sm');
    ctl.appendChild(checkBtn);
    group.appendChild(el('div.drow', null, [
      el('div.grow', null, [el('div.dt', { text: 'Обновления' }), status]),
      ctl,
    ]));

    const levels = (meta && Array.isArray(meta.effortLevels)) ? meta.effortLevels.join(' · ') : '';
    if (levels) {
      group.appendChild(drow('Уровни усилия', 'Доступные режимы reasoning effort.',
        el('span.dd', { text: levels, style: 'margin-top:0' })));
    }
    group.appendChild(drow('Лицензии', 'Открытые компоненты и модели (MIT-код; веса — отдельные лицензии).',
      el('span.sval', { text: 'офлайн' })));
    pane.appendChild(group);
  }

  // карта pane → рендерер
  /* Под капотом (service) — serviceGet: бэкенд служебного LLM + модель Codex.
   * Служебный LLM = саммари чатов, заголовки, диктовка, голос-план (НЕ сами
   * сессии агента). Бэкенд: Авто (claude→codex) / Claude / Codex. */
  /* Аккаунт Claude — подключить ПОДПИСКУ (claude setup-token → CLAUDE_CODE_OAUTH_TOKEN)
   * или API-ключ (sk-ant-api…). Подключённая учётка впрыскивается в служебные вызовы
   * claude. Дизайн — в общей системе настроек: сегмент-переключатель режима,
   * контекстная подсказка, поле-пароль с валидацией, статус подключения. */
  async function renderClaudeAccount(pane) {
    pane.appendChild(el('div.dsection', { text: 'Аккаунт Claude' }));
    const wrap = el('div.dgroup');
    wrap.appendChild(skelRow());
    pane.appendChild(wrap);
    const a = await safe(() => window.jarvis.claudeAuthGet(), null);
    wrap.textContent = '';
    if (!a) {
      wrap.appendChild(drow('Недоступно', 'Не удалось получить статус аккаунта.', []));
      return;
    }

    if (a.connected) {
      const label = a.mode === 'subscription' ? 'Подписка Claude (Pro/Max)' : 'API-ключ Anthropic';
      const sub = (a.hint ? a.hint + ' · ' : '') + 'служебные вызовы Claude идут через этот аккаунт';
      wrap.appendChild(drow(label, sub, el('span.sval.on', { text: 'подключён' })));
      wrap.appendChild(drow('Управление', 'Отключить и вернуться к собственному логину claude.',
        button('Отключить', async (b) => {
          b.disabled = true; b.textContent = 'Отключаю…';
          await safe(() => window.jarvis.claudeAuthDisconnect(), null);
          reRenderPane('service');
        }, 'sm danger')));
      return;
    }

    // не подключён → поток подключения
    let mode = 'key';
    wrap.appendChild(drow('Подключить аккаунт',
      'Чтобы служебный LLM работал на твоём аккаунте Anthropic — даже без логина в claude CLI.',
      segmented([
        { value: 'key', label: 'API-ключ' },
        { value: 'subscription', label: 'Подписка' },
      ], mode, (m) => { mode = m; renderHint(); })));

    const hintBox = el('div.dd', { style: 'padding:2px 16px 10px;line-height:1.5;max-width:none' });
    function renderHint() {
      hintBox.textContent = mode === 'key'
        ? 'Создай ключ: platform.claude.com → Settings → API keys → Create key. Выглядит как sk-ant-api… Оплата — из предоплаченных кредитов (от $5).'
        : 'Подписка Pro/Max: в терминале выполни  claude setup-token , авторизуйся в браузере и вставь напечатанный токен. Это твой ЛИЧНЫЙ аккаунт (не для общего/хостинга).';
    }
    renderHint();
    wrap.appendChild(hintBox);

    const input = el('input.s2-secret', {
      type: 'password', placeholder: 'sk-ant-… или токен подписки',
      autocomplete: 'off', spellcheck: 'false',
    });
    const cap = el('span.loadcap', { style: 'display:none' });
    const connect = button('Подключить', async (b) => {
      const val = (input.value || '').trim();
      if (!val) { input.focus(); return; }
      b.disabled = true; b.textContent = 'Проверяю…';
      cap.classList.remove('err'); cap.style.display = '';
      cap.textContent = 'проверяю крошечным запросом…';
      const r = await safe(() => window.jarvis.claudeAuthConnect(mode, val), null);
      if (r && r.ok) { reRenderPane('service'); return; }
      b.disabled = false; b.textContent = 'Подключить';
      cap.classList.add('err');
      cap.textContent = (r && r.error) ? r.error : 'не сработало';
    }, 'sm primary');
    input.addEventListener('keydown', (e) => { if (e.key === 'Enter') connect.click(); });
    wrap.appendChild(el('div.drow', null, [
      el('div.grow', null, [input, cap]),
      el('div.dctl', null, [connect]),
    ]));
  }

  async function renderService(pane) {
    pane.appendChild(el('div.dtitle', { text: 'Под капотом' }));
    const _sk = skelGroup(3); pane.appendChild(_sk);
    const v = await safe(() => window.jarvis.serviceGet(), null);
    _sk.remove();
    const group = el('div.dgroup');
    if (!v) {
      group.appendChild(drow('Недоступно', 'Не удалось получить настройки служебного LLM.', []));
      pane.appendChild(group);
      return;
    }

    // 1. Бэкенд: Авто / Claude / Codex
    const backends = [
      { value: 'auto', label: 'Авто' },
      { value: 'claude', label: 'Claude' },
      { value: 'codex', label: 'Codex' },
    ];
    group.appendChild(drow(
      'Бэкенд служебного LLM',
      'Что Jarvis использует под капотом для саммари чатов, заголовков, диктовки и голос-плана. ' +
        'Авто: Claude (haiku) → Codex. Фолбэк всегда включён, чтобы саммари не пропадали.',
      segmented(backends, v.backend || 'auto', async (b) => {
        await safe(() => window.jarvis.serviceSetBackend(b), null);
        reRenderPane('service');
      }),
    ));

    // 2. Что доступно сейчас
    const av = [
      v.claudeBin ? 'claude ✓' : 'claude ✗',
      v.codexBin ? 'codex ✓' : 'codex ✗',
      v.codexSidecar ? 'Codex-SDK ✓' : 'Codex-SDK ✗',
    ].join('  ·  ');
    group.appendChild(drow('Доступно', av, []));

    // Кнопка «Протестировать» — короткий запрос через ВЫБРАННЫЙ бэкенд: покажет,
    // какая модель ответила (прямой ответ, без преамбул) + за сколько.
    const testOut = el('div');
    testOut.style.cssText = 'font-size:12.5px;margin:0 16px 13px;font-variant-numeric:tabular-nums;color:var(--muted)';
    const testBtn = el('button.btn.sm', { text: 'Протестировать' });
    testBtn.addEventListener('click', async () => {
      testBtn.disabled = true;
      testBtn.textContent = 'Тестирую…';
      testOut.style.color = 'var(--muted)';
      testOut.textContent = 'жду ответ модели…';
      const r = await safe(() => window.jarvis.serviceTest(), null);
      testBtn.disabled = false;
      testBtn.textContent = 'Протестировать';
      if (r && r.ok) {
        testOut.style.color = 'var(--done, #41c98e)';
        testOut.textContent = '✓ ' + (r.result || '') + (r.ms ? `   ·   ${(r.ms / 1000).toFixed(1)} с` : '');
      } else {
        testOut.style.color = '#f26363';
        testOut.textContent = '✗ ' + ((r && r.error) || 'не ответил');
      }
    });
    group.appendChild(drow('Проверить ответ', 'Шлёт короткий запрос через выбранный бэкенд — покажет, какая модель ответила.', testBtn));
    group.appendChild(testOut);

    pane.appendChild(group);

    // 2b. Сеть: egress-прокси служебных вызовов. Ключевая причина, по которой Codex
    // молча уходил в таймаут — он ходит к OpenAI по HTTPS, а в окружении был только
    // HTTP_PROXY. Здесь можно задать прокси отдельно (применяется к Claude и Codex).
    pane.appendChild(el('div.dsection', { text: 'Сеть' }));
    const ng = el('div.dgroup');
    const proxyInput = el('input.s2-secret', {
      type: 'text', placeholder: 'http://user:pass@host:port  (пусто — из окружения)',
      autocomplete: 'off', spellcheck: 'false', value: v.proxy || '',
    });
    const proxyCap = el('span.loadcap', { style: 'display:none' });
    const proxySave = button('Сохранить', async (b) => {
      const val = (proxyInput.value || '').trim();
      b.disabled = true; b.textContent = 'Сохраняю…';
      proxyCap.classList.remove('err'); proxyCap.style.display = '';
      proxyCap.textContent = 'сохраняю…';
      const r = await safe(() => window.jarvis.serviceSetProxy(val), null);
      b.disabled = false; b.textContent = 'Сохранить';
      if (r && r.ok) {
        proxyCap.classList.remove('err');
        proxyCap.textContent = val ? 'прокси сохранён ✓' : 'очищен — снова из окружения';
      } else {
        proxyCap.classList.add('err');
        proxyCap.textContent = (r && r.error) ? r.error : 'не сохранилось';
      }
    }, 'sm primary');
    proxyInput.addEventListener('keydown', (e) => { if (e.key === 'Enter') proxySave.click(); });
    ng.appendChild(el('div.drow', null, [
      el('div.grow', null, [
        el('div.dt', { text: 'Egress-прокси' }),
        el('div.dd', {
          text: 'Codex общается с OpenAI по HTTPS — на прокси-сети без HTTPS_PROXY запрос висит до таймаута. '
            + 'Задай прокси здесь, и он применится и к Claude, и к Codex. Пусто → берётся из окружения процесса.',
        }),
        proxyInput, proxyCap,
      ]),
      el('div.dctl', null, [proxySave]),
    ]));
    pane.appendChild(ng);

    // 3. Аккаунт Claude — подписка (claude setup-token) или API-ключ
    await renderClaudeAccount(pane);

    // 4. Codex (Python SDK): модель + effort + установка сайдкара
    pane.appendChild(el('div.dsection', { text: 'Codex (Python SDK)' }));
    const cg = el('div.dgroup');

    // codexModels приходит уже как [{value,label}] из реального кэша моделей codex
    const models = (v.codexModels && v.codexModels.length)
      ? v.codexModels
      : [{ value: '', label: 'По умолчанию' }];
    const msel = customSelect(models, v.codexModel || '', async (m) => {
      await safe(() => window.jarvis.serviceSetModel(m), null);
    });
    cg.appendChild(drow('Модель Codex',
      'Для служебных вызовов через Codex. Список — из codex (включая gpt-5.3-codex-spark). «По умолчанию» — модель из codex config.', msel.node));

    const efforts = (v.efforts || ['low', 'medium', 'high']).map((e) => ({ value: e, label: e }));
    const esel = customSelect(efforts, v.codexEffort || 'low', async (e) => {
      await safe(() => window.jarvis.serviceSetEffort(e), null);
    });
    cg.appendChild(drow('Глубина рассуждений',
      'Меньше = быстрее и дешевле. Для саммари хватает low/minimal.', esel.node));

    if (v.codexSidecar) {
      cg.appendChild(drow('Codex-SDK сайдкар',
        'Установлен (openai-codex). Авторизация — существующий codex login, ключ API не нужен.',
        el('span.sval.on', { text: 'на месте' })));
    } else {
      const wrap = el('div.dctl', { style: 'flex-direction:column;align-items:flex-end;gap:6px' });
      const btn = el('button.btn.sm', null, [iconSpan('download'), document.createTextNode('Установить')]);
      btn.addEventListener('click', async () => {
        btn.disabled = true;
        btn.replaceChildren(document.createTextNode('Ставлю…'));
        await safe(() => window.jarvis.codexInstallSidecar(), null);
        // финал прилетит codex_install_done → перерисует панель
      });
      wrap.appendChild(btn);
      wrap.appendChild(el('div', { id: 's2-codex-progress' })); // плейсхолдер прогресса
      cg.appendChild(el('div.drow', null, [
        el('div.grow', null, [
          el('div.dt', { text: 'Codex-SDK сайдкар' }),
          el('div.dd', { text: 'Нужен для бэкенда Codex: Python-venv + openai-codex (тянет codex-бинарь). Ставится один раз.' }),
        ]),
        wrap,
      ]));
    }
    pane.appendChild(cg);
  }

  const RENDERERS = {
    general: renderGeneral,
    stt: renderStt,
    voice: renderVoice,
    wake: renderWake,
    notify: renderNotify,
    awake: renderAwake,
    keys: renderKeys,
    service: renderService,
    integration: renderIntegration,
    about: renderAbout,
  };

  /* ========================================================================
   * Перерисовать конкретную панель на месте (для live-событий и after-action).
   * ====================================================================== */
  // Перерисовать вкладку. Рендереры АСИНХРОННЫ (await *Get()): два наложившихся
  // вызова успевали оба дописать контент в один узел → дубль («почему 2»). Поэтому
  // сериализуем по вкладке: пока идёт рендер — повторный запрос лишь взводит флаг,
  // и после текущего мы перерисовываем РОВНО раз (коалесцируем частые события вроде
  // onAudioState). Узел чистим только в начале каждого витка.
  async function reRenderPane(pane) {
    if (!currentRoot) return;
    if (renderingPane[pane]) { renderPending[pane] = true; return; }
    renderingPane[pane] = true;
    try {
      do {
        renderPending[pane] = false;
        const node = currentRoot.querySelector('#s2-pane-' + pane);
        if (!node) break;
        node.textContent = '';
        const fn = RENDERERS[pane];
        if (fn) { try { await fn(node); } catch (e) {} }
      } while (renderPending[pane]);
    } finally {
      renderingPane[pane] = false;
    }
  }

  /* Финал загрузки: разнести успех/ошибку по активной модели. Бэкенд шлёт
   * {ok, error} — раньше это игнорировалось и статус молча «сбрасывался». */
  function finishDownload(res) {
    const id = activeDownload;
    activeDownload = null;
    if (!id) return;
    if (res && res.ok === false) {
      dlState[id] = { error: (res && res.error) || 'неизвестная ошибка (подробности в логах ~/.jarvis/jarvis.log)' };
    } else {
      delete dlState[id]; // успех — present-статус («✓ установлена») придёт перерисовкой
    }
  }

  /* ========================================================================
   * Подписка на live-события (идемпотентно — модульный флаг subscribed).
   * ====================================================================== */
  function subscribeOnce() {
    if (subscribed) return;
    subscribed = true;
    // прогресс установки STT → ТОЛЬКО в строку качаемой модели (не во все сразу)
    try {
      window.jarvis.onSttInstallProgress((step) => {
        if (!currentRoot || !activeDownload) return;
        const pct = step && typeof step.pct === 'number' ? step.pct : null;
        const h = currentRoot.querySelector('#s2-pane-stt [data-model="' + activeDownload + '"]');
        if (!h) return;
        h.textContent = '';
        if (step && step.msg) h.appendChild(el('span.loadcap', { text: step.msg }));
        if (pct != null) h.appendChild(progressBar(pct));
      });
    } catch (e) {}
    // финал установки STT → записать успех/ошибку и перерисовать stt-панель
    try { window.jarvis.onSttInstallDone((res) => { finishDownload(res); reRenderPane('stt'); }); } catch (e) {}
    // прогресс установки Codex-SDK сайдкара → обновить плейсхолдер в панели service
    try {
      window.jarvis.onCodexInstallProgress((step) => {
        if (!currentRoot) return;
        const h = currentRoot.querySelector('#s2-codex-progress');
        if (!h) return;
        h.textContent = '';
        if (step && step.msg) h.appendChild(el('span.loadcap', { text: step.msg }));
        const pct = step && typeof step.pct === 'number' ? step.pct : null;
        if (pct != null) h.appendChild(progressBar(pct));
      });
    } catch (e) {}
    // финал установки Codex-SDK → перерисовать service-панель
    try { window.jarvis.onCodexInstallDone(() => { reRenderPane('service'); }); } catch (e) {}
    // финал установки wake-моделей → записать успех/ошибку, перерисовать wake + stt
    try { window.jarvis.onWakeInstallDone((res) => { finishDownload(res); reRenderPane('wake'); reRenderPane('stt'); }); } catch (e) {}
    // состояние аудио → обновить индикаторы wake-панели (если открыта)
    try { window.jarvis.onAudioState(() => { if (activePane === 'wake') reRenderPane('wake'); }); } catch (e) {}

    // ── Единые события мультизагрузки (models_install) — прогресс по id модели ──
    try {
      window.jarvis.onModelInstallProgress(({ id, step }) => {
        if (!currentRoot || !id) return;
        const h = currentRoot.querySelector('[data-model="' + id + '"]');
        if (!h) return;
        h.textContent = '';
        if (step && step.msg) h.appendChild(el('span.loadcap', { text: step.msg }));
        const pct = step && typeof step.pct === 'number' ? step.pct : null;
        if (pct != null) h.appendChild(progressBar(pct));
      });
    } catch (e) {}
    try {
      window.jarvis.onModelInstallDone(({ id, ok, error }) => {
        if (!id) return;
        if (!ok) dlState[id] = { error: error || 'неизвестная ошибка (подробности в ~/.jarvis/jarvis.log)' };
        else { delete dlState[id]; selectedModels.delete(id); }
      });
    } catch (e) {}
    try {
      window.jarvis.onModelsInstallAllDone(() => {
        reRenderPane('stt'); reRenderPane('wake'); reRenderPane('voice');
      });
    } catch (e) {}
  }

  /* ========================================================================
   * Главная функция: построить весь UI в rootEl.
   * ====================================================================== */
  function initSettings2(rootEl) {
    if (!rootEl) return;
    injectStyle();
    currentRoot = rootEl;
    rootEl.textContent = ''; // полная очистка → ре-init не плодит дубли

    const win = el('div.swin2#settings2');

    // ── Сайдбар ──
    const sidebar = el('div.sidebar');
    sidebar.appendChild(el('div.ssearch', null, [
      el('span.si', null, icon('search')),
      el('input', { placeholder: 'Поиск настроек…' }), // визуальный no-op (по спеке)
    ]));
    sidebar.appendChild(el('div.saccount', null, [
      el('span.ava', { text: 'J' }),
      el('div', null, [el('div.nm', { text: 'Jarvis' }), el('div.sub', { id: 's2-ver', text: 'локально' })]),
    ]));
    // реальная версия приложения в подпись аккаунта (вместо захардкоженной)
    safe(() => window.jarvis.getMeta(), {}).then((m) => {
      const s = currentRoot && currentRoot.querySelector('#s2-ver');
      if (s && m && m.version) s.textContent = 'локально · v' + m.version;
    });
    const snav = el('div.snav');
    const navItems = {};
    for (const n of NAV) {
      if (n.sep) { snav.appendChild(el('div.sep')); continue; }
      const item = el('div.item' + (n.pane === activePane ? '.sel' : ''), { 'data-pane': n.pane }, [
        el('span.ic.' + n.ic, null, icon(n.icon)),
        document.createTextNode(n.label),
      ]);
      item.addEventListener('click', () => selectPane(n.pane));
      navItems[n.pane] = item;
      snav.appendChild(item);
    }
    sidebar.appendChild(snav);
    win.appendChild(sidebar);

    // ── Детальная колонка ──
    const detail = el('div.detail');
    // (стрелки ‹ › убраны — навигация только по сайдбару)
    // по одной панели-контейнеру на вкладку; активная получит .on
    const paneNodes = {};
    for (const n of NAV) {
      if (n.sep) continue;
      const p = el('div.dpane' + (n.pane === activePane ? '.on' : '') + '#s2-pane-' + n.pane);
      paneNodes[n.pane] = p;
      detail.appendChild(p);
    }
    win.appendChild(detail);
    rootEl.appendChild(win);

    // переключение панели сайдбара (ленивый рендер при первом открытии)
    function selectPane(pane) {
      activePane = pane;
      for (const k in navItems) navItems[k].classList.toggle('sel', k === pane);
      for (const k in paneNodes) paneNodes[k].classList.toggle('on', k === pane);
      closeAllSelects(null);
      const node = paneNodes[pane];
      // через reRenderPane (сериализованный) — чтобы прямой рендер не гонялся с
      // live-перерисовкой (onAudioState и т.п.) и не задваивал контент вкладки.
      if (node && !node.childNodes.length) reRenderPane(pane);
    }

    // глобальный «клик мимо» закрывает открытые селекты (ставим один раз)
    if (!docClickBound) {
      docClickBound = true;
      document.addEventListener('click', () => { if (currentRoot) closeAllSelects(null); });
    }

    subscribeOnce();

    // отрисовать активную панель сразу (через сериализованный reRenderPane)
    reRenderPane(activePane);
  }

  window.initSettings2 = initSettings2;
})();
