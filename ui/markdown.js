/* Микро-рендерер markdown для вьюера документов (спека 2026-07-18 §3.1).
 * Содержимое файла НЕДОВЕРЕННОЕ: весь текст проходит через escapeHtml, сырой
 * HTML не пропускается никогда. Ссылки кликабельны только http(s) — и то без
 * настоящего href (data-href, клик обрабатывает вьюер); относительные — текст
 * с классом; javascript:/data:/прочие схемы режутся (url выбрасывается).
 * Стороннюю либу не тянем сознательно: UI ванильный, безопасность важнее
 * полноты CommonMark. Выход — HTML-строка (единственное место innerHTML). */
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

  return Object.freeze({ render, renderInline, escapeHtml, classifyUrl, isMarkdownPath, isDocPath });
});
