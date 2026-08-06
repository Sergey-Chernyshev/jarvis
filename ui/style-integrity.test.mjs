import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

// Незакрытый или закрытый раньше времени комментарий не ломает сборку и не
// портит баланс скобок — он просто съедает следующее правило, и вёрстка молча
// разъезжается. Именно так строка чата один раз потеряла `display: grid`:
// комментарий закрылся на середине, четыре строки прозы стали «CSS», и
// селектор после них браузер отбросил. Проверка дешёвая, ловит целый класс.
const FILES = ["./index.html", "./onboarding.html", "./toast.html"];

/** Вырезать содержимое всех <style> из HTML. */
async function styleSheets(relativePath) {
  const source = await readFile(new URL(relativePath, import.meta.url), "utf8");
  return [...source.matchAll(/<style[^>]*>([\s\S]*?)<\/style>/g)].map(
    (match) => match[1],
  );
}

/** Убрать комментарии, сообщив о непарных маркерах. */
function stripComments(css, where) {
  let out = "";
  let rest = css;
  for (;;) {
    const open = rest.indexOf("/*");
    if (open === -1) {
      out += rest;
      break;
    }
    out += rest.slice(0, open);
    const close = rest.indexOf("*/", open + 2);
    assert.notEqual(close, -1, `${where}: комментарий не закрыт`);
    rest = rest.slice(close + 2);
  }
  // Уцелевший «*/» — признак, что комментарий закрылся дважды: между первым и
  // вторым закрытием текст стоит как CSS и убивает следующее правило.
  assert.equal(
    out.includes("*/"),
    false,
    `${where}: лишний «*/» — комментарий закрыт дважды, текст между закрытиями браузер прочтёт как CSS`,
  );
  return out;
}

for (const relativePath of FILES) {
  test(`${relativePath}: комментарии в CSS закрыты ровно один раз`, async () => {
    const sheets = await styleSheets(relativePath);
    assert.notEqual(sheets.length, 0, `${relativePath}: нет <style>`);
    sheets.forEach((sheet, i) => stripComments(sheet, `${relativePath} #${i}`));
  });

  test(`${relativePath}: фигурные скобки сбалансированы`, async () => {
    for (const [i, sheet] of (await styleSheets(relativePath)).entries()) {
      const css = stripComments(sheet, `${relativePath} #${i}`);
      let depth = 0;
      for (const ch of css) {
        if (ch === "{") depth += 1;
        else if (ch === "}") {
          depth -= 1;
          assert.ok(depth >= 0, `${relativePath} #${i}: лишняя «}»`);
        }
      }
      assert.equal(depth, 0, `${relativePath} #${i}: незакрытый блок`);
    }
  });
}

// Строка чата — грид, и это не косметика: заголовку отдана вся ширина, а
// колонок ровно столько, сколько видимых элементов. Если правило снова
// потеряется (как от сломанного комментария), список сложится в столбик.
test("строка чата остаётся гридом с тремя колонками", async () => {
  const [sheet] = await styleSheets("./index.html");
  const css = stripComments(sheet, "index.html");
  const rule = css.slice(css.indexOf(".hrow.chat {"));
  const body = rule.slice(0, rule.indexOf("}"));
  assert.match(body, /display:\s*grid/, "строка чата должна быть гридом");
  assert.match(
    body,
    /grid-template-columns:\s*minmax\(0,\s*1fr\)\s+auto\s+auto\s*;/,
    "колонки: заголовок + агент + время. Лишняя колонка = невидимая подпись, съедающая ширину заголовка",
  );
});
