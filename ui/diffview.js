(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.JarvisDiffView = api;
})(typeof globalThis !== 'undefined' ? globalThis : this, function () {
  'use strict';

  // Плоское описание диффа: список строк для рендера. Чистая функция —
  // тестируется без DOM; renderTo() строит из неё узлы через textContent
  // (без innerHTML — содержимое диффа недоверенное, правил агент).
  // Возвращает [{cls, oldNo, newNo, mark, s}] и {gap, count} для полос.
  function rows(hunks) {
    const out = [];
    if (!Array.isArray(hunks) || !hunks.length) return out;
    let prevOldEnd = null; // номер old-строки сразу за предыдущим ханком
    for (const h of hunks) {
      let oldNo = h.old_start | 0;
      let newNo = h.new_start | 0;
      if (prevOldEnd != null) {
        const gap = oldNo - prevOldEnd;
        if (gap > 0) out.push({ gap: true, count: gap });
      }
      for (const ln of h.lines || []) {
        if (ln.t === '-') {
          out.push({ cls: 'diff-del', oldNo, newNo: null, mark: '-', s: ln.s });
          oldNo += 1;
        } else if (ln.t === '+') {
          out.push({ cls: 'diff-add', oldNo: null, newNo, mark: '+', s: ln.s });
          newNo += 1;
        } else {
          out.push({ cls: 'diff-ctx', oldNo, newNo, mark: ' ', s: ln.s });
          oldNo += 1;
          newNo += 1;
        }
      }
      prevOldEnd = oldNo; // следующий неизменный блок начинается здесь
    }
    return out;
  }

  function plural(n) {
    return n === 1 ? 'строка' : n < 5 ? 'строки' : 'строк';
  }

  function span(doc, cls, text) {
    const el = doc.createElement('span');
    el.className = cls;
    el.textContent = text;
    return el;
  }

  // Построить узлы диффа в контейнере через DOM API (без innerHTML).
  function renderTo(root, hunks) {
    const doc = root.ownerDocument || document;
    root.textContent = '';
    const items = rows(hunks);
    if (!items.length) {
      root.appendChild(span(doc, 'diff-empty', 'Изменений нет'));
      return;
    }
    for (const r of items) {
      if (r.gap) {
        root.appendChild(
          span(doc, 'diff-gap', '⋯ ' + r.count + ' ' + plural(r.count) + ' без изменений')
        );
        continue;
      }
      const row = doc.createElement('div');
      row.className = 'diff-row ' + r.cls;
      row.appendChild(span(doc, 'diff-ln', r.oldNo == null ? '' : String(r.oldNo)));
      row.appendChild(span(doc, 'diff-ln', r.newNo == null ? '' : String(r.newNo)));
      row.appendChild(span(doc, 'diff-mark', r.mark));
      row.appendChild(span(doc, 'diff-s', r.s));
      root.appendChild(row);
    }
  }

  return Object.freeze({ rows, renderTo, plural });
});
