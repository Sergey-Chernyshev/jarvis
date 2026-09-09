import assert from 'node:assert/strict';
import test from 'node:test';
import { createRequire } from 'node:module';
import { parseHTML } from 'linkedom';

const require = createRequire(import.meta.url);
const MD = require('./markdown.js');

/* Реплики ассистента строятся узлами, поэтому renderChat нужен настоящий DOM.
 * Половина рендерера, которую человек видит чаще всего, при переезде из
 * renderer.js осталась без единого теста — тут она и закрывается. */
const { document } = parseHTML('<div id="root"></div>');
globalThis.document = document;
const chat = (text) => {
  const root = document.createElement('div');
  MD.renderChat(root, text);
  return root;
};

/* --- блоки --- */

test('заголовки #…###### и не-заголовок #######', () => {
  assert.equal(MD.render('# A'), '<h1>A</h1>');
  assert.equal(MD.render('###### B'), '<h6>B</h6>');
  // 7 решёток — не заголовок, обычный абзац
  assert.equal(MD.render('####### C'), '<p>####### C</p>');
});

test('абзац: строки склеиваются пробелом, пустая строка разделяет', () => {
  assert.equal(MD.render('раз\nдва\n\nтри'), '<p>раз два</p><p>три</p>');
});

test('фенс: моноблок без обработки markdown внутри', () => {
  const html = MD.render('```js\n**не жирный**\n<b>&\n```');
  assert.equal(html, '<pre><code>**не жирный**\n&lt;b&gt;&amp;</code></pre>');
});

test('незакрытый фенс дорендеривается как код', () => {
  assert.equal(MD.render('```\nхвост'), '<pre><code>хвост</code></pre>');
});

test('списки: вложенный ul внутри li, ol с цифрами', () => {
  assert.equal(
    MD.render('- a\n  - b\n- c'),
    '<ul><li>a<ul><li>b</li></ul></li><li>c</li></ul>'
  );
  assert.equal(MD.render('1. a\n2. b'), '<ol><li>a</li><li>b</li></ol>');
});

test('список: пустая строка между пунктами не рвёт список', () => {
  assert.equal(MD.render('- a\n\n- b'), '<ul><li>a</li><li>b</li></ul>');
});

test('список: отступный перенос — продолжение пункта', () => {
  assert.equal(MD.render('- длинный пункт\n  и его перенос'), '<ul><li>длинный пункт и его перенос</li></ul>');
});

test('цитата: рекурсивный рендер содержимого', () => {
  assert.equal(MD.render('> цитата\n> **жирная**'), '<blockquote><p>цитата <strong>жирная</strong></p></blockquote>');
});

test('простая таблица: th по шапке, td по строкам', () => {
  const html = MD.render('| a | b |\n|---|---|\n| 1 | 2 |');
  assert.equal(
    html,
    '<table><thead><tr><th>a</th><th>b</th></tr></thead><tbody><tr><td>1</td><td>2</td></tr></tbody></table>'
  );
});

test('горизонтальная линия', () => {
  assert.equal(MD.render('---'), '<hr>');
  assert.equal(MD.render('***'), '<hr>');
});

/* --- инлайны --- */

test('жирный/курсив/код', () => {
  assert.equal(MD.render('**ж** *к* `код`'), '<p><strong>ж</strong> <em>к</em> <code>код</code></p>');
});

/* --- экранирование: содержимое файла недоверенное --- */

test('сырой HTML экранируется в абзаце, коде и заголовке', () => {
  assert.equal(MD.render('<img src=x onerror=alert(1)>'), '<p>&lt;img src=x onerror=alert(1)&gt;</p>');
  assert.equal(MD.render('`<script>`'), '<p><code>&lt;script&gt;</code></p>');
  assert.equal(MD.render('# <b>x</b>'), '<h1>&lt;b&gt;x&lt;/b&gt;</h1>');
});

