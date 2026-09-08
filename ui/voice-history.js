/* Voice workspace. Every saved state comes from native storage; failed reads
 * remain distinct from empty lists. No local-success or built-in UI fallback. */
(function () {
  'use strict';
  let state = null, menu = null, menuAnchor = null, bound = false;
  const TITLES = { history: 'История диктовки', insights: 'Статистика', dict: 'Словарь', transforms: 'Преобразования', scratch: 'Черновик' };
  const NAV = [['history', 'history', 'История'], ['insights', 'insights', 'Статистика'], ['dict', 'dict', 'Словарь'], ['transforms', 'sparkle', 'Преобразования'], ['scratch', 'scratch', 'Черновик']];
  const TRANSFORMS = [['prompt', 'Промпт для агента'], ['commit', 'Коммит-сообщение'], ['clean', 'Чистовик'], ['translate', 'Перевод на English']];
  function el(tag, cls, text) { const node = document.createElement(tag); if (cls) node.className = cls; if (text != null) node.textContent = text; return node; }
  function button(text, cls, action) { const node = el('button', cls, text); node.type = 'button'; node.addEventListener('click', action); return node; }
  function icon(name) { return window.jarvisIcons?.create(name) || el('span', '', '·'); }
  function errorText(error) { return String(error?.message || error || 'Не получен ответ от Jarvis').replace(/([a-z]+:\/\/)[^\s/@]+@/gi, '$1•••@'); }
  function toast(text) { if (typeof window.showToast === 'function') window.showToast(text); else { state?.message?.replaceChildren(el('span', '', text)); } }
  function available(name) { return typeof window.jarvis?.[name] === 'function'; }
  async function call(name, ...args) {
    if (!available(name)) throw new Error('Эта функция недоступна. Открой раздел в актуальной версии Jarvis.');
    let timer;
    try {
      const result = await Promise.race([window.jarvis[name](...args), new Promise((_, reject) => { timer = setTimeout(() => reject(new Error('Jarvis не ответил. Проверь состояние и повтори действие.')), /Enhance|Retranscribe/.test(name) ? 90000 : 15000); })]);
      if (result?.ok === false || result?.error) throw new Error(result.error || 'Изменение не сохранено');
      return result;
    } finally { clearTimeout(timer); }
  }
  function requireList(value, key) { if (!value || !Array.isArray(value[key])) throw new Error('Jarvis вернул неполные данные. Повтори загрузку.'); return value[key]; }
  function requireSaved(value) { if (value?.ok !== true) throw new Error('Jarvis не подтвердил сохранение. Проверь состояние перед повтором.'); return value; }
  function errorBlock(message, retry) { const box = el('div', 'vh-error'); box.setAttribute('role', 'alert'); box.appendChild(el('span', '', errorText(message))); if (retry) box.appendChild(button('Повторить', 'btn', retry)); return box; }
  function note(text) { return el('div', 'vh-note', text); }
  function words(text) { return String(text || '').trim().split(/\s+/).filter(Boolean).length; }
  function dayKey(ts) { const d = new Date(ts * 1000); return `${d.getFullYear()}-${d.getMonth() + 1}-${d.getDate()}`; }
  function time(ts) { return new Date(ts * 1000).toLocaleTimeString('ru', { hour: '2-digit', minute: '2-digit' }); }
  function dayLabel(ts) { const date = new Date(ts * 1000); return dayKey(ts) === dayKey(Date.now() / 1000) ? 'Сегодня' : date.toLocaleDateString('ru', { day: 'numeric', month: 'long' }); }
  function closeMenu(restore = false) { menu?.remove(); menu = null; if (restore) menuAnchor?.focus(); menuAnchor = null; }
  function openMenu(anchor, container, actions) {
    closeMenu(); menuAnchor = anchor; menu = el('div', 'vh-tmenu'); menu.setAttribute('role', 'menu');
    for (const [label, action] of actions) { const entry = button(label, 'vh-ti', event => { event.stopPropagation(); closeMenu(); action(); }); entry.setAttribute('role', 'menuitem'); menu.appendChild(entry); }
    container.appendChild(menu); menu.querySelector('button')?.focus();
  }
  async function copy(text) { try { if (available('copyText')) await call('copyText', text); else await navigator.clipboard.writeText(text); toast('Скопировано'); } catch (error) { toast('Не удалось скопировать: ' + errorText(error)); } }

  async function loadResource(key) {
    const owner = state, version = ++owner.reads[key]; owner.loading[key] = true; renderResource(key);
    try {
      if (key === 'history') {
        const result = requireList(await call('transcriptsGet'), 'items');
        if (owner !== state || version !== owner.reads[key]) return;
        owner.items = result.filter(item => item?.source !== 'wake' && typeof item?.text === 'string').map(item => ({ ...item, hasAudio: item.hasAudio === true })).sort((a, b) => Number(b.ts) - Number(a.ts) || Number(b.id) - Number(a.id));
      } else if (key === 'dict') {
        const result = requireList(await call('dictionaryGet'), 'words');
        if (owner !== state || version !== owner.reads[key]) return;
        owner.dict = result;
      } else if (key === 'transforms') {
        const [prompts, settings] = await Promise.all([call('promptsGet'), call('promptsGetSettings')]);
        if (owner !== state || version !== owner.reads[key]) return;
        owner.prompts = requireList(prompts, 'prompts');
        if (typeof settings?.smart !== 'boolean') throw new Error('Не получено состояние умного режима');
        owner.smart = settings.smart;
      } else {
        const result = await call('scratchpadGet');
        if (typeof result?.text !== 'string') throw new Error('Не получен сохранённый черновик');
        if (owner !== state || version !== owner.reads[key]) return;
        owner.scratch = owner.scratchSaved = result.text; owner.scratchLoaded = true;
      }
      owner.errors[key] = '';
    } catch (error) { if (owner === state && version === owner.reads[key]) owner.errors[key] = errorText(error); }
    finally { if (owner === state && version === owner.reads[key]) { owner.loading[key] = false; renderResource(key); } }
  }
  function renderResource(key) {
    if (!state?.panes) return;
    if (key === 'history') { renderHistory(); renderInsights(); }
    else if (key === 'dict') renderDictionary();
    else if (key === 'transforms') renderTransforms();
    else renderScratch();
  }
  function contentStatus(key, container) {
    if (state.errors[key]) container.appendChild(errorBlock(state.errors[key], () => loadResource(key)));
    if (state.loading[key]) container.appendChild(el('p', 'vh-loading', 'Загружаем…'));
  }
  function renderHistory() {
    if (!state.feed) return;
    const feed = state.feed; feed.replaceChildren(); contentStatus('history', feed);
    if (state.errors.history || state.loading.history) return;
    const items = state.items.filter(item => item.text.toLowerCase().includes(state.query.toLowerCase()));
    if (!items.length) { feed.appendChild(el('div', 'vh-empty', state.query ? 'Ничего не найдено. Измени запрос.' : 'Здесь появятся сохранённые диктовки. Горячая клавиша и сохранение истории настраиваются в разделе «Диктовка».')); return; }
    let lastDay = '';
    for (const item of items) {
      const day = dayKey(item.ts); if (day !== lastDay) { feed.appendChild(el('div', 'dayhead', dayLabel(item.ts))); lastDay = day; }
      feed.appendChild(buildEntry(item));
    }
  }
  function buildEntry(item) {
    const entry = el('article', 'ent'); entry.dataset.id = String(item.id);
    const left = el('div', 'lc'); left.appendChild(el('span', 'tm', time(item.ts))); entry.appendChild(left);
    const body = el('div', 'vh-body');
    if (typeof item.rawText === 'string') body.appendChild(el('div', 'vh-origin-label', item.rawText !== item.text ? 'Отформатировано' : 'Распознано'));
    body.appendChild(el('div', 'tx vh-text', item.text)); entry.appendChild(body);
    if (typeof item.rawText === 'string' && item.rawText !== item.text) {
      const raw = el('details', 'vh-original'); raw.appendChild(el('summary', '', 'Исходное распознавание'));
      raw.appendChild(el('div', 'vh-original-text', item.rawText));
      raw.appendChild(button('Копировать исходный текст', '', () => copy(item.rawText))); body.appendChild(raw);
    }
    const actions = el('div', 'vh-acts');
    actions.appendChild(button('Копировать', '', () => copy(item.text)));
    if (available('transcriptUpdate')) actions.appendChild(button('Изменить', '', () => { state.editing = { id: item.id, draft: item.text }; renderHistory(); state.feed.querySelector('.vh-editor textarea')?.focus(); }));
    const transform = button('Преобразовать', 'vh-primary', event => { event.stopPropagation(); if (menuAnchor === transform) closeMenu(); else openMenu(transform, body, TRANSFORMS.map(([style, label]) => [label, () => enhance(item, body, style)])); });
    transform.disabled = !available('transcriptEnhance'); actions.appendChild(transform);
    if (item.hasAudio && available('transcriptRetranscribe')) actions.appendChild(button('Распознать снова', '', async event => {
      const control = event.currentTarget, owner = state; if (control.disabled) return; control.disabled = true;
      try { const result = requireSaved(await call('transcriptRetranscribe', item.id)); if (typeof result.text !== 'string') throw new Error('Не получен новый текст'); if (owner === state) { item.text = result.text; item.appliedStyle = null; renderHistory(); renderInsights(); } }
      catch (error) { body.appendChild(errorBlock(error)); } finally { control.disabled = false; }
    }));
    if (available('transcriptDelete')) {
      const remove = button('Удалить', 'vh-danger', async () => {
        if (remove.disabled) return; remove.disabled = true; const owner = state;
        try { requireSaved(await call('transcriptDelete', item.id)); if (state === owner) { owner.items = owner.items.filter(entry => entry.id !== item.id); renderHistory(); renderInsights(); } }
        catch (error) { body.appendChild(errorBlock(error)); } finally { remove.disabled = false; }
      }); actions.appendChild(remove);
    }
    body.appendChild(actions);
    if (state.editing?.id === item.id) body.appendChild(buildEditor(item));
    return entry;
  }
  function buildEditor(item, replacement) {
    const form = el('div', 'vh-editor'); form.setAttribute('role', 'group'); form.setAttribute('aria-label', 'Редактирование диктовки');
    const textarea = el('textarea'); textarea.value = replacement ?? state.editing?.draft ?? item.text; textarea.setAttribute('aria-label', 'Текст диктовки');
    textarea.addEventListener('input', () => { state.editing = { id: item.id, draft: textarea.value }; });
    const controls = el('div', 'vh-editor-actions'), error = el('div');
    const cancel = button('Отменить', 'btn', () => { state.editing = null; renderHistory(); });
    const save = button('Сохранить', 'btn', async () => {
      const text = textarea.value.trim(); if (save.disabled) return;
      if (!text) { error.replaceChildren(errorBlock('Текст не может быть пустым')); return; }
      const owner = state; save.disabled = cancel.disabled = textarea.disabled = true; error.replaceChildren();
      try { const result = requireSaved(await call('transcriptUpdate', item.id, text)); if (typeof result.text !== 'string') throw new Error('Jarvis не вернул сохранённый текст'); if (state === owner) { item.text = result.text; item.appliedStyle = null; owner.editing = null; renderHistory(); renderInsights(); } }
      catch (failure) { error.replaceChildren(errorBlock(failure)); }
      finally { save.disabled = cancel.disabled = textarea.disabled = false; }
    });
    controls.append(cancel, save); form.append(textarea, controls, error); return form;
  }
  async function enhance(item, body, style) {
    body.querySelector('.vh-enh')?.remove(); const owner = state;
    const resultBox = el('div', 'vh-enh'); const text = el('div', 'vh-etext', 'Преобразуем через Claude…'); resultBox.appendChild(text); body.appendChild(resultBox);
    try {
      const result = requireSaved(await call('transcriptEnhance', item.text, style));
      if (typeof result.result !== 'string' || !result.result.trim()) throw new Error('Преобразование вернуло пустой результат');
      if (owner !== state || !resultBox.isConnected) return;
      text.textContent = result.result; const controls = el('div', 'vh-eh');
      controls.appendChild(el('span', 'vh-chip', 'Предпросмотр · не сохранён'));
      controls.appendChild(button('Копировать', '', () => copy(result.result)));
      if (available('transcriptUpdate')) controls.appendChild(button('Изменить и сохранить', '', () => { state.editing = { id: item.id, draft: result.result }; renderHistory(); state.feed.querySelector('.vh-editor textarea')?.focus(); }));
      controls.appendChild(button('Скрыть', '', () => resultBox.remove())); resultBox.prepend(controls);
    } catch (error) { resultBox.replaceChildren(errorBlock(error, () => enhance(item, body, style))); }
  }
  function renderInsights() {
    const pane = state.panes.insights; pane.replaceChildren(); contentStatus('history', pane); if (state.errors.history || state.loading.history) return;
    const total = state.items.reduce((sum, item) => sum + words(item.text), 0), today = state.items.filter(item => dayKey(item.ts) === dayKey(Date.now() / 1000)).length;
    const grid = el('div', 'grid');
    for (const [value, label] of [[total, 'слов в истории'], [state.items.length, 'сохранённых диктовок'], [today, 'диктовок сегодня']]) { const card = el('div', 'bigcard'); card.append(el('div', 'bn', value.toLocaleString('ru')), el('div', 'bl', label)); grid.appendChild(card); }
    pane.appendChild(grid); const section = el('div', 'sect'); section.appendChild(note('Статистика рассчитана по сохранённой истории. Удалённые диктовки не учитываются. Источник приложения и время, сэкономленное на вводе, не измеряются.'));
    section.appendChild(el('h3', 'secth', 'Последние 16 недель')); const heat = el('div', 'heat');
    const counts = new Map(); for (const item of state.items) counts.set(dayKey(item.ts), (counts.get(dayKey(item.ts)) || 0) + 1);
    for (let ago = 111; ago >= 0; ago--) { const date = new Date(); date.setDate(date.getDate() - ago); const count = counts.get(dayKey(date.getTime() / 1000)) || 0; const cell = el('i', count ? count > 4 ? 'l3' : count > 1 ? 'l2' : 'l1' : ''); cell.title = `${date.toLocaleDateString('ru')} · ${count}`; cell.setAttribute('aria-label', cell.title); heat.appendChild(cell); }
    section.appendChild(heat); if (!state.items.length) section.appendChild(el('p', 'vh-empty', 'Пока нет сохранённых диктовок.')); pane.appendChild(section);
  }
  function buildDictionary() {
    const section = el('div', 'sect'); section.appendChild(note('Задай точную замену после распознавания. Например, «джарвис» → «Jarvis». Совпадают целые слова и фразы без учёта регистра. Замены действуют в диктовке и расшифровках встреч.'));
    const form = el('form', 'addrow');
    state.word = el('input'); state.word.placeholder = 'Как распознаётся'; state.word.setAttribute('aria-label', 'Распознанное слово или фраза'); state.word.maxLength = 128;
    state.replacement = el('input'); state.replacement.placeholder = 'Как писать'; state.replacement.setAttribute('aria-label', 'Правильная запись'); state.replacement.maxLength = 512;
    state.dictSave = button('Добавить', 'btn', () => {}); state.dictSave.type = 'submit';
    state.dictCancel = button('Отменить', 'btn', () => resetDictionaryForm()); state.dictCancel.hidden = true;
    form.append(state.word, state.replacement, state.dictSave, state.dictCancel);
    form.addEventListener('submit', event => { event.preventDefault(); mutateDictionary('add'); });
    section.appendChild(form); state.dictList = el('div', 'vh-dictlist'); section.appendChild(state.dictList); return section;
  }
  function resetDictionaryForm() { state.dictEditing = false; state.word.value = state.replacement.value = ''; state.word.readOnly = false; state.dictSave.textContent = 'Добавить'; state.dictCancel.hidden = true; }
  async function mutateDictionary(kind, word) {
    if (state.dictBusy || state.loading.dict || state.errors.dict && !state.dict) return;
    const owner = state; owner.dictBusy = true; owner.dictMutationError = ''; renderDictionary();
    try {
      const result = kind === 'add' ? await call('dictionaryAdd', owner.word.value.trim(), owner.replacement.value.trim()) : await call('dictionaryRemove', word);
      const entries = requireList(result, 'words'); if (state !== owner) return;
      owner.dict = entries; owner.errors.dict = ''; if (kind === 'add') resetDictionaryForm();
    } catch (error) { owner.dictMutationError = errorText(error); }
    finally { if (state === owner) { owner.dictBusy = false; renderDictionary(); } }
  }
  function renderDictionary() {
    if (!state.dictList) return; const list = state.dictList; list.replaceChildren(); contentStatus('dict', list);
    const disabled = state.dictBusy || state.loading.dict || Boolean(state.errors.dict);
    for (const node of [state.word, state.replacement, state.dictSave, state.dictCancel]) node.disabled = disabled;
    if (state.dictMutationError) list.appendChild(errorBlock(state.dictMutationError));
    if (state.loading.dict || state.errors.dict) return;
    if (!state.dict?.length) list.appendChild(el('div', 'vh-empty', 'Пока нет замен. Добавь слово или фразу выше.'));
    for (const entry of state.dict || []) {
      const row = el('div', 'lrow'); row.append(el('span', 'key', entry.word), el('span', '', '→'), el('span', 'val', entry.replacement));
      const edit = button('Изменить', 'btn', () => { state.dictEditing = true; state.word.value = entry.word; state.word.readOnly = true; state.replacement.value = entry.replacement; state.dictSave.textContent = 'Сохранить'; state.dictCancel.hidden = false; state.replacement.focus(); }); edit.disabled = state.dictBusy;
      const remove = button('Удалить', 'x', () => mutateDictionary('remove', entry.word)); remove.setAttribute('aria-label', 'Удалить замену ' + entry.word); remove.disabled = state.dictBusy;
      row.append(edit, remove); list.appendChild(row);
    }
  }
  async function setSmart(on) {
    if (state.smartBusy || state.loading.transforms || state.errors.transforms) return; const owner = state; owner.smartBusy = true; owner.smartError = ''; renderTransforms();
    try { requireSaved(await call('promptsSetSmart', on)); if (state === owner) owner.smart = on; }
    catch (error) { owner.smartError = errorText(error); }
    finally { if (state === owner) { owner.smartBusy = false; renderTransforms(); } }
  }
  function renderTransforms() {
    const pane = state.panes.transforms; pane.replaceChildren(); contentStatus('transforms', pane);
    if (state.errors.transforms || state.loading.transforms) return;
    const section = el('div', 'sect'); section.appendChild(note('Преобразования отправляют текст выбранному AI-сервису из настроек Jarvis. Нужна авторизация этого сервиса. Исходное распознавание хранится отдельно; промпт, коммит и перевод применяются вручную.'));
    const smart = el('div', 'tr'); const copy = el('div', 'vh-trmid'); copy.append(el('div', 'tn', 'AI-форматирование'), el('div', 'tdesc', 'Оформляет пунктуацию, абзацы и явные списки. Если модель недоступна, остаётся распознанный текст. Результат можно сверить с исходником в истории.'));
    const toggle = button('', state.smart ? 'tg' : 'tg off', () => setSmart(!state.smart)); toggle.setAttribute('role', 'switch'); toggle.setAttribute('aria-label', 'AI-форматирование'); toggle.setAttribute('aria-checked', String(state.smart)); toggle.disabled = state.smartBusy;
    smart.append(copy, toggle); section.appendChild(smart); if (state.smartError) section.appendChild(errorBlock(state.smartError, () => setSmart(!state.smart)));
    for (const prompt of state.prompts || []) { const row = el('div', 'tr'); const pic = el('span', 'ti'); pic.appendChild(icon('sparkle')); const body = el('div', 'vh-trmid'); body.append(el('div', 'tn', prompt.name), el('div', 'tdesc', prompt.desc), el('span', 'trig manual', prompt.auto ? 'AI-форматирование или вручную из истории' : 'Вручную из истории')); row.append(pic, body); section.appendChild(row); }
    pane.appendChild(section);
  }
  function buildScratch() {
    const wrap = el('div', 'scratch'); const intro = note('Личный черновик хранится локально в Jarvis. Текст сохраняется автоматически; состояние записи показано под полем.');
    state.scratchInput = el('textarea'); state.scratchInput.placeholder = 'Мысли, заметки, длинный промпт…'; state.scratchInput.setAttribute('aria-label', 'Черновик');
    state.scratchInput.addEventListener('input', () => { state.scratch = state.scratchInput.value; state.scratchSaveError = ''; updateScratchStatus(); saveScratch(); });
    state.scratchStatus = el('div', 'vh-save-state'); state.scratchStatus.setAttribute('role', 'status'); state.scratchErrorBox = el('div');
    wrap.append(intro, state.scratchInput, state.scratchStatus, state.scratchErrorBox); return wrap;
  }
  function renderScratch() {
    if (!state.scratchInput) return;
    state.scratchInput.disabled = !state.scratchLoaded || state.loading.scratch;
    if (state.scratchInput.value !== state.scratch) state.scratchInput.value = state.scratch;
    updateScratchStatus();
  }
  function updateScratchStatus() {
    const failure = state.errors.scratch || state.scratchSaveError; state.scratchErrorBox.replaceChildren();
    if (failure) state.scratchErrorBox.appendChild(errorBlock(failure, () => state.scratchLoaded ? saveScratch() : loadResource('scratch')));
    state.scratchStatus.textContent = state.loading.scratch ? 'Загружаем сохранённый черновик…' : !state.scratchLoaded ? 'Черновик не загружен' : state.scratchSaveError ? 'Не сохранено · текст остаётся в этом окне' : state.scratchSaving ? 'Сохраняем…' : state.scratch !== state.scratchSaved ? 'Есть несохранённые изменения' : 'Сохранено на этом устройстве';
  }
  async function saveScratch() {
    const owner = state; if (!owner.scratchLoaded || owner.scratchSaving) return; owner.scratchSaveError = ''; owner.scratchSaving = true; updateScratchStatus();
    try {
      while (owner.scratch !== owner.scratchSaved) {
        const text = owner.scratch; const result = requireSaved(await call('scratchpadSet', text));
        if (typeof result.text !== 'string' || result.text !== text) throw new Error('Jarvis не подтвердил сохранение этого текста');
        owner.scratchSaved = result.text;
      }
    } catch (error) { owner.scratchSaveError = errorText(error); }
    finally { owner.scratchSaving = false; if (owner === state) updateScratchStatus(); }
  }
  function switchSection(key) {
    if (!state || !TITLES[key]) return; closeMenu(); state.section = key; state.title.textContent = TITLES[key];
    state.nav.querySelectorAll('button').forEach(node => { const active = node.dataset.k === key; node.classList.toggle('on', active); node.setAttribute('aria-current', active ? 'page' : 'false'); });
    for (const [name, pane] of Object.entries(state.panes)) { pane.classList.toggle('on', name === key); pane.hidden = name !== key; }
    if (key === 'insights') renderInsights();
  }
  function onKey(event) {
    if (!state?.shell.isConnected || state.root.hidden || state.root.closest('[hidden]') || event.isComposing) return;
    if (menu && ['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) {
      const items = [...menu.querySelectorAll('button')], index = items.indexOf(document.activeElement); event.preventDefault(); event.stopImmediatePropagation(); const next = event.key === 'Home' ? 0 : event.key === 'End' ? items.length - 1 : (index + (event.key === 'ArrowDown' ? 1 : -1) + items.length) % items.length; items[next]?.focus(); return;
    }
    if (event.key !== 'Escape') return;
    let handled = true;
    if (menu) closeMenu(true);
    else if (state.editing && state.section === 'history' && !state.feed.querySelector('.vh-editor textarea')?.disabled) { state.editing = null; renderHistory(); }
    else if (state.section === 'history' && state.query) { state.query = ''; state.search.value = ''; renderHistory(); state.search.focus(); }
    else if (state.section === 'dict' && (state.word.value || state.replacement.value) && !state.dictBusy) { resetDictionaryForm(); state.word.focus(); }
    else if (state.section === 'scratch' && event.target === state.scratchInput) { state.scratchInput.blur(); saveScratch(); }
    else if (state.section !== 'history') { switchSection('history'); state.nav.querySelector('[data-k="history"]')?.focus(); }
    else handled = false;
    if (handled) { event.preventDefault(); event.stopImmediatePropagation(); }
  }
  function buildShell(root) {
    const shell = el('div', 'vh-shell'); state.shell = shell;
    const sidebar = el('aside', 'side'); const brand = el('div', 'brand'); brand.appendChild(el('span', 'nm', 'Голос')); sidebar.appendChild(brand);
    state.nav = el('nav', 'nav'); state.nav.setAttribute('aria-label', 'Разделы голоса');
    for (const [key, symbol, title] of NAV) { const nav = button('', 'it', () => switchSection(key)); nav.dataset.k = key; nav.append(icon(symbol), document.createTextNode(title)); state.nav.appendChild(nav); }
    sidebar.appendChild(state.nav); shell.appendChild(sidebar);
    const main = el('main', 'main'), header = el('div', 'mhead'); state.title = el('h2', 'h', TITLES.history); state.message = el('div', 'vh-inline-status'); state.message.setAttribute('role', 'status'); header.append(state.title, state.message); main.appendChild(header);
    state.panes = {}; for (const [key] of NAV) { const pane = el('section', 'pane'); pane.dataset.k = key; pane.setAttribute('aria-label', TITLES[key]); state.panes[key] = pane; main.appendChild(pane); }
    const history = el('div', 'feed'); state.search = el('input'); state.search.placeholder = 'Поиск по диктовкам'; state.search.setAttribute('aria-label', 'Поиск по диктовкам'); state.search.addEventListener('input', () => { state.query = state.search.value; renderHistory(); }); const search = el('div', 'vh-searchwrap'); search.append(icon('search'), state.search); history.appendChild(search); state.feed = el('div', 'vh-feedbody'); history.appendChild(state.feed); state.panes.history.appendChild(history);
    state.panes.dict.appendChild(buildDictionary()); state.panes.scratch.appendChild(buildScratch()); shell.appendChild(main); root.replaceChildren(shell); switchSection('history');
  }
  async function initVoiceHistory(root) {
    if (!root) return;
    if (state?.root === root && state.shell.isConnected) { if (!state.editing) await loadResource('history'); return; }
    if (!document.getElementById('voice-history-style')) { const style = el('style'); style.id = 'voice-history-style'; style.textContent = CSS; document.head.appendChild(style); }
    closeMenu();
    state = { root, section: 'history', query: '', items: [], dict: [], prompts: [], smart: false, dictBusy: false, smartBusy: false, dictMutationError: '', smartError: '', scratch: '', scratchSaved: '', scratchLoaded: false, scratchSaving: false, scratchSaveError: '', editing: null, loading: {}, errors: {}, reads: { history: 0, dict: 0, transforms: 0, scratch: 0 } };
    buildShell(root); if (!bound) { document.addEventListener('click', event => { if (!menu?.contains(event.target) && event.target !== menuAnchor) closeMenu(); }); document.addEventListener('keydown', onKey, true); bound = true; }
    await Promise.all(['history', 'dict', 'transforms', 'scratch'].map(loadResource));
  }
  window.initVoiceHistory = initVoiceHistory;

  const CSS = `
#voicehist {
  /* краска и поверхности — из theme.css; здесь только то, что нужно локально */
  --sidebar: var(--paper-2);
  --card: var(--surface);
  position: relative;
  width: 100%; height: 100%;
  display: flex; min-width: 0;
  overflow: hidden;
  font-family: var(--font);
  font-size: 13px;
  color: var(--ink);
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
  background: var(--accent);
  display: grid; place-items: center; color: var(--on-accent); font: 700 12px/1 var(--font);
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
#voicehist .nav .it.on { background: var(--fill-2); color: var(--text); }
#voicehist .nav .it .ph { --icon-size: 17px; width: 17px; height: 17px; flex: none; opacity: .8; }
#voicehist .nav .it.on .ph { color: var(--accent); opacity: 1; }
#voicehist .side .sp { flex: 1; }

#voicehist .smartcard {
  display: flex; align-items: center; gap: 10px; background: var(--accent-soft);
  border: 1px solid var(--accent-line); border-radius: 11px; padding: 10px 11px; margin-bottom: 10px;
}
#voicehist .smartcard .ic {
  width: 28px; height: 28px; border-radius: 8px; background: var(--accent-soft);
  display: grid; place-items: center; color: var(--accent); flex: none;
}
#voicehist .smartcard .ic .ph { --icon-size: 16px; width: 16px; height: 16px; }
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
#voicehist .foot .it .ph { --icon-size: 16px; width: 16px; height: 16px; opacity: .8; }

/* тумблеры (сайдкарта + ряды + умный режим) */
#voicehist .tg {
  width: 32px; height: 19px; border-radius: 10px; background: var(--accent);
  position: relative; flex: none;
}
#voicehist .tg::after {
  content: ""; position: absolute; top: 2px; right: 2px;
  width: 15px; height: 15px; border-radius: 50%; background: #fff; transition: .15s;
}
#voicehist .tg.off { background: var(--fill-3); }
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
#voicehist .autotag .ph { --icon-size: 10px; width: 10px; height: 10px; }

/* ховер-действия — отдельной строкой ПОД текстом (в потоке, не оверлеем):
   текст всегда на всю ширину, кнопки появляются под ним при наведении. */
#voicehist .ent .vh-acts {
  display: none; align-items: center; gap: 6px; margin-top: 10px;
}
#voicehist .ent:hover .vh-acts { display: flex; }
#voicehist .vh-acts button {
  appearance: none; border: 1px solid var(--hairline); background: var(--fill-1);
  color: var(--text-body); font: 500 11px/1 inherit; padding: 5px 9px; border-radius: 6px;
  cursor: default; display: flex; align-items: center; gap: 5px; white-space: nowrap;
}
#voicehist .vh-acts button:hover { background: var(--fill-2); color: var(--text); }
#voicehist .vh-acts button.vh-primary {
  border-color: var(--accent-line); background: var(--accent-soft); color: var(--accent);
}
#voicehist .vh-acts button.vh-icon { padding: 5px 7px; }
#voicehist .vh-acts button.vh-danger:hover { border-color: var(--danger); color: var(--danger); }

/* инлайн-результат преобразования */
#voicehist .vh-enh {
  margin: 9px 0 2px 0; border: 1px solid var(--hairline);
  background: var(--fill-1); border-radius: 10px; overflow: hidden;
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
  appearance: none; border: 1px solid var(--hairline); background: var(--fill-1);
  color: var(--text-body); font: 500 11px/1 inherit; padding: 5px 9px; border-radius: 6px; cursor: default;
}
#voicehist .vh-eh button:hover { color: var(--text); background: var(--fill-2); }
#voicehist .vh-eh button.vh-acc {
  border-color: var(--accent-line); background: var(--accent-soft); color: var(--accent);
}
#voicehist .vh-eh button.vh-eh-icon { padding: 4px 9px; font-size: 16px; line-height: .5; letter-spacing: 1px; }
#voicehist .vh-etext { padding: 11px 12px; font-size: 13px; line-height: 1.5; color: var(--text); }

/* меню преобразований (поповер) */
#voicehist .vh-tmenu {
  position: absolute; right: 10px; top: 40px; z-index: 5; width: 288px;
  background: rgba(28,28,32,0.98); border: 1px solid var(--border); border-radius: 11px;
  box-shadow: var(--shadow-pop); overflow: hidden;
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
#voicehist .heat i { aspect-ratio: 1; border-radius: 3px; background: var(--surface); }
#voicehist .heat i.l1 { background: color-mix(in srgb, var(--accent) 30%, var(--surface)); }
#voicehist .heat i.l2 { background: color-mix(in srgb, var(--accent) 58%, var(--surface)); }
#voicehist .heat i.l3 { background: var(--accent); }

#voicehist .bar { display: flex; align-items: center; gap: 12px; margin: 9px 0; }
#voicehist .bar .bl { width: 150px; font-size: 12.5px; color: var(--text-body); }
#voicehist .bar .bt { flex: 1; height: 8px; border-radius: 5px; background: var(--surface); overflow: hidden; }
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
#voicehist .btn:hover { background: var(--accent-soft); }
#voicehist .lrow {
  display: flex; align-items: center; gap: 12px; padding: 12px 14px;
  border: 1px solid var(--hairline); border-radius: 11px; margin-bottom: 8px; background: var(--card);
}
#voicehist .lrow .key { font-family: var(--mono); font-size: 13px; color: var(--accent); }
#voicehist .lrow .val { font-size: 13px; color: var(--text-body); flex: 1; }
#voicehist .lrow .meta { font-size: 11px; color: var(--faint); font-family: var(--mono); }
#voicehist .lrow .x { color: var(--faint); cursor: default; padding: 2px 6px; border-radius: 6px; }
#voicehist .lrow .x:hover { color: var(--danger); background: var(--danger-soft); }
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
  width: 30px; height: 30px; border-radius: 8px; background: var(--accent-soft);
  color: var(--accent); display: grid; place-items: center; flex: none;
}
#voicehist .tr .ti .ph, #voicehist .vh-smartrow .ti .ph { --icon-size: 16px; width: 16px; height: 16px; }
#voicehist .vh-trmid { flex: 1; min-width: 0; }
#voicehist .tn { font-size: 14px; font-weight: 500; }
#voicehist .tdesc { font-size: 12px; color: var(--muted); margin-top: 4px; line-height: 1.5; }
#voicehist .trig {
  display: inline-flex; align-items: center; gap: 5px; margin-top: 8px; font-size: 11px;
  color: var(--done); background: var(--done-soft); border: 1px solid var(--done-line);
  border-radius: 6px; padding: 2px 8px;
}
#voicehist .trig .ph { --icon-size: 11px; width: 11px; height: 11px; }
#voicehist .trig.manual {
  color: var(--muted); background: var(--surface); border-color: var(--border);
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
#voicehist .scratch .hint .ph { --icon-size: 14px; width: 14px; height: 14px; }

/* ════ Пусто ════ */
#voicehist .vh-empty {
  padding: 48px 24px; text-align: center; color: var(--faint); font-size: 13px; line-height: 1.5;
}

/* Usable native controls and bounded layouts for every voice subsection. */
#voicehist { --text: var(--ink); --text-body: var(--ink-2, var(--ink)); --muted: var(--ink-mute); --faint: var(--ink-faint); --hairline: var(--line); --border: var(--line-strong); --bg: var(--panel-glass, var(--paper)); --row-hover: var(--fill-2); --card: var(--fill-1); --sidebar: transparent; }
#voicehist *, #voicehist *::before, #voicehist *::after { box-sizing: border-box; }
#voicehist .vh-shell { display: flex; flex: 1; width: 100%; min-width: 0; height: 100%; overflow: hidden; }
#voicehist button, #voicehist input, #voicehist textarea { font: inherit; }
#voicehist button { cursor: pointer; }
#voicehist button:disabled { opacity: .5; cursor: default; }
#voicehist button:focus-visible, #voicehist input:focus-visible, #voicehist textarea:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
#voicehist .nav .it { width: 100%; border: 0; background: none; text-align: left; color: var(--muted); font-size: 12px; }
#voicehist .nav .it.on { color: var(--text); background: var(--fill-3); }
#voicehist .side { width: 182px; padding: 16px 10px; }
#voicehist .mhead { padding: 22px 24px 15px; gap: 12px; }
#voicehist .mhead .h { margin: 0; font-size: 22px; line-height: 1.3; letter-spacing: -.4px; }
#voicehist .pane { padding-bottom: 16px; }
#voicehist .pane[hidden] { display: none !important; }
#voicehist .feed { display: block; padding: 0 24px 24px; overflow: visible; }
#voicehist .vh-searchwrap { margin: 0 0 10px; }
#voicehist .vh-searchwrap input { min-width: 0; width: 100%; padding: 3px; }
#voicehist .ent .vh-acts { display: flex; flex-wrap: wrap; opacity: .78; }
#voicehist .ent:hover .vh-acts, #voicehist .ent:focus-within .vh-acts { opacity: 1; }
#voicehist .ent .vh-body { position: relative; }
#voicehist .vh-tmenu { background: var(--paper, #28282d); color: var(--ink); max-width: 100%; top: 65px; right: 0; box-shadow: 0 12px 38px rgba(0,0,0,.22); }
#voicehist .vh-ti { width: 100%; border: 0; border-radius: 0; background: transparent; color: var(--ink); font: inherit; text-align: left; }
#voicehist .vh-ti:focus-visible { outline-offset: -3px; }
#voicehist .vh-eh { flex-wrap: wrap; }
#voicehist .vh-note { margin: 6px 0 18px; background: var(--fill-2); border-color: var(--line); color: var(--muted); }
#voicehist .vh-error { display: flex; align-items: center; flex-wrap: wrap; gap: 10px; padding: 12px 14px; margin: 10px 0; color: var(--danger, #cb7385); border: 1px solid color-mix(in srgb, var(--danger, #cb7385) 25%, transparent); border-radius: 10px; background: color-mix(in srgb, var(--danger, #cb7385) 7%, transparent); font-size: 12px; line-height: 1.6; overflow-wrap: anywhere; }
#voicehist .vh-loading, #voicehist .vh-save-state, #voicehist .vh-inline-status { color: var(--muted); font-size: 11px; line-height: 1.5; }
#voicehist .vh-save-state { margin-top: 10px; }
#voicehist .vh-editor { margin-top: 12px; padding: 12px; border: 1px solid var(--line); border-radius: 12px; background: var(--fill-1); }
#voicehist .vh-editor textarea { width: 100%; min-height: 130px; padding: 10px; resize: vertical; border: 1px solid var(--line); border-radius: 8px; color: var(--ink); background: var(--fill-2); line-height: 1.6; }
#voicehist .vh-editor-actions { display: flex; justify-content: flex-end; gap: 9px; margin-top: 9px; }
#voicehist .addrow { flex-wrap: wrap; }
#voicehist .addrow input { min-width: 135px; width: 0; }
#voicehist .lrow { flex-wrap: wrap; gap: 10px; }
#voicehist .lrow .key, #voicehist .lrow .val { overflow-wrap: anywhere; min-width: 0; }
#voicehist .lrow .val { min-width: 70px; }
#voicehist .lrow .x { border: 0; background: transparent; font-size: 11px; }
#voicehist .btn { font-size: 11.5px; padding: 7px 10px; }
#voicehist .tg { border: 0; padding: 0; min-width: 34px; }
#voicehist .grid { grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 10px; padding-top: 6px; }
#voicehist .bigcard { min-width: 0; padding: 16px; }
#voicehist .bigcard .bl { font-size: 10px; line-height: 1.6; }
#voicehist .scratch { min-height: 300px; padding-top: 0; }
#voicehist .scratch textarea { min-height: 120px; }
@media(max-width: 760px) {
  #voicehist .side { width: 155px; padding: 14px 7px; }
  #voicehist .nav .it { font-size: 11px; padding: 9px 7px; gap: 7px; }
  #voicehist .mhead { padding: 18px 18px 12px; }
  #voicehist .mhead .h { font-size: 20px; }
  #voicehist .feed, #voicehist .sect, #voicehist .scratch { padding-left: 18px; padding-right: 18px; }
  #voicehist .grid { padding: 5px 18px 18px; }
  #voicehist .bigcard { padding: 11px; }
  #voicehist .bigcard .bn { font-size: 25px; }
}
#voicehist .vh-origin-label { margin-bottom: 5px; font-size: 10px; color: var(--muted); }
#voicehist .vh-original { margin-top: 10px; padding-top: 8px; border-top: 1px solid var(--line); color: var(--muted); font-size: 11px; }
#voicehist .vh-original summary { cursor: pointer; }
#voicehist .vh-original-text { margin: 8px 0; white-space: pre-wrap; user-select: text; }
#voicehist .vh-original button { font: inherit; color: var(--text); background: transparent; border: 0; padding: 5px 0; text-decoration: underline; cursor: pointer; }
@media(max-width: 560px) {
  #voicehist .vh-shell { flex-direction: column; }
  #voicehist .side { width: 100%; padding: 5px 10px; border: 0; border-bottom: 1px solid var(--line); }
  #voicehist .brand { display: none; }
  #voicehist .nav { flex-direction: row; gap: 2px; overflow-x: auto; }
  #voicehist .nav .it { width: auto; flex: 0 0 auto; padding: 9px 8px; font-size: 10.5px; }
  #voicehist .nav .it .ph { display: none; }
  #voicehist .main { flex: 1; min-height: 0; }
  #voicehist .ent { gap: 10px; }
  #voicehist .ent .lc { width: 43px; }
  #voicehist .grid { grid-template-columns: 1fr; }
  #voicehist .bigcard { display: flex; align-items: center; gap: 12px; }
  #voicehist .bigcard .bl { margin: 0; }
}
`;
})();
