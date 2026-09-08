/* ============================================================================
 * settings2.js — самодостаточный модуль страницы настроек Jarvis.
 *
 * Экспортирует window.initSettings2(rootEl): строит сайдбар + детальные панели
 * в стиле системных настроек / Raycast, грузит значения из IPC (window.jarvis)
 * и подписывается на live-события. Полностью изолирован: все стили под #settings2,
 * иконки — инлайновый SVG (офлайн, без CDN). Повторный вызов перестраивает UI
 * без утечек слушателей и без дублей стилей.
 *
 * ВАЖНО: ничего не импортирует, чистый ванильный JS под WKWebView.
 * Иконки берутся из локального Phosphor через DOM — без innerHTML, без XSS.
 * ========================================================================== */
(function () {
  'use strict';

  // ── Модульные флаги (живут между ре-init) ───────────────────────────────
  let subscribed = false;     // подписки на live-события поставлены лишь раз
  let docClickBound = false;  // глобальный «клик мимо» для закрытия селектов
  let currentRoot = null;     // активный rootEl (для live-перерисовок)
  let activePane = 'general'; // выбранная вкладка сайдбара
  let lastSettingsPane = 'general';
  const openMachines = () => window.dispatchEvent(new window.Event('jarvis:open-machines'));
  window.jarvisOpenSettingsPane = pane => {
    if (pane === 'remotes') { openMachines(); return; }
    if (Object.prototype.hasOwnProperty.call(RENDERERS, pane)) activePane = lastSettingsPane = pane;
  };
  const renderingPane = {};   // pane → идёт ли сейчас рендер (анти-гонка)
  const renderPending = {};   // pane → запрошен ли повторный рендер во время текущего
  // Состояние загрузок моделей. `activeDownload` — id модели, что качается СЕЙЧАС
  // (чтобы прогресс шёл только в её строку, а не во все). `dlState[id].error` —
  // текст последней ошибки (показываем в строке + retry), вместо тихого сброса.
  let activeDownload = null;
  const dlState = {};
  // Мультивыбор моделей для «Скачать выбранное» (чекбоксы в строках, id → выбран).
  const selectedModels = new Set();
  const machineDetails = new Map();

  function settingsDetails(id, title, children, initiallyOpen = false) {
    const box = el('details.s2-details', { id });
    box.open = machineDetails.has(id) ? machineDetails.get(id) : initiallyOpen;
    box.appendChild(el('summary', { text: title }));
    for (const child of children) box.appendChild(child);
    box.addEventListener('toggle', () => machineDetails.set(id, box.open));
    return box;
  }

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
  let cancelShortcutRecording = null;
  let controlId = 0;
  async function required(fn) {
    if (!window.jarvis) throw new Error('Нет связи с приложением. Повторите попытку.');
    const result = await fn();
    if (result?.ok === false) throw new Error(result.error || 'Изменение отклонено приложением.');
    return result;
  }
  const paneErrors = new Map();
  function paintSettingsError() {
    if (!currentRoot) return;
    let note = currentRoot.querySelector('#settings-save-error');
    const message = paneErrors.get(activePane);
    if (!message) { note?.remove(); return; }
    if (!note) { note = el('div#settings-save-error.meeting-status.error', { role: 'alert' }); currentRoot.querySelector('.detail')?.prepend(note); }
    note.textContent = message;
  }
  function showSettingsError(error, pane = activePane, control) {
    const label = control?.closest('.drow')?.querySelector('.dt')?.textContent || NAV.find(n => n.pane === pane)?.label || 'Настройка';
    paneErrors.set(pane, label + ' — не удалось применить: ' + (error?.message || String(error)));
    paintSettingsError();
  }
  function clearSettingsError(pane) {
    paneErrors.delete(pane); paintSettingsError();
  }
  window.addEventListener('jarvis:appearance-error', event => {
    showSettingsError(event.detail, 'look');
    if (activePane === 'look') reRenderPane('look');
  });
  function fire(fn) {
    const pane = activePane;
    return required(fn).then(result => {
      clearSettingsError(pane);
      return result;
    }).catch(error => { showSettingsError(error, pane); reRenderPane(pane); });
  }
  async function action(control, fn) {
    if (control.getAttribute('aria-busy') === 'true') return false;
    const pane = activePane;
    control.setAttribute('aria-busy', 'true');
    try {
      await required(fn);
      clearSettingsError(pane);
      return true;
    } catch (error) { showSettingsError(error, pane, control); return false; }
    finally { control.removeAttribute('aria-busy'); }
  }

  // ── Утилита формата размера на диске (порт fmtBytes из renderer.js) ──────
  function fmtBytes(n) {
    if (!n) return '0 МБ';
    const mb = n / (1024 * 1024);
    if (mb >= 1024) return (mb / 1024).toFixed(mb >= 10240 ? 0 : 1) + ' ГБ';
    return Math.max(1, Math.round(mb)) + ' МБ';
  }

  /* Подписи клавиш — из keys.js: он знает про ОС и рисует ⌘ только там, где
   * такая клавиша есть. Модуль могут ещё не подключить (окно настроек грузят
   * и отдельно) — тогда отдаём нейтральные слова, но маковских символов руками
   * не пишем никогда. */
  const displayHotkey = (acc) =>
    (window.jarvisKeys ? window.jarvisKeys.displayHotkey(acc) : String(acc || '').replaceAll('+', ' '));
  const hotkeyKeys = (acc) =>
    (window.jarvisKeys ? window.jarvisKeys.hotkeyKeys(acc) : displayHotkey(acc).split(' ').filter(Boolean));
  /** Перечисление модификаторов для подсказок — под текущую ОС. */
  const MODS_HINT = window.jarvisKeys && window.jarvisKeys.isMac ? '⌘/⌥/⌃' : 'Ctrl/Alt/Super';
  const KEY_FALLBACK = { enter: 'Enter', esc: 'Esc', del: 'Backspace', tab: 'Tab' };
  function keyName(n) {
    const K = window.jarvisKeys;
    if (K && K.NAMES && K.NAMES[n]) return K.NAMES[n];
    return KEY_FALLBACK[n] || n;
  }

  // Shared, locally bundled Phosphor icons; each label remains real text.
  function icon(name) {
    return window.jarvisIcons.create(name);
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
    const labelId = 's2-control-' + (++controlId);
    const grow = el('div.grow', null, [
      el('div.dt', { text: title, id: labelId }),
      desc ? el('div.dd', { text: desc }) : null,
    ]);
    const dctl = el('div.dctl' + ((opts && opts.ctlClass) ? '.' + opts.ctlClass : ''));
    if (opts && opts.ctlStyle) dctl.style.cssText = opts.ctlStyle;
    const arr = Array.isArray(ctlNodes) ? ctlNodes : [ctlNodes];
    for (const c of arr) if (c) dctl.appendChild(c);
    for (const control of dctl.querySelectorAll('input, select, .cstrigger, .seg')) {
      if (!control.getAttribute('aria-label')) control.setAttribute('aria-labelledby', labelId);
    }
    const leading = opts && opts.dot ? el('span.dot' + (opts.dot === true ? '' : '.' + opts.dot), { style: 'margin-top:5px' }) : null;
    return el('div.drow', null, [leading, grow, dctl]);
  }
  // переключатель (toggle) → IPC
  function toggle(checked, onChange, disabled) {
    const t = el('input.toggle', { type: 'checkbox' });
    t.checked = !!checked;
    if (disabled) t.disabled = true;
    let saved = t.checked;
    t.addEventListener('change', async () => {
      const next = t.checked;
      t.disabled = true;
      if (await action(t, () => onChange(next))) saved = next;
      t.checked = saved;
      t.disabled = !!disabled;
    });
    return t;
  }
  // кнопка (.btn / .btn.sm / .btn.primary / .btn.danger)
  function button(label, onClick, extra) {
    const b = el('button.btn' + (extra ? '.' + extra.split(' ').join('.') : ''), { text: label });
    b.type = 'button';
    b.addEventListener('click', async () => {
      if (b.getAttribute('aria-busy') === 'true') return;
      const contents = [...b.childNodes].map(n => n.cloneNode(true));
      const disabled = b.disabled;
      if (!await action(b, () => onClick(b))) {
        b.replaceChildren(...contents); b.disabled = disabled;
      }
    });
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
    // Keep an unknown saved device/model visible until the user changes it.
    const choices = [...options];
    if (value && !choices.some(o => o.value === value)) choices.unshift({ value, label: String(value) });
    let selected = choices.find(o => o.value === value) || choices[0] || { value: '', label: '—' };
    const valSpan = el('span.cval', { text: selected.label });
    const spin = el('span.spin', null, icon('loader-circle'));
    const chev = el('span.chev', null, icon('chevron-down'));
    const id = 's2-select-' + (++controlId);
    const trigger = el('button.cstrigger', { type: 'button', 'aria-haspopup': 'listbox', 'aria-expanded': 'false', 'aria-controls': id }, [valSpan, spin, chev]);
    const menu = el('div.cmenu', { id, role: 'listbox', 'aria-label': 'Варианты' });
    const root = el('div.cselect', null, [trigger, menu]);
    let focused = Math.max(0, choices.indexOf(selected));
    const close = (restore = false) => {
      root.classList.remove('open'); trigger.setAttribute('aria-expanded', 'false');
      if (restore) trigger.focus();
    };
    root.dismissSelect = close;
    const focusOption = i => {
      focused = Math.max(0, Math.min(choices.length - 1, i));
      const option = menu.children[focused];
      if (option) { option.focus(); option.scrollIntoView?.({ block: 'nearest' }); }
    };
    const open = () => {
      if (root.classList.contains('busy')) return;
      closeAllSelects(root); root.classList.add('open'); trigger.setAttribute('aria-expanded', 'true');
      focusOption(Math.max(0, choices.indexOf(selected)));
    };
    const busy = on => { root.classList.toggle('busy', !!on); trigger.disabled = !!on; if (on) close(); };
    const paint = () => {
      valSpan.textContent = selected.label;
      for (const opt of menu.children) {
        const current = opt.getAttribute('data-value') === String(selected.value);
        opt.classList.toggle('selected', current); opt.setAttribute('aria-selected', String(current));
      }
    };
    choices.forEach(o => {
      const opt = el('button.copt', { type: 'button', role: 'option', tabindex: '-1', 'data-value': o.value }, [document.createTextNode(o.label), el('span.ck', null, icon('check'))]);
      opt.addEventListener('click', async e => {
        e.stopPropagation();
        if (root.classList.contains('busy')) return;
        close(true);
        if (o.value === selected.value) return;
        busy(true);
        if (await action(root, () => onPick(o.value))) selected = o;
        busy(false); paint(); trigger.focus();
      });
      menu.appendChild(opt);
    });
    paint();
    trigger.addEventListener('click', e => { e.stopPropagation(); root.classList.contains('open') ? close() : open(); });
    root.addEventListener('keydown', e => {
      if (e.isComposing || e.altKey || e.ctrlKey || e.metaKey) return;
      if (e.key === 'Escape' && root.classList.contains('open')) { e.preventDefault(); e.stopPropagation(); close(true); return; }
      if (e.key === 'Tab') { close(); return; }
      if (['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(e.key)) {
        e.preventDefault(); e.stopPropagation();
        if (!root.classList.contains('open')) { open(); return; }
        focusOption(e.key === 'Home' ? 0 : e.key === 'End' ? choices.length - 1 : focused + (e.key === 'ArrowDown' ? 1 : -1));
      }
    });
    return { node: root, setBusy: busy };
  }
  function closeAllSelects(keep, restore = false) {
    if (!currentRoot) return false;
    let closed = false;
    for (const s of currentRoot.querySelectorAll('.cselect.open')) {
      if (s !== keep) { s.dismissSelect?.(restore); closed = true; }
    }
    return closed;
  }
  window.jarvisModuleBack = window.jarvisModuleBack || {};
  window.jarvisModuleBack.settings = () => {
    if (cancelShortcutRecording) { cancelShortcutRecording(); return true; }
    if (closeAllSelects(null, true)) return true;
    const search = currentRoot?.querySelector('#settingsSearch');
    if (search?.value) { search.value = ''; search.dispatchEvent(new Event('input', { bubbles: true })); search.focus(); return true; }
    return false;
  };
  window.jarvisModuleBack.machines = () => {
    if (closeAllSelects(null, true)) return true;
    if (remoteEditorOpen()) { closeMachineEditor?.(); return true; }
    const search = currentRoot?.querySelector('.connection-search input');
    if (search?.value) { search.value = ''; search.dispatchEvent(new window.Event('input', { bubbles: true })); search.focus(); return true; }
    return false;
  };

  function segmented(options, value, onPick) {
    const seg = el('div.seg', { role: 'group' });
    let selected = value;
    const paint = () => [...seg.children].forEach((b, i) => {
      const active = options[i].value === selected;
      b.classList.toggle('active', active); b.setAttribute('aria-pressed', String(active));
    });
    options.forEach(o => {
      const b = el('button.segbtn', { type: 'button', text: o.label });
      b.addEventListener('click', async () => {
        if (o.value === selected || seg.getAttribute('aria-busy') === 'true') return;
        for (const child of seg.children) child.disabled = true;
        if (await action(seg, () => onPick(o.value))) selected = o.value;
        for (const child of seg.children) child.disabled = false;
        paint();
      });
      seg.appendChild(b);
    });
    paint(); return seg;
  }

  /* ── Строка хоткея с инлайн-рекордером (Raycast-style) ────────────────────
   * b: { action, label, accel, default } из hotkeyBindings() (accel: null =
   * «не назначен»). Клик по капсуле → запись: бэкенд снимает ВСЕ глобальные
   * хоткеи (hotkeysSuspend — команды не срабатывают, и наши шорткаты не
   * съедают keydown), жмёшь комбо целиком → hotkeyAssign. Esc / клик мимо /
   * 12 с тишины — отмена (бэкенд сам вернёт хоткеи через 15 с, если UI умер).
   * Конфликт со своим хоткеем → красная строка + «Всё равно назначить»
   * (steal: у конфликтующего действия сочетание снимается в «не назначен»).
   * action='select': основная клавиша фиксирована «1…9» — в записи нужна
   * любая цифра, в акселератор идёт {n}. opts.after() — после успешного
   * применения (перерисовать пары-дубли в других вкладках). */
  function hotkeyRow(b, desc, opts) {
    const isSel = b.action === 'select';
    let acc = b.accel; // string | null
    const row = el('div.drow');
    const left = el('div.grow');
    left.appendChild(el('div.dt', { text: b.label }));
    if (desc) left.appendChild(el('div.dd', { text: desc }));
    const errBox = el('div.hkerr');
    errBox.style.display = 'none';
    left.appendChild(errBox);
    const cap = el('button.hkey.rec', { type: 'button', title: 'Кликни и нажми сочетание', 'aria-label': b.label + ': изменить сочетание' });
    const rb = el('button.hkreset', { title: 'Сбросить' }, icon('rotate-ccw'));
    const ctl = el('div.dctl.hk', null, [cap, rb]);
    row.appendChild(left);
    row.appendChild(ctl);

    const clearErr = () => { row.classList.remove('conflict'); errBox.style.display = 'none'; errBox.replaceChildren(); };
    const paint = () => {
      clearErr();
      cap.classList.remove('recording');
      cap.classList.toggle('none', !acc);
      cap.replaceChildren();
      if (!acc) { cap.appendChild(el('span.hknone', { text: 'не назначен' })); return; }
      if (isSel) {
        for (const k of hotkeyKeys(acc.replace('+{n}', ''))) cap.appendChild(el('kbd', { text: k }));
        cap.appendChild(el('kbd.fix', { text: '1…9' }));
      } else {
        for (const k of hotkeyKeys(acc)) cap.appendChild(el('kbd', { text: k }));
      }
    };
    const note = (txt) => { cap.replaceChildren(el('span.shortcut-note', { text: txt })); };
    const done = () => { paint(); if (opts && opts.after) opts.after(); };

    const showConflict = (conf, next) => {
      paint();
      row.classList.add('conflict');
      const shown = isSel ? displayHotkey(next.replace('{n}', '1…9')) : displayHotkey(next);
      errBox.appendChild(el('span', { text: '⚠ ' + shown + ' занято «' + conf.label + '» · ' }));
      const steal = el('button.hksteal', { text: 'Всё равно назначить' });
      steal.addEventListener('click', async (e) => {
        e.stopPropagation();
        const res = await safe(() => window.jarvis.hotkeyAssign(b.action, next, true), null);
        if (res && res.ok) { acc = res.accel; done(); }
        else { note((res && res.error) || 'не удалось'); setTimeout(paint, 1600); }
      });
      errBox.appendChild(steal);
      errBox.style.display = '';
    };

    const applyAccel = async (next) => {
      const res = await safe(() => window.jarvis.hotkeyAssign(b.action, next, false), null);
      if (res && res.ok) { acc = res.accel; done(); return; }
      if (res && res.conflict) { showConflict(res.conflict, next); return; }
      note((res && res.error) || 'не удалось');
      setTimeout(paint, 1600);
    };

    let recording = false, onKey = null, recTimer = 0;
    function stopRec() {
      if (!recording) return;
      recording = false;
      window.jarvisShortcutRecording = false;
      if (cancelShortcutRecording === stopRec) cancelShortcutRecording = null;
      clearTimeout(recTimer);
      if (onKey) { document.removeEventListener('keydown', onKey, true); onKey = null; }
      document.removeEventListener('click', onAway, true);
      fire(() => window.jarvis.hotkeysSuspend(false));
      paint();
    }
    function onAway(e) { if (!cap.contains(e.target)) stopRec(); }
    function startRec() {
      if (recording) return;
      cancelShortcutRecording?.();
      recording = true;
      window.jarvisShortcutRecording = true;
      cancelShortcutRecording = stopRec;
      clearErr();
      fire(() => window.jarvis.hotkeysSuspend(true));
      cap.classList.add('recording');
      cap.classList.remove('none');
      note(isSel ? 'Нажмите сочетание с цифрой…' : 'Нажмите сочетание…');
      recTimer = setTimeout(stopRec, 12000); // раньше авто-ресюма бэкенда (15 с)
      onKey = (e) => {
        e.preventDefault(); e.stopPropagation();
        if (e.key === 'Escape') { stopRec(); return; }
        if (['Shift', 'Control', 'Alt', 'Meta'].includes(e.key)) return; // ждём основную
        const { mods, key, isFn } = eventToAccel(e);
        if (!key) { note('Эта клавиша не поддерживается'); return; }
        if (isSel) {
          if (!/^\d$/.test(key)) { note('Нужна цифра 1–9'); return; }
          if (!mods.length) { note(`Нужен модификатор (${MODS_HINT})`); return; }
        } else if (!isFn && mods.length === 0) {
          note(`Нужен модификатор (${MODS_HINT}) или F-клавиша`); return;
        }
        const next = mods.concat(isSel ? '{n}' : key).join('+');
        stopRec();
        applyAccel(next);
      };
      document.addEventListener('keydown', onKey, true);
      document.addEventListener('click', onAway, true);
    }
    cap.addEventListener('click', (e) => { e.stopPropagation(); startRec(); });
    rb.addEventListener('click', (e) => { e.stopPropagation(); applyAccel(b.default); });
    paint();
    return row;
  }

  /* ── Инлайн-заметка ошибки загрузки (красная, под строкой модели) ───────
   * Показывает реальную причину провала скачивания (раньше ошибка молча
   * глоталась и статус «сбрасывался» в «не скачана»). */
  function dlErrorNote(msg) {
    const n = el('div.s2err');
    // el(tag, attrs, kids): иконка — третьим аргументом. Раньше она уходила
    // вторым, то есть на место атрибутов: svg перебирался как объект, на span
    // сыпались его DOM-свойства, а сама иконка не добавлялась никогда.
    n.appendChild(el('span.s2err-ic', null, icon('alert-triangle')));
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

  /* ── Полоса загрузки модели (.progress.striped) ─────────────────────────*/
  function progressBar(pct) {
    const bar = el('div.progress.striped', { style: 'margin-top:9px;max-width:280px' });
    bar.appendChild(el('i', { style: 'width:' + Math.max(0, Math.min(100, pct || 0)) + '%' }));
    return bar;
  }

  /* ── Кнопка удаления модели с двойным подтверждением «Точно?» ────────────*/
  function makeDeleteButton(id, after) {
    const del = el('button.btn.sm.danger', { type: 'button', 'aria-label': 'Удалить модель ' + id });
    const setIcon = () => { del.replaceChildren(icon('trash-2')); };
    setIcon();
    let armed = false;
    del.addEventListener('click', async () => {
      if (!armed) { armed = true; del.replaceChildren(document.createTextNode('Точно?')); setTimeout(() => { armed = false; setIcon(); }, 3000); return; }
      del.disabled = true; del.replaceChildren(document.createTextNode('…'));
      if (await action(del, () => window.jarvis.modelDelete(id))) { if (after) after(); }
      else { armed = false; del.disabled = false; setIcon(); }
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
  /* локальные токены поверх дизайн-системы (theme.css) */
  --working-soft: var(--accent-soft);
  --limit: var(--ink-faint);
  --s2-font: var(--font, -apple-system, BlinkMacSystemFont, "SF Pro Text", "Segoe UI", sans-serif);
  --s2-mono: var(--mono, ui-monospace, "SF Mono", Menlo, monospace);
}
/* организм: окно настроек — заполняет rootEl, две независимо-скроллящихся колонки */
#settings2.swin2 { display:flex; flex-direction:row; height:100%; width:100%;
  color: var(--ink); font-family: var(--s2-font); overflow:hidden; }
#settings2 *, #settings2 *::before, #settings2 *::after { box-sizing:border-box; }

/* ── Сайдбар ─────────────────────────────────────────────────────────── */
#settings2 .sidebar { width:248px; flex:none; border-right:1px solid var(--line);
  background: var(--paper-2); display:flex; flex-direction:column; min-height:0; }
#settings2 .ssearch { display:flex; align-items:center; gap:9px; margin:12px 12px 8px; padding:9px 12px;
  border-radius:9px; background: var(--surface); border:0; }
#settings2 .ssearch input { flex:1; background:transparent; border:0; outline:0; color:var(--ink); font:400 13.5px/1 var(--s2-font); min-width:0; }
#settings2 .ssearch input::placeholder { color:var(--ink-faint); }
#settings2 .ssearch .si { color:var(--ink-faint); display:inline-flex; }
#settings2 .saccount { display:flex; align-items:center; gap:10px; padding:8px 14px 12px; border-bottom:1px solid var(--line); }
#settings2 .saccount .ava { width:30px; height:30px; border-radius:9px; background:var(--accent); display:grid; place-items:center; color:var(--on-accent); font:700 13px/1 var(--s2-font); flex:none; }
#settings2 .saccount .nm { font-size:13.5px; font-weight:500; color:var(--ink); }
#settings2 .saccount .sub { font-size:12px; color:var(--ink-mute); margin-top:2px; }
#settings2 .snav { flex:1; overflow-y:auto; padding:8px 9px; min-height:0; }
#settings2 .snav::-webkit-scrollbar { width:0; }
#settings2 .snav .item { display:flex; align-items:center; gap:10px; padding:8px 10px; border-radius:var(--r-pill); font-size:13.5px; color:var(--ink-2); cursor:default; user-select:none; }
#settings2 .snav .item:hover { background: var(--fill-1); }
#settings2 .snav .item.sel { background: var(--accent-soft); color:var(--ink); font-weight:500; }
#settings2 .snav .item .ic { width:22px; height:22px; border-radius:7px; display:grid; place-items:center; font-size:12px; flex:none; }
#settings2 .snav .sep { height:1px; background:var(--line); margin:9px 9px; }
#settings2 .snav .grp { font:500 12px/1 var(--s2-font); letter-spacing:0; text-transform:none; color:var(--ink-mute); padding:12px 10px 6px; }

/* ── Детальная панель ────────────────────────────────────────────────── */
#settings2 .detail { flex:1; overflow-y:auto; padding:20px 28px 88px; min-height:0; min-width:0; }
#settings2 .detail::-webkit-scrollbar { width:0; }
#settings2 .dnav { display:inline-flex; gap:3px; padding:3px; border-radius:9px; background:var(--surface); border:0; margin-bottom:18px; }
#settings2 .dnav button { appearance:none; border:0; background:transparent; color:var(--ink-mute); width:28px; height:24px; border-radius:7px; cursor:default; font-size:13px; display:grid; place-items:center; }
#settings2 .dnav button:hover { background:var(--paper); color:var(--ink); box-shadow:var(--shadow-raised); }
#settings2 .dtitle { font-size:22px; font-weight:700; letter-spacing:-.03em; color:var(--ink); margin:2px 0 18px; }
/* заголовок секции — строчными и тихо, как в макете 14f */
#settings2 .dsection { font-size:12.5px; color:var(--ink-mute); font-weight:500; margin:8px 0 8px; }
#settings2 .s2-intro { color:var(--ink-mute); font-size:12px; line-height:1.65; margin:-5px 0 22px; }
#settings2 .s2-details { border:1px solid var(--line); border-radius:10px; margin:10px 0 18px; min-width:0; }
#settings2 .s2-details > summary { padding:12px 14px; color:var(--ink-2); font-size:12px; font-weight:500; cursor:pointer; }
#settings2 .s2-details > summary:focus-visible { outline:2px solid var(--accent); outline-offset:-3px; }
#settings2 .s2-details > .dgroup { margin:0; padding:0 14px; border:0; border-radius:0; }
#settings2 .s2-details > .s2-intro { margin:0; padding:0 14px 14px; }
#settings2 .s2-machine-head { display:flex; align-items:center; justify-content:space-between; gap:12px; margin:20px 0 8px; }
#settings2 .s2-machine-head .dsection { margin:0; }
#settings2 .s2-machine-actions { display:flex; align-items:center; justify-content:flex-end; flex-wrap:wrap; gap:6px; }
#settings2 .s2-vm-list .drow { align-items:flex-start; }
#settings2 .s2-vm-meta { margin-top:6px; color:var(--ink-faint); font-size:11px; line-height:1.5; overflow-wrap:anywhere; }
#settings2 .s2-vm-details { margin:10px 0 0; border:0; border-top:1px solid var(--line); border-radius:0; }
#settings2 .s2-vm-details > summary { padding:10px 0 0; font-size:11px; color:var(--ink-mute); }
#settings2 .s2-vm-details code { font-size:11px; overflow-wrap:anywhere; user-select:text; }
#settings2 .s2-vm-mount { padding-top:8px; color:var(--ink-mute); font-size:11px; line-height:1.5; overflow-wrap:anywhere; }
#settings2 .s2-vm-mount strong { color:var(--ink-2); font-weight:500; }
#settings2 .s2-vm-advanced { display:flex; gap:7px; flex-wrap:wrap; padding-top:12px; }
/* группа — не карточка, а полоса строк с волосяными стыками */
#settings2 .dgroup { background:transparent; border:0; border-radius:0; margin-bottom:24px; }
#settings2 .drow { display:flex; align-items:center; gap:20px; min-height:var(--h-srow); padding:11px 0; }
#settings2 .dgroup .drow:not(:last-child) { box-shadow: inset 0 -1px 0 var(--line); }
#settings2 .drow .dt { font-size:14.5px; font-weight:500; color:var(--ink); }
#settings2 .drow .dd { font-size:12.5px; color:var(--ink-mute); margin-top:4px; line-height:1.5; max-width:400px; }
#settings2 .drow .dctl { margin-left:auto; flex:none; display:flex; align-items:center; gap:8px; }
#settings2 .dpane { display:none; }
#settings2 .dpane.on { display:block; animation: s2fade .18s ease; }
#settings2 .grow { flex:1; min-width:0; }
#settings2 .mono { font-family: var(--s2-mono); }
@keyframes s2fade { from { opacity:0; transform:translateY(6px); } }

/* ── .ic плитки: тональные, одной краской (монохром + акцент) ─────────── */
#settings2 .ic.gray{background:var(--surface);color:var(--ink-mute)}
#settings2 .ic.blue{background:var(--accent-soft);color:var(--accent-text)}
#settings2 .ic.green{background:var(--accent-soft);color:var(--accent-text)}
#settings2 .ic.amber{background:var(--warn-soft);color:var(--warn)}
#settings2 .ic.orange{background:var(--warn-soft);color:var(--warn)}
#settings2 .ic.violet{background:var(--surface);color:var(--ink-mute)}
#settings2 .ic.teal{background:var(--accent-soft);color:var(--accent-text)}
#settings2 .ic.purple{background:var(--surface);color:var(--ink-mute)}

/* ── скелетоны: мерцающие плейсхолдеры, пока грузятся данные вкладки ──── */
#settings2 .skel{position:relative;overflow:hidden;background:var(--surface);border-radius:6px}
#settings2 .skel::after{content:'';position:absolute;inset:0;transform:translateX(-100%);background:linear-gradient(90deg,transparent,var(--fill-2),transparent);animation:s2shim 1.15s infinite}
@keyframes s2shim{100%{transform:translateX(100%)}}
#settings2 .skrow{display:flex;align-items:flex-start;gap:20px;padding:15px 0}
#settings2 .dgroup .skrow:not(:first-child){border-top:1px solid var(--line)}
#settings2 .skgrow{flex:1;min-width:0;display:flex;flex-direction:column;gap:9px}
#settings2 .skbar{height:13px}
#settings2 .skctl{width:50px;height:22px;border-radius:11px;flex:none;margin-left:auto}

/* ── поле-секрет (API-ключ / токен подписки) ─────────────────────────── */
#settings2 .s2agents-chips{display:flex;gap:6px;flex-wrap:wrap;margin:2px 0 10px}
#settings2 .s2agents-note{margin-top:8px}
#settings2 .s2-secret{width:100%;max-width:340px;background:var(--paper);border:0;box-shadow:inset 0 0 0 1.5px var(--line-strong);border-radius:9px;color:var(--ink);font:12.5px/1.3 var(--s2-mono,ui-monospace,monospace);padding:10px 12px;outline:none;transition:box-shadow .12s ease}
#settings2 .s2-secret:focus{box-shadow:inset 0 0 0 1.5px var(--accent)}
#settings2 .s2-secret::placeholder{color:var(--ink-faint)}
#settings2 .loadcap.err{color:var(--danger)}

/* ── статус-точка: монохром + краска, как в списке сессий ────────────── */
#settings2 .dot { width:7px; height:7px; border-radius:50%; flex:none; box-sizing:border-box; background:var(--dot-sleep); }
#settings2 .dot.working { background:var(--ink); animation: s2pulse 2.2s ease-in-out infinite; }
#settings2 .dot.waiting { background:var(--accent); }
#settings2 .dot.done { background:transparent; border:1.5px solid var(--ink); }
@keyframes s2pulse { 0%,100%{opacity:1;transform:scale(1)} 50%{opacity:.4;transform:scale(.8)} }

/* ── значение справа (есть/нет/активна) ──────────────────────────────── */
#settings2 .sval { font-size:13.5px; color:var(--ink-mute); }
#settings2 .sval.on { color:var(--accent-text); }

/* ── Toggle 40×24 ────────────────────────────────────────────────────── */
#settings2 .toggle { appearance:none; -webkit-appearance:none; width:40px; height:24px; border-radius:12px; background:var(--fill-3); position:relative; outline:0; transition:background 130ms ease; flex:none; cursor:default; }
#settings2 .toggle:checked { background:var(--accent); }
#settings2 .toggle::after { content:""; position:absolute; top:2px; left:2px; width:20px; height:20px; border-radius:50%; background:#fff; box-shadow:0 1px 2px rgba(23,32,26,0.2); transition:left 130ms ease; }
#settings2 .toggle:checked::after { left:18px; }
#settings2 .toggle:disabled { opacity:.45; }

/* ── Segmented ───────────────────────────────────────────────────────── */
#settings2 .seg { display:flex; gap:3px; padding:3px; border:0; border-radius:var(--r-seg); background:var(--surface); }
#settings2 .segbtn { appearance:none; border:0; background:transparent; color:var(--ink-mute); font:500 12.5px/1 var(--s2-font); padding:5px 12px; border-radius:calc(var(--r-seg) - 2px); cursor:default; }
#settings2 .segbtn.active { background:var(--paper); color:var(--ink); font-weight:600; box-shadow:var(--shadow-raised); }

/* ── Button ──────────────────────────────────────────────────────────── */
#settings2 .btn { font:500 13px/1 var(--s2-font); color:var(--ink); background:var(--surface); border:0; border-radius:var(--r-seg); padding:10px 15px; cursor:default; display:inline-flex; align-items:center; gap:6px; transition:filter .12s ease, background .12s ease; }
#settings2 .btn:hover { background:var(--surface-2); }
#settings2 .btn:disabled { opacity:.5; }
#settings2 .btn.primary { background:var(--accent); color:var(--on-accent); font-weight:600; }
#settings2 .btn.primary:hover { background:var(--accent); filter:brightness(1.06); }
#settings2 .btn.danger { color:var(--danger); background:var(--danger-soft); }
#settings2 .btn.danger:hover { background:var(--danger-soft); filter:brightness(.97); }
#settings2 .btn.danger .ph { --icon-size:14px; width:14px; height:14px; }
#settings2 .btn.sm { padding:6px 11px; font-size:12px; }

/* ── Progress ────────────────────────────────────────────────────────── */
#settings2 .progress { height:4px; border-radius:999px; background:var(--surface-2); overflow:hidden; }
#settings2 .progress > i { display:block; height:100%; border-radius:999px; background:var(--accent); }
#settings2 .progress.striped > i { background-image:linear-gradient(90deg, var(--accent), var(--accent-ink), var(--accent)); background-size:200% 100%; animation: s2stripe 1.2s linear infinite; }
@keyframes s2stripe { to { background-position:200% 0; } }

/* ── Хоткей-поле (инлайн-рекордер) ───────────────────────────────────── */
#settings2 .dctl.hk { gap:6px; }
#settings2 .hkey { display:inline-flex; align-items:center; gap:8px; padding:8px 13px; border-radius:9px; background:var(--surface); border:0; transition:background .15s ease, box-shadow .15s ease; }
#settings2 .hkey kbd { font:500 13.5px/1 var(--s2-font); color:var(--ink); background:transparent; border:0; padding:0; }
#settings2 .hkey kbd.fix { color:var(--accent-text); }
#settings2 .hkey.rec { background:var(--accent-soft); cursor:default; }
#settings2 .hkey.rec:hover { filter:brightness(.97); }
#settings2 .hkey.rec kbd { color:var(--accent-text); }
#settings2 .hkey .shortcut-note { font:500 12.5px/1 var(--s2-font); color:var(--accent-text); }
#settings2 .hkey.recording { background:var(--accent); animation:s2hkpulse 1.2s ease-in-out infinite; }
#settings2 .hkey.recording kbd, #settings2 .hkey.recording .shortcut-note { color:var(--on-accent); }
@keyframes s2hkpulse { 0%,100% { box-shadow:0 0 0 3px var(--accent-soft); } 50% { box-shadow:0 0 0 6px var(--accent-soft); } }
#settings2 .hkey.none { box-shadow:inset 0 0 0 1.5px var(--line-strong); background:transparent; }
#settings2 .hknone { font:400 12.5px/1 var(--s2-font); color:var(--ink-faint); font-style:italic; }
#settings2 .hkreset { width:32px; height:32px; border-radius:8px; display:grid; place-items:center; background:transparent; border:0; color:var(--ink-faint); cursor:default; visibility:hidden; }
#settings2 .drow:hover .hkreset { visibility:visible; }
#settings2 .hkreset:hover { color:var(--ink); background:var(--fill-2); }
#settings2 .hkreset .ph { --icon-size:15px; width:15px; height:15px; }
#settings2 .drow.conflict { background:var(--danger-soft); }
#settings2 .drow.conflict .hkey { box-shadow:inset 0 0 0 1.5px var(--danger); }
#settings2 .hkerr { display:flex; align-items:center; gap:6px; margin-top:7px; font-size:12px; color:var(--danger); flex-wrap:wrap; }
#settings2 .hksteal { appearance:none; border:0; background:transparent; padding:0; font:500 12px/1 var(--s2-font); color:var(--accent-text); text-decoration:underline; text-underline-offset:2px; cursor:default; }

/* ── Custom Select ───────────────────────────────────────────────────── */
#settings2 .cselect { position:relative; display:inline-block; }
#settings2 .cstrigger { display:inline-flex; align-items:center; gap:8px; font:500 13.5px/1 var(--s2-font); color:var(--ink); background:var(--surface); border:0; border-radius:9px; padding:9px 12px; cursor:default; }
#settings2 .cstrigger:hover { background:var(--surface-2); }
#settings2 .cstrigger .chev { color:var(--ink-faint); transition:transform .15s ease; display:inline-flex; }
#settings2 .cselect.open .cstrigger { background:var(--accent-soft); }
#settings2 .cselect.open .cstrigger .chev { transform:rotate(180deg); }
#settings2 .cmenu { position:absolute; top:calc(100% + 5px); right:0; min-width:100%; z-index:60; background:var(--paper); border:0; border-radius:var(--r-card); padding:5px; box-shadow:var(--shadow-pop); display:none; }
#settings2 .cselect.open .cmenu { display:block; animation: s2fade .12s ease; }
#settings2 .copt { appearance:none; border:0; background:transparent; width:100%; text-align:left; font-family:inherit; display:flex; align-items:center; gap:9px; padding:9px 11px; border-radius:7px; font-size:13.5px; color:var(--ink-2); cursor:default; white-space:nowrap; }
#settings2 .copt:hover, #settings2 .copt:focus-visible { background:var(--accent-soft); color:var(--ink); }
#settings2 .copt .ck { margin-left:auto; color:var(--accent-text); opacity:0; display:inline-flex; }
#settings2 .copt .ck .ph { --icon-size:13px; width:13px; height:13px; }
#settings2 .copt.selected { color:var(--ink); font-weight:500; }
#settings2 .copt.selected .ck { opacity:1; }
/* загрузка модели: спиннер в триггере вместо шеврона + подпись loadcap */
#settings2 .cselect .spin { display:none; }
#settings2 .cselect.busy .spin { display:inline-flex; }
#settings2 .cselect.busy .chev { display:none; }
#settings2 .spin .ph { --icon-size:14px; width:14px; height:14px; color:var(--accent-text); animation: s2spin .8s linear infinite; }
@keyframes s2spin { to { transform:rotate(360deg); } }
#settings2 .loadcap { font-size:12px; color:var(--ink-mute); }

/* ── ошибка загрузки модели (инлайн) ─────────────────────────────────── */
#settings2 .s2err { display:flex; align-items:center; gap:6px; margin-top:6px; font-size:12px;
  color:var(--danger); max-width:340px; line-height:1.4; }
#settings2 .s2err .s2err-ic .ph { --icon-size:13px; width:13px; height:13px; }
#settings2 .s2err-txt { word-break:break-word; }

/* ── выбор краски: три точки, выбранная в кольце (14f «вид») ─────────── */
#settings2 .paints { display:flex; align-items:center; gap:10px; }
#settings2 .paintdot { appearance:none; border:0; padding:0; width:22px; height:22px; border-radius:50%; cursor:default; flex:none; }
#settings2 .paintdot.active { box-shadow:0 0 0 2px var(--paper), 0 0 0 3.5px currentColor; }
#settings2 .paintown { position:relative; overflow:hidden; display:inline-block; }
#settings2 .paintown input { position:absolute; inset:-4px; opacity:0; cursor:default; padding:0; border:0; }

/* ── lucide общая геометрия ──────────────────────────────────────────── */
#settings2 .ph { --icon-size:15px; width:15px; height:15px; stroke-width:2; vertical-align:middle; flex:none; }
#settings2 .snav .item .ic .ph { --icon-size:14px; width:14px; height:14px; }
#settings2 .ssearch .si .ph { --icon-size:15px; width:15px; height:15px; }
#settings2 .dnav button .ph { --icon-size:15px; width:15px; height:15px; }
#settings2 .range { -webkit-appearance:none; appearance:none; height:4px; border-radius:999px; background:var(--surface-2); outline:0; width:140px; }
#settings2 .range::-webkit-slider-thumb { -webkit-appearance:none; width:16px; height:16px; border-radius:50%; background:var(--accent); cursor:default; box-shadow:var(--shadow-raised); }

/* ── Превью уведомления (раздел «Уведомления») ───────────────────────── */
#settings2 .npvbox { display:flex; justify-content:center; padding:28px 20px 24px; border:0; border-radius:var(--r-card); background:var(--surface); margin-bottom:8px; position:relative; }
#settings2 .npvbox .tag { position:absolute; top:12px; left:16px; font:500 12px/1 var(--s2-font); letter-spacing:0; color:var(--ink-mute); text-transform:none; }
#settings2 .npvcard { width:344px; padding:15px 18px 16px 20px; border-radius:var(--r-panel); background:var(--paper); border:0; box-shadow:var(--shadow-panel); }
#settings2 .npvcard .row { display:flex; align-items:center; gap:10px; }
#settings2 .npvdot { width:7px; height:7px; border-radius:50%; box-sizing:border-box; background:transparent; border:1.5px solid var(--ink); flex:none; }
#settings2 .npvtitle { font-size:15px; font-weight:600; letter-spacing:-.01em; color:var(--ink); flex:1; min-width:0; white-space:nowrap; overflow:hidden; text-overflow:ellipsis; }
#settings2 .npvx { width:26px; height:26px; border-radius:50%; flex:none; display:grid; place-items:center; color:var(--ink-faint); font-size:11px; border:2px solid var(--surface-2); }
#settings2 .npvmeta { margin:6px 16px 0 20px; font-size:12px; color:var(--ink-mute); display:flex; gap:7px; flex-wrap:wrap; align-items:center; }
#settings2 .npvmeta:empty { display:none; }
#settings2 .npvmeta .br { color:var(--ink-faint); font-size:11.5px; }
#settings2 .npvmeta .md { color:var(--ink-mute); }
#settings2 .npvmeta .ef { font:600 10px/1 var(--s2-font); color:var(--ink-mute); background:var(--surface); border:0; border-radius:5px; padding:3px 6px; }
#settings2 .npvmeta .sp { color:var(--ink-faint); }
#settings2 .npvbody { font-size:13px; line-height:1.55; color:var(--ink-mute); margin:7px 16px 0 20px; }

/* ── Пояснительная плашка (пустое состояние вкладки) ─────────────────────
   Тональная подложка + одна плитка краской — как .ic в сайдбаре. */
#settings2 .s2note { display:flex; align-items:flex-start; gap:14px; padding:16px 18px;
  border-radius:var(--r-card); background:var(--surface); margin:0 0 22px; }
#settings2 .s2note-ic { width:30px; height:30px; border-radius:9px; flex:none; display:grid; place-items:center;
  background:var(--accent-soft); color:var(--accent-text); }
