/* Микро-рендерер markdown (спека 2026-07-18 §3.1). Два выхода на общем доме:
 * render() — HTML-строка для вьюера документов, renderChat() — DOM для реплик
 * ассистента. Второй жил в renderer.js и потому не доставался чату агента:
 * вкладка и окно из трея рисовали ответы голым текстом.
 * Содержимое НЕДОВЕРЕННОЕ: весь текст проходит через escapeHtml, сырой
 * HTML не пропускается никогда. Ссылки кликабельны только http(s) — и то без
 * настоящего href (data-href, клик обрабатывает вьюер); относительные — текст
 * с классом; javascript:/data:/прочие схемы режутся (url выбрасывается).
 * Стороннюю либу не тянем сознательно: UI ванильный, безопасность важнее
 * полноты CommonMark. Выход render() — HTML-строка (единственное место
 * innerHTML); renderChat() строит узлы и не знает про innerHTML вовсе. */
(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.JarvisMarkdown = api;
})(typeof globalThis !== 'undefined' ? globalThis : this, function () {
  'use strict';

  // Единственная дверь текста в HTML — всё остальное строится вокруг неё.
  function escapeHtml(s) {
    return String(s)
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;')
      .replace(/'/g, '&#39;');
  }

  // url → 'external' (http/https, кликабельна) | 'relative' (без схемы, в v1
  // текст с классом) | null (опасная/чужая схема либо //host — режем).
  function classifyUrl(url) {
    const u = String(url).trim();
    const m = u.match(/^[a-zA-Z][a-zA-Z0-9+.-]*:/);
    if (m) return /^https?:$/i.test(m[0]) ? 'external' : null;
    if (u.startsWith('//')) return null; // протокол-относительная — фактически внешняя
    return 'relative';
  }

  function isMarkdownPath(path) {
    return /\.(md|markdown)$/i.test(String(path));
  }

  // «Документ» для бейджей/CTA (§3.3): docs/** или markdown-файл.
  function isDocPath(path) {
    return isMarkdownPath(path) || /(^|\/)docs\//.test(String(path));
  }

  // Инлайны: `код` | [текст](url) | **жирный** | *курсив*. Regex создаётся на
  // вызов: renderInline рекурсивен (жирный/курсив внутри), общий lastIndex
  // у глобального regex ломал бы обход.
  function renderInline(text) {
    const re = /(`+)([\s\S]*?)\1|\[([^\]\n]+)\]\(((?:[^()\s]|\([^()\s]*\))+)\)|\*\*([^*\n]+)\*\*|\*([^*\n]+)\*/g;
    let out = '';
    let last = 0;
    let m;
    while ((m = re.exec(text))) {
      out += escapeHtml(text.slice(last, m.index));
      last = re.lastIndex;
      if (m[1] !== undefined) {
        out += '<code>' + escapeHtml(m[2]) + '</code>';
      } else if (m[3] !== undefined) {
        const label = escapeHtml(m[3]);
        const kind = classifyUrl(m[4]);
        if (kind === 'external') out += '<a class="md-link" data-href="' + escapeHtml(m[4]) + '">' + label + '</a>';
        else if (kind === 'relative') out += '<span class="md-link-rel" title="' + escapeHtml(m[4]) + '">' + label + '</span>';
        else out += '<span class="md-link-dead">' + label + '</span>'; // url выброшен
      } else if (m[5] !== undefined) {
        out += '<strong>' + renderInline(m[5]) + '</strong>';
      } else {
        out += '<em>' + renderInline(m[6]) + '</em>';
      }
    }
    return out + escapeHtml(text.slice(last));
  }

  const TABLE_SEP_RE = /^\s*\|?\s*:?-+:?\s*(\|\s*:?-+:?\s*)+\|?\s*$/;

  // строка таблицы → ячейки (внешние | отрезаны; \| не поддерживаем — v1)
  function tableCells(s) {
    let t = s.trim();
    if (t.startsWith('|')) t = t.slice(1);
    if (t.endsWith('|')) t = t.slice(0, -1);
    return t.split('|').map((c) => c.trim());
  }

  // Блоки: заголовки #…######, абзацы, фенсы ```, списки (вложенные -/1.),
  // цитаты >, простые таблицы |, горизонтальная линия. Построчный проход.
  function render(text) {
    const lines = String(text).replace(/\r\n?/g, '\n').split('\n');
    const out = [];
    const para = [];
    // абзац: строки склеиваются пробелом (доки переносят прозу по ~80 колонок,
    // <br> на каждый перенос дал бы рваный текст)
    const flushPara = () => {
      if (!para.length) return;
      out.push('<p>' + para.map(renderInline).join(' ') + '</p>');
      para.length = 0;
    };
    // стек открытых списков; li держим открытым до следующего соседа/закрытия,
    // чтобы вложенный список попадал ВНУТРЬ li (валидная вложенность)
    const listStack = []; // { indent, tag, liOpen }
    const closeTopList = () => {
      const t = listStack.pop();
      out.push((t.liOpen ? '</li>' : '') + '</' + t.tag + '>');
    };
    const closeLists = () => {
      while (listStack.length) closeTopList();
    };
    const listItem = (indent, tag, content) => {
      while (listStack.length && indent < listStack[listStack.length - 1].indent) closeTopList();
      const top = listStack[listStack.length - 1];
      if (!top || indent > top.indent) {
        out.push('<' + tag + '>');
        listStack.push({ indent, tag, liOpen: false });
      } else if (top.tag !== tag) {
        closeTopList();
        out.push('<' + tag + '>');
        listStack.push({ indent, tag, liOpen: false });
      }
      const cur = listStack[listStack.length - 1];
      if (cur.liOpen) out.push('</li>');
      out.push('<li>' + renderInline(content));
      cur.liOpen = true;
    };

    let fence = null; // накопитель строк открытого фенса
    for (let i = 0; i < lines.length; i++) {
      const raw = lines[i];
      if (fence) {
        if (/^\s*```/.test(raw)) {
          out.push('<pre><code>' + escapeHtml(fence.join('\n')) + '</code></pre>');
          fence = null;
        } else fence.push(raw);
        continue;
      }
      const line = raw.replace(/\s+$/, '');
      // фенс — только строка, начинающаяся с ``` (язык после ``` игнорируем)
      if (/^\s*```/.test(line)) {
        flushPara();
        closeLists();
        fence = [];
        continue;
      }
      if (!line.trim()) {
        flushPara(); // списки НЕ закрываем: пустая строка между пунктами легальна
        continue;
      }
      const h = line.match(/^\s{0,3}(#{1,6})\s+(.*?)\s*#*$/);
      if (h) {
        flushPara();
        closeLists();
        out.push('<h' + h[1].length + '>' + renderInline(h[2]) + '</h' + h[1].length + '>');
        continue;
      }
      if (/^\s{0,3}(?:-{3,}|\*{3,}|_{3,})\s*$/.test(line)) {
        flushPara();
        closeLists();
        out.push('<hr>');
        continue;
      }
      if (/^\s*>/.test(line)) {
        flushPara();
        closeLists();
        const quote = [];
        while (i < lines.length && /^\s*>/.test(lines[i])) {
          quote.push(lines[i].replace(/^\s*> ?/, ''));
          i++;
        }
        i--;
        out.push('<blockquote>' + render(quote.join('\n')) + '</blockquote>');
        continue;
      }
      if (line.includes('|') && i + 1 < lines.length && TABLE_SEP_RE.test(lines[i + 1])) {
        flushPara();
        closeLists();
        let html = '<table><thead><tr>';
        for (const c of tableCells(line)) html += '<th>' + renderInline(c) + '</th>';
        html += '</tr></thead><tbody>';
        i += 2; // мимо шапки и разделителя
        for (; i < lines.length && lines[i].trim() && lines[i].includes('|'); i++) {
          html += '<tr>';
          for (const c of tableCells(lines[i])) html += '<td>' + renderInline(c) + '</td>';
          html += '</tr>';
        }
        i--;
        out.push(html + '</tbody></table>');
        continue;
      }
      const li = line.match(/^(\s*)(?:[-*+]|(\d{1,9})[.)])\s+(.*)$/);
      if (li) {
        flushPara();
        listItem(li[1].length, li[2] !== undefined ? 'ol' : 'ul', li[3]);
        continue;
      }
      // отступная строка при открытом li — продолжение пункта (перенос прозы)
      if (listStack.length && /^\s/.test(raw) && listStack[listStack.length - 1].liOpen) {
        out.push(' ' + renderInline(line.trim()));
        continue;
      }
      closeLists();
      para.push(line.trim());
    }
    flushPara();
    closeLists();
    if (fence) out.push('<pre><code>' + escapeHtml(fence.join('\n')) + '</code></pre>'); // незакрытый фенс
    return out.join('');
  }

  /* ---------- реплики ассистента: тот же markdown, но узлами ----------
   * Разбор здесь свой и нарочно бедный: у реплики свои жанры (Insight, чипы
   * кода) и нет innerHTML, а у дока — таблицы и ссылки. Общий вход — один
   * модуль: чат сессии, вкладка «Джарвис» и окно из трея зовут одно и то же. */

  // inline-SVG, собранный через DOM (без innerHTML — политика файла)
  const SVGNS = 'http://www.w3.org/2000/svg';
  function svgEl(tag, attrs) {
    const e = document.createElementNS(SVGNS, tag);
    for (const k in attrs) e.setAttribute(k, attrs[k]);
    return e;
  }

  function inlineInto(el, text) {
    for (const t of text.split(/(`[^`]+`|\*\*[^*]+\*\*)/g)) {
      if (!t) continue;
      if (t.length > 2 && t.startsWith('`') && t.endsWith('`')) {
        const code = document.createElement('code');
        code.textContent = t.slice(1, -1);
        el.appendChild(code);
      } else if (t.length > 4 && t.startsWith('**') && t.endsWith('**')) {
        const b = document.createElement('strong');
        b.textContent = t.slice(2, -2);
        el.appendChild(b);
      } else {
        el.appendChild(document.createTextNode(t));
      }
    }
  }

  // Русское склонение числительных. Одна на файл: «N заметок» у Insight и
  // «N реплик» у чата агента считались бы по разным правилам.
  function plural(n, one, few, many) {
    const m10 = n % 10, m100 = n % 100;
    if (m10 === 1 && m100 !== 11) return one;
    if (m10 >= 2 && m10 <= 4 && (m100 < 12 || m100 > 14)) return few;
    return many;
  }

  // Строки, по которым чат-ветка узнаёт блоки. Вынесены: по ним же ищет
  // границу стрима chatSplit, и разъехавшаяся копия резала бы посреди фенса.
  const FENCE_RE = /^\s*```/;
  const CALLOUT_RE = /^\s*`?\s*[★✦☆]\s*([^─━`]*?)\s*[─━]{3,}\s*`?\s*$/;
  const RULE_RE = /^\s*`?\s*[─━]{3,}\s*`?\s*$/;
  const HEAD_RE = /^\s{0,3}(#{1,6})\s+(.*?)\s*#*$/;
  // Пункт списка: цифра даёт <ol>, отступ — вложенность (как в render()).
  const ITEM_RE = /^(\s*)(?:[-*+•]|(\d{1,9})[.)])\s+(.*)$/;

  /* Граница «готового» куска для стрима: индекс сразу после последней пустой
   * строки вне фенса и вне Insight. До неё разметка уже не изменится — кусок
   * можно нарисовать один раз и больше не трогать, а хвост пересобирать. */
  function chatSplit(text, from) {
    const s = String(text);
    let i = Math.max(0, from | 0);
    let cut = i, inCode = false, inNote = false;
    for (let nl = s.indexOf('\n', i); nl >= 0; nl = s.indexOf('\n', i)) {
      const line = s.slice(i, nl);
      i = nl + 1;
      if (FENCE_RE.test(line)) { inCode = !inCode; continue; }
      if (inCode) continue;
      if (CALLOUT_RE.test(line)) { inNote = true; continue; }
      if (RULE_RE.test(line)) { inNote = false; continue; }
      if (!line.trim() && !inNote) cut = i;
    }
    return cut; // последняя строка без \n ещё растёт — её не отдаём
  }

  function renderChat(root, text) {
    const para = [];
    const code = [];
    let inCode = false;
    let callout = null; // .callout (свёрнутый Insight), пока открыт для записи
    let calloutBody = null; // .callout-body — туда пишется содержимое
    let calloutLabel = null; // span с текстом «Insight», в конце дополняем счётчиком

    const target = () => calloutBody || root;

    /* Финализировать Insight. Закрытый линией ─ сворачивается, как задумано;
     * оборванный концом текста — раскрытым: закрывающей линии при стриминге
     * ещё просто нет, и весь дальнейший ответ пропадал за строчкой «Insight». */
    const closeCallout = (byLine) => {
      if (!callout) return;
      const n = calloutBody.querySelectorAll('li, p').length;
      if (n) calloutLabel.textContent = `Insight · ${n} ${plural(n, 'заметка', 'заметки', 'заметок')}`;
      if (!byLine) callout.classList.add('open');
      callout = calloutBody = calloutLabel = null;
    };

    // Стек открытых списков: вложенный список кладём ВНУТРЬ своего пункта,
    // иначе уровни схлопываются в один плоский перечень.
    const lists = []; // { indent, tag, list, li }
    const closeLists = () => { lists.length = 0; };
    const addItem = (indent, tag, content) => {
      while (lists.length && indent < lists[lists.length - 1].indent) lists.pop();
      let top = lists[lists.length - 1];
      if (top && top.indent === indent && top.tag !== tag) { lists.pop(); top = lists[lists.length - 1]; }
      if (!top || indent > top.indent) {
        const list = document.createElement(tag);
        (top ? top.li || top.list : target()).appendChild(list);
        lists.push({ indent, tag, list, li: null });
        top = lists[lists.length - 1];
      }
      const li = document.createElement('li');
      inlineInto(li, content);
      top.list.appendChild(li);
      top.li = li;
    };

    const flushPara = () => {
      if (!para.length) return;
      const p = document.createElement('p');
      para.forEach((line, i) => {
        if (i) p.appendChild(document.createElement('br'));
        inlineInto(p, line);
      });
      target().appendChild(p);
      para.length = 0;
    };
    const flushCode = () => {
      const pre = document.createElement('pre');
      pre.textContent = code.join('\n');
      target().appendChild(pre);
      code.length = 0;
    };

    const src = String(text).split('\n');
    for (let li = 0; li < src.length; li++) {
      const raw = src[li];
      const line = raw.trimEnd();
      // фенс — только строка, начинающаяся с ``` (упоминание ``` в тексте — не фенс)
      if (FENCE_RE.test(line)) {
        if (inCode) flushCode();
        else { flushPara(); closeLists(); }
        inCode = !inCode;
        continue;
      }
      if (inCode) { code.push(raw); continue; }

      /* Таблица GFM. Узнаём по разделителю на СЛЕДУЮЩЕЙ строке — одна шапка без
       * него остаётся текстом, поэтому в стриме «половина таблицы» не мигает
       * таблицей: пока разделителя нет, это абзац. Дорисовывать построчно
       * безопасно — chatSplit режет ленту только по пустой строке, а таблица
       * пустых строк внутри не имеет и целиком живёт в хвосте. */
      if (line.includes('|') && li + 1 < src.length && TABLE_SEP_RE.test(src[li + 1])) {
        flushPara();
        closeLists();
        const wrap = document.createElement('div');
        wrap.className = 'tablewrap'; // узкое окно: прокрутка внутри, а не разъезд вёрстки
        const tbl = document.createElement('table');
        const head = document.createElement('tr');
        for (const c of tableCells(line)) {
          const th = document.createElement('th');
          inlineInto(th, c);
          head.appendChild(th);
        }
        const thead = document.createElement('thead');
        thead.appendChild(head);
        tbl.appendChild(thead);
        const tbody = document.createElement('tbody');
        li += 2; // мимо шапки и разделителя
        for (; li < src.length && src[li].trim() && src[li].includes('|'); li++) {
          const tr = document.createElement('tr');
          for (const c of tableCells(src[li])) {
            const td = document.createElement('td');
            inlineInto(td, c);
            tr.appendChild(td);
          }
          tbody.appendChild(tr);
        }
        li--;
        tbl.appendChild(tbody);
        wrap.appendChild(tbl);
        target().appendChild(wrap);
        continue;
      }

      // `★ Insight ───` → свёрнутая Insight-строка (раскрывается кликом)
      const co = line.match(CALLOUT_RE);
      if (co) {
        flushPara();
        closeLists();
        closeCallout(false); // вложенных Insight не бывает — закрываем предыдущий
        callout = document.createElement('div');
        callout.className = 'callout';
        const title = document.createElement('div');
        title.className = 'callout-title';
        title.appendChild(svgEl('svg', { width: '11', height: '11', viewBox: '0 0 12 12', fill: 'none', class: 'istar' }))
          .appendChild(svgEl('path', { d: 'M6 1 L7.3 4.4 L11 4.6 L8.1 6.9 L9.1 10.4 L6 8.4 L2.9 10.4 L3.9 6.9 L1 4.6 L4.7 4.4 Z', stroke: 'currentColor', 'stroke-width': '1.1', 'stroke-linejoin': 'round' }));
        calloutLabel = document.createElement('span');
        calloutLabel.textContent = co[1].trim() || 'Insight';
        title.appendChild(calloutLabel);
        const chev = svgEl('svg', { width: '9', height: '9', viewBox: '0 0 10 10', fill: 'none', class: 'ichev' });
        chev.appendChild(svgEl('path', { d: 'M3 2 L7 5 L3 8', stroke: 'currentColor', 'stroke-width': '1.4', 'stroke-linecap': 'round', 'stroke-linejoin': 'round' }));
        title.appendChild(chev);
        title.addEventListener('click', () => title.parentElement.classList.toggle('open'));
        callout.appendChild(title);
        calloutBody = document.createElement('div');
        calloutBody.className = 'callout-body';
        callout.appendChild(calloutBody);
        root.appendChild(callout);
        continue;
      }
      // линия из ─ : закрытие Insight либо просто разделитель
      if (RULE_RE.test(line)) {
        flushPara();
        closeLists();
        if (callout) closeCallout(true);
        else root.appendChild(Object.assign(document.createElement('div'), { className: 'md-hr' }));
        continue;
      }

      if (!line.trim()) { flushPara(); closeLists(); continue; }
      // Заголовок — свой блок, а не пометка на абзаце: `## Итог` и текст следом
      // без пустой строки делали заголовком весь блок.
      const h = line.match(HEAD_RE);
      if (h) {
        flushPara();
        closeLists();
        const p = document.createElement('p');
        p.className = 'md-h';
        inlineInto(p, h[2]);
        target().appendChild(p);
        continue;
      }
      const m = line.match(ITEM_RE);
      if (m) {
        flushPara();
        addItem(m[1].length, m[2] !== undefined ? 'ol' : 'ul', m[3]);
        continue;
      }
      closeLists();
      para.push(line);
    }
    flushPara();
    if (inCode && code.length) flushCode(); // незакрытый фенс — дорендерим как код
    closeCallout(false); // Insight без замыкающей линии ─ — раскрываем, а не прячем
  }

  return Object.freeze({
    render, renderInline, escapeHtml, classifyUrl, isMarkdownPath, isDocPath,
    renderChat, chatSplit, plural, svgEl,
  });
});
