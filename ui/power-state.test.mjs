import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const renderer = readFileSync(
  new URL("./renderer.js", import.meta.url),
  "utf8",
);
const settings = readFileSync(
  new URL("./settings2.js", import.meta.url),
  "utf8",
);

test("disabled clamshell cleanup debt stays visible and has an exact-release retry", () => {
  for (const source of [renderer, settings]) {
    assert.match(source, /helperLease/);
    assert.match(source, /pendingCleanup/);
    assert.match(source, /renewalError/);
    assert.match(source, /retry-cleanup/);
    assert.match(source, /repairAction/);
  }
});

test("lid commands render backend truth instead of unconditional sleep claims", () => {
  assert.match(renderer, /clamshellResultMessage/);
  assert.doesNotMatch(
    renderer,
    /showToast\(m === 'keep' \? 'Крышка закрыта — не уснёт' : 'Крышка закрыта — обычный сон'\)/,
  );
});