#settings2 .s2note-ic .ph { --icon-size:16px; width:16px; height:16px; }
#settings2 .s2note-t { font-size:13.5px; font-weight:500; color:var(--ink); margin-bottom:6px; }
#settings2 .s2note-p { font-size:12.5px; line-height:1.55; color:var(--ink-mute); max-width:520px; }
#settings2 .s2note-p + .s2note-p { margin-top:7px; }
#settings2 .s2note code { font:12px/1.4 var(--s2-mono); background:var(--paper); border-radius:5px; padding:1px 6px; color:var(--ink-2); }

/* ── Строка узла: на узкой панели не ломается — хост режется многоточием,
   статус не переносится, ошибка занимает всю ширину строки ─────────────── */
#settings2 .s2rmeta { max-width:none; white-space:nowrap; overflow:hidden; text-overflow:ellipsis; }
#settings2 .s2rstat { white-space:nowrap; }
#settings2 .s2rerr { max-width:none; }
#settings2 .s2rctl { flex-wrap:wrap; justify-content:flex-end; row-gap:6px; }
/* вторая дорога на машину (пароль) — равноправная с ключом, поэтому отделена
   линией, а не спрятана мелким шрифтом под ней */
#settings2 .s2rold { color:var(--warn); }
#settings2 .s2rpass { margin-top:12px; padding-top:12px; border-top:1px solid var(--line); }
#settings2 .s2rpass .s2rbtns { align-items:center; }
#settings2 .s2rpass input.s2-secret { flex:1 1 220px; min-width:0; }
#settings2 .s2rsteps { display:flex; flex-wrap:wrap; gap:8px 15px; list-style:none; padding:4px 0 16px; margin:0; border-bottom:1px solid var(--line); }
#settings2 .s2rsteps li { display:flex; align-items:center; gap:6px; font-size:10px; color:var(--ink-faint); line-height:1.4; }
#settings2 .s2rsteps li > span { display:grid; place-items:center; width:19px; height:19px; border:1px solid var(--line); border-radius:6px; font-size:10px; }
#settings2 .s2rsteps li.current { color:var(--info-text,var(--accent-text)); }
#settings2 .s2rsteps li.current > span { background:var(--info-soft,var(--accent-soft)); border-color:transparent; }
#settings2 .s2rsteps li.done { color:var(--success-text,var(--accent-text)); }
#settings2 .s2rtransport { display:flex; gap:7px; margin-bottom:13px; }
#settings2 .s2rtransport .selected { color:var(--info-text,var(--accent-text)); background:var(--info-soft,var(--accent-soft)); border-color:transparent; }
#settings2 .s2rfield { display:flex; flex:1; flex-direction:column; gap:7px; margin:14px 0 0; min-width:0; color:var(--ink-2); font-size:11px; }
#settings2 .s2rfield > .s2-secret { width:100%; max-width:none; min-width:0; }
#settings2 .s2rselect { width:100%; max-width:100%; font:inherit; color:var(--ink); border:0; padding-right:25px; }
#settings2 .s2raccess-line { display:flex; align-items:flex-end; gap:10px; flex-wrap:wrap; margin-top:14px; }
#settings2 .s2raccess-line .s2rfield { margin-top:0; min-width:160px; }
#settings2 .s2raccess-line > .dd { flex:1 1 180px; padding-bottom:5px; font-size:11px; }
#settings2 .s2raccess-grid { display:grid; grid-template-columns:repeat(2,minmax(0,1fr)); gap:12px; }
#settings2 .s2raccess-state { color:var(--ink-mute); font-size:11px; line-height:1.6; margin-top:12px; overflow-wrap:anywhere; }
#settings2 .s2raccess-state.on { color:var(--success-text,var(--accent-text)); }
#settings2 .s2raccess-state.warn { color:var(--warning-text,var(--warn)); }
#settings2 .s2raccess-state.error { color:var(--danger); }
#settings2 .s2radvanced { padding:0 14px 14px; }
#settings2 .s2radvanced > .dd { margin-top:12px; }
#settings2 .s2rconnection-summary > .drow { padding:0 14px 14px; }
#settings2 .s2rconnection-summary .s2rchange { float:right; margin-left:12px; color:var(--info-text,var(--accent-text)); font-size:11px; }
@media(max-width:650px) { #settings2 .s2raccess-grid { grid-template-columns:minmax(0,1fr); } #settings2 .s2rsteps { gap:8px 12px; } }

/* ── Форма в строке настройки: поля в ряд, на узкой панели переносятся ─── */
#settings2 .s2form { display:flex; flex-wrap:wrap; gap:8px; margin-top:11px; }
#settings2 .s2form input.s2-secret { flex:1 1 150px; width:auto; max-width:none; min-width:118px; }
#settings2 .s2hint { display:flex; align-items:center; gap:7px; margin-top:9px; font-size:12px; color:var(--ink-faint); }
#settings2 .s2hint kbd { font:500 11px/1.4 var(--s2-font); color:var(--ink-mute); background:var(--surface);
  border-radius:var(--r-key); padding:2px 6px; }

