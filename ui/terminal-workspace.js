/* A client-owned terminal: remote output never replaces the selected DOM text.
 * tmux owns processes; xterm owns rendering, scrollback and the local clipboard. */
(() => {
  'use strict';
  const encoder = new TextEncoder();
  const HISTORY_LINES = 100000;
  const MOUSE_MODES = new Set([9, 1000, 1002, 1003, 1005, 1006, 1015, 1016]);

  // Filter only mouse reporting DECSET/DECRST while reading. Keep all other VT
  // bytes, including split UTF-8 and mixed private modes, in their original order.
  function mouseFilter() {
    let pending = [], modes = new Set();
    return {
      push(bytes, interactive) {
        const output = [];
        for (const byte of bytes) {
          if (!pending.length && byte !== 27) { output.push(byte); continue; }
          pending.push(byte);
          const text = String.fromCharCode(...pending);
          if (text === '\x1b' || text === '\x1b[' || /^\x1b\[\?[\d;]*$/.test(text) && pending.length < 128) continue;
          const match = /^\x1b\[\?([\d;]+)([hl])$/.exec(text);
          if (match) {
            const values = match[1].split(';').map(Number);
            for (const mode of values) if (MOUSE_MODES.has(mode)) match[2] === 'h' ? modes.add(mode) : modes.delete(mode);
            const retained = interactive ? values : values.filter(mode => !MOUSE_MODES.has(mode));
            if (retained.length) output.push(...encoder.encode(`\x1b[?${retained.join(';')}${match[2]}`));
          } else output.push(...pending);
          pending = [];
        }
        return Uint8Array.from(output);
      },
      sync(interactive) {
        return encoder.encode(`\x1b[?${[...MOUSE_MODES].join(';')}l` + (interactive && modes.size ? `\x1b[?${[...modes].join(';')}h` : ''));
      },
    };
  }

  function bufferText(buffer) {
    let text = '';
    for (let row = 0; row < buffer.length; row++) {
      const line = buffer.getLine(row);
      if (!line) continue;
      if (row && !line.isWrapped) text += '\n';
      text += line.translateToString(!buffer.getLine(row + 1)?.isWrapped);
    }
    return text.replace(/\n+$/, '');
  }

  // The remote screen can be taller than our read-only viewport. Follow its
  // actual cursor, not the last (often empty) row of that screen. Coordinates
  // here are unzoomed CSS pixels within the outer scroll container.
  function cursorViewportScroll(buffer, rows, { screenTop, screenHeight, viewportHeight, scrollHeight }) {
    if (!buffer || buffer.viewportY !== buffer.baseY || !Number.isFinite(rows) || rows <= 0 ||
        ![buffer.cursorY, screenTop, screenHeight, viewportHeight, scrollHeight].every(Number.isFinite) ||
        screenHeight <= 0 || viewportHeight <= 0) return null;
    const row = Math.max(0, Math.min(rows - 1, buffer.cursorY));
    const bottom = screenTop + (row + 1) * screenHeight / rows;
    return Math.max(0, Math.min(Math.max(0, scrollHeight - viewportHeight), bottom - viewportHeight));
  }

  function create({ bridge, toast = () => {}, openExternal = () => {} }) {
    const el = (tag, cls, text) => { const node = document.createElement(tag); if (cls) node.className = cls; if (text != null) node.textContent = text; return node; };
    const root = el('section', 'sw-terminal tw-root'); root.hidden = true; root.setAttribute('aria-label', 'Терминал сессии');
    const head = el('div', 'tw-head');
    const stateDot = el('span', 'tw-state-dot'), title = el('strong', 'tw-title', 'Терминал'), location = el('span', 'tw-location');
    const actions = el('div', 'tw-actions');
    const button = (label, fn, icon, parent = actions) => {
      const b = el('button', 'tw-button'); b.type = 'button'; b.title = label; b.setAttribute('aria-label', label);
      if (icon && window.jarvisIcons?.create) b.append(window.jarvisIcons.create(icon));
      b.append(el('span', '', label)); b.addEventListener('click', fn); parent.append(b); return b;
    };
    head.append(stateDot, title, location, actions);
    const findBar = el('div', 'tw-find'); findBar.hidden = true;
    const query = el('input'); query.type = 'search'; query.placeholder = 'Найти в выводе'; query.setAttribute('aria-label', 'Найти в выводе терминала');
    const matches = el('span', 'tw-matches'); matches.setAttribute('aria-live', 'polite'); findBar.append(query, matches);
    const viewport = el('div', 'tw-viewport');
    const screen = el('div', 'tw-screen'); viewport.append(screen);
    const bottom = button('К последнему', () => followLatest(), 'arrow-down', viewport); bottom.classList.add('tw-bottom'); bottom.hidden = true;
    const footer = el('div', 'tw-footer'), status = el('span', 'tw-status', 'Подключение…'), geometry = el('span', 'tw-geometry');
    status.setAttribute('role', 'status'); footer.append(status, geometry); root.append(head, findBar, viewport, footer);
    let term, search, fit, filter, sessionId = null, streamId = null, cursor = 0, generation = 0;
    let desired = false, connecting = false, interactive = false, connected = false, inputAvailable = false, retryTimer, retries = 0;
    let writeQueue = Promise.resolve(), inputQueue = Promise.resolve(), queuedInput = 0, pendingRaw = null, disposed = false, resizeTimer;
    let historyNote = '', lastError = '', pendingResize = false, inputEpoch = 0;
    let requestedHistoryLines = 2000, followScreen = true, wasAtBottom = true, expectedViewportScroll = null, followOnWheelEnd = false;
    const isCurrent = (gen, id) => !disposed && generation === gen && sessionId === id;
    const call = async (action, payload = {}, id = sessionId) => {
      const result = await bridge.terminalAction(id, action, payload);
      if (!result?.ok) {
        const messages = {
          stream_missing: 'Соединение с терминалом закрыто. Переподключись через меню «Ещё».',
          stream_limit: 'Открыто слишком много терминалов. Закрой одну из панелей.',
          input_unavailable: 'Панель изменилась, находится в режиме копирования или передаёт ввод нескольким панелям. Проверь её во внешнем терминале.',
          input_uncertain: 'Нет подтверждения доставки клавиш. Проверь вывод перед повторной отправкой.',
          not_ready: 'Терминал ещё не готов к вводу.',
        };
        const error = new Error(messages[result?.code] || result?.error || 'Не удалось выполнить действие терминала'); Object.assign(error, result); throw error;
      }
      return result;
    };
    const setStatus = (message, state = 'connected') => { status.textContent = message; root.dataset.state = state; };
    const copy = async text => {
      if (!text) return;
      try { const r = await bridge.copyText(text); if (r?.ok === false) throw new Error(r.error); toast('Скопировано'); }
      catch (e) { toast(e.message || String(e)); }
    };
    const copySelected = () => copy(term?.getSelection());
    const findButton = button('Найти', () => showFind(), 'magnifying-glass');
    const copyButton = button('Копировать', copySelected, 'copy'); copyButton.disabled = true;
    const more = el('details', 'tw-more'); actions.append(more);
    const moreToggle = el('summary', 'tw-button', 'Ещё'); moreToggle.setAttribute('aria-label', 'Другие действия терминала'); more.append(moreToggle);
    const menu = el('div', 'tw-menu'); more.append(menu);
    const menuAction = (label, fn) => button(label, () => { more.open = false; fn(); }, null, menu);
    menuAction('Копировать весь загруженный вывод', () => term && copy(bufferText(term.buffer.active)));
    menuAction('Выделить всё', () => { term?.selectAll(); term?.focus(); });
    menuAction('Сохранить вывод в Загрузки', async () => {
      const id = sessionId, sid = streamId;
      if (!sid) { toast('Сначала подключи терминал'); return; }
      try { const r = await call('export', { streamId: sid }, id); toast(`Сохранено: ${r.path}${r.truncated ? ' (доступная часть истории)' : ''}`); } catch (e) { toast(e.message); }
    });
    menuAction('Загрузить больше истории (до 100 000 строк)', () => { requestedHistoryLines = HISTORY_LINES; reconnect(); });
    menuAction('Переподключиться', () => reconnect());
    menuAction('Открыть во внешнем терминале', () => openExternal());
    menuAction('Увеличить шрифт', () => setFont(1)); menuAction('Уменьшить шрифт', () => setFont(-1));
    const expandButton = button('Развернуть', () => {
      root.classList.toggle('tw-expanded'); expandButton.setAttribute('aria-pressed', String(root.classList.contains('tw-expanded')));
      expandButton.querySelector('span').textContent = root.classList.contains('tw-expanded') ? 'Свернуть' : 'Развернуть';
      scheduleResize();
    }, 'arrows-out');
    const inputButton = button('Включить ввод', () => toggleInput(), 'keyboard'); inputButton.disabled = true; inputButton.setAttribute('aria-pressed', 'false');
    const previous = button('Предыдущее совпадение', () => find(true), null, findBar); previous.textContent = '↑';
    const next = button('Следующее совпадение', () => find(false), null, findBar); next.textContent = '↓';
    const caseButton = button('Учитывать регистр', () => { caseButton.setAttribute('aria-pressed', String(caseButton.getAttribute('aria-pressed') !== 'true')); find(false, true); }, null, findBar);
    caseButton.textContent = 'Aa'; caseButton.setAttribute('aria-pressed', 'false');
    const closeFind = button('Закрыть поиск', () => { findBar.hidden = true; search?.clearDecorations(); term?.focus(); scheduleResize(); }, null, findBar); closeFind.textContent = '×';
    query.addEventListener('input', () => find(false, true));
    query.addEventListener('keydown', event => {
      if (event.key === 'Enter') { event.preventDefault(); find(event.shiftKey); }
      if (event.key === 'Escape') { event.preventDefault(); closeFind.click(); }
    });
    root.addEventListener('keydown', event => event.stopPropagation());
    root.addEventListener('keyup', event => event.stopPropagation());
    root.addEventListener('paste', event => {
      if (!event.target.closest('.tw-screen')) return;
      event.preventDefault(); event.stopPropagation();
      const text = event.clipboardData?.getData('text/plain');
      if (text && interactive) sendInput(encoder.encode(text), true);
    }, true);
    root.addEventListener('copy', event => {
      if (!term?.hasSelection() || event.target.closest('.tw-find')) return;
      event.preventDefault(); event.stopPropagation(); event.clipboardData?.setData('text/plain', term.getSelection());
    });
    root.addEventListener('focusout', event => { if (!root.contains(event.relatedTarget)) more.open = false; });
    viewport.addEventListener('scroll', () => {
      // A cursor-follow scroll need not reach the physical end of a mostly
      // empty screen. Its asynchronous scroll event must not disable follow.
      if (expectedViewportScroll !== null && Math.abs(viewport.scrollTop - expectedViewportScroll) < 1) {
        expectedViewportScroll = null; return;
      }
      expectedViewportScroll = null;
      followScreen = !term?.hasSelection() && atCursorPosition();
    }, { passive: true });
    viewport.addEventListener('pointerdown', () => { expectedViewportScroll = null; followOnWheelEnd = false; }, { passive: true });

    function theme() {
      const styles = getComputedStyle(root), value = (name, fallback) => styles.getPropertyValue(name).trim() || fallback;
      return { background: value('--paper-2', '#202630'), foreground: value('--ink', '#f2f5fa'), cursor: value('--accent', '#70dfad'),
        selectionBackground: '#6688bb66', selectionInactiveBackground: '#6688bb44', black: '#455064', red: '#e56f82', green: '#51b98b', yellow: '#d7a850', blue: '#649ddb', magenta: '#b393df', cyan: '#62b7c2', white: '#d5deec',
        brightBlack: '#8998ac', brightRed: '#f29ca8', brightGreen: '#8cddba', brightYellow: '#edcc8e', brightBlue: '#a5c9f1', brightMagenta: '#d2bbed', brightCyan: '#a5dce3', brightWhite: '#ffffff' };
    }
    function init() {
      if (term) return;
      if (!window.Terminal || !window.SearchAddon || !window.FitAddon) throw new Error('Терминал не загрузился. Перезапусти Jarvis.');
      let fontSize = 13;
      try { fontSize = Math.max(10, Math.min(22, Number(localStorage.getItem('jarvis.terminal.fontSize')) || 13)); } catch {}
      term = new window.Terminal({ cols: 100, rows: 24, scrollback: HISTORY_LINES, fontSize, fontFamily: '"SFMono-Regular", Menlo, Consolas, monospace', lineHeight: 1.25,
        cursorBlink: false, disableStdin: true, screenReaderMode: true, allowProposedApi: true, theme: theme(), scrollOnUserInput: false, smoothScrollDuration: 80 });
      search = new window.SearchAddon.SearchAddon(); fit = new window.FitAddon.FitAddon(); term.loadAddon(search); term.loadAddon(fit); term.open(screen);
      term.textarea?.setAttribute('aria-label', 'Терминал — режим чтения');
      search.onDidChangeResults(result => { matches.textContent = result.resultCount ? `${result.resultIndex + 1} из ${result.resultCount}` : query.value ? 'Не найдено' : ''; });
      term.onSelectionChange(() => { copyButton.disabled = !term.hasSelection(); }); term.onScroll(updatePosition);
      term.onData(data => sendInput(encoder.encode(data)));
      term.onBinary(data => sendInput(Uint8Array.from(data, c => c.charCodeAt(0))));
      term.attachCustomKeyEventHandler(event => {
        if (event.type !== 'keydown') return true;
        const mod = event.metaKey || event.ctrlKey;
        if (mod && event.key.toLowerCase() === 'f') { event.preventDefault(); showFind(); return false; }
        if (mod && event.key.toLowerCase() === 'c' && term.hasSelection()) { event.preventDefault(); copySelected(); return false; }
        if (event.metaKey && event.key === 'a' || event.ctrlKey && event.shiftKey && event.key.toLowerCase() === 'a') { event.preventDefault(); term.selectAll(); return false; }
        if (event.metaKey && event.key === 'End') { event.preventDefault(); followLatest(); return false; }
        return true;
      });
      term.attachCustomWheelEventHandler(event => {
        if (interactive && !event.shiftKey) return true;
        if (event.ctrlKey || event.metaKey) return false;
        expectedViewportScroll = null; followOnWheelEnd = false;
        // A read-only client keeps the remote geometry. Reveal the clipped
        // screen first, then move through terminal history with the same wheel.
        const maxScroll = viewport.scrollHeight - viewport.clientHeight;
        if (term.buffer.active.viewportY === term.buffer.active.baseY &&
            (event.deltaY < 0 && viewport.scrollTop > 0 || event.deltaY > 0 && viewport.scrollTop < maxScroll - 1)) {
          viewport.scrollTop += event.deltaY;
          followScreen = !term.hasSelection() && atCursorPosition();
          return false;
        }
        // xterm animates scrollLines. Resume only when its later onScroll
        // actually reaches the live buffer, never from the old viewportY.
        followOnWheelEnd = event.deltaY > 0; followScreen = false;
        term.scrollLines(Math.sign(event.deltaY) * Math.max(1, Math.round(Math.abs(event.deltaY) / 30)));
        updatePosition(); return false;
      });
    }
    function screenGeometry() {
      if (!term) return null;
      const terminalScreen = screen.querySelector('.xterm-screen');
      if (!terminalScreen) return null;
      const frame = viewport.getBoundingClientRect(), rendered = terminalScreen.getBoundingClientRect();
      const scale = frame.height / viewport.offsetHeight;
      if (!Number.isFinite(scale) || scale <= 0) return null;
      return {
        screenTop: (rendered.top - frame.top) / scale + viewport.scrollTop - viewport.clientTop,
        screenHeight: rendered.height / scale, viewportHeight: viewport.clientHeight, scrollHeight: viewport.scrollHeight,
      };
    }
    function cursorScroll() {
      const size = screenGeometry();
      return size && cursorViewportScroll(term.buffer.active, term.rows, size);
    }
    function atCursorPosition() {
      const size = screenGeometry();
      if (!size) return false;
      const buffer = term.buffer.active, top = cursorViewportScroll(buffer, term.rows, size);
      if (top === null) return false;
      if (Math.abs(viewport.scrollTop - top) < 2) return true;
      // At the physical end, padding can place the fully visible cursor a
      // few pixels above its calculated follow position.
      const cursorTop = size.screenTop + buffer.cursorY * size.screenHeight / term.rows;
      return viewport.scrollTop >= size.scrollHeight - size.viewportHeight - 2 &&
        cursorTop >= viewport.scrollTop && cursorTop + size.screenHeight / term.rows <= viewport.scrollTop + size.viewportHeight;
    }
    function scrollViewport(top) {
      const before = viewport.scrollTop;
      viewport.scrollTop = top;
      if (viewport.scrollTop !== before) expectedViewportScroll = viewport.scrollTop;
    }
    function followLatest() {
      followOnWheelEnd = false; followScreen = true; term?.clearSelection(); term?.scrollToBottom(); updatePosition();
    }
    function updatePosition() {
      if (!term) return;
      const buffer = term.buffer.active;
      const atBottom = buffer.baseY === buffer.viewportY;
      if (atBottom && followOnWheelEnd) { followOnWheelEnd = false; followScreen = !term.hasSelection() && atCursorPosition(); }
      if (!term.hasSelection()) {
        if (atBottom && followScreen) {
          const top = cursorScroll(); if (top !== null) scrollViewport(top);
        } else if (!atBottom && wasAtBottom) { followScreen = false; scrollViewport(0); }
      }
      bottom.hidden = atBottom && followScreen;
      wasAtBottom = atBottom;
      const type = buffer.type === 'alternate' ? 'Полноэкранное приложение' : `${Math.max(0, buffer.length - term.rows).toLocaleString('ru')} строк истории`;
      geometry.textContent = `${term.cols} × ${term.rows} · ${type}`;
    }
    function showFind() { findBar.hidden = false; query.focus(); query.select(); scheduleResize(); }
    function find(backward, incremental = false) {
      if (!search || !query.value) { search?.clearDecorations(); matches.textContent = ''; return; }
      followScreen = false; followOnWheelEnd = false;
      const smoothScrollDuration = term.options.smoothScrollDuration;
      term.options.smoothScrollDuration = 0;
      const found = search[backward ? 'findPrevious' : 'findNext'](query.value, { incremental, caseSensitive: caseButton.getAttribute('aria-pressed') === 'true',
        decorations: { matchBackground: '#465164', matchOverviewRuler: '#8095b3', activeMatchBackground: '#786124', activeMatchColorOverviewRuler: '#efc66a' } });
      if (found) {
        // Search is an explicit navigation action. Its new selection must be
        // visible even when the remote screen is taller than the read viewport.
        const selectedRow = term.getSelectionPosition()?.start.y ?? term.buffer.active.viewportY;
        term.scrollToLine(selectedRow);
        const size = screenGeometry();
        followScreen = false;
        scrollViewport(size ? Math.max(0, size.screenTop + (selectedRow - term.buffer.active.viewportY) * size.screenHeight / term.rows - size.viewportHeight / 3) : 0);
      }
      term.options.smoothScrollDuration = smoothScrollDuration;
      if (!found) matches.textContent = 'Не найдено'; updatePosition();
    }
    function write(bytes, gen = generation) {
      const target = term;
      writeQueue = writeQueue.then(() => {
        if (gen !== generation || !target || target !== term) return;
        return new Promise(resolve => target.write(bytes, resolve));
      });
      return writeQueue;
    }
    function readingStatus() { return historyNote || (interactive ? 'Ввод включён · Shift + выделение копирует текст' : 'Режим чтения · выделяй и копируй текст'); }
    async function toggleInput() {
      if (!term || !connected || !inputAvailable) return;
      const epoch = ++inputEpoch, gen = generation, id = sessionId;
      interactive = !interactive; term.options.disableStdin = !interactive; term.options.cursorBlink = interactive;
      inputButton.setAttribute('aria-pressed', String(interactive)); inputButton.querySelector('span').textContent = interactive ? 'Выключить ввод' : 'Включить ввод';
      term.textarea?.setAttribute('aria-label', interactive ? 'Терминал — ввод агенту' : 'Терминал — режим чтения');
      await write(filter.sync(interactive));
      if (!isCurrent(gen, id) || epoch !== inputEpoch) return;
      setStatus(readingStatus()); if (interactive) { term.focus(); scheduleResize(); }
    }
    function disableInput() {
      inputEpoch++; interactive = false; inputButton.disabled = !connected || !inputAvailable; inputButton.setAttribute('aria-pressed', 'false'); inputButton.querySelector('span').textContent = 'Включить ввод';
      if (term) {
        term.options.disableStdin = true; term.options.cursorBlink = false;
        term.textarea?.setAttribute('aria-label', 'Терминал — режим чтения');
        if (filter) write(filter.sync(false));
      }
    }
    function sendInput(bytes, paste = false) {
      if (!interactive || !connected || !streamId || !bytes.length) return;
      if (queuedInput + bytes.length > 1024 * 1024) { disableInput(); toast('Слишком большая вставка. Ввод остановлен.'); return; }
      const id = sessionId, sid = streamId, gen = generation, epoch = inputEpoch;
      queuedInput += bytes.length;
      // While a request crosses SSH, collect subsequent raw keys in order.
      // One HTTP round trip per queued character otherwise builds typing lag.
      if (!paste && pendingRaw && pendingRaw.gen === gen && pendingRaw.epoch === epoch && pendingRaw.length + bytes.length <= 65536) {
        pendingRaw.parts.push(bytes); pendingRaw.length += bytes.length; return;
      }
      const job = { parts: [bytes], length: bytes.length, gen, epoch };
      pendingRaw = paste ? null : job;
      inputQueue = inputQueue.then(async () => {
        if (pendingRaw === job) pendingRaw = null;
        if (!isCurrent(gen, id) || epoch !== inputEpoch || !interactive || !connected) return;
        const data = job.parts.length === 1 ? bytes : new Uint8Array(job.length);
        if (job.parts.length > 1) { let offset = 0; for (const part of job.parts) { data.set(part, offset); offset += part.length; } }
        if (paste) {
          await call('input', { streamId: sid, data: Array.from(data), paste: true }, id);
          return;
        }
        for (let offset = 0; offset < data.length; offset += 8192) {
          if (!isCurrent(gen, id) || epoch !== inputEpoch || !interactive || !connected) return;
          await call('input', { streamId: sid, data: Array.from(data.subarray(offset, offset + 8192)) }, id);
        }
      }).catch(error => {
        if (!isCurrent(gen, id) || epoch !== inputEpoch) return;
        disableInput(); setStatus(`Ввод остановлен: ${error.message}. Проверь терминал перед повтором.`, 'error');
      }).finally(() => { queuedInput -= job.length; });
    }
    function setFont(delta) {
      if (!term) return;
      term.options.fontSize = Math.max(10, Math.min(22, term.options.fontSize + delta));
      try { localStorage.setItem('jarvis.terminal.fontSize', String(term.options.fontSize)); } catch {}
      scheduleResize();
    }
    function scheduleResize() {
      if (followScreen && !term?.hasSelection()) requestAnimationFrame(() => { if (followScreen && !term?.hasSelection()) updatePosition(); });
      clearTimeout(resizeTimer); resizeTimer = setTimeout(async () => {
        if (!desired || !term || !streamId || !interactive || pendingResize) return;
        const size = fit.proposeDimensions(); if (!size || size.cols < 20 || size.rows < 3) return;
        size.cols = Math.min(500, size.cols); size.rows = Math.min(300, size.rows);
        if (size.cols === term.cols && size.rows === term.rows) return;
        const gen = generation, id = sessionId; pendingResize = true;
        try {
          const r = await call('resize', { streamId, cols: size.cols, rows: size.rows }, id);
          if (isCurrent(gen, id)) {
            term.resize(r.cols || size.cols, r.rows || size.rows); updatePosition();
            if (r.readOnlyGeometry) setStatus('Размер задан раскладкой или другим подключением tmux.', 'warning');
          }
        } catch (e) { if (isCurrent(gen, id)) setStatus(e.message, 'warning'); }
        finally { pendingResize = false; }
      }, 180);
    }
    const resizeObserver = typeof ResizeObserver !== 'undefined' ? new ResizeObserver(scheduleResize) : null; resizeObserver?.observe(screen);
    const themeObserver = typeof MutationObserver !== 'undefined' ? new MutationObserver(() => { if (term) term.options.theme = theme(); }) : null;
    themeObserver?.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme', 'data-paint', 'style'] });
    async function closeStream() {
      const id = sessionId, sid = streamId; streamId = null; connected = false; inputAvailable = false; connecting = false;
      generation++; clearTimeout(retryTimer); disableInput();
      if (sid) { try { await call('close', { streamId: sid }, id); } catch {} }
    }
    async function connect() {
      if (!desired || connecting || streamId || disposed || !sessionId) return;
      const gen = generation, id = sessionId; connecting = true; filter = mouseFilter();
      setStatus(retries ? 'Восстанавливаем соединение…' : 'Подключение…', 'connecting');
      try {
        init();
        const r = await call('open', { historyLines: requestedHistoryLines }, id);
        if (!isCurrent(gen, id) || !desired) { call('close', { streamId: r.streamId }, id).catch(() => {}); return; }
        streamId = r.streamId; cursor = r.cursor || 0; connected = true; inputAvailable = r.connection?.canInput !== false; connecting = false; retries = 0; lastError = '';
        // Reset only on a fresh connection, never on output or state refresh.
        await writeQueue; if (!isCurrent(gen, id) || !desired) return; term.reset(); term.resize(r.cols || 100, r.rows || 24);
        historyNote = r.historyTruncated ? requestedHistoryLines < HISTORY_LINES
          ? 'Загружены последние строки · «Ещё» → загрузить больше истории'
          : 'Загружена доступная часть истории (до 100 000 строк и 4 МБ).' : '';
        await write(filter.push(r.initial || [], false), gen);
        if (!isCurrent(gen, id)) return;
        followScreen = true; wasAtBottom = true;
        inputButton.disabled = !inputAvailable; setStatus(readingStatus()); updatePosition(); scheduleResize(); poll(gen, id);
      } catch (e) {
        if (!isCurrent(gen, id)) return;
        connecting = false; fail(e, gen, id);
      }
    }
    function fail(error, gen, id) {
      if (!isCurrent(gen, id)) return;
      connected = false; disableInput(); lastError = error.message;
      setStatus(lastError, 'error');
      if (error.needsUpdate || error.unsupported || error.needsTmux || error.closed || error.code === 'stream_unavailable') return;
      const delay = Math.min(15000, 1000 * 2 ** Math.min(retries++, 4));
      retryTimer = setTimeout(async () => {
        if (!isCurrent(gen, id) || !desired) return;
        // Keep the last screen readable while offline. No input is replayed.
        await closeStream(); if (desired && sessionId === id) connect();
      }, delay);
    }
    async function poll(gen, id) {
      while (isCurrent(gen, id) && desired && streamId) {
        try {
          const r = await call('poll', { streamId, cursor }, id);
          if (!isCurrent(gen, id) || !desired) return;
          if (r.connection?.canInput === false && inputAvailable) {
            inputAvailable = false; disableInput(); setStatus('Сессия завершилась. Дочитываем оставшийся вывод.', 'closed');
          }
          if (r.gap) { const e = new Error('Часть потока пропущена. Восстанавливаем экран из истории…'); fail(e, gen, id); return; }
          for (const chunk of r.chunks || []) {
            if (chunk.seq <= cursor) continue;
            await write(filter.push(chunk.data || [], interactive), gen);
            if (!isCurrent(gen, id)) return;
            cursor = chunk.seq;
          }
          cursor = Math.max(cursor, r.cursor || 0);
          if (r.cols && r.rows && (r.cols !== term.cols || r.rows !== term.rows)) term.resize(r.cols, r.rows);
          updatePosition();
          if (r.closed) { connected = false; disableInput(); setStatus(r.error || 'Сессия завершена. Вывод доступен для копирования.', r.error ? 'error' : 'closed'); return; }
          if (r.error) throw new Error(r.error);
          // Empty responses are normally a one-second long poll. Throttle old
          // or faulty servers which return immediately rather than busy-loop.
          if (!(r.chunks || []).length) await new Promise(resolve => setTimeout(resolve, 60));
        } catch (e) { fail(e, gen, id); return; }
      }
    }
    async function reconnect() { await closeStream(); retries = 0; if (desired) connect(); }
    function setSession(id, visible, label = '') {
      const changed = id !== sessionId;
      desired = !!visible && !!id; root.hidden = !desired; location.textContent = label;
      if (changed) {
        closeStream(); sessionId = id; historyNote = ''; lastError = ''; retries = 0; requestedHistoryLines = 2000; followScreen = true; wasAtBottom = true; expectedViewportScroll = null; followOnWheelEnd = false;
        term?.dispose(); term = null; search = null; fit = null; writeQueue = Promise.resolve(); copyButton.disabled = true;
        root.classList.remove('tw-expanded'); expandButton.setAttribute('aria-pressed', 'false'); expandButton.querySelector('span').textContent = 'Развернуть'; bottom.hidden = true;
      }
      if (!desired) { if (streamId || connecting) closeStream(); }
      else connect();
    }
    const visibility = () => {
      if (!desired) return;
      if (document.hidden) closeStream(); else connect();
    };
    document.addEventListener('visibilitychange', visibility);
    return { root, setSession, reconnect, focus: () => term?.focus(),
      dispose() { desired = false; closeStream(); disposed = true; resizeObserver?.disconnect(); themeObserver?.disconnect(); term?.dispose(); clearTimeout(resizeTimer); document.removeEventListener('visibilitychange', visibility); },
    };
  }
  const api = { create, mouseFilter, bufferText, cursorViewportScroll, HISTORY_LINES };
  if (typeof window !== 'undefined') window.JarvisTerminal = api;
  if (typeof module !== 'undefined') module.exports = api;
})();