test('кавычка в url не вырывается из атрибута data-href', () => {
  const html = MD.render('[t](https://e.com/"onmouseover="x)');
  assert.ok(html.includes('data-href="https://e.com/&quot;onmouseover=&quot;x"'), html);
  assert.ok(!/"\s*onmouseover\s*=/.test(html.replace(/&quot;/g, '')), 'атрибут не инжектится');
});

/* --- ссылки: только http(s) кликабельны, опасные схемы режутся --- */

test('http(s)-ссылка — <a class="md-link"> с data-href, без настоящего href', () => {
  const html = MD.render('[доки](https://example.com/x)');
  assert.equal(html, '<p><a class="md-link" data-href="https://example.com/x">доки</a></p>');
  assert.ok(!html.includes(' href='), 'настоящий href не ставим — навигация вебвью запрещена');
});

test('javascript:/data:/mailto: режутся — url выбрасывается из выхода', () => {
  for (const url of ['javascript:alert(1)', 'data:text/html,x', 'mailto:a@b.c', 'file:///etc/passwd']) {
    const html = MD.render(`[клик](${url})`);
    assert.equal(html, '<p><span class="md-link-dead">клик</span></p>', url);
    assert.ok(!html.includes(url.split(':')[0] + ':'), `схема ${url} не утекла`);
  }
});

test('JAVASCRIPT: в верхнем регистре тоже режется', () => {
  const html = MD.render('[x](JAVASCRIPT:alert(1))');
  assert.ok(!/javascript:/i.test(html), html);
});

test('относительная ссылка — текст с классом, не кликабельна', () => {
  const html = MD.render('[спека](docs/design.md)');
  assert.equal(html, '<p><span class="md-link-rel" title="docs/design.md">спека</span></p>');
  assert.ok(!html.includes('<a '), 'ссылки нет');
});

test('протокол-относительный //host режется', () => {
  const html = MD.render('[x](//evil.com/p)');
  assert.equal(html, '<p><span class="md-link-dead">x</span></p>');
  assert.ok(!html.includes('evil.com'));
});

/* --- утилиты --- */

test('classifyUrl', () => {
  assert.equal(MD.classifyUrl('https://a.b'), 'external');
  assert.equal(MD.classifyUrl('http://a.b'), 'external');
  assert.equal(MD.classifyUrl('./a.md'), 'relative');
  assert.equal(MD.classifyUrl('#anchor'), 'relative');
  assert.equal(MD.classifyUrl('javascript:x'), null);
  assert.equal(MD.classifyUrl('data:text/html'), null);
  assert.equal(MD.classifyUrl('//evil'), null);
});

test('isMarkdownPath / isDocPath', () => {
  assert.ok(MD.isMarkdownPath('a/b.md'));
  assert.ok(MD.isMarkdownPath('a/B.MARKDOWN'));
  assert.ok(!MD.isMarkdownPath('a/b.rs'));
  assert.ok(MD.isDocPath('docs/x.txt'), 'docs/** — док даже не-markdown');
  assert.ok(MD.isDocPath('src/README.md'));
  assert.ok(!MD.isDocPath('src/main.rs'));
});

test('escapeHtml экранирует все спецсимволы', () => {
  assert.equal(MD.escapeHtml(`&<>"'`), '&amp;&lt;&gt;&quot;&#39;');
});

/* --- renderChat: реплика ассистента --- */

test('нумерованный список остаётся списком, а не склеенным абзацем', () => {
  const r = chat('1. раз\n2. два\n3. три');
  assert.equal(r.querySelectorAll('ol').length, 1, 'цифры ушли в абзац');
  assert.equal(r.querySelectorAll('ol > li').length, 3);
  assert.equal(r.querySelectorAll('p').length, 0, 'пункты склеились через <br>');
});

test('вложенность списка сохраняется, а не схлопывается в плоский ul', () => {
  const r = chat('- верх\n  - низ\n  - ещё низ\n- сосед');
  assert.equal(r.children.length, 1, 'уровни разъехались в соседние списки');
  const top = r.firstElementChild;
  assert.equal(top.tagName.toLowerCase(), 'ul');
  assert.equal(top.children.length, 2, 'вложенные пункты всплыли на верхний уровень');
  assert.equal(top.firstElementChild.querySelectorAll('ul > li').length, 2);
});

test('смешанный список: цифры и дефисы дают разные списки', () => {
  const r = chat('1. раз\n2. два\n\n- пункт');
  assert.equal(r.querySelectorAll('ol').length, 1);
  assert.equal(r.querySelectorAll('ul').length, 1);
});

test('заголовком становится строка, а не весь абзац следом за ней', () => {
  const r = chat('## Итог\nвсё сошлось\nи проверено');
  const h = r.querySelectorAll('.md-h');
  assert.equal(h.length, 1, 'заголовков не один');
  assert.equal(h[0].textContent, 'Итог', 'решётка или текст утекли в заголовок');
  // текст следом — обычный абзац: иначе заголовком читается вся простыня
  const p = [...r.querySelectorAll('p')].filter((x) => !x.classList.contains('md-h'));
  assert.equal(p.length, 1);
  assert.match(p[0].textContent, /всё сошлось/);
  assert.match(p[0].textContent, /и проверено/);
});

test('заголовок в середине ответа разрывает абзац', () => {
  const r = chat('вступление\n### Дальше\nхвост');
  assert.equal(r.children.length, 3, 'блоки склеились: ' + r.children.length);
  assert.equal(r.children[1].className, 'md-h');
});

test('незакрытый Insight не проглатывает остаток ответа', () => {
  const r = chat('★ Insight ─────\n- заметка\n\nа дальше главное');
  const box = r.querySelector('.callout');
  assert.ok(box, 'Insight не собрался');
  // закрывающей линии нет (при стриминге её ещё просто нет) — тело обязано быть
  // видно, иначе весь дальнейший ответ исчезает за тихой строчкой «Insight»
  assert.ok(box.classList.contains('open'), 'ответ спрятан за свёрнутым Insight');
  assert.match(box.textContent, /а дальше главное/);
});

test('закрытый линией Insight сворачивается и считает заметки', () => {
  const r = chat('★ Insight ─────\n- раз\n- два\n─────\nпродолжение');
  const box = r.querySelector('.callout');
  assert.equal(box.classList.contains('open'), false, 'свёрнутый Insight раскрылся сам');
  assert.match(box.querySelector('.callout-title').textContent, /Insight · 2 заметки/);
  // текст после закрывающей линии — вне Insight, на своём месте
  assert.equal(box.textContent.includes('продолжение'), false);
  assert.match(r.textContent, /продолжение/);
});

test('склонение заметок считает по-русски', () => {
  const label = (n) => chat('★ Insight ───\n' + Array.from({ length: n }, (_, i) => '- ' + i).join('\n') + '\n───')
    .querySelector('.callout-title').textContent;
  assert.match(label(1), /1 заметка/);
  assert.match(label(3), /3 заметки/);
  assert.match(label(5), /5 заметок/);
  assert.match(label(11), /11 заметок/);
});

test('фенс в реплике — блок кода без разбора разметки внутри', () => {
  const r = chat('текст\n```js\n- не пункт\n**не жирный**\n```');
  assert.equal(r.querySelectorAll('ul').length, 0);
  assert.equal(r.querySelector('pre').textContent, '- не пункт\n**не жирный**');
});

test('незакрытый фенс всё равно виден как код', () => {
  assert.equal(chat('```\nхвост').querySelector('pre').textContent, 'хвост');
});

test('сырой HTML в реплике остаётся текстом', () => {
  const r = chat('<img src=x onerror=alert(1)>');
  assert.equal(r.querySelectorAll('img').length, 0, 'разметка исполнилась');
  assert.match(r.textContent, /<img src=x/);
});

/* Граница стрима: до неё разметка уже не изменится, и нарисованное можно не
 * трогать. Резать посреди фенса или Insight нельзя — куски склеятся в мусор. */
test('chatSplit не режет внутри фенса и внутри Insight', () => {
  const closed = 'абзац\n\n';
  assert.equal(MD.chatSplit(closed + 'хвост', 0), closed.length);
  // пустая строка внутри фенса границей не считается
  assert.equal(MD.chatSplit('```\nраз\n\nдва\n', 0), 0);
  // и внутри незакрытого Insight тоже: закрывающая линия ещё не пришла
  assert.equal(MD.chatSplit('★ Insight ───\n- раз\n\n- два\n', 0), 0);
  // а после закрывающей линии — снова можно
  const note = '★ Insight ───\n- раз\n───\n\n';
  assert.equal(MD.chatSplit(note + 'дальше', 0), note.length);
  // последняя строка без перевода ещё растёт — её не отдаём
  assert.equal(MD.chatSplit('раз\n\nдва', 0), 5);
});

test('дописанный по кускам ответ выглядит как нарисованный целиком', () => {
  const full = '## Итог\n\n1. раз\n2. два\n\n```\ncargo test\n```\n\nхвост';
  const whole = chat(full);
  // стрим: рисуем готовую часть один раз, хвост пересобираем
  const grown = document.createElement('div');
  const steps = [];
  for (let i = 7; i < full.length; i += 7) steps.push(i);
  steps.push(full.length);
  let at = 0;
  for (const i of steps) {
    const text = full.slice(0, i);
    while (grown.childElementCount > (grown.kept || 0)) grown.lastElementChild.remove();
    const edge = MD.chatSplit(text, at);
    if (edge > at) { MD.renderChat(grown, text.slice(at, edge)); at = edge; grown.kept = grown.childElementCount; }
    MD.renderChat(grown, text.slice(at));
  }
  assert.equal(grown.childElementCount, whole.childElementCount, 'блоки разъехались');
  assert.equal(grown.textContent, whole.textContent);
});

test('plural — общая на файл, как и склонение заметок', () => {
  const f = (n) => MD.plural(n, 'реплика', 'реплики', 'реплик');
  assert.deepEqual([1, 2, 5, 11, 21, 104].map(f), ['реплика', 'реплики', 'реплик', 'реплик', 'реплика', 'реплики']);
});

/* Образец с экрана владельца — байт в байт, включая пробел в конце последней
 * строки и разную ширину колонок в разделителе. Таблицы в чате были только у
 * документного рендерера; в ленте лежал сырой текст с палками. */
test('renderChat рисует таблицу, а не палки', () => {
  const sample = [
    '| Задача | Кому | Пишет код? |',
    '|---|---|---|',
    '| Ревалидация: 69 коллизий canonical_url, падает каждый прогон с 26.07 | claude, отдельный worktree | да |',
    '| wantapply (60% текущих потерь) + adndx.ru → на готовый HH API-парсер | claude, отдельный worktree | да |',
    '| Проверить, что на проде реально крутится — снапшот или заморозка сессии | kimi | нет, только чтение | ',
  ].join('\n');
  const root = chat(sample);

  const tbl = root.querySelector('table');
  assert.ok(tbl, 'таблицы нет вовсе: ' + root.textContent.slice(0, 80));
  assert.equal(root.querySelectorAll('thead th').length, 3, 'колонок не три');
  assert.equal(root.querySelectorAll('tbody tr').length, 3, 'строк не три');
  assert.equal(root.querySelectorAll('tbody tr:last-child td').length, 3,
    'хвостовой пробел съел колонку');
  assert.match(root.querySelector('tbody tr:last-child td:last-child').textContent,
    /нет, только чтение/);
  // палок в тексте больше нет — иначе это по-прежнему сырой markdown
  assert.doesNotMatch(root.textContent, /\|---/, 'разделитель остался текстом');
  assert.ok(root.querySelector('.tablewrap'), 'нет обёртки — узкое окно разъедет вёрстку');
});

test('разделитель другой ширины и выравнивание таблицу не роняют', () => {
  const root = chat(['| a | b |', '| :--- | ----------: |', '| 1 | 2 |'].join('\n'));
  assert.equal(root.querySelectorAll('thead th').length, 2);
  assert.equal(root.querySelectorAll('tbody td').length, 2);
});

test('шапка без разделителя остаётся текстом — в стриме таблица не мигает', () => {
  const root = chat('| Задача | Кому |');
  assert.equal(root.querySelectorAll('table').length, 0,
    'одна строка стала таблицей — при стриме это мигало бы на каждой дельте');
  assert.match(root.textContent, /\| Задача \| Кому \|/);
});