/* ── Мастер подключения машины: разведка, лог установки, ssh-ключ ─────────
   Вывод ssh (отказ разведки, ошибка установки) — это пошаговая инструкция с
   путями и командами: моноширинно, с переносами и БЕЗ обрезки, иначе совет
   теряет смысл. Прокрутка только у самого блока, страница не разъезжается. */
#settings2 .s2rpre { margin-top:9px; padding:10px 12px; border-radius:9px; background:var(--surface);
  font:12px/1.55 var(--s2-mono); color:var(--ink-2); white-space:pre-wrap; overflow-wrap:anywhere;
  max-height:230px; overflow-y:auto; }
#settings2 .s2rpre.bad { background:var(--danger-soft); color:var(--danger); }
/* галочки разведки: две колонки на широкой панели, один столбец на узкой */
#settings2 .s2rchecks { display:flex; flex-wrap:wrap; gap:7px 22px; margin-top:12px; }
#settings2 .s2rchk { display:flex; align-items:center; gap:8px; flex:1 1 160px; font-size:12.5px; color:var(--ink-2); }
#settings2 .s2rchk .nm { flex:1; min-width:0; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }
#settings2 .s2rchk .vl { flex:none; color:var(--ink-faint); }
#settings2 .s2rchk.on .vl { color:var(--accent-text); }
/* строка-пояснение с иконкой (откуда возьмётся узел, чего не хватает) */
#settings2 .s2rnode { display:flex; align-items:flex-start; gap:9px; margin-top:12px; padding-top:11px;
  border-top:1px solid var(--line); font-size:12.5px; line-height:1.5; color:var(--ink-mute); }
#settings2 .s2rnode + .s2rnode { border-top:0; padding-top:0; margin-top:7px; }
#settings2 .s2rnode .ph { margin-top:2px; color:var(--ink-faint); }
#settings2 .s2rnode.warn .ph { color:var(--warn); }
/* живой лог установки: фаза + сообщение, состояние — формой точки и цветом */
#settings2 .s2rlog { margin-top:11px; max-height:200px; overflow-y:auto; display:flex; flex-direction:column; gap:6px; }
#settings2 .s2rln { display:flex; align-items:flex-start; gap:9px; font-size:12.5px; line-height:1.45; color:var(--ink-mute); }
#settings2 .s2rln .dot { margin-top:5px; }
#settings2 .s2rln .install-phase { flex:none; width:82px; color:var(--ink-2); font-weight:500; }
#settings2 .s2rln .msg { flex:1; min-width:0; overflow-wrap:anywhere; }
#settings2 .s2rln.done .msg { color:var(--ink-2); }
#settings2 .s2rln.warn .dot { background:var(--warn); }
#settings2 .s2rln.warn .msg { color:var(--warn); }
#settings2 .s2rbtns { display:flex; flex-wrap:wrap; align-items:center; gap:8px; margin-top:11px; }
/* «узел уже стоит — добавить вручную»: тихая ссылка, а не вторая кнопка */
#settings2 .s2rmore { padding:12px 0 2px; }
#settings2 .s2rlink { appearance:none; border:0; background:transparent; padding:0; cursor:default;
  font:500 12.5px/1 var(--s2-font); color:var(--accent-text); text-decoration:underline; text-underline-offset:2px; }
