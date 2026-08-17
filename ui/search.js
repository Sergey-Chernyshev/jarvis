/* Поиск по проекту задачи.
 *
 * Ревью упирается в «а где это ещё» быстрее, чем в «что тут изменилось».
 * Панель отвечает ровно на это: строка запроса, список совпадений с путём и
 * номером строки, клик — открыть файл в редакторе.
 *
 * Совпадения писал не мы: строки кода недоверенные, поэтому только textContent.
 */

(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.JarvisSearch = api;
})(typeof globalThis !== 'undefined' ? globalThis : this, function () {
  'use strict';

  const el = (tag, attrs, ...kids) => {
    const [name, ...cls] = tag.split('.');
    const n = document.createElement(name || 'div');
    if (cls.length) n.className = cls.join(' ');
    const isProps = attrs != null && typeof attrs === 'object' && !Array.isArray(attrs) && !attrs.nodeType;
    if (isProps) {
      for (const [k, v] of Object.entries(attrs)) {
        if (v == null || v === false) continue;
        if (k === 'text') n.textContent = v;
        else if (k.startsWith('on')) n.addEventListener(k.slice(2), v);
        else n.setAttribute(k, v === true ? '' : v);
      }
    } else if (attrs != null) {
      kids.unshift(attrs);
    }
    for (const kid of kids.flat(Infinity)) if (kid) n.appendChild(kid);
    return n;
  };

  /* Итог поиска словами. «Ничего» — это тоже ответ, и его надо сказать: пустой
   * список неотличим от «ещё не искали». */
  function summary(hits, capped, query) {
    if (!query) return 'что искать?';
    const n = Array.isArray(hits) ? hits.length : 0;
    if (!n) return 'ничего не нашлось';
    const files = new Set(hits.map((h) => h.path)).size;
    const tail = capped ? ' (показаны первые)' : '';
    return `${n} ${plural(n, 'совпадение', 'совпадения', 'совпадений')} в ${files} ${plural(files, 'файле', 'файлах', 'файлах')}${tail}`;
  }

  function plural(n, one, few, many) {
    const d10 = n % 10;
    const d100 = n % 100;
    if (d100 >= 11 && d100 <= 14) return many;
    if (d10 === 1) return one;
    if (d10 >= 2 && d10 <= 4) return few;
    return many;
  }

  /* Группировка по файлам: двадцать строк из одного файла подряд читаются как
   * шум, а сгруппированные — как «вот тут и тут». */
  function group(hits) {
    const out = [];
    for (const h of Array.isArray(hits) ? hits : []) {
      const last = out[out.length - 1];
      if (last && last.path === h.path) last.lines.push(h);
      else out.push({ path: h.path, lines: [h] });
    }
    return out;
  }

  let state = { sessionId: '', query: '', hits: [], capped: false, busy: false, error: '' };
  let root = null;
  let bridge = null;
  let onOpen = () => {};

  function mount(node, jarvis, openFile) {
    root = node;
    bridge = jarvis;
    if (typeof openFile === 'function') onOpen = openFile;
  }

  function open(sessionId) {
    state = { sessionId, query: state.query, hits: [], capped: false, busy: false, error: '' };
    render();
  }

  async function run(query) {
    state.query = query;
    if (!query.trim()) { state.hits = []; render(); return; }
    state.busy = true;
    render();
    let res = null;
    try {
      res = await bridge.sessionSearch(state.sessionId, query);
    } catch (e) {
      res = { ok: false, error: String((e && e.message) || e) };
    }
    state.busy = false;
    if (!res || !res.ok) {
      state.error = (res && res.error) || 'поиск не вышел';
      state.hits = [];
    } else {
      state.error = '';
      state.hits = Array.isArray(res.hits) ? res.hits : [];
      state.capped = !!res.capped;
    }
    render();
  }

  function render() {
    if (!root) return;
    root.textContent = '';
    const input = el('input.srch-input', { type: 'text', placeholder: 'что искать в проекте' });
    input.value = state.query;
    input.addEventListener('keydown', (e) => {
      e.stopPropagation();
      if (e.key === 'Enter') run(input.value);
    });
    root.appendChild(
      el(
        'div.srch-head',
        input,
        el('button.srch-go', { text: state.busy ? 'Ищу…' : 'Найти', onclick: () => run(input.value) })
      )
    );
    root.appendChild(el('div.srch-sum', { text: state.error || (state.busy ? 'ищу…' : summary(state.hits, state.capped, state.query)) }));

    for (const g of group(state.hits)) {
      const box = el('div.srch-file');
      box.appendChild(
        el('button.srch-path', {
          text: g.path,
          title: 'Открыть в редакторе',
          onclick: () => onOpen(g.path),
        })
      );
      for (const h of g.lines) {
        box.appendChild(
          el('div.srch-line', el('span.srch-no', { text: String(h.line) }), el('span.srch-text', { text: h.text }))
        );
      }
      root.appendChild(box);
    }
  }

  return { mount, open, run, render, summary, group, plural, _state: () => state };
});
