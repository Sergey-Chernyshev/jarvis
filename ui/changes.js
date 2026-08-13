/* Изменения задачи: что агент наделал в рабочем каталоге.
 *
 * Панель отвечает на два вопроса, ради которых к агенту и возвращаются: «что
 * он натворил» и «беру или откатываю». Список файлов со счётчиками, дифф по
 * клику, галочки для выбора и одно решение внизу — принять выбранное коммитом.
 *
 * Работает одинаково для местной задачи и для задачи на узле: git считает та
 * машина, где сессия живёт (см. src-tauri/src/changes.rs).
 *
 * Всё содержимое — недоверенное (пути и строки диффа писал агент), поэтому
 * узлы строятся через textContent, а дифф рисует diffview.js. */

(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.JarvisChanges = api;
})(typeof globalThis !== 'undefined' ? globalThis : this, function () {
  'use strict';

  /* Сводка списка одной строкой: сколько файлов и на сколько строк. Цифры
   * человек читает раньше имён — по ним решают, смотреть ли вообще. */
  function summary(files) {
    const list = Array.isArray(files) ? files : [];
    if (!list.length) return 'Изменений нет';
    const add = list.reduce((n, f) => n + (f.added | 0), 0);
    const del = list.reduce((n, f) => n + (f.removed | 0), 0);
    const word = plural(list.length, 'файл', 'файла', 'файлов');
    const counts = add || del ? ` · +${add} −${del}` : '';
    return `${list.length} ${word}${counts}`;
  }

  function plural(n, one, few, many) {
    const d10 = n % 10;
    const d100 = n % 100;
    if (d100 >= 11 && d100 <= 14) return many;
    if (d10 === 1) return one;
    if (d10 >= 2 && d10 <= 4) return few;
    return many;
  }

  /* Что показать в строке файла. Чистая функция — на ней и держится тест:
   * счётчики у нового файла git не считает, и «+0 −0» было бы враньём. */
  function row(f) {
    const name = String(f.path || '');
    const counts = f.untracked && !f.added && !f.removed ? '' : `+${f.added | 0} −${f.removed | 0}`;
    return {
      path: name,
      state: String(f.state || ''),
      counts,
      // Новый файл git не помнит: откатывать нечем, и кнопку не рисуем.
      canRevert: !f.untracked,
    };
  }

  /* Сообщение коммита по умолчанию: не выдумываем смысл правок, но и не
   * заставляем набирать очевидное. Человек правит строку одним движением. */
  function defaultMessage(files) {
    const list = Array.isArray(files) ? files : [];
    if (!list.length) return '';
    if (list.length === 1) return `правки: ${String(list[0].path).split('/').pop()}`;
    return `правки агента: ${list.length} ${plural(list.length, 'файл', 'файла', 'файлов')}`;
  }

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

  let state = {
    sessionId: '', branch: '', files: [], picked: new Set(), open: '',
    busy: false, error: '',
    /* Ревью агентом: вердикт и что он сказал. Живёт до перезагрузки свода —
     * после коммита оно уже про другое. */
    review: null, reviewing: false,
  };
  let root = null;
  let bridge = null;
  let onToast = () => {};

  function mount(node, jarvis, toast) {
    root = node;
    bridge = jarvis;
    if (typeof toast === 'function') onToast = toast;
  }

  async function open(sessionId) {
    state = {
      sessionId, branch: '', files: [], picked: new Set(), open: '',
      busy: true, error: '', review: null, reviewing: false,
    };
    render();
    await reload();
  }

  async function reload() {
    state.busy = true;
    render();
    let res = null;
    try {
      res = await bridge.sessionChanges(state.sessionId);
    } catch (e) {
      res = { ok: false, error: String((e && e.message) || e) };
    }
    state.busy = false;
    if (!res || !res.ok) {
      state.error = (res && res.error) || 'не удалось спросить git';
      state.files = [];
    } else {
      state.error = '';
      state.branch = res.branch || '';
      state.files = Array.isArray(res.files) ? res.files : [];
      // По умолчанию выбрано всё: обычный случай — принять работу целиком.
      state.picked = new Set(state.files.map((f) => f.path));
    }
    render();
  }

  async function showDiff(path) {
    state.open = state.open === path ? '' : path;
    render();
    if (!state.open) return;
    const res = await bridge.sessionChangeDiff(state.sessionId, path);
    if (state.open !== path) return; // успели переключить
    const box = root && root.querySelector ? root.querySelector('.chg-diff') : null;
    if (!box) return;
    box.textContent = '';
    if (!res || !res.ok || !(res.hunks || []).length) {
      box.appendChild(el('div.chg-empty', { text: (res && res.error) || 'Дифф пуст' }));
      return;
    }
    if (typeof JarvisDiffView !== 'undefined') JarvisDiffView.renderTo(box, res.hunks);
  }

  /* Ревью агентом. Отдельная кнопка, а не автомат: чужой агент, читающий твой
   * дифф, стоит токенов и времени — это решение человека. */
  async function review() {
    if (state.reviewing) return;
    state.reviewing = true;
    state.review = null;
    render();
    let res = null;
    try {
      res = await bridge.sessionReview(state.sessionId);
    } catch (e) {
      res = { ok: false, error: String((e && e.message) || e) };
    }
    state.reviewing = false;
    state.review = res && res.ok ? { verdict: res.verdict, text: res.text } : { verdict: 'fail', text: (res && res.error) || 'ревью не вышло' };
    render();
  }

  /* Как назвать вердикт человеку. Слово важнее значка: «замечания» — это
   * приглашение прочитать, а галочка приглашает не читать. */
  function verdictWord(v) {
    if (v === 'ok') return 'можно принимать';
    if (v === 'ask') return 'нужен ты';
    if (v === 'return') return 'есть замечания';
    return 'не вышло';
  }

  async function commit() {
    const paths = state.files.map((f) => f.path).filter((p) => state.picked.has(p));
    if (!paths.length) { onToast('Не выбрано ни одного файла'); return; }
    const input = root.querySelector('.chg-msg');
    const message = (input && input.value) || '';
    state.busy = true;
    render();
    const res = await bridge.sessionCommit(state.sessionId, message, paths);
    state.busy = false;
    if (!res || !res.ok) { onToast((res && res.error) || 'Коммит не прошёл'); render(); return; }
    onToast(`Принято · ${res.sha || 'коммит'}`);
    await reload();
  }

  async function revert(path) {
    const res = await bridge.sessionRevert(state.sessionId, path);
    if (!res || !res.ok) { onToast((res && res.error) || 'Откат не прошёл'); return; }
    onToast(`Откачено: ${path.split('/').pop()}`);
    await reload();
  }

  function render() {
    if (!root) return;
    root.textContent = '';
    const head = el(
      'div.chg-head',
      el('span.chg-title', { text: 'Изменения' }),
      state.branch ? el('span.chg-branch', { text: state.branch }) : null,
      el('span.spacer'),
      el('span.chg-sum', { text: state.busy ? 'считаю…' : summary(state.files) })
    );
    root.appendChild(head);

    if (state.error) {
      root.appendChild(el('div.chg-empty', { text: state.error }));
      return;
    }
    if (!state.busy && !state.files.length) {
      root.appendChild(el('div.chg-empty', { text: 'Рабочее дерево чистое — агент ничего не менял.' }));
      return;
    }

    if (state.review || state.reviewing) {
      const r = state.review;
      root.appendChild(
        el(
          'div.chg-review',
          el('div.chg-verdict', { text: state.reviewing ? 'агент читает дифф…' : verdictWord(r.verdict) }),
          r && r.text ? el('div.chg-reviewtext', { text: r.text }) : null
        )
      );
    }

    const list = el('div.chg-list');
    for (const f of state.files) {
      const r = row(f);
      const box = el('input.chg-pick', { type: 'checkbox' });
      box.checked = state.picked.has(r.path);
      box.addEventListener('change', () => {
        if (box.checked) state.picked.add(r.path);
        else state.picked.delete(r.path);
      });
      const line = el(
        'div.chg-row',
        box,
        el('button.chg-path', { text: r.path, title: r.path, onclick: () => showDiff(r.path) }),
        el('span.chg-state', { text: r.state }),
        el('span.chg-counts', { text: r.counts }),
        r.canRevert
          ? el('button.chg-revert', { text: 'Откатить', title: 'Вернуть файл к последнему коммиту', onclick: () => revert(r.path) })
          : null
      );
      if (state.open === r.path) line.classList.add('open');
      list.appendChild(line);
      if (state.open === r.path) list.appendChild(el('div.chg-diff'));
    }
    root.appendChild(list);

    const msg = el('input.chg-msg', { type: 'text', placeholder: 'сообщение коммита' });
    msg.value = defaultMessage(state.files.filter((f) => state.picked.has(f.path)));
    root.appendChild(
      el(
        'div.chg-foot',
        el('button.chg-review-btn', {
          text: state.reviewing ? 'Проверяю…' : 'Пусть проверит',
          title: 'Позвать агента отревьюить эти правки',
          disabled: state.reviewing || undefined,
          onclick: review,
        }),
        msg,
        el('button.chg-accept', {
          text: state.busy ? 'Принимаю…' : 'Принять выбранное',
          disabled: state.busy || undefined,
          onclick: commit,
        })
      )
    );
  }

  return { mount, open, reload, render, summary, row, defaultMessage, plural, verdictWord, _state: () => state };
});