`;
    const style = document.createElement('style');
    style.id = 'settings2-style';
    // Form controls are shared by Settings and the standalone Machines module.
    style.textContent = css.replaceAll('#settings2', ':is(#settings2, #machines2)');
    const workspaceStyle = document.querySelector('link[href="./workspace.css"]');
    if (workspaceStyle) document.head.insertBefore(style, workspaceStyle);
    else document.head.appendChild(style);
  }

  /* ========================================================================
   * Список вкладок сайдбара.
   * ====================================================================== */
  const NAV = [
    { pane: 'general', label: 'Основное', icon: 'settings', ic: 'gray', group: 'Приложение' },
    { pane: 'look', label: 'Вид', icon: 'palette', ic: 'green' },
    { pane: 'keys', label: 'Горячие клавиши', icon: 'keyboard', ic: 'violet' },
    { pane: 'notify', label: 'Уведомления', icon: 'bell', ic: 'amber' },
    { pane: 'agents', label: 'Агенты', icon: 'terminal', ic: 'violet', group: 'Агенты и подключения' },
    { pane: 'launch', label: 'Локальный запуск', icon: 'terminal', ic: 'green' },
    { pane: 'integration', label: 'Интеграция', icon: 'cable', ic: 'teal' },
    { pane: 'stt', label: 'Голосовой ввод', icon: 'mic', ic: 'blue', group: 'Голос' },
    { pane: 'voice', label: 'Голос', icon: 'volume-2', ic: 'green' },
    { pane: 'wake', label: 'Пробуждение', icon: 'mic', ic: 'blue' },
    { pane: 'awake', label: 'Бодрость', icon: 'coffee', ic: 'orange', group: 'Система' },
    { pane: 'service', label: 'Под капотом', icon: 'cpu', ic: 'purple' },
    { pane: 'about', label: 'О программе', icon: 'info', ic: 'gray' },
  ];

  // Search works before a pane has loaded: it must not open devices or make
  // service requests merely to discover a setting. Rendered rows enrich this
  // index with their current labels and descriptions (including custom agents).
  const SEARCH_ROWS = {
    general: [['Показать Jarvis', 'панель сочетание'], ['Позиция панели', 'центр угол'], ['Запускать при старте', 'автозапуск вход систему'], ['Режим логов', 'диагностика ошибки логи metrics']],
    look: [['Режим', 'окно оверлей'], ['Тема', 'светлая тёмная системная фон'], ['Краска', 'акцент цвет'], ['Внизу панели', 'лимит расход'], ['Плотность', 'размер строк'], ['Скругление', 'углы'], ['Масштаб', 'размер шрифт'], ['Сбросить настройку вида', 'сброс']],
    stt: [['Движок распознавания', 'диктовка stt whisper qwen модель'], ['Микрофон', 'устройство ввод звук'], ['Шумодав (VAD) · альфа', 'шум голос тишина'], ['Модели', 'скачать whisper qwen']],
    voice: [['Диктор', 'tts silero голос'], ['Скорость', 'темп речь'], ['Проверить голос', 'тест озвучка'], ['Без звука', 'заглушить'], ['Пауза чужого звука', 'музыка видео'], ['Только через Bluetooth', 'гарнитура наушники']],
    wake: [['Состояние микрофона', 'доступ разрешение'], ['Активация по фразе', 'hey jarvis wake'], ['Заглушить микрофон', 'выключить'], ['Порог срабатывания', 'чувствительность'], ['Модели openWakeWord', 'скачать']],
    notify: [['Текущая ветка', 'git'], ['Модель', 'claude codex'], ['Уровень усилия', 'reasoning effort'], ['Время', 'дата'], ['Когда агент закончил', 'завершение'], ['Когда ждёт тебя', 'вопрос ответ'], ['Продолжать после лимита', 'автоматически'], ['Позиция', 'карточки уведомления тост'], ['Автоскрытие', 'таймер длительность']],
    awake: [['Не спать', 'сон питание'], ['Держать, пока работают агенты', 'автоматически'], ['Не гасить заодно и экран', 'дисплей'], ['Работать с закрытой крышкой', 'ноутбук']],
    keys: [['Горячие клавиши', 'сочетания клавиатура хоткей shortcut диктовка панель']],
    launch: [['Терминал', 'tmux iterm kitty локальный запуск'], ['Шаблон команды', 'custom'], ['Команда прокси', 'proxy https сеть'], ['Разрешения без настроек задачи', 'опасный режим разрешения sandbox yolo legacy']],
    service: [['Бэкенд служебного LLM', 'claude codex модель'], ['Проверить ответ', 'тест'], ['Egress-прокси', 'proxy https сеть'], ['Подключить аккаунт', 'авторизация claude api ключ токен'], ['Модель Codex', 'gpt'], ['Глубина рассуждений', 'reasoning effort'], ['Codex-SDK сайдкар', 'установить python']],
    integration: [['Тихий режим', 'звук уведомления'], ['Переустановить интеграцию', 'claude codex cli хуки события подключение mcp'], ['Удалить интеграцию', 'отключить']],
    remotes: [['SSH-подключения', 'удалённые ssh сервер vps машина добавить подключение ключ пароль'], ['Виртуальные машины', 'agent-vm avm vm linux lima tart запустить остановить'], ['Docker', 'образ контейнер container docker изоляция'], ['Отдельная ветка', 'worktree git проект изоляция']],
    agents: [['Агенты', 'cli claude codex qwen opencode добавить']],
    about: [['Версия', 'обновление'], ['Лицензии', 'компоненты']],
  };
  const searchIndex = Object.entries(SEARCH_ROWS).filter(([pane]) => pane !== 'remotes').flatMap(([pane, rows]) => rows.map(([label, words]) => ({ pane, label, words })));
  let pendingSettingFocus = null;
  function focusSearchTarget(pane) {
    if (!pendingSettingFocus || pendingSettingFocus.pane !== pane || pane !== activePane) return;
    const label = pendingSettingFocus.label.toLocaleLowerCase();
    const node = currentRoot?.querySelector('#s2-pane-' + pane);
    if (!node || renderingPane[pane]) return;
    const title = [...node.querySelectorAll('.dt, .dsection, .dtitle')].find(n => n.textContent.toLocaleLowerCase() === label);
    const target = title?.closest('.drow') || title || node;
    for (let parent = target.parentElement; parent && parent !== node; parent = parent.parentElement) {
      if (parent.tagName === 'DETAILS') parent.open = true;
    }
    target.setAttribute('tabindex', '-1'); target.focus({ preventScroll: true });
    target.scrollIntoView?.({ block: 'center' });
    pendingSettingFocus = null;
  }

  /* ========================================================================
   * ОТРИСОВКА ОТДЕЛЬНЫХ ПАНЕЛЕЙ. Каждая async, грузит из IPC, заполняет
   * переданный контейнер pane (его внутренность очищается заранее).
   * ====================================================================== */

  // 1. Основное (general) — settings_get
  async function renderGeneral(pane) {
    pane.appendChild(el('div.dtitle', { text: 'Основное' }));
    const _sk = skelGroup(4); pane.appendChild(_sk);
    const s = await required(() => window.jarvis.getSettings());
    _sk.remove();
    const group = el('div.dgroup');

    // глобальный хоткей — тот же рекордер, что во вкладке «Горячие клавиши»
    const hkr = await safe(() => window.jarvis.hotkeyBindings(), null);
    const pb = hkr && hkr.ok && (hkr.bindings || []).find((x) => x.action === 'panel');
    if (pb) group.appendChild(hotkeyRow(pb, 'Открыть панель Jarvis из любого места.', {}));

    // позиция панели (seg: Центр / Угол)
    group.appendChild(drow('Позиция панели', 'Где появляется панель на экране.',
      segmented([{ value: 'center', label: 'Центр' }, { value: 'corner', label: 'Угол' }],
        s.position || 'center',
        (v) => required(() => window.jarvis.setSettings({ position: v })))));

    // автозапуск (перечитываем реальное состояние — система может отказать)
    group.appendChild(drow('Запускать при старте', 'Автозапуск при входе в систему.',
      toggle(s.openAtLogin, async (on) => {
        await required(() => window.jarvis.setSettings({ openAtLogin: on }));
        reRenderPane('general'); // отразить то, что реально записалось в систему
      })));

    // режим логов / диагностика
    group.appendChild(drow('Режим логов',
      'Тайминги пайплайна, RAM/CPU и события (доставка ответов, уведомления, лимиты) → ~/.jarvis/metrics.jsonl и jarvis.log. ' +
      'Без конф. данных: текст промптов/ответов, тело уведомлений и транскрипты не пишутся — только типы событий, счётчики и усечённые id сессий. Файлы локальные, никуда не отправляются.',
      toggle(!!s.diagnostics, (on) => required(() => window.jarvis.setSettings({ diagnostics: on })))));

    pane.appendChild(group);
  }

  // 2. Голосовой ввод (stt) — sttGet + modelsGet
  async function renderStt(pane) {
    pane.appendChild(el('div.dtitle', { text: 'Голосовой ввод' }));
    const _sk = skelGroup(3); pane.appendChild(_sk);
    const v = await required(() => window.jarvis.sttGet());
    _sk.remove();
    const group = el('div.dgroup');
    pane.appendChild(group);

    if (!v) {
      group.appendChild(drow('STT недоступен', 'Данные распознавания речи недоступны.', []));
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
        try { await required(() => window.jarvis.sttSetEngine(engine)); reRenderPane('stt'); }
        finally { sel.setBusy(false); cap.style.display = 'none'; }
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
    sel.node.querySelector('.cstrigger').setAttribute('aria-label', 'Движок распознавания');

    // устройство ввода (микрофон) — селектор + горячее применение (без перезапуска)
    const deviceRow = drow('Микрофон', 'Получаю список устройств…', []);
    group.appendChild(deviceRow);
    const dev = await safe(() => window.jarvis.sttInputDevices(), {
      devices: [], current: null, error: 'Не удалось получить список микрофонов. Попробуйте открыть раздел ещё раз.',
    });
    const deviceNames = [...new Set([...(dev.devices || []), ...(dev.current ? [dev.current] : [])])];
    const devOpts = [{ value: '', label: 'Системный по умолчанию' }]
      .concat(deviceNames.map((n) => ({ value: n, label: n })));
    const devSel = customSelect(devOpts, dev.current || '', async (name) => {
      if (devSel.setBusy) devSel.setBusy(true);
      await required(() => window.jarvis.sttSetInputDevice(name || null));
      if (devSel.setBusy) devSel.setBusy(false);
    });
    deviceRow.replaceWith(drow('Микрофон',
      dev.error || 'С какого устройства писать речь. Выбери встроенный микрофон, если гарнитура шумит.',
      devSel.node));

    // клавиша диктовки — общий рекордер (пресеты убраны: запись работает)
    const hkr = await safe(() => window.jarvis.hotkeyBindings(), null);
    const db = hkr && hkr.ok && (hkr.bindings || []).find((x) => x.action === 'dictation');
    if (db) group.appendChild(hotkeyRow(db, 'Зажми и говори (push-to-talk). Кликни и нажми новое сочетание.', {}));

    // шумодав (VAD-гейт): пропускать диктовку, если речи не слышно. АЛЬФА.
    group.appendChild(drow('Шумодав (VAD) · альфа',
      'Пропускает диктовку, если речи не слышно (фон/тишина). Пока нестабилен и может портить распознавание — по умолчанию выключен. Включайте на свой риск.',
      toggle(!!v.noiseGate, (on) => required(() => window.jarvis.sttSetNoiseGate(on)))));

    // тест микрофона
    group.appendChild(renderMicTestRow());

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
    const r = await required(() => window.jarvis.modelsGet());
    group.textContent = '';
    const models = (r && r.models) || [];
    if (r.error) {
      group.appendChild(drow('Не удалось загрузить модели', r.error, []));
      return;
    }
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
      if (!await action(b, () => window.jarvis.modelsInstall(ids))) {
        b.disabled = false; b.textContent = 'Скачать выбранное';
      }
    });
    return b;
  }

  // выбор download-action по id (порт downloadActionFor из renderer.js)
  function downloadActionFor(m) {
    if (m.present || m.available === false) return null;
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
    if (m.available === false) grow.appendChild(el('div.dd', { text: m.unavailableReason || 'Недоступно в этой сборке.' }));
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
        if (!await action(btn, () => window.jarvis.modelsInstall([m.id]))) {
          btn.disabled = false; btn.textContent = 'Повторить';
        }
      });
      const cb = el('input', { type: 'checkbox', 'aria-label': 'Выбрать модель ' + m.label, style: 'margin-right:6px;vertical-align:middle' });
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
    if (m.kind === 'stt' && m.present && m.available !== false && !m.active) {
      ctl.appendChild(button('Сделать активной', async (b) => {
        b.disabled = true; b.textContent = 'Включаю…';
        await required(() => window.jarvis.sttSetEngine(m.id));
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
    const v = await required(() => window.jarvis.voiceGet());
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
        await required(() => window.jarvis.voiceSetSpeaker(sp));
      });
      group.appendChild(drow('Диктор', 'Голос синтеза · движок ' + (v.engine || 'Silero') + ', локально.', sel.node));
    }

    // скорость — segmented
    const RATE_LABELS = { slow: 'медленно', medium: 'норма', fast: 'быстро', 'x-fast': 'очень' };
    const rates = (v.rates || ['slow', 'medium', 'fast', 'x-fast']).map((r) => ({ value: r, label: RATE_LABELS[r] || r }));
    group.appendChild(drow('Скорость', 'Темп речи.',
      segmented(rates, v.rate, (r) => required(() => window.jarvis.voiceSetRate(r)))));

    // тест
    group.appendChild(drow('Проверить голос', 'Сказать короткий образец вслух.',
      button('Тест', () => required(() => window.jarvis.voiceTest()), 'sm')));

    // без звука
    group.appendChild(drow('Без звука', 'Временно заглушить озвучку.',
      toggle(v.mute, (on) => required(() => window.jarvis.voiceSetMute(on)))));

    // пауза чужого звука (duck)
    group.appendChild(drow('Пауза чужого звука', 'Приглушать музыку/видео на время реплики, как Siri.',
      toggle(v.duck !== false, (on) => required(() => window.jarvis.voiceSetDuck(on)))));

    // озвучка только при Bluetooth-гарнитуре
    group.appendChild(drow('Только через Bluetooth', 'Озвучивать, лишь когда подключена Bluetooth-гарнитура.',
      toggle(v.bluetoothOnly !== false, (on) => required(() => window.jarvis.voiceSetBluetoothOnly(on)))));

    pane.appendChild(group);
  }

  // 4. Пробуждение (wake) — wakeGet
  async function renderWake(pane) {
    pane.appendChild(el('div.dtitle', { text: 'Пробуждение' }));
    const _sk = skelGroup(4); pane.appendChild(_sk);
    const v = await required(() => window.jarvis.wakeGet());
    _sk.remove();
    const group = el('div.dgroup');
    if (!v) {
      group.appendChild(drow('Wake-word недоступен', 'Данные активации по фразе недоступны.', []));
      pane.appendChild(group);
      return;
    }
    const audioLabels = { 'permission-pending': 'Ожидаем разрешения на микрофон', starting: 'Подключаем микрофон…',
      denied: 'Нет доступа к микрофону', 'no-device': 'Микрофон не найден', listening: 'Микрофон подключён', muted: 'Микрофон заглушён', idle: 'Микрофон не используется' };
    const audioState = v.muted ? 'muted' : v.audio_state || (v.listening ? 'listening' : 'idle');
    const hint = audioState === 'permission-pending' ? 'Ответьте на системный запрос macOS, затем повторите включение.'
      : audioState === 'denied' ? 'Системные настройки → Конфиденциальность → Микрофон → Jarvis.'
      : 'Актуальное состояние захвата звука.';
    group.appendChild(drow('Состояние микрофона', hint, el('span.sval', { text: audioLabels[audioState] || audioState, role: 'status' })));

    // вкл/выкл активацию по фразе
    group.appendChild(drow('Активация по фразе',
      v.model_present
        ? 'Скажи «Hey Jarvis», чтобы разбудить ассистента. Работает офлайн.'
        : 'Сначала скачайте модель openWakeWord ниже, чтобы включить.',
      toggle(!!v.enabled, async (on) => { await required(() => window.jarvis.wakeSetEnabled(on)); reRenderPane('wake'); }, !v.model_present)));

    // заглушить микрофон (mute у источника)
    group.appendChild(drow('Заглушить микрофон', 'Полностью отключить микрофон у источника.',
      toggle(!!v.muted, async (on) => { await required(() => window.jarvis.audioSetMute(on)); reRenderPane('wake'); })));

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
        await required(() => window.jarvis.wakeInstallModels());
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
    const s = await required(() => window.jarvis.getSettings());
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

    const saveContent = async (k, on) => {
      const next = Object.assign({}, content, { [k]: on });
      await required(() => window.jarvis.setSettings({ notify: Object.assign({}, nf, { content: next }) }));
      content[k] = on; renderPreview();
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
      toggle(s.notifyDone, (on) => required(() => window.jarvis.setSettings({ notifyDone: on })))));
    eg.appendChild(drow('Когда ждёт тебя', 'Уведомлять, когда агенту нужен ответ.',
      toggle(s.notifyWaiting, (on) => required(() => window.jarvis.setSettings({ notifyWaiting: on })))));
    eg.appendChild(drow('Продолжать после лимита', 'Авто-«продолжай» при сбросе лимита.',
      toggle(s.autoResume !== false, (on) => required(() => window.jarvis.setSettings({ autoResume: on })))));
    pane.appendChild(eg);

    pane.appendChild(el('div.dsection', { text: 'Вид и поведение' }));
    const vg = el('div.dgroup');
    vg.appendChild(drow('Позиция', 'где появляются карточки',
      segmented([{ value: 'center', label: 'Центр' }, { value: 'corner', label: 'Угол' }],
        s.position || 'center', (v) => required(() => window.jarvis.setSettings({ position: v })))));
    const ttlNow = (typeof nf.ttlSec === 'number') ? nf.ttlSec : 8;
    vg.appendChild(drow('Автоскрытие', 'через сколько прятать карточку (после озвучки, если она есть)',
      segmented([{ value: 5, label: '5с' }, { value: 8, label: '8с' }, { value: 0, label: 'Не прятать' }],
        ttlNow, (v) => required(() => window.jarvis.setSettings({ notify: Object.assign({}, nf, { ttlSec: Number(v) }) })))));
    pane.appendChild(vg);
  }

  // 6. Бодрость (awake) — keep-awake через плагины (getPlugins / pluginCmd)
  async function renderAwake(pane) {
    pane.appendChild(el('div.dtitle', { text: 'Бодрость' }));
    const _sk = skelGroup(3); pane.appendChild(_sk);
    const plugins = await required(() => window.jarvis.getPlugins());
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
    const runSeg = async (id) => {
      const map = {
        off: () => window.jarvis.pluginCmd('keep-awake', 'stop'),
        '15m': () => window.jarvis.pluginCmd('keep-awake', 'start-timer', { minutes: 15 }),
        '1h': () => window.jarvis.pluginCmd('keep-awake', 'start-timer', { minutes: 60 }),
        '4h': () => window.jarvis.pluginCmd('keep-awake', 'start-timer', { minutes: 240 }),
        inf: () => window.jarvis.pluginCmd('keep-awake', 'start-manual'),
      };
      await required(map[id]);
      setTimeout(() => reRenderPane('awake'), 300);
    };
    group.appendChild(drow('Не спать', 'Не давать маку засыпать, пока работают агенты.',
      segmented(SEG, seg, runSeg)));

    // держать, пока работают агенты + не гасить экран
    group.appendChild(drow('Держать, пока работают агенты', 'Авто-включение при активных сессиях.',
      toggle(!!st.autoEnabled, (on) => required(() => window.jarvis.pluginCmd('keep-awake', 'set', { auto: on })))));
    group.appendChild(drow('Не гасить заодно и экран', 'Дисплей тоже остаётся активным.',
      toggle(!!st.keepDisplayOn, (on) => required(() => window.jarvis.pluginCmd('keep-awake', 'set', { keepDisplayOn: on })))));

    // крышка (clamshell) — Спать / Не спать
    const cs = byId('clamshell');
    if (cs) {
      const armed = !!(cs.status && cs.status.armed);
      group.appendChild(drow('Работать с закрытой крышкой', 'Не засыпать при закрытии крышки · требует питания от сети.',
        segmented([{ value: 'sleep', label: 'Спать' }, { value: 'keep', label: 'Не спать' }],
          armed ? 'keep' : 'sleep',
          async (val) => {
            if (val === 'keep') {
              if (cs && cs.enabled === false) await required(() => window.jarvis.pluginCmd('clamshell', '_enable', { on: true }));
              await required(() => window.jarvis.pluginCmd('clamshell', 'arm'));
            } else {
              await required(() => window.jarvis.pluginCmd('clamshell', 'disarm'));
            }
            setTimeout(() => reRenderPane('awake'), 300);
          })));
    }

    pane.appendChild(group);
  }

  // 7. Горячие клавиши (keys) — hotkey_bindings (единый реестр действий)
  async function renderKeys(pane) {
    pane.appendChild(el('div.dtitle', { text: 'Горячие клавиши' }));
    // На Wayland (Sway, Hyprland, GNOME) клавиатуру раздаёт композитор:
    // приложение не может перехватить сочетание глобально, и молчать об этом
    // нельзя — человек будет думать, что сломалось у него.
    const meta = await safe(() => window.jarvis.getMeta(), null);
    if (meta && meta.wayland) {
      pane.appendChild(el('div.dgroup', null, [
        drow(
          'Wayland: клавиши у композитора',
          'Глобальные сочетания здесь раздаёт не приложение. Повесь их в конфиге: '
            + 'bindsym $mod+j exec jarvis --toggle (ещё понимает --show, --hide, --quit). '
            + 'Готовый кусок — docs/sway/jarvis.conf.',
          []
        ),
      ]));
    }
    const _sk = skelGroup(4); pane.appendChild(_sk);
    const r = await safe(() => window.jarvis.hotkeyBindings(), null);
    _sk.remove();
    if (!r || !r.ok) {
      pane.appendChild(el('div.dgroup', null, [drow('Недоступно', 'Не удалось получить привязки хоткеев.', [])]));
      return;
    }
    const by = {};
    for (const x of r.bindings || []) by[x.action] = x;
    const DESC = {
      panel: 'Показать или скрыть Jarvis.',
      continue: 'Возобновить последнюю сессию.',
      repeat: 'Повторить последнее уведомление.',
      select: 'Выбрать вариант активного вопроса — сочетание + цифра.',
      mute: 'Заглушить уведомления и голос.',
      quiet: 'Копить статистику без тостов.',
      dictation: 'Зажми и говори (push-to-talk). Дублируется в «Голосовом вводе».',
    };
    const GROUPS = [
      ['Панель и сессии', ['panel', 'continue', 'repeat', 'select']],
      ['Звук и уведомления', ['mute', 'quiet']],
      ['Голос', ['dictation']],
    ];
    // перехват меняет ЧУЖУЮ строку («не назначен») — перерисовать вкладку
    const after = () => reRenderPane('keys');
    for (const [title, actions] of GROUPS) {
      pane.appendChild(el('div.dsection', { text: title }));
      const g = el('div.dgroup');
      for (const id of actions) if (by[id]) g.appendChild(hotkeyRow(by[id], DESC[id], { after }));
      pane.appendChild(g);
    }
  }

  // 8. Интеграция (integration) — integrationGet
  async function renderIntegration(pane) {
    pane.appendChild(el('div.dtitle', { text: 'Интеграция' }));
    const _sk = skelGroup(3); pane.appendChild(_sk);
    const info = await required(() => window.jarvis.integrationGet());
    _sk.remove();
    if (!info) {
      pane.appendChild(el('div.dgroup', null, [drow('Данные недоступны', 'Не удалось получить статус интеграции.', [])]));
      return;
    }
    const st = info.status || {};
    const readiness = info.readiness || {};
    const integrated = typeof readiness.coreReady === 'boolean'
      ? readiness.coreReady
      : (st.hooks && st.shim);

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
    devGroup.appendChild(drow('Тихий режим', `Копить статистику без тостов, голоса и показа панели · ${window.jarvisKeys.k('J', { alt: true })}.`,
      toggle(!!info.quiet, (on) => required(() => window.jarvis.quietSet(on)))));
    pane.appendChild(devGroup);

    // управление и диск
    pane.appendChild(el('div.dsection', { text: 'Управление и диск' }));
    const manGroup = el('div.dgroup');
    manGroup.appendChild(drow('Переустановить интеграцию', 'Обновить хуки, шим и транспорт.',
      button(integrated ? 'Переустановить' : 'Настроить', () => required(() => window.jarvis.onboardingOpen()), 'sm')));
    if (integrated) {
      const rm = el('button.btn.sm.danger', { text: 'Удалить' });
      let armed = false;
      rm.addEventListener('click', async () => {
        if (!armed) { armed = true; rm.textContent = 'Точно удалить?'; setTimeout(() => { armed = false; rm.textContent = 'Удалить'; }, 3000); return; }
        rm.disabled = true; rm.textContent = 'Удаляю…';
        if (!await action(rm, () => window.jarvis.integrationRemove())) {
          armed = false; rm.disabled = false; rm.textContent = 'Удалить'; return;
        }
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
    const a = await required(() => window.jarvis.claudeAuthGet());
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
          await required(() => window.jarvis.claudeAuthDisconnect());
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
    const v = await required(() => window.jarvis.serviceGet());
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
        await required(() => window.jarvis.serviceSetBackend(b));
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
        testOut.style.color = 'var(--accent-text)';
        testOut.textContent = '✓ ' + (r.result || '') + (r.ms ? `   ·   ${(r.ms / 1000).toFixed(1)} с` : '');
      } else {
        testOut.style.color = 'var(--danger)';
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
      await required(() => window.jarvis.serviceSetModel(m));
    });
    cg.appendChild(drow('Модель Codex',
      'Для служебных вызовов через Codex. Список — из codex (включая gpt-5.3-codex-spark). «По умолчанию» — модель из codex config.', msel.node));

    const efforts = (v.efforts || ['low', 'medium', 'high']).map((e) => ({ value: e, label: e }));
    const esel = customSelect(efforts, v.codexEffort || 'low', async (e) => {
      await required(() => window.jarvis.serviceSetEffort(e));
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
        if (!await action(btn, () => window.jarvis.codexInstallSidecar())) {
          btn.disabled = false; btn.textContent = 'Повторить';
        }
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

  // Запуск — параметры запуска сессии из вкладки «Проекты»: терминал, прокси-команда,
  // «опасный режим». Флэт-ключи settings (launchTerminal/launchCustomCmd/launchProxyCmd/
  // launchDangerous) пишутся через generic setSettings (поверхностный merge).
  async function renderLaunch(pane) {
    pane.appendChild(el('div.dtitle', { text: 'Локальный запуск' }));
    pane.appendChild(el('p.s2-intro', { text: 'Как открывать агента на этом компьютере. Машину, рабочую папку и разрешения конкретной задачи выбирай в проектах или новом чате.' }));
    const _sk = skelGroup(3); pane.appendChild(_sk);
    const s = await required(() => window.jarvis.getSettings());
    _sk.remove();
    const term = s.launchTerminal || 'terminal-app';
    const group = el('div.dgroup');

    // выбор терминала
    // на Linux Terminal.app/iTerm2 не существует — предлагаем тамошние эмуляторы;
    // «Авто» отдаёт выбор коду: он берёт первый найденный в PATH
    const TERMINALS = window.jarvisKeys.isMac
      ? [
          { value: 'terminal-app', label: 'Terminal.app' },
          { value: 'iterm2', label: 'iTerm2' },
        ]
      : [
          { value: 'terminal-app', label: 'Авто (первый найденный)' },
          { value: 'gnome-terminal', label: 'GNOME Terminal' },
          { value: 'konsole', label: 'Konsole' },
          { value: 'ptyxis', label: 'Ptyxis' },
          { value: 'xfce4-terminal', label: 'Xfce Terminal' },
          { value: 'kitty', label: 'kitty' },
          { value: 'alacritty', label: 'Alacritty' },
          { value: 'wezterm', label: 'WezTerm' },
          { value: 'foot', label: 'foot' },
          { value: 'tilix', label: 'Tilix' },
          { value: 'terminator', label: 'Terminator' },
          { value: 'xterm', label: 'xterm' },
        ];
    const termSel = customSelect(
      [...TERMINALS, { value: 'custom', label: 'Кастомная команда' }],
      term,
      async (v) => {
        await required(() => window.jarvis.setSettings({ launchTerminal: v }));
        reRenderPane('launch'); // показать/скрыть поле шаблона
      });
    group.appendChild(drow('Терминал',
      'Приложение для локальных сессий и кнопки «Открыть терминал». Для другого эмулятора выбери свою команду.', termSel.node));

    // шаблон кастомной команды — только для custom
    if (term === 'custom') {
      const tmplInput = el('input.s2-secret', {
        type: 'text',
        placeholder: window.jarvisKeys.isMac ? 'ghostty -e bash -lc {cmd}' : 'kitty sh -lc {cmd}',
        autocomplete: 'off', spellcheck: 'false', value: s.launchCustomCmd || '',
      });
      const tmplCap = el('span.loadcap', { style: 'display:none' });
      const tmplSave = button('Сохранить', async (b) => {
        const val = (tmplInput.value || '').trim();
        b.disabled = true; b.textContent = 'Сохраняю…';
        await required(() => window.jarvis.setSettings({ launchCustomCmd: val }));
        b.disabled = false; b.textContent = 'Сохранить';
        tmplCap.style.display = ''; tmplCap.textContent = 'сохранено ✓';
      }, 'sm primary');
      tmplInput.addEventListener('keydown', (e) => { if (e.key === 'Enter') tmplSave.click(); });
      group.appendChild(el('div.drow', null, [
        el('div.grow', null, [
          el('div.dt', { text: 'Шаблон команды' }),
          el('div.dd', { text: 'Плейсхолдер {cmd} заменяется на команду запуска (cd + агент). Без {cmd} запуск не сработает.' }),
          tmplInput, tmplCap,
        ]),
        el('div.dctl', null, [tmplSave]),
      ]));
    }

    pane.appendChild(group);
    const advanced = el('div.dgroup');

    // команда прокси — выполняется ПЕРЕД запуском агента
    const proxyInput = el('input.s2-secret', {
      type: 'text', placeholder: 'export HTTPS_PROXY=http://…',
      autocomplete: 'off', spellcheck: 'false', value: s.launchProxyCmd || '',
    });
    const proxyCap = el('span.loadcap', { style: 'display:none' });
    const proxySave = button('Сохранить', async (b) => {
      const val = (proxyInput.value || '').trim();
      b.disabled = true; b.textContent = 'Сохраняю…';
      await required(() => window.jarvis.setSettings({ launchProxyCmd: val }));
      b.disabled = false; b.textContent = 'Сохранить';
      proxyCap.style.display = ''; proxyCap.textContent = val ? 'сохранено ✓' : 'очищено';
    }, 'sm primary');
    proxyInput.addEventListener('keydown', (e) => { if (e.key === 'Enter') proxySave.click(); });
    advanced.appendChild(el('div.drow', null, [
      el('div.grow', null, [
        el('div.dt', { text: 'Команда прокси' }),
        el('div.dd', { text: 'Переменные окружения перед локальным запуском агента, например export HTTPS_PROXY=… . Сеть служебного LLM настраивается отдельно в «Под капотом».' }),
        proxyInput, proxyCap,
      ]),
      el('div.dctl', null, [proxySave]),
    ]));

    // Compatibility fallback only: the explicit mode from project/chat wins.
    const fallback = customSelect([
      { value: 'ask', label: 'С подтверждением' }, { value: 'yolo', label: 'Полный доступ' },
    ], s.launchDangerous ? 'yolo' : 'ask', mode => required(() => window.jarvis.setSettings({ launchDangerous: mode === 'yolo' })));
    advanced.appendChild(drow('Разрешения без настроек задачи',
      'Для старых способов запуска, которые не передают свой режим. Выбор разрешений в новом чате или проекте всегда важнее этой настройки.', fallback.node));
    pane.appendChild(settingsDetails('s2-launch-advanced', 'Дополнительные настройки запуска', [advanced]));
  }

  /* 1b. Вид (look) — тема, краска и что показывать внизу панели.
   * Раздел «вид» экрана 14f: тема переключается сегментом, краска — точкой,
   * нижняя полоска панели показывает лимит подписки или расход за день. */
  async function renderLook(pane) {
    pane.appendChild(el('div.dtitle', { text: 'Вид' }));
    const _sk = skelGroup(3); pane.appendChild(_sk);
    const s = await required(() => window.jarvis.getSettings());
    _sk.remove();

    const cur = (window.jarvisTheme && window.jarvisTheme.get()) || { theme: 'light', paint: 'clover', mode: 'overlay' };
    const group = el('div.dgroup');

    // режим окна: накладка ⌘J поверх всего или обычное окно со списком слева (14h)
    group.appendChild(drow('При запуске',
      `Быстрый доступ также открывается по ${window.jarvisKeys.k('J')}.`,
      segmented(
        [{ value: 'overlay', label: 'быстрый доступ' }, { value: 'window', label: 'рабочее окно' }],
        cur.mode || 'overlay',
        (v) => { window.jarvisTheme && window.jarvisTheme.set({ mode: v }); })));

    group.appendChild(drow('Тема', null,
      segmented(
        [{ value: 'light', label: 'светлая' }, { value: 'dark', label: 'тёмная' }, { value: 'auto', label: 'системная' }],
        cur.theme,
        (v) => { window.jarvisTheme && window.jarvisTheme.set({ theme: v }); })));

    // краска: точки, выбранная в кольце своего же цвета; последняя — своя,
    // по клику открывает системный пикер и выводит из тона всю акцентную семью
    const PAINTS = [
      { value: 'coal', label: 'Уголь', color: '#1B1A16' },
      { value: 'clover', label: 'Клевер', color: '#0B6B44' },
      { value: 'raspberry', label: 'Малина', color: '#C0103F' },
    ];
    const paints = el('div.paints');
    const clearRings = () => { for (const x of paints.querySelectorAll('.paintdot')) x.classList.remove('active'); };
    for (const pnt of PAINTS) {
      const b = el('button.paintdot' + (pnt.value === cur.paint ? '.active' : ''), { title: pnt.label });
      b.style.background = pnt.color;
      b.style.color = pnt.color; // кольцо выбранного берёт currentColor
      b.addEventListener('click', () => {
        clearRings();
        b.classList.add('active');
        if (window.jarvisTheme) window.jarvisTheme.set({ paint: pnt.value });
      });
      paints.appendChild(b);
    }
    // «своя»: сам кружок — это <input type=color>, поэтому клик сразу открывает
    // системный пикер, а не заводит лишний шаг «сначала выбери, потом настрой»
    const own = el('label.paintdot.paintown' + (cur.paint === 'custom' ? '.active' : ''),
      { title: 'Своя краска' });
    const ownInput = el('input', { type: 'color', 'aria-label': 'Свой цвет акцента' });
    ownInput.value = cur.accent || '#0B6B44';
    own.style.background = ownInput.value;
    own.style.color = ownInput.value;
    ownInput.addEventListener('input', () => {
      own.style.background = ownInput.value;
      own.style.color = ownInput.value;
      clearRings();
      own.classList.add('active');
      if (window.jarvisTheme) window.jarvisTheme.set({ paint: 'custom', accent: ownInput.value });
    });
    own.appendChild(ownInput);
    paints.appendChild(own);

    group.appendChild(drow('Краска', 'Цвет основных действий. Последний образец — свой цвет.', paints));

    group.appendChild(drow('Внизу панели', 'Полоска лимита подписки с окном до сброса — или расход за день.',
      segmented(
        [{ value: 'limit', label: 'лимит' }, { value: 'spend', label: 'расход' }],
        s.footerBottom === 'spend' ? 'spend' : 'limit',
        async (v) => {
          await required(() => window.jarvis.setSettings({ footerBottom: v }));
          window.dispatchEvent(new CustomEvent('jarvis:footer-bottom', { detail: v }));
        })));

    pane.appendChild(group);

    /* ── Настройка вида: плотность, скругление, масштаб ──────────────────
     * Всё это переопределяет токены дизайн-системы, поэтому меняется живьём
     * и одинаково во всех окнах. */
    pane.appendChild(el('div.dsection', { text: 'Размер и форма' }));
    const tune = el('div.dgroup');

    tune.appendChild(drow('Плотность', null,
      segmented(
        [{ value: 'compact', label: 'плотно' }, { value: 'normal', label: 'обычно' }, { value: 'roomy', label: 'просторно' }],
        cur.density || 'normal',
        (v) => { window.jarvisTheme && window.jarvisTheme.set({ density: v }); })));

    tune.appendChild(drow('Скругление', null,
      segmented(
        [{ value: 'sharp', label: 'острое' }, { value: 'normal', label: 'обычное' }, { value: 'soft', label: 'мягкое' }],
        cur.radius || 'normal',
        (v) => { window.jarvisTheme && window.jarvisTheme.set({ radius: v }); })));

    // масштаб — ползунок с живым значением: тянешь и сразу видишь результат
    const scaleVal = el('span.sval', { text: Math.round((cur.scale || 1) * 100) + '%' });
    const scale = el('input.range', { type: 'range', min: '85', max: '140', step: '5' });
    scale.value = String(Math.round((cur.scale || 1) * 100));
    scale.addEventListener('input', () => {
      scaleVal.textContent = scale.value + '%';
      if (window.jarvisTheme) window.jarvisTheme.set({ scale: Number(scale.value) / 100 });
    });
    tune.appendChild(drow('Масштаб', 'Размер текста и элементов.',
      [scaleVal, scale]));

    tune.appendChild(drow('Сбросить настройку вида', 'Вернуть плотность, скругление и масштаб к заводским.',
      button('Сбросить', () => {
        if (window.jarvisTheme) window.jarvisTheme.reset();
        reRenderPane('look');
      }, 'sm')));

    pane.appendChild(tune);
  }

  /* 1c. Удалённые (remotes) — узлы на других машинах.
   * Узел = тонкий транспорт на VPS: принимает хуки, копит события и умеет
   * tmux send-keys; ходим к нему по SSH. Здесь — только список, проверка
   * связи и добавление; всё остальное делает ноут своим обычным кодом.
   *
   * Контракт IPC: remotesList / remotesAdd / remotesRemove / remotesTest —
   * список и ручное добавление; remotesPreflight / remotesInstall /
   * remotesSshKey + события onRemoteInstallStep/Done — мастер «настроить VPS
   * с нуля». Старая сборка без этих методов не должна ронять панель — отсюда
   * safe() и явная проверка наличия метода перед показом каждой части. */
  function remotesApiReady() {
    try { return !!(window.jarvis && typeof window.jarvis.remotesList === 'function'); } catch (e) { return false; }
  }
  // мастер требует и разведку, и установку: без любой из них дороги «с нуля» нет
  function remotesWizardReady() {
    try {
      return !!(window.jarvis && typeof window.jarvis.remotesPreflight === 'function'
        && typeof window.jarvis.remotesInstall === 'function');
    } catch (e) { return false; }
  }

  // The selected host owns its actions; inventory cards are navigation only.
  const machineView = { selected: '', filter: 'all', query: '', editor: false, detail: 'overview', notifiedConnection: '' };
  function remoteEditorOpen() { return !!currentRoot?.querySelector('#s2-ssh-setup')?.open; }
  function resumeRemoteWork() {
    if (!remoteWiz.install?.running && !remoteWiz.manualSaving) return false;
    machineView.editor = true; reRenderPane('remotes'); return true;
  }
  function machineAddress(r) {
    const host = r.sshHost || '';
    const at = host.lastIndexOf('@');
    return { address: at < 0 ? host : host.slice(at + 1), user: at < 0 ? '' : host.slice(0, at) };
  }
  function machineBadge(text, state) { return el('span.connection-badge', { text, 'data-state': state }); }
  function remoteRow(r, changed = () => {}) {
    const panel = el('section.connection-host', { 'data-machine-name': r.name, 'aria-label': 'Подключение ' + r.name });
    const avatar = el('span.connection-avatar.large', { 'data-kind': r.transport === 'teleport' ? 'teleport' : 'ssh', 'aria-hidden': 'true' }, icon(r.transport === 'teleport' ? 'shield-check' : 'server'));
    const stat = machineBadge('', '');
    const meta = el('div.connection-host-meta', null, [el('span', { text: r.transport === 'teleport' ? 'Teleport' : 'SSH' }), stat]);
    const head = el('div.connection-host-head', null, [avatar, el('div', null, [el('h2', { text: r.name || 'Машина' }), meta])]);
    panel.append(head);
    const errLine = el('div.connection-alert', { role: 'status' });
    const paint = (on, text, error) => {
      stat.textContent = text; stat.dataset.state = on ? 'connected' : error ? 'error' : 'unknown';
      errLine.textContent = remoteDisplayText(error || ''); errLine.hidden = !error;
    };
    paint(!!r.connected, r.connected ? 'На связи' : r.error ? 'Нет связи' : 'Не проверено', r.connected ? null : r.error);
    panel.append(errLine);
    const actions = el('div.connection-primary-actions');
    if (r.connected && window.jarvisSessionWorkspace?.newChat) actions.appendChild(button('Новый чат', () => window.jarvisSessionWorkspace.newChat({ machine: r.name, cwd: '' }), 'primary'));
    const test = button('Проверить', async b => {
      b.disabled = true; b.textContent = 'Проверяем…';
      const result = await safe(() => window.jarvis.remotesTest(r.name), null);
      b.disabled = false; b.textContent = 'Проверить';
      r.connected = !!result?.ok; r.error = result?.ok ? null : result?.error || 'Машина не ответила. Проверь доступ к ней.';
      if (result?.ok && result.version) r.version = result.version;
      if (result?.ok && Array.isArray(result.sources)) r.sources = result.sources;
      paint(r.connected, r.connected ? 'На связи' : 'Нет связи', r.error);
      changed(r);
    }, r.connected ? '' : 'primary');
    actions.appendChild(test);
    if (r.transport === 'teleport' && !r.connected) actions.appendChild(button('Обновить вход', async () => {
      if (resumeRemoteWork()) return;
      remoteWizReset(); remoteWiz.name = r.name; remoteWiz.host = r.sshHost || ''; remoteWiz.dir = r.jarvisDir || '~/.jarvis';
      for (const key of ['transport', 'sshConfigFile', 'teleportProxy', 'teleportCluster', 'runAsUser', 'nodeTcpPort']) remoteWiz[key] = r[key] || (key === 'transport' ? 'teleport' : '');
      remoteWiz.teleport.reconnectName = r.name; machineView.editor = true;
      await reRenderPane('remotes'); refreshTeleportStatus();
    }));
    panel.append(actions);
    const nav = el('div.connection-detail-tabs', { role: 'group', 'aria-label': 'Детали машины' });
    const overview = el('div.connection-overview');
    const maintenance = el('div.connection-maintenance');
    const tabs = [];
    const show = mode => {
      machineView.detail = mode; overview.hidden = mode !== 'overview'; maintenance.hidden = mode !== 'settings';
      tabs.forEach(([key, btn]) => btn.setAttribute('aria-pressed', String(key === mode)));
    };
    for (const [key, label] of [['overview', 'Обзор'], ['settings', 'Настройки']]) {
      const btn = button(label, () => show(key), 'sm'); tabs.push([key, btn]); nav.append(btn);
    }
    panel.append(nav, overview, maintenance);
    const address = machineAddress(r);
    const facts = el('dl.connection-facts');
    const fact = (label, value) => { if (value) facts.appendChild(el('div', null, [el('dt', { text: label }), el('dd', { text: value, title: value })])); };
    fact('Адрес', address.address || 'Не указан'); fact('Пользователь', address.user || 'Из SSH config');
    fact('Кластер', r.transport === 'teleport' ? r.teleportCluster : '');
    overview.append(el('h3.connection-section-label', { text: 'Подключение' }), facts);
    const profiles = el('div.connection-profiles');
    overview.append(el('h3.connection-section-label', { text: 'Агенты на машине' }), profiles);
    if (Array.isArray(r.sources) && r.sources.length) {
      for (const source of r.sources) {
        const profile = el('div.connection-profile');
        const copy = el('div', null, [el('strong', { text: source.label || source.agent || 'Профиль' }), el('span', { text: source.agent === 'codex' ? 'Codex' : source.agent === 'claude' ? 'Claude Code' : source.agent || 'Агент' })]);
        profile.append(el('span.connection-profile-icon', { 'data-kind': source.agent || '', 'aria-hidden': 'true' }, icon('terminal')), copy);
        profiles.append(profile);
      }
    } else profiles.append(el('p.connection-muted', { text: r.connected ? 'Профили пока не обнаружены.' : 'Профили появятся после подключения.' }));
    if (r.outdated) overview.append(el('div.connection-alert', { text: 'Доступно обновление Jarvis на этой машине.', 'data-tone': 'warning' }), button('Обновить узел', () => reinstall(), 'sm'));
    const maintenanceFacts = el('dl.connection-facts');
    for (const [label, value] of [['Каталог Jarvis', r.jarvisDir || '~/.jarvis'], ['Владелец агентов', r.runAsUser || address.user || 'Из SSH config'], ['SSH config', r.sshConfigFile], ['Teleport proxy', r.teleportProxy], ['Версия узла', r.version ? 'v' + r.version : 'Неизвестна']]) {
      if (value) maintenanceFacts.appendChild(el('div', null, [el('dt', { text: label }), el('dd', { text: value })]));
    }
    maintenance.append(el('h3.connection-section-label', { text: 'Параметры подключения' }), maintenanceFacts);
    for (const source of r.sources || []) {
      if (source.agent !== 'codex' || !window.jarvis.remotesRepairSource) continue;
      const b = button('Проверить хуки', async control => {
        control.disabled = true;
        try { const result = await window.jarvis.remotesRepairSource(r.name, source.id || source.sourceId); if (result?.ok === false) throw new Error(result.error || 'Хуки не настроены'); control.textContent = 'Хуки настроены'; }
        catch (error) { errLine.textContent = remoteDisplayText(error?.message || String(error)); errLine.hidden = false; }
        finally { control.disabled = false; }
      }, 'sm');
      maintenance.append(el('div.connection-setting-action', null, [el('div', null, [el('strong', { text: source.label || 'Codex' }), el('p', { text: 'События, вопросы и уведомления' })]), b]));
    }
    function reinstall() {
      if (resumeRemoteWork()) return;
      if (!remotesWizardReady()) return;
      remoteWizReset(); remoteWiz.host = r.sshHost || ''; remoteWiz.name = r.name; remoteWiz.dir = r.jarvisDir || '~/.jarvis';
      for (const key of ['transport', 'sshConfigFile', 'teleportProxy', 'teleportCluster', 'runAsUser', 'nodeTcpPort']) remoteWiz[key] = r[key] || (key === 'transport' ? 'ssh' : '');
      machineView.editor = true; startRemoteInstall(true); reRenderPane('remotes');
    }
    if (remotesWizardReady()) maintenance.append(el('div.connection-setting-action', null, [el('div', null, [el('strong', { text: 'Компонент Jarvis' }), el('p', { text: 'Обновить или восстановить установку' })]), button('Переустановить', reinstall, 'sm')]));
    const del = el('button.btn.sm.danger', { text: 'Удалить подключение' });
    let armed = false;
    del.addEventListener('click', async () => {
      if (resumeRemoteWork()) return;
      if (!armed) { armed = true; del.textContent = 'Подтвердить удаление'; setTimeout(() => { if (del.isConnected) { armed = false; del.textContent = 'Удалить подключение'; } }, 4000); return; }
      del.disabled = true;
      if (!await action(del, () => window.jarvis.remotesRemove(r.name))) { armed = false; del.disabled = false; del.textContent = 'Удалить подключение'; return; }
      machineView.selected = ''; reRenderPane('remotes');
    });
    maintenance.append(el('div.connection-remove', null, [el('p', { text: 'Убирает машину из Jarvis. Файлы на сервере сохранятся.' }), del]));
    show(machineView.detail);
    return panel;
  }

  // пустое состояние: что это вообще и что нужно на той стороне
  // wizard=false — старая сборка без мастера: обещать установку «отсюда» нельзя
  function remotesEmptyNote(wizard) {
    const body = el('div.grow', null, [
      el('div.s2note-t', { text: 'Узлов пока нет' }),
      el('div.s2note-p', { text: 'Узел — это Jarvis на чужой машине: Claude или Codex работают на VPS, '
        + 'а видно и слышно их здесь — в общем списке, с уведомлениями и ответом прямо из панели. '
        + 'Пока ноут спит, узел копит события и отдаёт их, когда ты вернёшься.' }),
      el('div.s2note-p', { text: 'На той стороне нужны две вещи: SSH-доступ (ходим твоими ключами и ~/.ssh/config — '
        + 'своих паролей Jarvis не заводит) и tmux — без него ответ в сессию не вставить, ровно как локально.' }),
    ]);
    const p3 = el('div.s2note-p');
    if (wizard) {
      p3.appendChild(document.createTextNode('Всё это ставится прямо отсюда: дай ssh-хост, нажми «Проверить машину» — '
        + 'Jarvis сходит туда, покажет, чего не хватает, и поставит узел сам. Если узел уже ставили через '));
      p3.appendChild(el('code', { text: 'jarvis-setup remote add' }));
      p3.appendChild(document.createTextNode(' — его можно просто прописать вручную.'));
    } else {
      p3.appendChild(document.createTextNode('Сам узел ставится командой '));
      p3.appendChild(el('code', { text: 'jarvis-setup remote add <имя> <ssh-хост>' }));
      p3.appendChild(document.createTextNode(' — здесь остаётся только прописать его.'));
    }
    body.appendChild(p3);
    return el('div.s2note', null, [el('div.s2note-ic', null, icon('server')), body]);
  }

  /* ── Мастер «подключить машину с нуля» ───────────────────────────────────
   * Состояние живёт в модуле, а не в DOM: установка идёт минутами и приезжает
   * событиями, панель за это время перерисовывается — введённые поля, отчёт
   * разведки и лог обязаны это пережить. */
  // Older backends and successful command output can still contain terminal
  // escapes. Keep this boundary plain text and bounded, including OSC payloads.
  function remoteDisplayText(value) {
    const original = String(value ?? ''), limit = 4096, suffix = '… (вывод сокращён)';
    let text = original.slice(0, 65536)
      .replace(/(?:\x1b\]|\x9d)[\s\S]*?(?:\x07|\x1b\\|\x9c|$)/g, '')
      .replace(/(?:\x1b[PX^_]|[\x90\x98\x9e\x9f])[\s\S]*?(?:\x1b\\|\x9c|$)/g, '')
      .replace(/(?:\x1b\[|\x9b)[0-?]*[ -/]*(?:[@-~]|$)/g, '')
      .replace(/\x1b[ -/]*[0-~]/g, '')
      .replace(/\r\n?/g, '\n')
      .replace(/[\x00-\x08\x0b-\x1f\x7f-\x9f\u202a-\u202e\u2066-\u2069]/g, '').trim();
    if (original.length > 65536 || text.length > limit) {
      text = text.slice(0, limit - suffix.length).replace(/[\ud800-\udbff]$/, '') + suffix;
    }
    return text;
  }
  const remoteWiz = {
    host: '', name: '', dir: '',
    transport: 'ssh', sshConfigFile: '', teleportProxy: '', teleportCluster: '', runAsUser: '', nodeTcpPort: null, sshDraftHost: '',
    teleport: null,
    probe: null,      // успешный ответ remotesPreflight
    probeErr: null,   // текст отказа ssh (многострочный — показываем как есть)
    busy: false,      // идёт разведка
    key: null,        // {publicKey, path} — ssh-ключ ЭТОЙ машины, ленивая загрузка
    keyBusy: false,
    manual: false, manualDraft: null, manualError: '', manualSaving: false,
    formErr: null,
    install: null,    // {name, steps:[…], pct, running, error}
    flash: null, connectedName: null,
    authErr: null,    // отказ входа по паролю; сам пароль тут НЕ живёт
  };
  let remoteProbeSequence = 0, teleportStatusSequence = 0, teleportNodesSequence = 0, teleportPollTimer = null;
  function newTeleportState() {
    return { status: null, busy: false, checked: false, error: '', nodes: [], nodesBusy: false, nodesLoaded: false, nodesError: '', login: '', node: '', manualTarget: false, loginPending: false, loginAt: 0 };
  }
  function resetRemoteProbe() {
    remoteProbeSequence++;
    remoteWiz.probe = null; remoteWiz.probeErr = null; remoteWiz.busy = false; remoteWiz.formErr = null; remoteWiz.authErr = null; remoteWiz.flash = null;
    if (!remoteWiz.install?.running) remoteWiz.install = null;
  }
  function resetTeleportState() {
    teleportStatusSequence++; teleportNodesSequence++; clearTimeout(teleportPollTimer); teleportPollTimer = null;
    remoteWiz.teleport = newTeleportState();
  }
  function remoteConnection(w) {
    return { sshHost: (w.host || '').trim(), transport: w.transport || 'ssh',
      ...((w.sshConfigFile || '').trim() && w.transport !== 'teleport' ? { sshConfigFile: w.sshConfigFile.trim() } : {}),
      ...((w.teleportProxy || '').trim() && w.transport === 'teleport' ? { teleportProxy: w.teleportProxy.trim() } : {}),
      ...((w.teleportCluster || '').trim() && w.transport === 'teleport' ? { teleportCluster: w.teleportCluster.trim() } : {}),
      ...(Number.isInteger(w.nodeTcpPort) && w.nodeTcpPort > 0 ? { nodeTcpPort: w.nodeTcpPort } : {}),
      ...((w.runAsUser || '').trim() ? { runAsUser: w.runAsUser.trim() } : {}) };
  }
  function remoteWizReset() {
    machineView.notifiedConnection = '';
    remoteWiz.host = ''; remoteWiz.name = ''; remoteWiz.dir = '';
    remoteWiz.transport = 'ssh'; remoteWiz.sshConfigFile = ''; remoteWiz.teleportProxy = ''; remoteWiz.teleportCluster = ''; remoteWiz.runAsUser = ''; remoteWiz.nodeTcpPort = null; remoteWiz.sshDraftHost = '';
    remoteWiz.probe = null; remoteWiz.probeErr = null; remoteWiz.busy = false;
    remoteWiz.formErr = null; remoteWiz.install = null; remoteWiz.manual = false; remoteWiz.manualDraft = null; remoteWiz.manualError = ''; remoteWiz.manualSaving = false;
    remoteWiz.authErr = null;
    remoteWiz.connectedName = null;
    resetRemoteProbe(); resetTeleportState();
  }
  // имя по умолчанию из ssh-хоста: dev@vps.example:22 → vps
  function remoteGuessName(host) {
    const h = String(host || '').trim().split('@').pop().split(':')[0];
    const first = (h.split('.')[0] || h).replace(/[^A-Za-z0-9_.-]+/g, '-');
    return first.slice(0, 24) || 'узел';
  }
  // перерисовать ТОЛЬКО мастер (список узлов и запросы к бэкенду не трогаем)
  function repaintRemoteWiz() {
    const box = currentRoot && currentRoot.querySelector('#s2-rwiz');
    if (!box) return;
    box.textContent = '';
    paintRemoteWiz(box);
    syncRemoteEditorChrome();
  }
  function syncRemoteEditorChrome() {
    const setup = currentRoot?.querySelector('#s2-ssh-setup');
    if (!setup) return;
    const cancel = setup.querySelector('[data-connection-cancel]');
    if (cancel) { cancel.textContent = remoteWiz.install?.running ? 'Свернуть' : 'Отмена'; cancel.disabled = !!remoteWiz.manualSaving; }
    const title = setup.querySelector('.connection-editor-head h2');
    if (title) title.textContent = remoteWiz.install ? 'Подготовка машины' : remoteWiz.teleport?.reconnectName ? 'Обновление доступа' : remoteWiz.flash ? 'Подключение готово' : 'Новое подключение';
  }
  function normalizedTeleportProxy(value) {
    const text = String(value || '').trim(); if (!text) return '';
    try { const url = new URL(/^https?:\/\//i.test(text) ? text : 'https://' + text); return url.hostname.toLowerCase() + (url.port && url.port !== '443' ? ':' + url.port : ''); }
    catch { return text.toLowerCase().replace(/\/$/, ''); }
  }
  const sameTeleportProxy = (a, b) => normalizedTeleportProxy(a) === normalizedTeleportProxy(b);
  const teleportSessionKey = p => p ? JSON.stringify([normalizedTeleportProxy(p.proxy), p.cluster || '', p.username || '', p.validUntil || '']) : '';
  function teleportProfiles() {
    const s = remoteWiz.teleport?.status;
    const profiles = Array.isArray(s?.profiles) ? [...s.profiles] : [];
    if (s?.authenticated && !profiles.some(p => sameTeleportProxy(p.proxy, s.proxy) && p.cluster === s.cluster)) profiles.unshift(s);
    return profiles.filter(p => p.authenticated === true && sameTeleportProxy(p.proxy, remoteWiz.teleportProxy));
  }
  function teleportProfile() { return teleportProfiles().find(p => p.cluster === remoteWiz.teleportCluster) || null; }
  function teleportLogins() { return [...new Set((teleportProfile()?.logins || []).filter(x => typeof x === 'string' && x))]; }
  function remoteSelect(label, options, value, changed, disabled) {
    const select = el('select.s2rselect.s2-secret', { 'aria-label': label });
    for (const [id, name] of options) select.appendChild(el('option', { value: id, text: name }));
    for (const option of select.options) option.selected = false;
    const selected = [...select.options].find(option => option.value === value); if (selected) selected.selected = true;
    select.disabled = !!disabled; select.addEventListener('change', () => changed(select.value)); return select;
  }
  function remoteField(label, input) { return el('label.s2rfield.s2-field', null, [el('span.s2-field-label', { text: label }), input]); }
  function syncTeleportLogin() {
    const w = remoteWiz, ts = w.teleport, logins = teleportLogins();
    if (!logins.includes(ts.login)) ts.login = logins.length === 1 ? logins[0] : '';
  }
  function clearTeleportTarget() {
    const w = remoteWiz, ts = w.teleport;
    teleportNodesSequence++; ts.nodes = []; ts.nodesLoaded = false; ts.nodesBusy = false; ts.nodesError = ''; ts.node = ''; w.host = '';
    resetRemoteProbe();
  }
  async function loadTeleportNodes() {
    const w = remoteWiz, ts = w.teleport;
    if (w.transport !== 'teleport' || !teleportProfile() || !ts.login) return;
    const request = ++teleportNodesSequence, proxy = w.teleportProxy.trim(), cluster = w.teleportCluster, login = ts.login;
    ts.nodesBusy = true; ts.nodesError = ''; repaintRemoteWiz();
    const result = await safe(() => window.jarvis.teleportNodes(proxy || null, cluster || null), null);
    if (w.teleport !== ts || request !== teleportNodesSequence || w.transport !== 'teleport' || w.teleportProxy.trim() !== proxy || w.teleportCluster !== cluster || ts.login !== login) return;
    ts.nodesBusy = false; ts.nodesLoaded = true;
    if (result?.ok) ts.nodes = Array.isArray(result.nodes) ? result.nodes : [];
    else { ts.nodes = []; ts.nodesError = result?.error || 'Не удалось получить машины Teleport.'; }
    if (ts.node && !ts.nodes.some(n => (n.target || n.id || n.name) === ts.node)) { ts.node = ''; if (!ts.manualTarget) w.host = ''; resetRemoteProbe(); }
    repaintRemoteWiz();
  }
  function scheduleTeleportPoll(ts) {
    clearTimeout(teleportPollTimer);
    if (remoteWiz.teleport !== ts || !ts.loginPending) return;
    teleportPollTimer = setTimeout(() => {
      teleportPollTimer = null;
      if (remoteWiz.teleport !== ts || remoteWiz.transport !== 'teleport' || !ts.loginPending) return;
      if (activePane !== 'remotes' || !remoteEditorOpen() || currentRoot?.closest('[hidden]')) return;
      if (Date.now() - ts.loginAt > 300000) { ts.loginPending = false; ts.error = 'Вход ещё не подтверждён. Заверши его в терминале и обнови доступ.'; repaintRemoteWiz(); return; }
      refreshTeleportStatus(true);
    }, 2000);
  }
  function resumeTeleportPolling() {
    if (remoteWiz.transport === 'teleport' && remoteWiz.teleport?.loginPending && remoteEditorOpen()) refreshTeleportStatus(true);
  }
  async function refreshTeleportStatus(poll = false) {
    const w = remoteWiz, ts = w.teleport || (w.teleport = newTeleportState());
    if (w.transport !== 'teleport') return;
    if (ts.busy) { if (poll && ts.loginPending) scheduleTeleportPoll(ts); return; }
    const request = ++teleportStatusSequence, proxy = w.teleportProxy.trim();
    ts.busy = true; ts.error = ''; repaintRemoteWiz();
    const result = await safe(() => window.jarvis.teleportStatus(proxy || null), null);
    if (w.teleport !== ts || request !== teleportStatusSequence || w.transport !== 'teleport' || w.teleportProxy.trim() !== proxy) return;
    const oldIdentity = JSON.stringify([w.teleportCluster, teleportProfile()?.username || '', ts.login]);
    ts.busy = false; ts.checked = true; ts.status = result;
    if (!result?.ok) ts.error = result?.error || 'Не удалось проверить Teleport.';
    else {
      if (!w.teleportProxy && result.proxy) w.teleportProxy = result.proxy;
      const profiles = teleportProfiles();
      if (!profiles.some(p => p.cluster === w.teleportCluster)) w.teleportCluster = profiles.find(p => p.cluster === result.cluster)?.cluster || profiles[0]?.cluster || '';
      if (teleportProfile()) {
        const renewed = ts.loginPending && (!ts.loginBefore || teleportSessionKey(teleportProfile()) !== ts.loginBefore);
        if (!ts.loginPending || renewed) { ts.loginPending = false; clearTimeout(teleportPollTimer); }
        syncTeleportLogin();
        const identity = JSON.stringify([w.teleportCluster, teleportProfile()?.username || '', ts.login]);
        if (identity !== oldIdentity) { clearTeleportTarget(); w.manualDraft = null; }
        else if (renewed) { ts.nodesLoaded = false; ts.nodesError = ''; resetRemoteProbe(); }
      } else {
        clearTeleportTarget(); ts.login = '';
        if (result.error) ts.error = result.error;
      }
    }
    repaintRemoteWiz();
    if (teleportProfile() && ts.login && !ts.nodesLoaded) loadTeleportNodes();
    if (ts.loginPending) scheduleTeleportPoll(ts);
  }
  async function startTeleportLogin(renew = false) {
    const w = remoteWiz, ts = w.teleport, proxy = w.teleportProxy.trim();
    if (!proxy) { ts.error = 'Укажи адрес Teleport proxy.'; repaintRemoteWiz(); return; }
    if (ts.loginPending || ts.busy) return;
    ts.loginBefore = renew ? teleportSessionKey(teleportProfile()) : '';
    ts.loginPending = true; ts.loginAt = Date.now(); ts.error = ''; repaintRemoteWiz();
    const result = await safe(() => window.jarvis.teleportLogin(proxy, !!renew), null);
    if (w.teleport !== ts || w.transport !== 'teleport' || w.teleportProxy.trim() !== proxy) return;
    if (!result?.ok) { ts.loginPending = false; ts.error = result?.error || 'Не удалось открыть вход в Teleport.'; }
    else scheduleTeleportPoll(ts);
    repaintRemoteWiz();
  }
  function setRemoteTransport(value) {
    const w = remoteWiz;
    if (w.transport === value || w.install?.running) return;
    if (w.transport === 'ssh') w.sshDraftHost = w.host;
    w.transport = value; w.host = value === 'ssh' ? w.sshDraftHost : '';
    w.manualDraft = null; w.manualError = ''; w.manualSaving = false;
    resetRemoteProbe(); resetTeleportState(); repaintRemoteWiz();
    if (value === 'teleport') refreshTeleportStatus();
  }

  // строка проверки: точка формой (кольцо — нашлось, залитая — нет) + «есть/нет»
  function remoteCheck(ok, label) {
    return el('div.s2rchk' + (ok ? '.on' : ''), null, [
      el('span.dot' + (ok ? '.done' : '')),
      el('span.nm', { text: label, title: label }),
      el('span.vl', { text: ok ? 'есть' : 'нет' }),
    ]);
  }
  // строка-пояснение с иконкой (nodeNote и предупреждения разведки)
  function remoteHintLine(ic, text, warn) {
    return el('div.s2rnode' + (warn ? '.warn' : ''), null, [icon(ic), el('span', { text })]);
  }

  // карточка разведки: ОС, куда встанет узел, что нашлось и откуда возьмётся бинарь
  const NODE_SRC_ICON = { local: 'server', download: 'download', build: 'cpu', none: 'alert-triangle' };
  function remoteProbeCard() {
    const p = remoteWiz.probe || {};
    const inst = remoteWiz.install;
    const grow = el('div.grow');
    grow.appendChild(el('div.dt', { text: 'Машина проверена' }));
    grow.appendChild(el('div.dd', { text: [p.os, p.arch, p.claude ? 'Claude Code' : '', p.codex ? 'Codex' : ''].filter(Boolean).join(' · ') || 'Доступ подтверждён' }));
    const dir = p.dir || (remoteWiz.dir.trim() || '~/.jarvis');
    const details = el('div.s2radvanced');
    details.appendChild(el('div.dd.mono.s2rmeta', { text: 'Каталог: ' + dir, title: dir }));
    details.appendChild(el('div.s2rchecks', null, [
      remoteCheck(p.tmux, 'tmux'),
      remoteCheck(p.curl, 'curl'),
      remoteCheck(p.claude, 'Claude Code'),
      remoteCheck(p.codex, 'Codex'),
      remoteCheck(p.systemd, 'systemd — автозапуск'),
      remoteCheck(p.cargo, 'cargo — сборка на месте'),
    ]));
    // строку про происхождение бинаря отдаёт бэкенд — показываем как есть
    if (p.nodeNote) details.appendChild(remoteHintLine(NODE_SRC_ICON[p.nodeSource] || 'info', p.nodeNote, p.nodeSource === 'none'));
    const runtime = p.runtimeSetup;
    if (runtime?.automatic && runtime.missing?.length) grow.appendChild(el('div.s2raccess-state', { text: 'Установим ' + runtime.missing.join(' + ') + ' и подключим Jarvis.' }));
    else if (runtime?.missing?.length) {
      grow.appendChild(el('div.s2raccess-state.warn', { text: 'Нужно подготовить ' + runtime.missing.join(' + ') + '. Инструкция — ниже.' }));
      if (runtime.command) details.appendChild(el('pre.s2rpre', { text: runtime.command }));
    } else grow.appendChild(el('div.dd', { text: 'Подключим Jarvis и найденные профили агентов.' }));
    if (!p.claude && !p.codex) {
      grow.appendChild(remoteHintLine('alert-triangle',
        'Claude Code и Codex не найдены. Их нужно установить отдельно.', true));
    }
    grow.appendChild(settingsDetails('s2-preflight-details', 'Результат проверки', [details]));

    // пока идёт (или упала) установка, единственная точка действия — карточка
    // установки ниже: две одинаковые кнопки на экране только путают
    const blocked = p.nodeSource === 'none';
    const run = button('Настроить автоматически', () => startRemoteInstall(), 'sm primary');
    run.disabled = blocked;
    if (blocked) run.title = 'Взять бинарь узла неоткуда — смотри пояснение слева';
    return el('div.drow', null, [grow, el('div.dctl', { style: 'align-self:flex-start;margin-top:2px' }, inst ? [] : [run])]);
  }

  /* Помощь по ssh: разведка не прошла — чаще всего на свежий VPS просто не
   * пустили по ключу. Показываем публичный ключ этой машины, чтобы его было
   * куда скопировать, и заводим ключ, если его вообще нет. */
  function remoteSshKeyApi() {
    try { return !!(window.jarvis && typeof window.jarvis.remotesSshKey === 'function'); } catch (e) { return false; }
  }
  function loadRemoteSshKey(create) {
    const w = remoteWiz;
    if (w.keyBusy || !remoteSshKeyApi()) return;
    w.keyBusy = true;
    safe(() => window.jarvis.remotesSshKey(!!create), null).then((res) => {
      w.keyBusy = false;
      w.key = (res && res.ok)
        ? { publicKey: res.publicKey || '', path: res.path || '', created: !!res.created }
        : { publicKey: '', error: (res && res.error) || 'не удалось прочитать ssh-ключ' };
      repaintRemoteWiz();
    });
  }
  // Разведка отдельной функцией: её дёргает и кнопка, и удачный вход по паролю
  // (после него человек не должен жать «Проверить машину» ещё раз).
  function runRemoteProbe() {
    const w = remoteWiz;
    const host = (w.host || '').trim();
    if (!host) { w.formErr = w.transport === 'teleport' ? 'Выбери пользователя и машину Teleport.' : 'Укажи user@адрес или SSH-алиас.'; repaintRemoteWiz(); return; }
    if (w.transport === 'teleport' && (!teleportProfile() || !w.teleport?.login || w.teleport.loginPending)) { w.formErr = 'Сначала войди в Teleport и выбери пользователя SSH.'; repaintRemoteWiz(); return; }
    if (w.busy) return;
    w.formErr = null; w.busy = true; w.probe = null; w.probeErr = null; w.flash = null;
    const request = ++remoteProbeSequence, connection = remoteConnection(w), fingerprint = JSON.stringify(connection), dir = (w.dir || '').trim() || '~/.jarvis';
    repaintRemoteWiz();
    safe(() => window.jarvis.remotesPreflight(host, dir, connection), null).then((res) => {
      if (request !== remoteProbeSequence || JSON.stringify(remoteConnection(w)) !== fingerprint || ((w.dir || '').trim() || '~/.jarvis') !== dir) return;
      w.busy = false;
      if (res && res.ok) {
        w.probe = res;
        if (!(w.name || '').trim()) w.name = remoteGuessName(host);
      } else {
        w.probeErr = (res && res.error) || 'Машина не ответила. Проверь доступ и адрес.';
      }
      repaintRemoteWiz();
    });
  }

  function remoteAuthorizeApi() {
    try { return !!(window.jarvis && typeof window.jarvis.remotesSshAuthorize === 'function'); } catch (e) { return false; }
  }

  /* Вторая дорога на машину, куда ключ ещё не положен: войти по паролю и
   * положить его самим. Пароль живёт только в этом поле — ни в состоянии
   * мастера, ни в логах его нет, и на сервер он уходит один раз. Иначе и
   * нельзя: туннель к узлу переподнимается сам после сна и смены сети, и
   * спросить пароль в этот момент не у кого. */
  function remotePasswordBlock() {
    const w = remoteWiz;
    const box = el('div.s2rpass');
    box.appendChild(el('div.dt', { text: 'Или войти по паролю' }));
    box.appendChild(el('div.dd', { text: 'Пароль пользователя на той машине. Нужен один раз: Jarvis положит '
      + 'туда свой ключ и дальше будет ходить без пароля — пароль никуда не сохраняется.' }));
    const pass = el('input.s2-secret', {
      type: 'password', placeholder: 'пароль на сервере', autocomplete: 'off', spellcheck: 'false',
    });
    const note = el('div.dd');
    const go = button('Войти по паролю', (b) => {
      const value = pass.value;
      if (w.transport !== 'ssh') return;
      if (!value) { note.textContent = 'Пустой пароль'; return; }
      b.disabled = true; pass.disabled = true; b.textContent = 'Захожу…';
      note.textContent = ''; w.authErr = null;
      const identity = remoteProbeSequence, connection = remoteConnection(w), fingerprint = JSON.stringify(connection);
      safe(() => window.jarvis.remotesSshAuthorize((w.host || '').trim(), value, connection), null).then((res) => {
        if (w.transport !== 'ssh' || identity !== remoteProbeSequence || JSON.stringify(remoteConnection(w)) !== fingerprint) return;
        b.disabled = false; pass.disabled = false; b.textContent = 'Войти по паролю';
        if (res && res.ok) {
          pass.value = ''; // дальше он не нужен — не держим его в DOM
          w.probeErr = null;
          w.flash = res.createdKey
            ? 'Ключ создан и добавлен на машину — дальше без пароля.'
            : 'Ключ добавлен на машину — дальше без пароля.';
          runRemoteProbe(); // сразу показываем разведку, второй клик не нужен
          return;
        }
        // пароль оставляем: чаще всего это опечатка, а не отказ сервера
        w.authErr = (res && res.error) || 'Не вышло войти по паролю.';
        repaintRemoteWiz();
      });
    }, 'sm primary');
    pass.addEventListener('keydown', (e) => { if (e.key === 'Enter' && !go.disabled) go.click(); });
    box.appendChild(el('div.s2rbtns', null, [pass, go]));
    if (w.authErr) box.appendChild(el('div.s2rpre.bad', { text: remoteDisplayText(w.authErr) }));
    box.appendChild(note);
    return box;
  }

  function remoteSshHelpCard() {
    const w = remoteWiz;
    const grow = el('div.grow');
    grow.appendChild(el('div.dt', { text: 'Машина не пустила по SSH' }));
    grow.appendChild(el('div.s2rpre.bad', { text: remoteDisplayText(w.probeErr) }));

    if (remoteSshKeyApi()) {
      if (w.key === null) {
        loadRemoteSshKey(false);
        grow.appendChild(el('div.dd', { text: 'Смотрю, есть ли ssh-ключ на этой машине…' }));
      } else if (w.key.publicKey) {
        grow.appendChild(el('div.dd', { text: 'Публичный ключ этой машины. Добавь его строкой в ~/.ssh/authorized_keys '
          + 'на VPS — или вставь в поле «SSH-ключ» в панели хостера, когда создаёшь сервер, и проверь ещё раз.' }));
        grow.appendChild(el('div.s2rpre', { text: w.key.publicKey }));
        if (w.key.path) grow.appendChild(el('div.dd.mono.s2rmeta', { text: w.key.path, title: w.key.path }));
        const copy = button('Скопировать', (b) => {
          try { navigator.clipboard.writeText(w.key.publicKey); } catch (e) {}
          b.textContent = 'Скопировано';
          setTimeout(() => { b.textContent = 'Скопировать'; }, 1600);
        }, 'sm');
        grow.appendChild(el('div.s2rbtns', null, [copy, el('span.dd', { text: w.key.created ? 'ключ только что создан' : '' })]));
      } else {
        grow.appendChild(el('div.dd', { text: w.key.error
          || 'Ssh-ключа на этой машине ещё нет. Заведём ed25519 и покажем публичную часть — её и надо отдать серверу.' }));
        const mkkey = button(w.keyBusy ? 'Создаю…' : 'Создать ключ', (b) => {
          b.disabled = true; b.textContent = 'Создаю…';
          loadRemoteSshKey(true);
        }, 'sm');
        mkkey.disabled = w.keyBusy;
        grow.appendChild(el('div.s2rbtns', null, [mkkey]));
      }
    }
    if (w.transport === 'ssh' && remoteAuthorizeApi()) grow.appendChild(remotePasswordBlock());
    return el('div.drow', null, [
      el('div.s2note-ic', { style: 'align-self:flex-start;margin-top:2px' }, icon('key')), grow,
    ]);
  }

  // ── Установка: запуск, живой лог, финал ────────────────────────────────
  function startRemoteInstall(existing = false) {
    const w = remoteWiz;
    if (w.install?.running) return;
    if (!(w.host || '').trim() || (!existing && !w.probe)) { w.formErr = 'Сначала проверь выбранную машину.'; repaintRemoteWiz(); return; }
    const name = (w.name || '').trim() || remoteGuessName(w.host);
    w.name = name; w.flash = null;
    const installation = { name, steps: [], pct: null, running: true, error: null, existing };
    w.install = installation;
    repaintRemoteWiz();
    safe(() => window.jarvis.remotesInstall({
      ...remoteConnection(w),
      name, sshHost: (w.host || '').trim(), jarvisDir: (w.dir || '').trim() || '~/.jarvis',
    }), null).then((res) => {
      // отказ на входе (кривой ввод / установка уже идёт) — событий не будет
      if (res && res.ok) return;
      if (w.install !== installation) return;
      w.install.running = false;
      w.install.error = remoteDisplayText(res && res.error) || 'не удалось запустить установку';
      repaintRemoteWiz();
    });
  }
  // лог рисуем отдельно от мастера: шаги сыплются часто, а перерисовка всей
  // карточки сбрасывала бы прокрутку лога и мигала кнопками
  function fillRemoteLog(node) {
    const st = remoteWiz.install;
    node.textContent = '';
    if (!st) return;
    st.steps.forEach((s, i) => {
      const last = i === st.steps.length - 1;
      const kind = s.state === 'warn' ? '.warn' : (s.state === 'done' ? '.done' : '');
      // точка формой: кольцо — шаг закрыт, пульс — текущий шаг, серая — прочее
      const dot = s.state === 'done' ? '.done' : (last && st.running && s.state === 'start' ? '.working' : '');
      node.appendChild(el('div.s2rln' + kind, null, [
        el('span.dot' + dot),
        el('span.install-phase', { text: s.phase || '' }),
        el('span.msg', { text: s.msg || '' }),
      ]));
    });
    node.scrollTop = node.scrollHeight;
  }
  function repaintRemoteInstall() {
    const log = currentRoot && currentRoot.querySelector('#s2-rlog');
    if (!log) { repaintRemoteWiz(); return; }
    fillRemoteLog(log);
    const st = remoteWiz.install;
    const prog = currentRoot.querySelector('#s2-rprog');
    if (prog) {
      prog.textContent = '';
      // pct приходит не всегда — без него полосу не рисуем вовсе
      if (st && st.running && st.pct != null) prog.appendChild(progressBar(st.pct));
    }
  }
  function remoteInstallCard() {
    const st = remoteWiz.install;
    const grow = el('div.grow');
    grow.appendChild(el('div.dt', { text: st.running ? 'Ставлю узел «' + st.name + '»' : 'Установка не удалась' }));
    if (st.running) {
      grow.appendChild(el('div.dd', { text: 'Подготавливаем Jarvis и подключаем агентов. Можно свернуть этот экран — установка продолжится.' }));
    }
    grow.appendChild(el('div#s2-rprog'));
    const log = el('div.s2rlog#s2-rlog');
    grow.appendChild(log);
    // ошибка установки — та же пошаговая инструкция: пусть занимает всю ширину
    // строки, поэтому кнопки уходят под неё, а не в узкий .dctl справа
    if (st.error) grow.appendChild(el('div.s2rpre.bad', { text: remoteDisplayText(st.error) }));
    if (!st.running) {
      grow.appendChild(el('div.s2rbtns', null, [
        button('Повторить', () => startRemoteInstall(st.existing), 'sm primary'),
        button('Закрыть', () => { remoteWiz.install = null; repaintRemoteWiz(); }, 'sm'),
      ]));
    }
    const row = el('div.drow', null, [grow]);
    // лог и полосу наполняем после вставки в DOM — scrollTop до этого не работает
    setTimeout(repaintRemoteInstall, 0);
    return row;
  }

  function repaintRemoteField(input) {
    const key = input.dataset.remoteField, start = input.selectionStart, end = input.selectionEnd;
    repaintRemoteWiz();
    const next = currentRoot?.querySelector('[data-remote-field="' + key + '"]');
    if (next) { next.focus(); try { next.setSelectionRange(start, end); } catch {} }
  }
  function remoteInput(ph, key, label = ph) {
    const w = remoteWiz;
    const input = el('input.s2-secret', { type: 'text', placeholder: ph, autocomplete: 'off', spellcheck: 'false', value: w[key] || '', 'aria-label': label, 'data-remote-field': key });
    input.disabled = !!w.install?.running || w.busy || w.manualSaving;
    input.addEventListener('input', () => { w[key] = input.value; if (key !== 'name') { resetRemoteProbe(); repaintRemoteField(input); } });
    input.addEventListener('keydown', e => { if (e.key === 'Enter') { e.preventDefault(); runRemoteProbe(); } });
    return input;
  }
  function teleportAccess(grow, blocked) {
    const w = remoteWiz, ts = w.teleport || (w.teleport = newTeleportState());
    if (!window.jarvis.teleportStatus || !window.jarvis.teleportNodes || !window.jarvis.teleportLogin) {
      grow.appendChild(el('div.dd', { text: 'Обнови Jarvis, чтобы подключать Teleport отсюда.' })); return;
    }
    const proxy = el('input.s2-secret', { type: 'text', value: w.teleportProxy, placeholder: 'teleport.example.com', 'aria-label': 'Teleport proxy', 'data-remote-field': 'teleportProxy', autocomplete: 'off', spellcheck: 'false' });
    proxy.disabled = blocked;
    proxy.addEventListener('input', () => { w.teleportProxy = proxy.value; w.teleportCluster = ''; w.host = ''; w.manualDraft = null; w.manualError = ''; w.manualSaving = false; resetRemoteProbe(); resetTeleportState(); repaintRemoteField(proxy); });
    proxy.addEventListener('keydown', e => { if (e.key === 'Enter') { e.preventDefault(); refreshTeleportStatus(); } });
    const check = button(ts.busy ? 'Проверяем…' : 'Обновить доступ', () => refreshTeleportStatus(), 'sm'); check.disabled = blocked || ts.busy;
    grow.appendChild(el('div.s2raccess-line', null, [remoteField('Teleport proxy', proxy), check]));
    const status = ts.status;
    if (status?.available === false) {
      grow.appendChild(el('div.s2raccess-state', { text: 'tsh не найден. Установи клиент Teleport и обнови доступ.' }));
      grow.appendChild(button('Установить tsh', () => required(() => window.jarvis.openUrl('https://goteleport.com/docs/connect-your-client/teleport-clients/tsh/')), 'sm'));
      return;
    }
    const profile = teleportProfile();
    if (profile) {
      const date = new Date(profile.validUntil || status?.validUntil || '');
      const expiry = Number.isNaN(date.getTime()) ? '' : ' · до ' + date.toLocaleString('ru-RU', { day: 'numeric', month: 'short', hour: '2-digit', minute: '2-digit' });
      grow.appendChild(el('div.s2raccess-state.on', { text: (profile.username || status?.username || 'Teleport') + ' · вход активен' + expiry }));
      if (ts.loginPending) grow.appendChild(el('div.s2raccess-state', { text: 'Ждём новый вход через SSO/MFA. Заверши его в открывшемся терминале.' }));
      if (ts.error || ts.nodesError || w.probeErr || ts.reconnectName) {
        const again = button(ts.loginPending ? 'Ждём вход…' : 'Выйти и войти заново', () => startTeleportLogin(true), 'sm'); again.disabled = blocked || ts.busy || ts.loginPending;
        grow.appendChild(el('div.s2raccess-line', null, [again]));
      }
      const profiles = Array.isArray(status?.profiles) ? status.profiles : [];
      const proxies = [...new Set(profiles.filter(p => p.authenticated).map(p => p.proxy).filter(Boolean))];
      if (proxies.length > 1) grow.appendChild(remoteField('Профиль', remoteSelect('Профиль Teleport', proxies.map(value => [value, value]), w.teleportProxy, value => { w.teleportProxy = value; w.teleportCluster = ''; w.host = ''; resetRemoteProbe(); resetTeleportState(); repaintRemoteWiz(); refreshTeleportStatus(); }, blocked)));
      const clusters = [...new Set(teleportProfiles().map(p => p.cluster).filter(Boolean))];
      const cluster = remoteSelect('Кластер Teleport', clusters.map(value => [value, value]), w.teleportCluster, value => {
        w.teleportCluster = value; clearTeleportTarget(); ts.reconnectName = ''; w.manualDraft = null; ts.login = ''; syncTeleportLogin(); repaintRemoteWiz(); if (ts.login) loadTeleportNodes();
      }, blocked);
      const login = remoteSelect('Пользователь SSH', [['', 'Выбери пользователя'], ...teleportLogins().map(value => [value, value])], ts.login, value => { clearTeleportTarget(); w.manualDraft = null; ts.login = value; repaintRemoteWiz(); if (value) loadTeleportNodes(); }, blocked);
      grow.appendChild(el('div.s2raccess-grid', null, [remoteField('Кластер', cluster), remoteField('Пользователь SSH', login)]));
      if (!teleportLogins().length) grow.appendChild(el('div.s2raccess-state', { text: 'В профиле нет разрешённых SSH-пользователей. Проверь доступ у администратора Teleport.' }));
    } else {
      const login = button(ts.loginPending ? 'Ждём вход…' : 'Войти через Teleport', () => startTeleportLogin(false), 'sm primary'); login.disabled = blocked || ts.busy || ts.loginPending || !w.teleportProxy.trim();
      grow.appendChild(el('div.s2raccess-line', null, [login, el('span.dd', { text: ts.loginPending ? 'Заверши SSO/MFA в открывшемся терминале и браузере.' : 'Вход через официальный tsh. Пароль здесь не нужен.' })]));
    }
    if (ts.error) grow.appendChild(el('div.s2raccess-state.error', { role: 'alert', text: remoteDisplayText(ts.error) }));
  }
  function teleportMachine(grow, blocked) {
    const w = remoteWiz, ts = w.teleport;
    if (!teleportProfile() || !ts.login) return;
    const nodes = ts.nodes.map(n => [n.target || n.id || n.name, n.name || n.hostname || n.id]);
    const select = remoteSelect('Машина Teleport', [['', ts.nodesBusy ? 'Загружаем машины…' : 'Выбери машину'], ...nodes], ts.node, value => {
      ts.node = value; w.host = value ? ts.login + '@' + value : ''; resetRemoteProbe(); if (!w.name && value) w.name = remoteGuessName(ts.nodes.find(n => (n.target || n.id || n.name) === value)?.name || value); repaintRemoteWiz();
    }, blocked || ts.nodesBusy || !nodes.length);
    grow.appendChild(remoteField('Машина Teleport', select));
    const selected = ts.nodes.find(n => (n.target || n.id || n.name) === ts.node);
    if (selected?.hostname) grow.appendChild(el('div.s2raccess-state', { text: selected.hostname }));
    if (ts.nodesError) grow.appendChild(el('div.s2raccess-state.error', { role: 'alert', text: remoteDisplayText(ts.nodesError) }));
    else if (ts.nodesLoaded && !nodes.length) grow.appendChild(el('div.s2raccess-state', { text: 'Доступных машин нет. Проверь выбранный кластер и права доступа.' }));
    if (ts.nodesLoaded) { const retry = button('Обновить машины', () => { clearTeleportTarget(); loadTeleportNodes(); }, 'sm'); retry.disabled = blocked || ts.nodesBusy; grow.appendChild(el('div.s2raccess-line', null, [retry])); }
  }
  function paintRemoteWiz(box) {
    const w = remoteWiz, running = !!w.install?.running;
    w.teleport ||= newTeleportState();
    const stage = w.flash ? 4 : w.install || w.probe ? 3 : (w.transport === 'ssh' || teleportProfile()) ? 2 : 1;
    const steps = el('ol.s2rsteps', { 'aria-label': 'Подключение машины' });
    for (const [number, label] of [[1, 'Доступ'], [2, 'Машина и агенты'], [3, 'Автонастройка'], [4, 'Подключено']]) steps.appendChild(el('li' + (number === stage ? '.current' : number < stage ? '.done' : ''), { 'aria-current': number === stage ? 'step' : null }, [el('span', { text: String(number) }), label]));
    box.appendChild(steps);
    if (w.flash) {
      const actions = el('div.s2raccess-line');
      if (w.connectedName && window.jarvisSessionWorkspace?.newChat) { const machine = w.connectedName; actions.appendChild(button('Открыть чаты', () => window.jarvisSessionWorkspace.newChat({ machine, cwd: '' }), 'sm primary')); }
      actions.appendChild(button('Подключить ещё', () => { remoteWizReset(); repaintRemoteWiz(); }, 'sm'));
      box.append(el('div.s2raccess-state.on', { role: 'status', text: w.flash }), actions); return;
    }
    if (remotesWizardReady()) {
      const grow = el('div.grow');
      const access = el('div.s2rtransport', { role: 'group', 'aria-label': 'Способ подключения' });
      for (const [value, label] of [['ssh', 'SSH'], ['teleport', 'Teleport (tsh)']]) { const b = button(label, () => setRemoteTransport(value), 'sm' + (w.transport === value ? ' selected' : '')); b.prepend(icon(value === 'ssh' ? 'terminal' : 'shield-check')); b.setAttribute('aria-pressed', String(w.transport === value)); b.disabled = running || w.manualSaving; access.appendChild(b); }
      grow.appendChild(access);
      if (w.transport === 'teleport') teleportAccess(grow, running || w.busy || w.manualSaving);
      else grow.appendChild(el('div.dd', { text: 'Обычный SSH: user@адрес или алиас из ~/.ssh/config.' }));
      const ready = w.transport === 'ssh' || !!(teleportProfile() && w.teleport.login);
      if (w.teleport.reconnectName && w.transport === 'teleport') {
        const reconnectName = w.teleport.reconnectName;
        grow.appendChild(el('div.s2raccess-state', { text: 'Обновление доступа: ' + reconnectName }));
        if (teleportProfile()) grow.appendChild(el('div.s2raccess-line', null, [button('Проверить подключение', async b => {
          const ts = w.teleport; b.disabled = true;
          const result = await safe(() => window.jarvis.remotesTest(reconnectName), null);
          if (w.teleport !== ts) return;
          if (result?.ok) { remoteWizReset(); w.connectedName = reconnectName; w.flash = 'Машина «' + reconnectName + '» на связи.'; reRenderPane('remotes'); }
          else { ts.error = result?.error || 'Вход активен, но машина пока не ответила. Попробуй проверить ещё раз.'; repaintRemoteWiz(); }
        }, 'sm primary')]));
      } else if (ready) {
        if (w.transport === 'ssh') grow.appendChild(remoteField('Машина', remoteInput('ssh-хост · user@адрес', 'host', 'SSH-хост')));
        else teleportMachine(grow, running || w.busy || w.manualSaving);
        grow.appendChild(remoteField('Название', remoteInput('имя · vps', 'name', 'Имя подключения')));
        const advanced = el('div.s2radvanced');
        advanced.appendChild(remoteField('Каталог Jarvis', remoteInput('~/.jarvis', 'dir', 'Каталог Jarvis')));
        if (w.transport === 'ssh') advanced.appendChild(remoteField('SSH config', remoteInput('Абсолютный путь · необязательно', 'sshConfigFile', 'SSH config')));
        else {
          const manual = el('input.s2-secret', { type: 'text', placeholder: 'Имя VM или UUID', 'aria-label': 'Адрес машины Teleport', 'data-remote-field': 'teleportTarget', value: w.teleport.manualTarget ? w.host.slice(w.host.indexOf('@') + 1) : '' }); manual.disabled = running || w.busy;
          manual.addEventListener('input', () => { w.teleport.manualTarget = true; w.teleport.node = ''; w.host = manual.value.trim() ? w.teleport.login + '@' + manual.value.trim() : ''; resetRemoteProbe(); repaintRemoteField(manual); });
          advanced.appendChild(remoteField('Адрес вручную', manual));
        }
        advanced.appendChild(remoteField('Владелец агентов', remoteInput('Если отличается от SSH login', 'runAsUser', 'Пользователь агента на узле')));
        advanced.appendChild(el('div.dd', { text: 'Владелец определяет, чьи профили Claude и Codex будут подключены.' }));
        grow.appendChild(settingsDetails('s2-connection-transport', 'Дополнительно', [advanced]));
        const probe = button(w.busy ? 'Проверяем машину…' : 'Проверить машину', runRemoteProbe, 'sm' + (w.probe || w.install || w.manual ? '' : ' primary')); probe.disabled = w.busy || running || w.manualSaving || !w.host.trim() || (w.transport === 'teleport' && (w.teleport.loginPending || w.teleport.nodesBusy));
        if (!running) grow.appendChild(el('div.s2raccess-line', null, [probe, el('span.dd', { text: 'Проверим доступ и найдём агентов.' })]));
      }
      if (w.formErr) grow.appendChild(el('div.s2raccess-state.error', { role: 'alert', text: remoteDisplayText(w.formErr) }));
      if (w.probe || w.install) {
        const summary = el('details.s2-details.s2rconnection-summary', null, [el('summary', null, [el('span', { text: 'Подключение · ' + (w.transport === 'teleport' ? 'Teleport' : 'SSH') + ' · ' + (w.name || w.host) }), el('span.s2rchange', { text: running ? 'Подробнее' : 'Изменить' })]), el('div.drow', null, [grow])]);
        box.appendChild(summary);
      } else box.appendChild(el('div.drow', null, [grow]));
      if (w.probeErr) {
        if (w.transport === 'ssh') box.appendChild(remoteSshHelpCard());
        else box.appendChild(el('div.s2raccess-state.error', { role: 'alert', text: remoteDisplayText(w.probeErr) }));
      }
      if (w.probe && !w.manual) box.appendChild(remoteProbeCard());
      if (w.install) box.appendChild(remoteInstallCard());
    }
    if (w.manual || !remotesWizardReady()) box.appendChild(remotesAddRow());
    if (remotesWizardReady()) {
      const link = el('button.s2rlink', { text: w.manual ? 'Свернуть ручное добавление' : 'Jarvis уже установлен — добавить вручную' });
      link.addEventListener('click', () => { w.manual = !w.manual; repaintRemoteWiz(); }); link.disabled = running;
      box.appendChild(el('div.s2rmore', null, [link]));
    }
  }

  // форма добавления: имя, ssh-хост, каталог jarvis (по умолчанию ~/.jarvis)
  function remotesAddRow() {
    const w = remoteWiz;
    const draft = w.manualDraft || (w.manualDraft = { name: w.name, host: w.host, dir: w.dir });
    const mk = (ph, key, label) => {
      const input = el('input.s2-secret', { type: 'text', placeholder: ph, autocomplete: 'off', spellcheck: 'false', value: draft[key] || '', 'aria-label': label });
      input.disabled = w.manualSaving;
      input.addEventListener('input', () => { draft[key] = input.value; }); return input;
    };
    const nameIn = mk('имя · vps', 'name', 'Имя установленного узла');
    const hostIn = mk('ssh-хост · user@адрес', 'host', 'Адрес установленного узла');
    const dirIn = mk('~/.jarvis', 'dir', 'Каталог установленного узла');
    const err = el('div.s2err', { style: w.manualError ? '' : 'display:none', text: remoteDisplayText(w.manualError), role: 'alert' });
    const showErr = t => { w.manualError = t || ''; err.textContent = t || ''; err.style.display = t ? '' : 'none'; };

    const add = button('Добавить', async (b) => {
      if (w.manualSaving) return;
      const name = (draft.name || '').trim();
      const sshHost = (draft.host || '').trim();
      const jarvisDir = (draft.dir || '').trim() || '~/.jarvis';
      if (!name || !sshHost) { showErr('Нужны имя и ssh-хост — остальное можно оставить как есть.'); return; }
      if (w.transport === 'teleport' && (!teleportProfile() || !teleportLogins().includes(sshHost.split('@')[0]))) { showErr('Войди в Teleport и укажи разрешённого SSH-пользователя перед @.'); return; }
      showErr(null);
      const connection = remoteConnection(w), identity = JSON.stringify(connection);
      w.manualSaving = true; repaintRemoteWiz();
      const res = await safe(() => window.jarvis.remotesAdd({ ...connection, name, sshHost, jarvisDir }), null);
      if (w.manualDraft !== draft) return;
      if (JSON.stringify(remoteConnection(w)) !== identity) { w.manualSaving = false; w.manualError = 'Подключение изменилось во время сохранения. Проверь список машин.'; repaintRemoteWiz(); return; }
      w.manualSaving = false;
      if (res && res.ok) { remoteWizReset(); w.connectedName = name; w.flash = 'Машина «' + name + '» добавлена.'; reRenderPane('remotes'); return; }
      w.manualError = (res && res.error) || 'Не удалось добавить узел'; repaintRemoteWiz();
    }, 'sm primary');
    add.disabled = w.manualSaving; if (w.manualSaving) add.textContent = 'Добавляем…';

    for (const i of [nameIn, hostIn, dirIn]) {
      i.addEventListener('keydown', (e) => { if (e.key === 'Enter') add.click(); });
    }

    const hint = el('div.s2hint', null, [
      el('kbd', { text: keyName('enter') }),
      el('span', { text: 'добавить · каталог по умолчанию ~/.jarvis' }),
    ]);

    return el('div.drow', null, [
      el('div.grow', null, [
        el('div.dt', { text: 'Узел уже стоит' }),
        el('div.dd', { text: 'Ставили через jarvis-setup remote add — тогда установка не нужна, узел надо просто '
          + 'прописать. Имя — как звать его в списке сессий, хост — то же, что пишешь в ssh, каталог — где живёт jarvis-node.' }),
        el('div.s2raccess-grid', null, [remoteField('Имя', nameIn), remoteField('Адрес', hostIn), remoteField('Каталог Jarvis', dirIn)]),
        err,
        hint,
      ]),
      el('div.dctl', null, [add]),
    ]);
  }

  function vmCard(info, error) {
    const section = el('section.s2-vm-area', { 'aria-label': 'Виртуальные машины' });
    section.appendChild(el('div.s2-machine-head', null, [
      el('div.dsection', { text: 'Виртуальные машины' }),
      button('Обновить VM', () => reRenderPane('remotes'), 'sm'),
    ]));
    const group = el('div.dgroup.s2-vm-list');
    section.appendChild(group);
    if (error || !info || info.ok === false) {
      group.appendChild(drow('Не удалось проверить VM', (error ? error.message || String(error) : info?.error) || 'Управление VM недоступно в этой сборке.', el('span.s2-vm-status', { text: 'Нет данных', 'data-state': 'unknown' })));
      return section;
    }
    if (!info.available) {
      group.appendChild(drow('agent-vm не найден',
        'Установи agent-vm для локальных Linux-окружений.',
        button('Как установить', () => required(() => window.jarvis.openUrl('https://github.com/MikD1/agent-vm')), 'sm')));
      if (info.error) group.appendChild(drow('Проверка VM', info.error, []));
      return section;
    }
    const generation = info.generation === 'modern'
      ? 'Одна VM может содержать несколько подключённых проектов. Их папки задаются в конфигурации этой VM.'
      : info.generation === 'legacy'
        ? 'В этой версии отдельная VM привязана к проекту. Более новые версии поддерживают несколько проектов в одной VM.'
        : 'Версия CLI не распознана. Доступны только действия, которые подтвердил адаптер.';
    if (info.error) group.appendChild(el('div.s2-vm-warning', { text: info.error, role: 'status' }));
    const vms = Array.isArray(info.vms) ? info.vms : [];
    for (const vm of vms) {
      const projects = Array.isArray(vm.projects) ? vm.projects : [];
      const status = { running: 'Работает', stopped: 'Остановлена', missing: 'Не найдена', unknown: 'Статус неизвестен' }[vm.status] || 'Статус неизвестен';
      const controls = el('div.s2-machine-actions');
      const run = (command, label) => button(label, async b => {
        b.disabled = true; b.textContent = command === 'start' ? 'Запускаем…' : command === 'stop' ? 'Останавливаем…' : 'Открываем…';
        await required(() => window.jarvis.vmAction(vm.name, command));
        if (command === 'open-config') { b.disabled = false; b.textContent = label; }
        else await reRenderPane('remotes');
      }, command === 'start' && !vm.connection?.canConnect ? 'sm primary' : 'sm');
      if (vm.status === 'stopped' && vm.capabilities?.start === true) controls.appendChild(run('start', 'Запустить'));
      if (vm.status === 'running' && vm.capabilities?.stop === true) controls.appendChild(run('stop', 'Остановить'));
      if (vm.connection?.canConnect) controls.appendChild(button('Подключить к Jarvis', async () => {
        if (resumeRemoteWork()) return;
        remoteWizReset(); const c = vm.connection;
        remoteWiz.host = c.sshHost || ''; remoteWiz.name = c.name || vm.name; remoteWiz.dir = c.jarvisDir || '~/.jarvis';
        for (const key of ['transport', 'sshConfigFile', 'teleportProxy', 'teleportCluster', 'runAsUser', 'nodeTcpPort']) remoteWiz[key] = c[key] || (key === 'transport' ? 'ssh' : '');
        machineView.editor = true; await reRenderPane('remotes'); runRemoteProbe();
      }, 'sm primary'));
      const copy = projects.length ? 'Проектов: ' + projects.length : 'Без подключённых проектов';
      const row = drow(vm.name || 'VM без имени', copy, [el('span.s2-vm-status', { text: status, 'data-state': vm.status || 'unknown' }), controls]);
      row.dataset.vmName = vm.name || '';
      const grow = row.querySelector('.grow');
      if (vm.registryStatus === 'unmanaged') grow.appendChild(el('div.s2-vm-meta', { text: 'Эта VM не управляется agent-vm.' }));
      if (vm.registryStatus === 'orphaned') grow.appendChild(el('div.s2-vm-meta', { text: 'Проверь соответствие конфигурации и VM.' }));
      const content = [];
      if (vm.directory) content.push(el('div.s2-vm-mount', null, [el('strong', { text: 'Папка окружения' }), el('div', null, el('code', { text: vm.directory }))]));
      for (const project of projects) {
        const mount = el('div.s2-vm-mount', null, [el('strong', { text: project.name || 'Проект' })]);
        if (project.path) mount.appendChild(el('div', null, el('code', { text: project.path })));
        if (project.guestPath) mount.appendChild(el('div', { text: 'Внутри VM: ' + project.guestPath }));
        content.push(mount);
      }
      if (vm.configPath) content.push(el('div.s2-vm-mount', null, [el('strong', { text: 'Конфигурация VM' }), el('div', null, el('code', { text: vm.configPath }))]));
      if (vm.capabilities?.openConfig === true) content.push(el('div.s2-vm-advanced', null, [run('open-config', 'Открыть конфигурацию')]));
      if (content.length) {
        const details = settingsDetails('s2-vm-details-' + vm.name, 'Проекты и конфигурация', content);
        details.classList.add('s2-vm-details'); grow.appendChild(details);
      }
      group.appendChild(row);
    }
    if (!vms.length) group.appendChild(drow('VM пока нет', 'Создай окружение в agent-vm и обнови список.', []));
    const environment = settingsDetails('s2-vm-environment', 'О среде agent-vm', [
      el('div.s2-vm-runtime', null, [
        el('strong', { text: info.version ? 'Версия ' + info.version : 'Версия неизвестна' }),
        el('p', { text: generation }),
        el('p', { text: 'Подключение к Jarvis добавит чаты и уведомления этой VM.' }),
      ]),
    ]);
    section.appendChild(environment);
    return section;
  }

  function isolationSettings(settings) {
    const group = el('div.dgroup');
    group.appendChild(drow('Отдельная ветка', 'Worktree создаёт отдельную рабочую папку и Git-ветку на выбранной машине. Включается для конкретной задачи в проекте.', []));
    const docker = drow('Docker', 'Контейнер изолирует инструменты внутри выбранного образа. Включается отдельно для задачи; образ используется при локальном запуске.', []);
    const image = el('input.s2-secret', { type: 'text', 'aria-label': 'Образ Docker', placeholder: 'registry.example.com/agents/dev:latest', value: settings.launchDockerImage || '', autocomplete: 'off', spellcheck: 'false' });
    const status = el('span.loadcap', { role: 'status' });
    const save = button('Сохранить образ', async b => {
      const value = image.value.trim();
      if (/\s/.test(value)) throw new Error('Укажи имя образа без пробелов, например registry/team/agent:tag.');
      b.disabled = true; b.textContent = 'Сохраняем…';
      await required(() => window.jarvis.setSettings({ launchDockerImage: value }));
      b.disabled = false; b.textContent = 'Сохранить образ';
      status.textContent = value ? 'Образ сохранён' : 'Образ не выбран';
    }, 'sm');
    image.addEventListener('keydown', e => { if (e.key === 'Enter') { e.preventDefault(); save.click(); } });
    docker.querySelector('.grow').append(el('div.s2-vm-meta', { text: 'Образ должен содержать нужные CLI и зависимости проекта. Наличие Docker проверяется при старте задачи.' }), image, status);
    docker.querySelector('.dctl').appendChild(save);
    group.appendChild(docker);
    return settingsDetails('s2-task-isolation', 'Изоляция задачи: worktree и Docker', [group]);
  }

  let closeMachineEditor = null;
  async function renderRemotes(pane) {
    pane.classList.add('connections-pane');
    const header = el('div.connection-page-head');
    const subtitle = el('p', { text: 'Серверы и рабочие окружения' });
    const add = button('Добавить машину', () => openEditor(), 'primary');
    add.prepend(icon('plus'));
    header.append(el('div', null, [el('h1.dtitle', { text: 'Машины' }), subtitle]));
    pane.append(header);
    const ready = remotesApiReady(); add.disabled = true;
    const skeleton = skelGroup(2); pane.appendChild(skeleton);
    const [remoteResult, vmResult, settingsResult] = await Promise.allSettled([
      ready ? required(() => window.jarvis.remotesList()) : Promise.resolve(null),
      typeof window.jarvis.vmStatus === 'function' ? required(() => window.jarvis.vmStatus()) : Promise.resolve(null),
      required(() => window.jarvis.getSettings()),
    ]);
    if (!pane.isConnected) return;
    skeleton.remove();
    const raw = remoteResult.status === 'fulfilled' ? remoteResult.value : null;
    const nodes = Array.isArray(raw) ? raw : raw?.remotes || [];
    const vmInfo = vmResult.status === 'fulfilled' ? vmResult.value : null;
    const vms = Array.isArray(vmInfo?.vms) ? vmInfo.vms : [];
    const inventory = el('div.connection-inventory');
    const detail = el('aside.connection-detail', { 'aria-label': 'Выбранная машина' });
    const workspace = el('div.connection-workspace', null, [inventory, detail]);
    const tools = el('div.connection-toolbar');
    const query = el('input', { type: 'search', placeholder: 'Найти машину…', 'aria-label': 'Найти машину', value: machineView.query });
    tools.append(el('label.connection-search', null, [icon('search'), query]));
    const refresh = button('Обновить', () => reRenderPane('remotes'), 'sm'); refresh.prepend(icon('refresh-cw')); tools.append(refresh, add);
    const filters = el('div.connection-filters', { role: 'group', 'aria-label': 'Тип машин' });
    const filterButtons = [];
    for (const [value, label, count] of [['all', 'Все', nodes.length + vms.length], ['remote', 'Удалённые', nodes.length], ['local', 'Локальные VM', vms.length]]) {
      const b = button(label, () => { machineView.filter = value; paintInventory(); }, 'sm');
      b.append(el('span', { text: String(count), 'aria-hidden': 'true' })); filterButtons.push([value, b]); filters.append(b);
    }
    const grid = el('div.connection-grid', { 'aria-label': 'Машины' });
    inventory.append(tools, filters, grid);
    const notices = el('div.connection-inventory-notices'); inventory.append(notices);
    if (remoteResult.status === 'rejected') notices.append(el('div.connection-alert', { role: 'alert', text: 'Не удалось получить подключения: ' + (remoteResult.reason?.message || remoteResult.reason) }));
    if (vmResult.status === 'rejected' || vmInfo?.error) notices.append(el('div.connection-alert', { role: 'status', text: vmResult.status === 'rejected' ? String(vmResult.reason?.message || vmResult.reason) : vmInfo.error }));
    if (!vmInfo?.available && !vmResult.reason) notices.append(el('div.connection-local-note', null, [icon('cube'), el('div', null, [el('strong', { text: 'Локальные VM' }), el('p', { text: 'agent-vm не найден. Удалённые подключения уже доступны.' })]), button('Как установить', () => required(() => window.jarvis.openUrl('https://github.com/MikD1/agent-vm')), 'sm')]));
    if (settingsResult.status === 'fulfilled') {
      const preferences = el('div.connection-preferences', null, [isolationSettings(settingsResult.value || {})]); inventory.append(preferences);
    }
    const setup = el('section.connection-editor#s2-ssh-setup', { 'aria-label': 'Добавить машину', 'data-escape-owner': '' });
    const editorTitle = el('h2', { text: 'Новое подключение' });
    const cancel = button('Отмена', () => closeMachineEditor?.(), 'sm');
    cancel.dataset.connectionCancel = '';
    setup.append(el('div.connection-editor-head', null, [el('div', null, [editorTitle, el('p', { text: 'Выбери способ подключения и укажи машину.' })]), cancel]));
    const box = el('div#s2-rwiz'); setup.append(box); paintRemoteWiz(box);
    pane.append(workspace, setup);
    function syncEditor() {
      setup.open = machineView.editor; setup.hidden = !machineView.editor; workspace.hidden = machineView.editor; add.hidden = machineView.editor;
      syncRemoteEditorChrome();
      if (machineView.editor && remoteWiz.transport === 'teleport' && remoteWiz.teleport?.loginPending) refreshTeleportStatus(true);
    }
    function openEditor() {
      machineView.editor = true; repaintRemoteWiz(); syncEditor();
      (box.querySelector('input:not(:disabled)') || cancel).focus();
    }
    closeMachineEditor = () => {
      if (!setup.isConnected || !machineView.editor || remoteWiz.manualSaving) return false;
      machineView.editor = false;
      if (!remoteWiz.install?.running) { resetRemoteProbe(); remoteWiz.formErr = null; }
      clearTimeout(teleportPollTimer);
      repaintRemoteWiz(); syncEditor(); paintInventory(); add.focus(); return true;
    };
    setup.addEventListener('keydown', event => {
      if (event.key === 'Escape' && !event.isComposing) { event.preventDefault(); event.stopPropagation(); closeMachineEditor(); }
    });
    query.addEventListener('input', () => { machineView.query = query.value; paintInventory(); });
    const items = [...nodes.map(remote => ({ key: 'remote:' + remote.name, kind: 'remote', value: remote })), ...vms.map(vm => ({ key: 'vm:' + vm.name, kind: 'local', value: vm }))];
    if (remoteWiz.connectedName && remoteWiz.connectedName !== machineView.notifiedConnection && items.some(item => item.key === 'remote:' + remoteWiz.connectedName)) {
      machineView.selected = 'remote:' + remoteWiz.connectedName;
      machineView.notifiedConnection = remoteWiz.connectedName;
    }
    if (!items.some(item => item.key === machineView.selected)) machineView.selected = items[0]?.key || '';
    function paintDetail() {
      detail.replaceChildren();
      const item = items.find(item => item.key === machineView.selected);
      if (!item) {
        detail.append(el('div.connection-empty-detail', null, [el('span.connection-avatar.large', { 'aria-hidden': 'true' }, icon('server')), el('h2', { text: 'Твои машины — здесь' }), el('p', { text: 'Добавь сервер по SSH или выбери машину в Teleport. Jarvis проверит доступ и найдёт агентов.' }), button('Подключить машину', openEditor, 'primary')])); return;
      }
      if (item.kind === 'remote') detail.append(remoteRow(item.value, () => { paintInventory(); paintDetail(); }));
      else {
        const vm = item.value;
        detail.append(el('div.connection-host-head', null, [el('span.connection-avatar.large', { 'data-kind': 'local', 'aria-hidden': 'true' }, icon('cube')), el('div', null, [el('h2', { text: vm.name }), el('p', { text: 'Локальная виртуальная машина' })])]));
        const card = vmCard({ ...vmInfo, vms: [vm] }, null); detail.append(card);
        card.querySelector('.s2-machine-head')?.remove();
      }
    }
    function paintInventory() {
      grid.replaceChildren();
      for (const [value, b] of filterButtons) b.setAttribute('aria-pressed', String(machineView.filter === value));
      const needle = machineView.query.trim().toLocaleLowerCase();
      const matches = items.filter(item => (machineView.filter === 'all' || item.kind === machineView.filter) && [item.value.name, item.value.sshHost, item.value.transport, item.value.teleportCluster].join(' ').toLocaleLowerCase().includes(needle));
      for (const item of matches) {
        const value = item.value, remote = item.kind === 'remote';
        const kind = remote ? value.transport === 'teleport' ? 'teleport' : 'ssh' : 'local';
        const card = el('button.connection-card', { type: 'button', 'aria-pressed': String(machineView.selected === item.key), 'aria-label': value.name });
        if (remote) card.dataset.machineName = value.name; else card.dataset.vmName = value.name;
        const status = remote ? value.connected ? 'На связи' : value.error ? 'Нет связи' : 'Не проверено' : { running: 'Работает', stopped: 'Остановлена', unknown: 'Статус неизвестен', missing: 'Не найдена' }[value.status] || 'Статус неизвестен';
        const state = remote ? value.connected ? 'connected' : value.error ? 'error' : 'unknown' : value.status;
        const type = remote ? kind === 'teleport' ? 'Teleport' : 'SSH' : 'Локальная VM';
        card.append(el('div.connection-card-top', null, [el('span.connection-avatar', { 'data-kind': kind, 'aria-hidden': 'true' }, icon(kind === 'teleport' ? 'shield-check' : kind === 'local' ? 'cube' : 'server')), machineBadge(status, state)]), el('strong', { text: value.name }), el('span.connection-card-address', { text: remote ? machineAddress(value).user ? machineAddress(value).user + ' · ' + type : type : 'Проектов: ' + (value.projects?.length || 0) }), el('span.connection-card-footer', null, [el('span', { text: type }), icon('arrow-right')]));
        card.addEventListener('click', () => {
          machineView.selected = item.key; machineView.detail = 'overview'; paintInventory(); paintDetail();
          [...grid.querySelectorAll('button')].find(b => b.getAttribute('aria-label') === value.name)?.focus();
          if (typeof matchMedia === 'function' && matchMedia('(max-width: 800px)').matches) detail.scrollIntoView({ behavior: matchMedia('(prefers-reduced-motion: reduce)').matches ? 'instant' : 'smooth', block: 'start' });
        });
        grid.append(card);
      }
      if (!matches.length) grid.append(el('div.connection-empty-list', null, [icon(needle ? 'search' : 'server'), el('strong', { text: needle ? 'Машины не найдены' : 'Подключений пока нет' }), el('p', { text: needle ? 'Попробуй другое имя или адрес.' : 'Добавь первую машину, чтобы работать с её агентами из Jarvis.' })]));
      notices.querySelector('.connection-install-resume')?.remove();
      if (remoteWiz.install?.running || remoteWiz.flash) notices.prepend(el('div.connection-install-resume', null, [el('span', { text: remoteWiz.install?.running ? 'Подготовка «' + remoteWiz.install.name + '» продолжается' : remoteWiz.flash }), button('Открыть', openEditor, 'sm')]));
    }
    paintInventory(); paintDetail(); syncEditor(); add.disabled = !ready;
  }

  /* 1d. Агенты (agents) — свои CLI помимо claude и codex.
   *
   * Человек вводит путь до бинарника — остальное (tmux-шим и хуки жизненного
   * цикла) настраивается само при сохранении. Возможности честно ограничены:
   * сессия в списке, живой экран паны, ответ в пану. Статусов «думает /
   * спрашивает» и транскрипта у чужого CLI нет — их не выдумываем. */
  async function renderAgents(pane) {
    pane.appendChild(el('div.dtitle', { text: 'Агенты' }));
    if (window.JarvisInstances) await window.JarvisInstances.render(pane);
    const ready = typeof window.jarvis.agentsList === 'function';
    const _sk = skelGroup(2); pane.appendChild(_sk);
    const res = ready ? await safe(() => window.jarvis.agentsList(), null) : null;
    _sk.remove();
    if (!res || !res.ok) {
      pane.appendChild(el('div.dgroup', null, [
        drow('Недоступно', 'Нужна свежая сборка приложения.', []),
      ]));
      return;
    }
    let list = (res.agents || []).map((a) => ({ ...a }));
    const presets = res.presets || [];
    let editingId = null, customId = false, busy = false;
    const reserved = new Set(['claude', 'codex', 'jarvis', 'tmux', 'sh', 'bash', 'zsh']);
    const section = el('section.s2-custom-agents', { 'aria-label': 'Другие агенты' });
    const note = el('p.s2-agent-status', { id: 's2-agent-feedback', role: 'status', 'aria-live': 'polite' });
    note.hidden = true;
    const paintNote = (text, bad = false) => {
      note.textContent = text || '';
      note.hidden = !text;
      note.dataset.kind = bad ? 'error' : 'success';
      note.setAttribute('role', bad ? 'alert' : 'status');
      if (bad && !form.hidden) formTitle.after(note);
      else if (section.contains(group)) section.insertBefore(note, group);
    };
    const group = el('div.dgroup.s2-agent-list');
    const form = el('form.s2-agent-form', { 'aria-label': 'Настройка агента', 'aria-describedby': 's2-agent-feedback', 'data-escape-owner': '' });
    form.hidden = true;
    const field = (label, name, placeholder, hint) => {
      const id = 's2-agent-' + name;
      const control = el('input.s2-secret', { id, name, type: 'text', placeholder, autocomplete: 'off', spellcheck: 'false', 'aria-labelledby': id + '-label' });
      const wrapper = el('label.s2-field', { for: id }, [
        el('span.s2-field-label', { id: id + '-label', text: label }), control,
        hint ? el('span.s2-hint', { id: id + '-hint', text: hint }) : null,
      ]);
      if (hint) control.setAttribute('aria-describedby', id + '-hint');
      return { control, wrapper };
    };
    const nameField = field('Название', 'name', 'Например, Qwen Code');
    const binField = field('Программа', 'bin', 'qwen или /путь/к/qwen', 'Имя установленного CLI или полный путь к программе.');
    const idField = field('Идентификатор', 'id', 'qwen', 'Создаётся автоматически. Используется в командах и для связи с чатами.');
    const resumeField = field('Команда продолжения чата', 'resume', 'qwen --resume {sid}', 'Необязательно. Вставь {sid} на месте номера чата.');
    const nameIn = nameField.control, binIn = binField.control, idIn = idField.control, resumeIn = resumeField.control;
    idIn.maxLength = 24;
    const formTitle = el('h3', { text: 'Новый агент' });
    const chips = el('div.s2agents-chips', { 'aria-label': 'Готовые варианты' });
    const advanced = el('details.s2-details.s2-agent-advanced', null, [
      el('summary', { text: 'Дополнительно' }),
      el('div.s2-form-grid', null, [idField.wrapper, resumeField.wrapper]),
    ]);
    const suggestId = () => {
      if (editingId || customId) return;
      const bin = binIn.value.trim().split('/').filter(Boolean).pop();
      let base = (bin || nameIn.value || 'agent').toLowerCase().replace(/[^a-z0-9_-]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 24) || 'agent';
      if (reserved.has(base)) base = 'external-' + base;
      base = base.slice(0, 24);
      let candidate = base, suffix = 2;
      while (list.some(agent => agent.id === candidate)) {
        const ending = '-' + suffix++;
        candidate = base.slice(0, 24 - ending.length) + ending;
      }
      idIn.value = candidate;
    };
    nameIn.addEventListener('input', suggestId);
    binIn.addEventListener('input', suggestId);
    idIn.addEventListener('input', () => { customId = !!idIn.value.trim(); if (!customId) suggestId(); });
    const clearInvalid = () => { for (const control of form.querySelectorAll('input')) control.removeAttribute('aria-invalid'); };
    const invalid = (control, message) => {
      clearInvalid(); control.setAttribute('aria-invalid', 'true');
      if (advanced.contains(control)) advanced.open = true;
      paintNote(message, true); control.focus(); return false;
    };
    const closeForm = () => {
      form.hidden = true; editingId = null; paintNote(''); clearInvalid(); addButton.hidden = false; addButton.focus();
    };
    const openForm = (agent = null) => {
      if (busy) return;
      editingId = agent?.id || null;
      customId = !!agent;
      nameIn.value = agent?.name || ''; binIn.value = agent?.bin || '';
      idIn.value = agent?.id || ''; resumeIn.value = agent?.resume || '';
      idIn.readOnly = !!agent;
      formTitle.textContent = agent ? 'Настройка ' + (agent.name || agent.id) : 'Новый агент';
      saveButton.textContent = agent ? 'Сохранить изменения' : 'Добавить агента';
      deleteButton.hidden = !agent;
      chips.hidden = !!agent || !presets.length;
      advanced.open = false;
      clearInvalid(); paintNote(''); form.hidden = false; addButton.hidden = true;
      if (!agent) suggestId();
      nameIn.focus();
    };
    // Commit the in-memory list only after persistence succeeds. A failed
    // rename, removal or duplicate entry must never erase a working agent.
    const saveAll = async (next, message = 'Сохранено. Агент доступен в «Проектах».') => {
      if (busy) return false;
      busy = true;
      section.setAttribute('aria-busy', 'true');
      const controls = [...section.querySelectorAll('button, input')].map(control => [control, control.disabled]);
      for (const [control] of controls) control.disabled = true;
      try {
        const result = await required(() => window.jarvis.agentsSave(next));
        if (!result?.ok) throw new Error(result?.error || 'Не удалось сохранить агента.');
        list = next;
        paintList();
        closeForm();
        paintNote(result.missing?.length
          ? 'Сохранено. Программа не найдена: ' + result.missing.join(', ') + '. Установи её или измени путь.'
          : message);
        if (result.missing?.length) note.dataset.kind = 'warning';
        return true;
      } catch (error) {
        paintNote(error?.message || 'Не удалось сохранить. Попробуй ещё раз.', true);
        return false;
      } finally {
        busy = false; section.removeAttribute('aria-busy');
        for (const [control, disabled] of controls) control.disabled = disabled;
        if (form.hidden) addButton.focus();
      }
    };
    const saveForm = async () => {
      if (busy) return;
      suggestId(); clearInvalid();
      const name = nameIn.value.trim(), bin = binIn.value.trim(), id = idIn.value.trim(), resume = resumeIn.value.trim();
      if (!name) return invalid(nameIn, 'Укажи название агента.');
      if (!bin) return invalid(binIn, 'Укажи программу для запуска.');
      if (!/^[a-z0-9_-]{1,24}$/.test(id) || reserved.has(id)) return invalid(idIn, 'Выбери свободный идентификатор: до 24 латинских букв, цифр, дефисов или подчёркиваний.');
      if (list.some(agent => agent.id === id && agent.id !== editingId)) return invalid(idIn, 'Этот идентификатор уже используется. Укажи другой.');
      if (resume && !resume.includes('{sid}')) return invalid(resumeIn, 'Добавь {sid} в команду продолжения — сюда подставится номер чата.');
      const previous = list.find(agent => agent.id === editingId);
      const candidate = { ...(previous || { dangerousFlag: '' }), id: previous?.id || id, name, bin, resume };
      await saveAll(previous ? list.map(agent => agent.id === editingId ? candidate : agent) : [...list, candidate]);
    };
    const addButton = button('Добавить агента', () => openForm(), 'sm');
    const saveButton = el('button.btn.primary', { type: 'submit', text: 'Добавить агента' });
    const cancelButton = button('Отмена', closeForm);
    const deleteButton = button('Удалить агента', () => saveAll(list.filter(agent => agent.id !== editingId), 'Агент удалён из Jarvis.'), 'danger');
    deleteButton.hidden = true;
    form.addEventListener('submit', event => { event.preventDefault(); saveForm(); });
    form.addEventListener('keydown', event => {
      if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); if (!busy) closeForm(); }
    });
    for (const preset of presets) {
      chips.appendChild(button(preset.name, () => {
        nameIn.value = preset.name; binIn.value = preset.bin; resumeIn.value = preset.resume || '';
        customId = false; suggestId(); clearInvalid(); nameIn.focus();
      }, 'sm'));
    }
    form.append(formTitle, chips,
      el('div.s2-form-grid', null, [nameField.wrapper, binField.wrapper]),
      advanced,
      el('div.s2-form-actions', null, [saveButton, cancelButton, deleteButton]));
    const paintList = () => {
      group.replaceChildren();
      if (!list.length) {
        group.appendChild(el('p.s2-hint.s2-agent-empty', { text: 'Подключи другой установленный CLI, чтобы запускать его из «Проектов».' }));
      }
      for (const agent of list) {
        const row = drow(agent.name || agent.id, agent.bin || 'Программа не указана', [
          el('span.s2-agent-kind', { text: 'Терминал' }),
          button('Изменить', () => openForm(agent), 'sm'),
        ]);
        row.dataset.agentId = agent.id;
        row.querySelector('.dd').classList.add('s2-agent-command');
        row.querySelector('.dd').title = agent.bin || 'Укажи программу в настройках агента';
        row.querySelector('button').setAttribute('aria-label', 'Настроить ' + (agent.name || agent.id));
        group.appendChild(row);
      }
    };
    section.append(
      el('div.s2-agent-section-head', null, [el('h2.dsection', { text: 'Другие агенты' }), addButton]),
      group, form, note,
      el('details.s2-details.s2-agent-capabilities', null, [
        el('summary', { text: 'Возможности подключения' }),
        el('p.s2-hint', { text: 'Запуск, просмотр терминала и ответы доступны в Jarvis. Начало и завершение сессии отслеживаются автоматически.' }),
        el('p.s2-hint', { text: 'История сообщений, расход токенов и вопросы с вариантами ответа требуют отдельной интеграции агента.' }),
      ]));
    pane.appendChild(section);
    paintList();
  }

  const RENDERERS = {

    general: renderGeneral,
    look: renderLook,
    remotes: renderRemotes,
    agents: renderAgents,
    stt: renderStt,
    voice: renderVoice,
    wake: renderWake,
    notify: renderNotify,
    awake: renderAwake,
    keys: renderKeys,
    launch: renderLaunch,
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
        if (fn) {
          try { await fn(node); }
          catch (e) {
            for (const skeleton of node.querySelectorAll('.skrow')) skeleton.remove();
            node.appendChild(el('div.meeting-status.error', { text: 'Не удалось загрузить раздел: ' + String(e), role: 'alert' }));
            node.appendChild(button('Повторить загрузку', () => reRenderPane(pane), 'sm'));
          }
        }
      } while (renderPending[pane]);
    } finally {
      renderingPane[pane] = false;
      const node = currentRoot?.querySelector('#s2-pane-' + pane);
      for (const row of node?.querySelectorAll('.drow') || []) {
        const label = row.querySelector('.dt')?.textContent;
        if (pane !== 'remotes' && label && !searchIndex.some(entry => entry.pane === pane && entry.label === label)) {
          searchIndex.push({ pane, label, words: row.querySelector('.dd')?.textContent || '' });
        }
      }
      focusSearchTarget(pane);
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

    /* ── Установка удалённого узла: шаги копим в состоянии мастера, а в DOM
     * трогаем только лог с полосой — иначе перерисовка мигала бы кнопками и
     * сбрасывала прокрутку лога на каждом шаге. ─────────────────────────── */
    try {
      window.jarvis.onRemoteInstallStep((s) => {
        const st = remoteWiz.install;
        if (!st || !s) return;
        st.steps.push({ phase: remoteDisplayText(s.phase), state: s.state || 'info', msg: remoteDisplayText(s.msg) });
        // лог длинной установки не должен расти без границ
        if (st.steps.length > 200) st.steps.splice(0, st.steps.length - 200);
        if (typeof s.pct === 'number') st.pct = s.pct;
        repaintRemoteInstall();
      });
    } catch (e) {}
    try {
      window.jarvis.onRemoteInstallDone((r) => {
        const st = remoteWiz.install;
        if (!st || (r?.name && r.name !== st.name)) return;
        st.running = false;
        if (r && r.ok) {
          // успех: мастер сворачиваем, список перечитываем — узел уже там
          const name = (r && r.name) || st.name;
          remoteWizReset();
          remoteWiz.connectedName = name;
          remoteWiz.flash = 'Машина «' + name + '» подключена.';
          reRenderPane('remotes');
          return;
        }
        st.error = remoteDisplayText(r && r.error) || 'установка не удалась — подробности в ~/.jarvis/jarvis.log';
        repaintRemoteWiz();
      });
    } catch (e) {}
  }

  /* ========================================================================
   * Главная функция: построить весь UI в rootEl.
   * ====================================================================== */
  function mountSurface(rootEl, pane) {
    cancelShortcutRecording?.();
    closeAllSelects(null);
    clearTimeout(teleportPollTimer);
    injectStyle();
    // Shared operation state survives navigation; form DOM belongs to one
    // mounted surface so callbacks never find duplicate controls in hidden UI.
    if (currentRoot && currentRoot !== rootEl) currentRoot.replaceChildren();
    currentRoot = rootEl;
    activePane = pane;
    rootEl.replaceChildren();
    if (!docClickBound) {
      docClickBound = true;
      document.addEventListener('click', () => { if (currentRoot) closeAllSelects(null); });
    }
    subscribeOnce();
  }

  function initMachines(rootEl) {
    if (!rootEl) return;
    mountSurface(rootEl, 'remotes');
    const pane = el('div.dpane.on.connections-pane#s2-pane-remotes');
    rootEl.append(el('div.settings-surface#machines2', null, [el('div.detail.machines-content', null, [pane])]));
    return reRenderPane('remotes').then(() => {
      paintSettingsError();
      if (currentRoot === rootEl && !rootEl.closest('[hidden]') && document.activeElement?.id === 'pageBack') {
        const target = machineView.editor ? rootEl.querySelector('.connection-editor input:not(:disabled)') : rootEl.querySelector('.connection-search input');
        target?.focus();
      }
    });
  }

  function initSettings2(rootEl) {
    if (!rootEl) return;
    mountSurface(rootEl, lastSettingsPane);

    const win = el('div.swin2.settings-surface#settings2');

    // ── Сайдбар ──
    const sidebar = el('div.sidebar');
    sidebar.appendChild(el('div.ssearch', null, [
      el('span.si', null, icon('search')),
      el('input#settingsSearch', { placeholder: 'Найти настройку…', 'aria-label': 'Поиск настроек' }),
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
      if (n.group) snav.appendChild(el('div.grp', { text: n.group }));
      const item = el('button.item' + (n.pane === activePane ? '.sel' : ''), { 'data-pane': n.pane, type: 'button' }, [
        el('span.ic.' + n.ic, null, icon(n.icon)),
        document.createTextNode(n.label),
      ]);
      item.addEventListener('click', () => selectPane(n.pane));
      navItems[n.pane] = item;
      snav.appendChild(item);
    }
    snav.addEventListener('keydown', event => {
      if (event.isComposing || event.metaKey || event.ctrlKey || event.altKey) return;
      if (!['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) return;
      const items = Object.values(navItems).filter(item => !item.hidden);
      const index = items.indexOf(event.target.closest('.item'));
      if (index < 0) return;
      const next = event.key === 'Home' ? 0 : event.key === 'End' ? items.length - 1
        : (index + (event.key === 'ArrowDown' ? 1 : -1) + items.length) % items.length;
      event.preventDefault(); event.stopPropagation(); items[next].focus(); items[next].click();
    });
    const searchEmpty = el('div.settings-search-empty', { text: 'Ничего не найдено. Попробуй «микрофон», «клавиши» или «Claude».' });
    searchEmpty.hidden = true;
    sidebar.appendChild(snav);
    sidebar.appendChild(searchEmpty);
    const search = sidebar.querySelector('#settingsSearch');
    function searchNav() {
      const words = search.value.toLocaleLowerCase().trim().split(/\s+/).filter(Boolean);
      const matches = words.length ? searchIndex.filter(entry => words.every(word =>
        (entry.label + ' ' + entry.words + ' ' + NAV.find(n => n.pane === entry.pane).label).toLocaleLowerCase().includes(word))) : [];
      let count = 0;
      for (const n of NAV) {
        if (n.sep) continue;
        const match = !words.length || matches.some(entry => entry.pane === n.pane);
        navItems[n.pane].hidden = !match; if (match) count++;
      }
      for (const sep of snav.querySelectorAll('.sep, .grp')) sep.hidden = words.length > 0;
      searchEmpty.hidden = count > 0;
      searchResults.replaceChildren(); searchResults.hidden = !words.length;
      for (const node of Object.values(paneNodes)) node.hidden = words.length > 0;
      currentRoot.querySelector('#settings-save-error')?.remove();
      if (!words.length) { paintSettingsError(); return; }
      searchResults.appendChild(el('div.dtitle', { text: 'Настройки' }));
      searchResults.appendChild(el('p.settings-result-count', { role: 'status', text: matches.length ? 'Найдено: ' + matches.length : 'Нет подходящих настроек. Попробуйте другой запрос.' }));
      for (const entry of matches) {
        searchResults.appendChild(el('button.settings-result', { type: 'button', onclick: () => selectPane(entry.pane, entry.label) }, [
          el('strong', { text: entry.label }), el('span', { text: NAV.find(n => n.pane === entry.pane).label }),
        ]));
      }
    }
    search.addEventListener('input', searchNav);
    search.addEventListener('keydown', e => {
      if (e.isComposing) return;
      if (e.key === 'Escape' && search.value) { e.preventDefault(); e.stopPropagation(); search.value = ''; searchNav(); }
      if (e.key === 'Enter' || e.key === 'ArrowDown') {
        const first = searchResults.querySelector('button') || Object.values(navItems).find(n => !n.hidden);
        if (first) { e.preventDefault(); if (e.key === 'Enter') first.click(); first.focus(); }
      }
    });
    win.appendChild(sidebar);

    // ── Детальная колонка ──
    const detail = el('div.detail');
    const searchResults = el('div.settings-results', { 'aria-label': 'Результаты поиска настроек' });
    searchResults.hidden = true;
    searchResults.addEventListener('keydown', event => {
      if (!['ArrowUp', 'ArrowDown', 'Home', 'End'].includes(event.key) || event.isComposing) return;
      const rows = [...searchResults.querySelectorAll('button')], index = rows.indexOf(event.target);
      if (index < 0) return;
      event.preventDefault(); event.stopPropagation();
      const next = event.key === 'Home' ? 0 : event.key === 'End' ? rows.length - 1 : Math.max(0, Math.min(rows.length - 1, index + (event.key === 'ArrowDown' ? 1 : -1)));
      rows[next].focus(); rows[next].scrollIntoView?.({ block: 'nearest' });
    });
    detail.appendChild(searchResults);
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
    function selectPane(pane, label) {
      activePane = lastSettingsPane = pane;
      pendingSettingFocus = label ? { pane, label } : null;
      if (search.value) { search.value = ''; searchNav(); }
      paintSettingsError();
      for (const k in navItems) navItems[k].classList.toggle('sel', k === pane);
      for (const k in paneNodes) paneNodes[k].classList.toggle('on', k === pane);
      closeAllSelects(null);
      const node = paneNodes[pane];
      // через reRenderPane (сериализованный) — чтобы прямой рендер не гонялся с
      // live-перерисовкой (onAudioState и т.п.) и не задваивал контент вкладки.
      if (node && !node.childNodes.length) reRenderPane(pane);
      else focusSearchTarget(pane);
      if (pane === 'remotes') resumeTeleportPolling();
    }
    window.jarvisOpenSettingsPane = pane => {
      if (pane === 'remotes') { openMachines(); return; }
      if (paneNodes[pane] && currentRoot === rootEl && win.isConnected) selectPane(pane);
      else if (Object.prototype.hasOwnProperty.call(RENDERERS, pane)) lastSettingsPane = pane;
    };

    // отрисовать активную панель сразу (через сериализованный reRenderPane)
    reRenderPane(activePane);
    paintSettingsError();
  }

  window.initSettings2 = initSettings2;
  window.initMachines = initMachines;
})();
