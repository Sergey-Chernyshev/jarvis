import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

async function dynamicStyleInjector(relativePath) {
  const source = await readFile(new URL(relativePath, import.meta.url), "utf8");
  const start = source.indexOf("function injectStyle()");
  assert.notEqual(start, -1, `${relativePath} has a dynamic style injector`);
  const end = source.indexOf("document.head.appendChild(style);", start);
  assert.notEqual(end, -1, `${relativePath} appends its dynamic style`);
  return source.slice(start, end + "document.head.appendChild(style);".length);
}

for (const relativePath of ["./settings2.js", "./voice-history.js"]) {
  test(`${relativePath} copies Tauri's CSP nonce before appending dynamic CSS`, async () => {
    const injector = await dynamicStyleInjector(relativePath);
    const nonceLookup = injector.indexOf(
      "document.querySelector('style[nonce]')",
    );
    const nonceCopy = injector.indexOf("style.nonce = nonceSource.nonce");
    const append = injector.indexOf("document.head.appendChild(style);");

    assert.notEqual(nonceLookup, -1, "finds the static Tauri style nonce");
    assert.notEqual(nonceCopy, -1, "copies the nonce to the dynamic style");
    assert.ok(nonceLookup < nonceCopy, "reads the nonce before copying it");
    assert.ok(
      nonceCopy < append,
      "copies the nonce before appending the style",
    );
  });
}
