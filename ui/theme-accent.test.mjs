import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = readFileSync(new URL("./theme.js", import.meta.url), "utf8");

/** Запустить theme.js на минимальном DOM-стабе и вернуть выставленные токены. */
function applyAppearance(appearance) {
  const vars = new Map();
  const attrs = new Map();
  const root = {
    style: {
      setProperty: (key, value) => vars.set(key, value),
      removeProperty: (key) => vars.delete(key),
    },
    setAttribute: (key, value) => attrs.set(key, value),
    removeAttribute: (key) => attrs.delete(key),
    getAttribute: (key) => attrs.get(key) ?? null,
    classList: { add() {}, remove() {}, toggle() {} },
  };
  const context = {
    document: {
      documentElement: root,
      addEventListener() {},
      readyState: "complete",
    },
    localStorage: {
      getItem: () => JSON.stringify(appearance),
      setItem() {},
    },
    matchMedia: () => ({
      matches: false,
      addEventListener() {},
      addListener() {},
    }),
    setInterval: () => 0,
    clearInterval() {},
    setTimeout: () => 0,
    JSON,
    Math,
    Object,
    Number,
    String,
    Array,
  };
  context.window = context;
  vm.runInNewContext(source, context, { filename: "theme.js" });
  return vars;
}

const parseHex = (value) => ({
  r: parseInt(value.slice(1, 3), 16),
  g: parseInt(value.slice(3, 5), 16),
  b: parseInt(value.slice(5, 7), 16),
});

/** Насыщенность HSL: 0 — чистый серый. */
function saturation({ r, g, b }) {
  const max = Math.max(r, g, b) / 255;
  const min = Math.min(r, g, b) / 255;
  if (max === min) return 0;
  const l = (max + min) / 2;
  return (max - min) / (l > 0.5 ? 2 - max - min : max + min);
}

test("своя краска чёрного тона не оставляет интерфейс бесцветным", () => {
  // Настоящее сочетание из профиля владельца: чёрный акцент на тёмной теме
  // раньше вырождался в серый #616161 — различить состояния было нечем.
  const vars = applyAppearance({
    theme: "midnight",
    paint: "custom",
    accent: "#000000",
  });

  const accent = vars.get("--accent");
  assert.ok(accent, "--accent должен быть выставлен");
  assert.ok(
    saturation(parseHex(accent)) >= 0.27,
    `акцент ${accent} остался серым — интерфейс снова бесцветный`,
  );
});

test("чистый серый и белый тона тоже получают цвет, не выворачиваясь", () => {
  for (const base of ["#888888", "#ffffff", "#1b1a16"]) {
    const vars = applyAppearance({
      theme: "midnight",
      paint: "custom",
      accent: base,
    });
    const accent = parseHex(vars.get("--accent"));
    assert.ok(
      saturation(accent) >= 0.27,
      `${base}: акцент остался серым`,
    );
    // Клампинг светлоты не должен выворачивать тон в едкий цвет.
    const channels = [accent.r, accent.g, accent.b];
    assert.ok(
      Math.max(...channels) <= 255 && Math.min(...channels) >= 0,
      `${base}: канал вышел за границы`,
    );
  }
});

test("насыщенный тон проходит без изменений", () => {
  // Правка чинит только серость — выбранный цвет искажать нельзя.
  const vars = applyAppearance({
    theme: "light",
    paint: "custom",
    accent: "#c0103f",
  });

  assert.equal(vars.get("--accent").toLowerCase(), "#c0103f");
});
