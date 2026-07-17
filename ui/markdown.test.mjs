import assert from 'node:assert/strict';
import test from 'node:test';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const MD = require('./markdown.js');

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
