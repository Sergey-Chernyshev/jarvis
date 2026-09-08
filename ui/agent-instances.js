/* Provider profiles are discovered by the backend. A Desktop launcher is not
 * a CLI, and selecting a profile never copies credentials between accounts. */
(() => {
  const make = (tag, cls, text) => { const n = document.createElement(tag); n.className = cls || ''; if (text != null) n.textContent = text; return n; };
  const button = (label, fn, cls = '') => { const b = make('button', `instance-button ${cls}`, label); b.type = 'button'; if (fn) b.addEventListener('click', fn); return b; };
  const field = (label, value = '', placeholder = '') => {
    const wrap = make('label', 'instance-field');
    const input = make('input', 'instance-input'); input.setAttribute('aria-label', label); input.placeholder = placeholder; input.value = value;
    wrap.append(make('span', 'instance-field-label', label), input);
    return { wrap, input };
  };
  const pluralChats = value => {
    const n = Math.max(0, Number(value) || 0), last = n % 10, lastTwo = n % 100;
    return `${n} ${last === 1 && lastTwo !== 11 ? 'чат' : last >= 2 && last <= 4 && (lastTwo < 12 || lastTwo > 14) ? 'чата' : 'чатов'}`;
  };
  const hookState = (instance, health) => {
    if (!instance.enabled) return { label: 'Наблюдение выключено', tone: 'muted' };
    if (health.errors?.length || instance.exists === false) return { label: 'Требуется внимание', tone: 'warning' };
    if (!health.rulesInstalled || ['untrusted', 'modified', 'disabled'].includes(health.trustStatus)) return { label: 'Настрой хуки', tone: 'warning' };
    if (instance.lastHookAt) return { label: 'События получены', tone: 'success' };
    return { label: 'Ждём первое событие', tone: 'info' };
  };
  const trustLabel = value => ({ trusted: 'Разрешено', untrusted: 'Нужно разрешение', modified: 'Правила изменились', disabled: 'Отключено' })[value] || 'Ещё не проверено';
  let renderId = 0;

  async function render(pane) {
    const host = make('section', 'instance-settings'); host.setAttribute('aria-label', 'Профили Codex'); pane.append(host);
    const prefix = `instance-settings-${++renderId}`;
    const openProfiles = new Set(), drafts = new Map();
    let data, busy = false, stale = false, addOpen = false;
    const addDraft = { home: '', label: '' };
    const status = make('div', 'instance-status'); status.setAttribute('role', 'status'); status.setAttribute('aria-live', 'polite');
    function say(message, tone = 'info') { status.textContent = message; status.dataset.tone = tone; status.hidden = !message; }
    function lock() {
      host.setAttribute('aria-busy', String(busy));
      host.querySelectorAll('button,input,select').forEach(n => { n.disabled = busy || n.dataset.unavailable === 'true' || (stale && n.dataset.mutation === 'true'); });
    }
    function focus(key) {
      const control = [...host.querySelectorAll('[data-focus-key]')].find(n => n.dataset.focusKey === key);
      if (control && !control.disabled && !control.closest('[hidden]')) control.focus({ preventScroll: true });
    }
    const identify = (control, key, mutation = false) => { control.dataset.focusKey = key; if (mutation) control.dataset.mutation = 'true'; return control; };
    async function run(operation, message, success, applied, changes = true) {
      if (busy) return;
      const focusKey = document.activeElement?.dataset.focusKey;
      let accepted = false, nextFocus = focusKey;
      busy = true; lock(); say(message);
      try {
        const result = await operation();
        if (result?.ok === false) throw new Error(result.error || 'Не удалось применить изменение');
        accepted = true;
        nextFocus = applied?.() || nextFocus;
        await refresh();
        say(success || 'Готово', 'success');
      } catch (e) {
        if (accepted && changes) {
          // A second save must not overwrite a successful edit using an old
          // config if the subsequent discovery request failed.
          stale = true;
          say(`Изменение применено. Обнови список профилей: ${e?.message || String(e)}`, 'warning');
        } else {
          say(e?.message || String(e), 'error');
          const current = host.querySelector('select[aria-label="Профиль Codex по умолчанию"]');
          if (current && data) current.value = data.defaultCodexInstance;
          host.querySelectorAll('input[data-instance-id]').forEach(n => { n.checked = !!data?.instances.find(i => i.id === n.dataset.instanceId)?.enabled; });
        }
      } finally { busy = false; lock(); focus(nextFocus); }
    }
    function entry(instance, patch) {
      const config = structuredClone(data.config);
      const found = config.entries.findIndex(e => e.home === instance.home || e.home === instance.canonicalHome);
      const value = { ...(found >= 0 ? config.entries[found] : { home: instance.canonicalHome, machine: instance.machine }), label: instance.label, enabled: instance.enabled, ...patch };
      if (found >= 0) config.entries[found] = value; else config.entries.push(value);
      return config;
    }
    function profile(instance, index) {
      const health = (data.health || []).find(h => h.instanceId === instance.id) || {};
      const card = make('article', 'instance-profile'); card.dataset.instanceId = instance.id;
      card.setAttribute('aria-label', instance.label);
      const row = make('div', 'instance-profile-row');
      const avatar = make('span', `instance-avatar ${index % 2 ? 'instance-avatar-blue' : ''}`);
      avatar.setAttribute('aria-hidden', 'true');
      if (window.jarvisIcons?.create) avatar.append(window.jarvisIcons.create('terminal-window')); else avatar.textContent = '>_';
      const copy = make('div', 'instance-identity');
      copy.append(make('h3', 'instance-name', instance.label));
      const meta = make('div', 'instance-meta');
      meta.append(make('span', 'instance-chat-count', pluralChats(instance.observedChats)));
      const state = hookState(instance, health), chip = make('span', 'instance-badge', state.label); chip.dataset.tone = state.tone; meta.append(chip);
      copy.append(meta);
      const actions = make('div', 'instance-row-actions');
      const tracking = make('label', 'instance-tracking');
      const enabled = identify(make('input', 'toggle'), `tracking:${instance.id}`, true);
      enabled.type = 'checkbox'; enabled.checked = instance.enabled; enabled.dataset.instanceId = instance.id;
      enabled.setAttribute('aria-label', `Отслеживать ${instance.label}`);
      enabled.addEventListener('change', () => run(() => window.jarvis.agentInstancesSave(entry(instance, { enabled: enabled.checked })), 'Сохраняем…', 'Наблюдение обновлено'));
      tracking.append(make('span', '', 'Следить'), enabled);
      const body = make('div', 'instance-details'); body.id = `${prefix}-profile-${index}`;
      const toggle = identify(button('Настроить', () => {
        if (openProfiles.has(instance.id)) openProfiles.delete(instance.id); else openProfiles.add(instance.id);
        updateDisclosure();
      }, 'instance-disclosure'), `details:${instance.id}`);
      toggle.setAttribute('aria-label', `Настроить ${instance.label}`); toggle.setAttribute('aria-controls', body.id);
      const caret = make('span', 'instance-caret', '⌄'); caret.setAttribute('aria-hidden', 'true'); toggle.append(caret);
      function updateDisclosure() {
        const open = openProfiles.has(instance.id); body.hidden = !open; toggle.setAttribute('aria-expanded', String(open));
        if (open) card.dataset.escapeOwner = 'true'; else delete card.dataset.escapeOwner;
      }
      updateDisclosure();
      card.addEventListener('keydown', event => {
        if (event.key === 'Escape' && !busy && openProfiles.has(instance.id)) { event.preventDefault(); event.stopPropagation(); openProfiles.delete(instance.id); updateDisclosure(); toggle.focus(); }
      });
      actions.append(tracking, toggle); row.append(avatar, copy, actions);

      const form = make('form', 'instance-rename');
      const label = field('Название профиля', drafts.get(instance.id) ?? instance.label);
      label.input.required = true; label.input.maxLength = 80;
      identify(label.input, `name:${instance.id}`, true);
      label.input.addEventListener('input', () => drafts.set(instance.id, label.input.value));
      const save = identify(button('Сохранить', null, 'instance-primary'), `save:${instance.id}`, true); save.type = 'submit';
      form.addEventListener('submit', event => {
        event.preventDefault();
        run(() => {
          const name = label.input.value.trim(); if (!name) throw new Error('Введи название профиля');
          return window.jarvis.agentInstancesSave(entry(instance, { label: name }));
        }, 'Сохраняем название…', 'Название сохранено', () => { drafts.delete(instance.id); });
      });
      form.append(label.wrap, save); body.append(form);

      const hook = make('div', 'instance-hook-setting');
      const hookCopy = make('div', 'instance-hook-copy');
      hookCopy.append(make('strong', '', 'События агента'), make('p', '', health.rulesInstalled
        ? 'Хуки установлены. Проверка обновит правила и разрешения Codex.'
        : 'Подключи хуки, чтобы получать статусы и уведомления о работе агента.'));
      const repair = identify(button(health.rulesInstalled ? 'Проверить хуки' : 'Настроить хуки', () => run(() => window.jarvis.agentInstancesRepair([instance.id]), 'Настраиваем хуки этого профиля…', 'Хуки проверены'), health.rulesInstalled ? '' : 'instance-primary'), `repair:${instance.id}`, true);
      repair.dataset.unavailable = String(!instance.enabled);
      if (!instance.enabled) repair.title = 'Сначала включи наблюдение за профилем';
      hook.append(hookCopy, repair); body.append(hook);
      if (!instance.enabled) body.append(make('p', 'instance-hint', 'Включи «Следить», чтобы настраивать хуки и видеть чаты этого профиля.'));
      if (health.errors?.length) {
        const errors = make('div', 'instance-errors'); errors.setAttribute('role', 'note');
        for (const error of health.errors) errors.append(make('p', '', error));
        body.append(errors);
      }
      const technical = make('dl', 'instance-facts');
      const fact = (name, value, code = false) => {
        const pair = make('div', 'instance-fact'); pair.append(make('dt', '', name), make('dd', code ? 'instance-path' : '', value)); technical.append(pair);
      };
      fact('Каталог профиля', instance.canonicalHome || instance.home, true);
      fact('Codex CLI', instance.cli || 'Не найден', !!instance.cli);
      fact('Разрешение хуков', trustLabel(health.trustStatus));
      const date = instance.lastHookAt ? new Date(instance.lastHookAt) : null;
      fact('Последнее событие', date && Number.isFinite(date.getTime()) ? date.toLocaleString() : 'Ещё не получено');
      body.append(technical); card.append(row, body); return card;
    }
    function paint() {
      const heading = make('div', 'instance-heading');
      heading.append(make('h2', 'instance-section-title', 'Профили Codex'));
      heading.append(identify(button('Обновить', () => run(async () => {}, 'Ищем профили…', 'Список обновлён', null, false), 'instance-quiet'), 'refresh'));
      host.replaceChildren(heading, status);

      const defaultRow = make('div', 'instance-default');
      const defaultCopy = make('div', 'instance-default-copy');
      const label = make('label', 'instance-field-label', 'Для новых задач'); label.htmlFor = `${prefix}-default`;
      defaultCopy.append(label, make('p', 'instance-hint', 'Также используется для сводок.'));
      const select = identify(make('select', 'instance-input instance-select'), 'default', true);
      select.id = label.htmlFor; select.setAttribute('aria-label', 'Профиль Codex по умолчанию');
      const available = data.instances.filter(i => i.enabled);
      for (const i of available) { const option = make('option', '', i.label); option.value = i.id; select.append(option); }
      if (!available.length) { select.append(make('option', '', 'Нет активных профилей')); select.dataset.unavailable = 'true'; }
      else select.value = data.defaultCodexInstance;
      select.addEventListener('change', () => run(() => window.jarvis.agentInstancesSave({ ...data.config, defaultCodexInstance: select.value }), 'Сохраняем профиль…', 'Профиль по умолчанию сохранён'));
      defaultRow.append(defaultCopy, select); host.append(defaultRow);
      const group = make('div', 'instance-profile-list');
      data.instances.forEach((instance, index) => group.append(profile(instance, index)));
      if (!data.instances.length) {
        const empty = make('div', 'instance-empty');
        empty.append(make('strong', '', 'Подключи первый профиль'), make('p', '', 'Jarvis найдёт чаты в каталоге Codex.'));
        group.append(empty);
      }
      host.append(group);

      const add = identify(button('Добавить профиль', () => { addOpen = true; paint(); focus('add-home'); }, 'instance-add-button'), 'add-open');
      add.hidden = addOpen;
      const form = make('form', 'instance-add-form'); form.hidden = !addOpen; form.dataset.escapeOwner = 'true';
      const title = make('h3', 'instance-form-title', 'Новый профиль');
      const home = field('Каталог CODEX_HOME', addDraft.home, '/Users/имя/.codex'); home.input.required = true; home.input.spellcheck = false; home.input.autocomplete = 'off'; home.input.classList.add('instance-path-input');
      identify(home.input, 'add-home', true); home.input.addEventListener('input', () => { addDraft.home = home.input.value; });
      const name = field('Название профиля', addDraft.label, 'Например, Рабочий'); name.input.required = true; name.input.maxLength = 80;
      identify(name.input, 'add-name', true); name.input.addEventListener('input', () => { addDraft.label = name.input.value; });
      const controls = make('div', 'instance-form-actions');
      const save = identify(button('Добавить', null, 'instance-primary'), 'add-save', true); save.type = 'submit';
      const cancel = identify(button('Отмена', () => { addOpen = false; addDraft.home = ''; addDraft.label = ''; paint(); focus('add-open'); }), 'add-cancel');
      controls.append(save, cancel);
      form.append(title, home.wrap, name.wrap, make('p', 'instance-hint', 'Укажи существующий каталог. Личный и рабочий профили с ярлыками находятся автоматически.'), controls);
      form.addEventListener('submit', event => {
        event.preventDefault();
        run(() => {
          if (!home.input.value.trim()) throw new Error('Укажи каталог Codex');
          if (!name.input.value.trim()) throw new Error('Введи название профиля');
          return window.jarvis.agentInstancesSave({ ...data.config, entries: [...data.config.entries, { home: home.input.value.trim(), label: name.input.value.trim(), enabled: true, machine: 'local' }] });
        }, 'Проверяем каталог…', 'Профиль добавлен', () => { addOpen = false; addDraft.home = ''; addDraft.label = ''; return 'add-open'; });
      });
      // Escape cancels only the open form; a second Escape may navigate back.
      form.addEventListener('keydown', event => { if (event.key === 'Escape' && !busy) { event.preventDefault(); event.stopPropagation(); cancel.click(); } });
      host.append(add, form); lock();
    }
    async function refresh() {
      const next = await window.jarvis.agentInstancesList();
      if (!Array.isArray(next?.instances) || !Array.isArray(next.config?.entries)) throw new Error('Не удалось загрузить профили');
      data = next; stale = false; paint();
    }
    say('Ищем профили Codex…'); host.append(status);
    try { await refresh(); say(''); }
    catch (e) { say(e?.message || String(e), 'error'); host.append(identify(button('Повторить', () => run(async () => {}, 'Ищем профили…', 'Список обновлён', null, false)), 'refresh')); }
  }
  window.JarvisInstances = { render };
})();
