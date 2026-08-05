(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.JarvisModelCheckbox = api;
})(typeof globalThis !== 'undefined' ? globalThis : this, function () {
  'use strict';

  function create(documentRef, options) {
    if (!documentRef || typeof documentRef.createElement !== 'function') {
      throw new TypeError('documentRef with createElement is required');
    }

    const opts = options || {};
    const node = documentRef.createElement('label');
    node.className = 'model-check';

    const input = documentRef.createElement('input');
    input.className = 'model-check-input';
    input.type = 'checkbox';
    input.checked = !!opts.checked;
    input.disabled = !!opts.disabled;
    input.setAttribute('aria-label', opts.label || 'Выбрать модель');

    const mark = documentRef.createElement('span');
    mark.className = 'model-check-mark';
    mark.setAttribute('aria-hidden', 'true');

    if (typeof opts.onChange === 'function') {
      input.addEventListener('change', () => opts.onChange(input.checked));
    }
    node.append(input, mark);

    return Object.freeze({ node, input, mark });
  }

  return Object.freeze({ create });
});
