/* One picker surface for the app. Native selects remain the form/IPC authority;
 * the visible control never invents values or bypasses existing change handlers. */
(() => {
  'use strict';
  const states = new Map();
  const optionOwners = new WeakMap();
  let serial = 0, active = null, queued = false;
  const chevron = '<svg viewBox="0 0 16 16" aria-hidden="true"><path d="m4.5 6 3.5 3.5L11.5 6"/></svg>';
  const check = '<svg viewBox="0 0 16 16" aria-hidden="true"><path d="m3.5 8 3 3 6-6"/></svg>';
  const visible = node => !!node?.isConnected && !!node.getClientRects().length && getComputedStyle(node).visibility !== 'hidden';
  const enabled = option => !option.disabled && !option.closest('optgroup')?.disabled && !option.hidden;
  const labelFor = source => {
    const explicit = source.getAttribute('aria-label') || source.title;
    if (explicit) return explicit;
    const label = source.labels?.[0]?.cloneNode(true);
    label?.querySelectorAll('select, button, .jselect').forEach(node => node.remove());
    return label?.textContent.trim() || 'Выберите вариант';
  };
  const schedule = () => {
    if (queued) return;
    queued = true;
    queueMicrotask(() => { queued = false; refresh(); });
  };

  // Property assignments do not produce MutationObserver records. Wrap only
  // controls owned by this component, without changing DOM prototypes globally.
  function watchProperty(node, prototype, key, changed) {
    const descriptor = Object.getOwnPropertyDescriptor(prototype, key);
    if (!descriptor?.set || Object.hasOwn(node, key)) return;
    Object.defineProperty(node, key, {
      configurable: true, enumerable: descriptor.enumerable,
      get() { return descriptor.get.call(this); },
      set(value) { descriptor.set.call(this, value); changed(); },
    });
  }

  function enhance(source) {
    if (states.has(source)) return states.get(source);
    if (source.multiple || source.size > 1 || source.dataset.nativeSelect !== undefined) return null;
    const hadFocus = document.activeElement === source;
    const wrapper = document.createElement('span');
    wrapper.className = 'jselect';
    const trigger = document.createElement('button');
    trigger.type = 'button'; trigger.className = 'jselect-trigger';
    trigger.setAttribute('role', 'combobox');
    trigger.setAttribute('aria-haspopup', 'listbox');
    trigger.setAttribute('aria-expanded', 'false');
    trigger.tabIndex = source.tabIndex;
    const value = document.createElement('span'); value.className = 'jselect-value';
    const arrow = document.createElement('span'); arrow.className = 'jselect-chevron'; arrow.innerHTML = chevron;
    trigger.append(value, arrow);
    source.before(wrapper); wrapper.append(source, trigger);
    source.classList.add('jselect-source'); source.tabIndex = -1;
    source.setAttribute('aria-hidden', 'true');
    const state = { source, wrapper, trigger, value, id: `jselect-${++serial}`, signature: '' };
    trigger.setAttribute('aria-controls', state.id);
    states.set(source, state);
    for (const key of ['value', 'selectedIndex', 'disabled', 'hidden']) {
      watchProperty(source, key === 'hidden' ? HTMLElement.prototype : HTMLSelectElement.prototype, key, schedule);
    }
    source.addEventListener('focus', () => { sync(state); trigger.focus({ preventScroll: true }); });
    source.addEventListener('invalid', event => { event.preventDefault(); state.invalid = true; sync(state); trigger.focus(); });
    source.addEventListener('click', event => { event.preventDefault(); if (!trigger.disabled) open(state); });
    source.addEventListener('change', () => sync(state));
    source.addEventListener('input', () => sync(state));
    trigger.addEventListener('click', event => {
      event.preventDefault(); event.stopPropagation();
      active?.state === state ? close(true) : open(state);
    });
    sync(state);
    if (hadFocus && !trigger.disabled) trigger.focus({ preventScroll: true });
    return state;
  }

  function sync(state) {
    const { source, wrapper, trigger, value } = state;
    if (states.get(source) !== state) return;
    if (!source.isConnected) {
      if (active?.state === state) close();
      trigger.remove();
      // A caller may replace just the source control inside our wrapper.
      // Unwrap any replacement instead of leaving an orphaned trigger behind.
      wrapper.replaceWith(...wrapper.childNodes);
      states.delete(source); return;
    }
    wrapper.hidden = source.hidden;
    trigger.disabled = source.matches(':disabled') || !source.options.length;
    const label = labelFor(source);
    trigger.setAttribute('aria-label', label);
    for (const name of ['aria-labelledby', 'aria-describedby', 'aria-invalid']) {
      const attr = source.getAttribute(name);
      if (attr) trigger.setAttribute(name, attr); else trigger.removeAttribute(name);
    }
    trigger.setAttribute('aria-required', String(source.required));
    if (state.invalid && source.validity.valid) state.invalid = false;
    wrapper.classList.toggle('is-invalid', !!state.invalid);
    if (state.invalid) trigger.setAttribute('aria-invalid', 'true');
    if (source.dataset.focusKey) trigger.dataset.focusKey = source.dataset.focusKey;
    const selected = source.selectedOptions[0];
    const caption = selected?.label || source.dataset.placeholder || 'Выберите…';
    if (value.textContent !== caption) value.textContent = caption;
    trigger.title = state.invalid ? source.validationMessage : `${label}: ${caption}`;
    wrapper.dataset.empty = String(!selected || selected.value === '');
    for (const option of source.options) {
      if (!optionOwners.has(option)) {
        optionOwners.set(option, source);
        watchProperty(option, HTMLOptionElement.prototype, 'selected', schedule);
      }
    }
    if (active?.state === state) {
      if (trigger.disabled || !visible(trigger)) { close(); return; }
      const signature = JSON.stringify([...source.options].map(o => [o.value, o.label, o.selected, enabled(o)]));
      if (signature !== state.signature) { state.signature = signature; renderOptions(); }
      position();
    }
  }

  function refresh(source) {
    if (source) { const state = enhance(source); if (state) sync(state); return; }
    document.querySelectorAll('select').forEach(enhance);
    for (const state of states.values()) sync(state);
  }

  function close(restore = false) {
    if (!active) return;
    const { state, popup } = active;
    active = null;
    state.trigger.setAttribute('aria-expanded', 'false');
    state.trigger.removeAttribute('aria-activedescendant');
    state.wrapper.classList.remove('is-open');
    popup.classList.add('is-closing'); popup.setAttribute('aria-hidden', 'true');
    popup.inert = true;
    // Keep the exit animation, but retire its accessible identity immediately.
    // A quick reopen must not resolve aria-controls to this departing menu.
    popup.querySelectorAll('[id]').forEach(node => node.removeAttribute('id'));
    popup.querySelectorAll('[aria-controls], [aria-activedescendant]').forEach(node => {
      node.removeAttribute('aria-controls'); node.removeAttribute('aria-activedescendant');
    });
    setTimeout(() => popup.remove(), matchMedia('(prefers-reduced-motion: reduce)').matches ? 0 : 120);
    if (restore && visible(state.trigger) && !state.trigger.disabled) state.trigger.focus({ preventScroll: true });
  }

  function open(state) {
    sync(state);
    if (!state.source.isConnected || states.get(state.source) !== state || state.trigger.disabled || !visible(state.trigger)) return;
    close();
    document.querySelectorAll('.cselect.open').forEach(node => node.dismissSelect?.());
    const popup = document.createElement('div'); popup.className = 'jselect-popover';
    popup.dataset.escapeOwner = 'select';
    const list = document.createElement('div'); list.className = 'jselect-options';
    list.id = state.id; list.setAttribute('role', 'listbox'); list.setAttribute('aria-label', labelFor(state.source));
    let search = null;
    if (state.source.options.length >= 8) {
      const field = document.createElement('div'); field.className = 'jselect-search';
      const icon = document.createElement('span');
      icon.innerHTML = '<svg viewBox="0 0 16 16" aria-hidden="true"><circle cx="7" cy="7" r="4.5"/><path d="m10.5 10.5 3 3"/></svg>';
      search = document.createElement('input'); search.type = 'search'; search.autocomplete = 'off'; search.spellcheck = false;
      search.placeholder = 'Найти вариант…'; search.setAttribute('aria-label', `Найти: ${labelFor(state.source)}`);
      search.setAttribute('aria-controls', state.id); search.setAttribute('role', 'combobox');
      search.setAttribute('aria-expanded', 'true'); search.setAttribute('aria-autocomplete', 'list');
      search.addEventListener('input', () => { if (active?.state === state) { renderOptions(); position(); } });
      field.append(icon, search); popup.append(field);
    }
    popup.append(list); document.body.append(popup);
    active = { state, popup, list, search, rows: [], index: -1, typed: '', typedAt: 0 };
    state.wrapper.classList.add('is-open'); state.trigger.setAttribute('aria-expanded', 'true');
    state.signature = JSON.stringify([...state.source.options].map(o => [o.value, o.label, o.selected, enabled(o)]));
    renderOptions(); position();
    (search || state.trigger).focus({ preventScroll: true });
    requestAnimationFrame(() => { if (active?.state === state) { position(); popup.classList.add('is-visible'); } });
  }

  function renderOptions() {
    if (!active) return;
    const current = active;
    const { state, list, search } = current;
    const query = (search?.value || '').trim().toLocaleLowerCase();
    const previous = current.rows[current.index]?.option;
    list.replaceChildren(); current.rows = [];
    let group = null;
    for (const option of state.source.options) {
      if (option.hidden || (query && !option.label.toLocaleLowerCase().includes(query))) continue;
      const optgroup = option.closest('optgroup');
      if (optgroup && group !== optgroup) {
        const heading = document.createElement('div'); heading.className = 'jselect-group'; heading.textContent = optgroup.label;
        heading.setAttribute('role', 'presentation'); list.append(heading);
      }
      group = optgroup;
      const row = document.createElement('div'); row.className = 'jselect-option';
      row.id = `${state.id}-option-${option.index}`; row.setAttribute('role', 'option');
      row.setAttribute('aria-selected', String(option.selected)); row.setAttribute('aria-disabled', String(!enabled(option)));
      const caption = document.createElement('span'); caption.className = 'jselect-option-label'; caption.textContent = option.label;
      const tick = document.createElement('span'); tick.className = 'jselect-check'; tick.innerHTML = check;
      row.append(caption, tick);
      const index = current.rows.length;
      row.addEventListener('pointermove', () => { if (active === current && enabled(option)) highlight(index, false); });
      // Keep the combobox's focus while clicking an option, including in WKWebView.
      row.addEventListener('pointerdown', event => event.preventDefault());
      row.addEventListener('click', () => { if (active === current && enabled(option)) commit(option); });
      current.rows.push({ option, row }); list.append(row);
    }
    if (!current.rows.length) {
      const empty = document.createElement('div'); empty.className = 'jselect-empty'; empty.textContent = 'Ничего не найдено';
      empty.setAttribute('role', 'status'); list.append(empty);
    }
    const prior = current.rows.findIndex(item => item.option === previous && enabled(item.option));
    const selected = current.rows.findIndex(item => item.option.selected && enabled(item.option));
    highlight(prior >= 0 ? prior : selected >= 0 ? selected : current.rows.findIndex(item => enabled(item.option)), false);
    requestAnimationFrame(() => { if (active === current) current.rows[current.index]?.row.scrollIntoView({ block: 'nearest' }); });
  }

  function highlight(index, scroll = true) {
    if (!active) return;
    active.index = index;
    active.rows.forEach((item, i) => item.row.classList.toggle('is-active', i === index));
    const row = active.rows[index]?.row;
    for (const control of [active.state.trigger, active.search].filter(Boolean)) {
      if (row) control.setAttribute('aria-activedescendant', row.id); else control.removeAttribute('aria-activedescendant');
    }
    if (scroll) row?.scrollIntoView({ block: 'nearest' });
  }

  function commit(option) {
    const { state } = active;
    const changed = state.source.selectedIndex !== option.index;
    const label = labelFor(state.source), sourceId = state.source.id;
    close(true);
    if (!changed) return;
    state.source.selectedIndex = option.index;
    sync(state);
    state.source.dispatchEvent(new Event('input', { bubbles: true }));
    state.source.dispatchEvent(new Event('change', { bubbles: true }));
    // Some forms replace their whole DOM on change. Preserve keyboard position
    // only when that replacement removed the focused control.
    queueMicrotask(() => {
      refresh();
      if (state.source.isConnected || document.activeElement !== document.body) return;
      const replacement = [...states.values()].find(s => visible(s.trigger) && !s.trigger.disabled && (sourceId ? s.source.id === sourceId : labelFor(s.source) === label));
      replacement?.trigger.focus({ preventScroll: true });
    });
  }

  function position() {
    if (!active) return;
    const { state, popup, list } = active;
    const rect = state.trigger.getBoundingClientRect();
    // Jarvis scales the document with CSS zoom. Portal coordinates and measured
    // rectangles must use the same units at every supported UI scale.
    const zoom = Number.parseFloat(getComputedStyle(document.documentElement).zoom) || 1;
    const width = innerWidth / zoom, height = innerHeight / zoom, gap = 7, edge = 12;
    const anchor = { left: rect.left / zoom, right: rect.right / zoom, top: rect.top / zoom, bottom: rect.bottom / zoom, width: rect.width / zoom };
    const menuWidth = Math.min(Math.max(anchor.width, 240), 360, width - edge * 2);
    popup.style.width = `${menuWidth}px`;
    const overhead = active.search ? 61 : 14;
    const idealHeight = Math.min(list.scrollHeight + overhead, 352);
    const below = height - anchor.bottom - gap - edge, above = anchor.top - gap - edge;
    const up = below < idealHeight && above > below;
    const available = Math.max(70, Math.min(352, up ? above : below));
    list.style.maxHeight = `${Math.max(40, available - overhead)}px`;
    popup.style.left = `${Math.max(edge, Math.min(anchor.left, width - menuWidth - edge))}px`;
    popup.dataset.side = up ? 'top' : 'bottom';
    const menuHeight = popup.offsetHeight;
    popup.style.top = `${Math.max(edge, Math.min(up ? anchor.top - gap - menuHeight : anchor.bottom + gap, height - menuHeight - edge))}px`;
  }

  function tabAway(event) {
    const trigger = active.state.trigger;
    const scope = trigger.closest('[role="dialog"]') || document;
    const nodes = [...scope.querySelectorAll('button, input, textarea, select, a[href], [tabindex]')]
      .filter(node => node.tabIndex >= 0 && !node.matches(':disabled') && !node.closest('[inert], [aria-hidden="true"], .jselect-popover') && visible(node));
    const index = nodes.indexOf(trigger);
    const next = nodes[index + (event.shiftKey ? -1 : 1)] || (scope !== document ? nodes[event.shiftKey ? nodes.length - 1 : 0] : null);
    close();
    if (next) { event.preventDefault(); next.focus(); } else trigger.focus();
  }

  // Register before page navigation's capture listener so Escape closes just
  // this popup, and Enter cannot also submit the chat/form underneath it.
  window.addEventListener('keydown', event => {
    if (event.isComposing || event.altKey || event.ctrlKey || event.metaKey) return;
    if (!active) {
      const state = [...states.values()].find(s => s.trigger === event.target);
      if (state && ['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) {
        event.preventDefault(); event.stopImmediatePropagation(); open(state);
        if (active && event.key === 'End') highlight(active.rows.findLastIndex(item => enabled(item.option)));
      }
      return;
    }
    const controlled = ['Escape', 'Tab', 'Enter', 'ArrowDown', 'ArrowUp', 'Home', 'End'];
    if (!active.search) controlled.push(' ');
    if (controlled.includes(event.key)) {
      // Home/End in search should continue editing the query normally.
      if (event.target === active.search && ['Home', 'End'].includes(event.key)) return;
      event.stopImmediatePropagation();
      if (event.key === 'Tab') { tabAway(event); return; }
      event.preventDefault();
      if (event.key === 'Escape') { close(true); return; }
      if (event.key === 'Enter' || event.key === ' ') { const choice = active.rows[active.index]?.option; if (choice && enabled(choice)) commit(choice); return; }
      const indices = active.rows.flatMap((item, index) => enabled(item.option) ? [index] : []);
      const index = indices.indexOf(active.index);
      const next = event.key === 'Home' ? 0 : event.key === 'End' ? indices.length - 1 : Math.max(0, Math.min(indices.length - 1, index + (event.key === 'ArrowDown' ? 1 : -1)));
      highlight(indices[next] ?? -1);
    } else if (!active.search && event.key.length === 1) {
      event.preventDefault(); event.stopImmediatePropagation();
      const now = Date.now(); active.typed = (now - active.typedAt > 650 ? '' : active.typed) + event.key.toLocaleLowerCase(); active.typedAt = now;
      const match = active.rows.findIndex(item => enabled(item.option) && item.option.label.toLocaleLowerCase().startsWith(active.typed));
      if (match >= 0) highlight(match);
    }
  }, true);
  window.addEventListener('pointerdown', event => { if (active && !active.popup.contains(event.target) && !active.state.wrapper.contains(event.target)) close(); }, true);
  window.addEventListener('focusin', event => { if (active && !active.popup.contains(event.target) && !active.state.wrapper.contains(event.target)) close(); });
  window.addEventListener('resize', position);
  window.addEventListener('scroll', event => { if (active && !active.popup.contains(event.target)) position(); }, true);
  document.addEventListener('reset', schedule, true);
  new MutationObserver(records => {
    for (const record of records) {
      const target = record.target;
      if (target.closest?.('select') || target.matches?.('fieldset') ||
          [...record.addedNodes, ...record.removedNodes].some(node => node.nodeType === 1 && (node.matches?.('select') || node.querySelector?.('select'))) ||
          (active && target.contains?.(active.state.source) && !target.closest?.('.jselect'))) { schedule(); return; }
    }
  }).observe(document.body, { subtree: true, childList: true, characterData: true, attributes: true, attributeFilter: ['disabled', 'hidden', 'selected', 'value', 'label', 'required', 'aria-label', 'aria-labelledby', 'aria-describedby', 'aria-invalid', 'class', 'style'] });
  window.JarvisSelect = { enhance, refresh, close, isOpen: () => !!active };
  refresh();
})();
