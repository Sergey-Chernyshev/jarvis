/* Consistent, accessible states for asynchronous workspace operations. */
(() => {
  const node = (tag, cls, text) => { const el = document.createElement(tag); el.className = cls; if (text) el.textContent = text; return el; };
  function status(root, text, kind = 'loading') {
    root.dataset.state = kind;
    root.setAttribute('role', kind === 'error' ? 'alert' : 'status');
    root.setAttribute('aria-live', kind === 'error' ? 'assertive' : 'polite');
    root.replaceChildren();
    if (kind === 'loading') { const spinner = node('span', 'ui-spinner'); spinner.setAttribute('aria-hidden', 'true'); root.append(spinner); }
    root.append(node('span', 'ui-state-label', text));
  }
  function message({ title, detail, kind = 'empty', action, onAction } = {}) {
    const root = node('div', 'ui-state'); root.dataset.state = kind;
    root.setAttribute('role', kind === 'error' ? 'alert' : 'status');
    const copy = node('div', 'ui-state-copy'); copy.append(node('strong', '', title), node('p', '', detail)); root.append(copy);
    if (action && onAction) { const button = node('button', 'ui-state-action', action); button.type = 'button'; button.addEventListener('click', onAction); root.append(button); }
    return root;
  }
  function skeleton(layout, label) {
    const root = node('div', 'ui-skeleton ui-skeleton-' + layout); root.setAttribute('role', 'status'); root.setAttribute('aria-label', label); root.setAttribute('aria-busy', 'true');
    const accessible = node('span', 'ui-sr-only', label); root.append(accessible);
    for (let row = 0; row < (layout === 'history' ? 3 : 4); row++) {
      const group = node('div', 'ui-skeleton-group'); group.setAttribute('aria-hidden', 'true');
      for (let line = 0; line < (layout === 'history' ? 3 : 2); line++) group.append(node('span', 'ui-skeleton-line'));
      root.append(group);
    }
    return root;
  }
  function button(root, busy, label) {
    if (!root) return;
    root.classList.toggle('ui-button-busy', !!busy); root.setAttribute('aria-busy', String(!!busy));
    if (label) { root.setAttribute('aria-label', label); root.title = label; }
  }
  window.JarvisAsyncState = { status, message, skeleton, button };
})();
