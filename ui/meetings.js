/* Explicit recording controls. A meeting never writes into another app. */
(() => {
  'use strict';
  let host, bound = false, active = null, selected = null, pending = false, sequence = 0, detailSequence = 0, operationError = null;
  const labels = { recording: 'Идёт запись', transcribing: 'Распознаём речь', ready: 'Готово', error: 'Нужна помощь', interrupted: 'Запись прервана' };
  const el = (tag, cls, text) => { const n = document.createElement(tag); n.className = cls; if (text) n.textContent = text; return n; };
  function duration(ms) { const s = Math.max(0, Math.floor(ms / 1000)); return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`; }
  function notice(text, error = false) { if (!host) return; if (error) operationError = text; const n = host.querySelector('.meeting-status'); n.hidden = !text; n.textContent = text; n.classList.toggle('error', error); }
  function status() {
    if (!host) return;
    const button = host.querySelector('.meeting-primary');
    button.textContent = active?.status === 'recording' ? 'Остановить запись' : active?.status === 'transcribing' ? 'Распознаём…' : 'Начать запись';
    button.disabled = pending || active?.status === 'transcribing';
    host.querySelector('.meeting-input').disabled = pending || !!active;
    host.querySelector('.meeting-source-select').disabled = pending || !!active;
    if (operationError) { notice(operationError, true); return; }
    if (active) notice(`${labels[active.status]} · ${active.title} · ${duration(active.status === 'recording' ? Date.now() - active.startedAt : active.durationMs)}${active.status === 'transcribing' ? '. Аудио сохранено, можно перейти в другой раздел.' : ''}`);
  }
  async function run(action) {
    if (pending) return;
    pending = true; operationError = null; status();
    try { const item = await action(); if (item?.id) selected = item.id; await refresh(); }
    catch (e) { notice(String(e), true); }
    finally { pending = false; status(); }
  }
  async function detail(id) {
    selected = id;
    const request = ++detailSequence;
    const item = await window.jarvis.meetingsGet(id);
    if (!host || selected !== id || request !== detailSequence) return;
    const box = host.querySelector('.meeting-detail'); box.textContent = ''; box.hidden = false;
    box.appendChild(el('h2', '', item.title));
    box.appendChild(el('p', 'meeting-source', `${labels[item.status] || item.status} · ${duration(item.durationMs)} · ${item.source === 'microphone-system' ? 'Микрофон и звук приложений' : 'Микрофон'}`));
    const actions = el('div', 'meeting-actions');
    const copy = el('button', '', 'Копировать текст'); copy.disabled = !item.transcript;
    copy.addEventListener('click', async () => { try { await window.jarvis.copyText(item.transcript); copy.textContent = 'Скопировано'; } catch (e) { notice(`Не удалось скопировать: ${e}`, true); } });
    const retry = el('button', '', 'Распознать снова'); retry.disabled = !!active || pending || item.status === 'recording' || item.status === 'transcribing';
    retry.addEventListener('click', () => run(() => window.jarvis.meetingsRetranscribe(id)));
    actions.append(copy, retry); box.appendChild(actions);
    if (item.error) box.appendChild(el('p', 'meeting-status error', item.error));
    if (item.warning) box.appendChild(el('p', 'meeting-source', item.warning));
    box.appendChild(el('pre', '', item.transcript || (item.status === 'transcribing' ? 'Расшифровка появится после обработки аудио.' : 'В этой записи пока нет расшифровки. Аудио можно распознать повторно.')));
    box.appendChild(el('p', 'meeting-source', `Аудиофайл: ${item.audioPath}`));
  }
  async function refresh() {
    const request = ++sequence;
    const [items, current] = await Promise.all([window.jarvis.meetingsList(), window.jarvis.meetingsStatus()]);
    if (!host || request !== sequence) return;
    active = current;
    if (!active) notice('');
    status();
    const list = host.querySelector('.meeting-list'); list.textContent = '';
    if (!items.length) list.appendChild(el('div', 'meeting-empty', 'Сохрани разговор, чтобы вернуться к деталям. Дай встрече название и нажми «Начать запись». После остановки здесь появится расшифровка.'));
    for (const item of items) {
      const b = el('button', 'meeting-row');
      b.append(el('strong', '', item.title), el('small', '', labels[item.status] || item.status), el('small', '', new Date(item.startedAt).toLocaleDateString('ru-RU')), el('small', '', duration(item.durationMs)));
      b.addEventListener('click', () => detail(item.id).catch(e => notice(String(e), true))); list.appendChild(b);
    }
    if (selected && items.some(x => x.id === selected)) await detail(selected);
  }
  function init(root) {
    host = root;
    // Preserve focus and the unfinished title when switching sections.
    if (!root.childElementCount) {
      const header = el('div', 'meeting-header'); const copy = el('div', '');
      copy.append(el('h1', '', 'Встречи'), el('p', '', 'Разговор заканчивается. Детали остаются.'));
      const start = el('button', 'meeting-primary', 'Начать запись'); header.append(copy, start);
      const name = el('input', 'meeting-input'); name.placeholder = 'Название встречи'; name.maxLength = 160; name.setAttribute('aria-label', 'Название встречи');
      const source = el('select', 'meeting-input meeting-source-select'); source.setAttribute('aria-label', 'Источник звука');
      const mic = el('option', '', 'Микрофон · встреча рядом'); mic.value = 'microphone'; source.appendChild(mic);
      const sourceNote = el('p', 'meeting-source source-description', 'Записывается выбранный микрофон. Аудио хранится локально; речь распознаёт выбранный движок Jarvis.');
      window.jarvis.meetingsSources().then(options => {
        for (const option of options) {
          if (option.id === 'microphone') continue;
          const opt = el('option', '', option.label + (!option.available ? ' · недоступно' : ''));
          opt.value = option.id; opt.disabled = !option.available; opt.title = option.reason || ''; source.appendChild(opt);
          if (!option.available && option.reason) sourceNote.textContent += ' ' + option.reason;
        }
      }).catch(e => { sourceNote.textContent += ' Не удалось проверить системный звук: ' + String(e); });
      source.addEventListener('change', () => {
        sourceNote.textContent = source.value === 'microphone-system'
          ? 'Для онлайн-встреч: микрофон и звук приложений. При первом запуске macOS запросит доступ к системному звуку. Запись начнётся после нажатия кнопки.'
          : 'Для встречи рядом: выбранный микрофон. Аудио хранится локально; речь распознаёт выбранный движок Jarvis.';
      });
      const message = el('div', 'meeting-status'); message.hidden = true; message.setAttribute('role', 'status'); message.setAttribute('aria-live', 'polite');
      const list = el('div', 'meeting-list');
      const transcript = el('article', 'meeting-detail'); transcript.hidden = true;
      root.append(header, name, source, sourceNote, message, list, transcript);
      start.addEventListener('click', () => run(() => active?.status === 'recording' ? window.jarvis.meetingsStop() : window.jarvis.meetingsStart(name.value.trim() || null, source.value)));
    }
    if (!bound) {
      bound = true;
      window.jarvis.onMeetingsChanged(() => { if (host && !host.hidden) refresh().catch(e => notice(String(e), true)); });
      setInterval(() => { if (host && !host.hidden && active?.status === 'recording') status(); }, 1000);
    }
    return refresh().catch(e => notice(`Не удалось загрузить встречи: ${e}`, true));
  }
  window.initMeetings = init;
})();
