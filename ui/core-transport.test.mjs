import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

import { parseClassicExternalScripts } from "../scripts/check-tauri-acl.mjs";

const root = new URL("../", import.meta.url);
const expectedCsp =
  "default-src 'self'; object-src 'none'; base-uri 'none'; frame-src 'none'; img-src 'self' data:; font-src 'self'; style-src 'self' 'unsafe-inline'; script-src 'self'; connect-src 'self' ipc: http://ipc.localhost";
const trustedDocuments = new Map([
  ["ui/index.html", "./bridge.js"],
  ["ui/toast.html", "./toast-bridge.js"],
  ["ui/onboarding.html", "onboarding.js"],
  ["ui/agent-chat.html", "agent-chat.js"],
]);

async function source(relative) {
  return readFile(new URL(relative, root), "utf8");
}

function scriptSources(html) {
  return parseClassicExternalScripts(html, "core transport test").map(
    (script) => script.src,
  );
}

function trustedHtml(body) {
  return `<!doctype html><html><head><title>fixture</title></head><body>${body}</body></html>`;
}

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function rendererHarness(refresh) {
  const events = [];
  const handlers = new Map();
  const elements = new Map();
  const selectorElements = new Map();
  const pending = new Promise(() => {});

  function element(id = "") {
    const children = [];
    let textContent = "";
    const value = {
      id,
      children,
      dataset: {},
      style: {},
      hidden: false,
      value: "",
      checked: false,
      disabled: false,
      className: "",
      tagName: "DIV",
      selectionStart: 0,
      selectionEnd: 0,
      classList: {
        add(name) {
          if (id === "panel") events.push(`animation:add:${name}`);
        },
        remove(name) {
          if (id === "panel") events.push(`animation:remove:${name}`);
        },
        toggle() {},
        contains() {
          return false;
        },
      },
      addEventListener() {},
      removeEventListener() {},
      append(...nodes) {
        children.push(...nodes);
      },
      appendChild(node) {
        children.push(node);
        return node;
      },
      remove() {},
      replaceChildren(...nodes) {
        children.splice(0, children.length, ...nodes);
      },
      querySelector() {
        return null;
      },
      querySelectorAll() {
        return [];
      },
      setAttribute() {},
      removeAttribute() {},
      focus() {},
      blur() {},
      click() {},
      scrollBy() {},
      scrollTo() {},
      setSelectionRange() {},
      closest() {
        return null;
      },
      contains() {
        return false;
      },
      getBoundingClientRect() {
        return { width: 0, height: 0, top: 0, right: 0, bottom: 0, left: 0 };
      },
    };
    Object.defineProperties(value, {
      textContent: {
        get() {
          return textContent;
        },
        set(next) {
          textContent = next;
          if (id === "list") events.push("render:list");
        },
      },
      offsetWidth: {
        get() {
          if (id === "panel") events.push("animation:reflow");
          return 1;
        },
      },
      scrollHeight: {
        get() {
          return 0;
        },
      },
    });
    return value;
  }

  const document = {
    activeElement: null,
    // <html> несёт data-mode/data-theme: windowMode() и светофор читают его
    documentElement: element("html"),
    body: element("body"),
    getElementById(id) {
      if (!elements.has(id)) elements.set(id, element(id));
      return elements.get(id);
    },
    querySelector(selector) {
      if (selector === ".toast") return null;
      if (!selectorElements.has(selector)) {
        selectorElements.set(selector, element(selector));
      }
      return selectorElements.get(selector);
    },
    querySelectorAll() {
      return [];
    },
    createElement() {
      return element();
    },
    createElementNS() {
      return element();
    },
    createTextNode(text) {
      return { textContent: text };
    },
    // светофор оконного режима спрашивает фокус на старте
    hasFocus() {
      return true;
    },
    addEventListener() {},
    removeEventListener() {},
  };

  const jarvis = new Proxy(
    {},
    {
      get(_target, name) {
        if (typeof name === "string" && name.startsWith("on")) {
          return (callback) => {
            handlers.set(name, callback);
            return Promise.resolve(() => {});
          };
        }
        return () => pending;
      },
    },
  );

  const sandbox = {
    document,
    jarvis,
    console: { error() {}, warn() {}, log() {} },
    localStorage: {
      getItem() {
        return null;
      },
      setItem() {},
    },
    navigator: { clipboard: { writeText() {} } },
    setTimeout() {
      return 0;
    },
    clearTimeout() {},
    setInterval() {
      return 0;
    },
    clearInterval() {},
    addEventListener() {},
    removeEventListener() {},
    JarvisMarkdown: {
      isDocPath() {
        return false;
      },
      isMarkdownPath() {
        return false;
      },
      render() {
        return "";
      },
    },
    JarvisDiffView: { renderTo() {} },
    JarvisQuestionAnswer: {
      customAllowed() {
        return false;
      },
      normalizeText(value) {
        return value;
      },
    },
    JarvisAgentVm: {
      activeEnvironments() {
        return [];
      },
    },
    JarvisStateSync: {
      create(options) {
        return Object.freeze({
          start() {
            events.push("sync:start");
            return Promise.resolve();
          },
          refresh() {
            events.push("sync:refresh");
            return refresh.promise;
          },
          options,
        });
      },
    },
  };
  sandbox.window = sandbox;
  return { sandbox, document, elements, events, handlers };
}

test("bridge initializes from the explicit core transport without touching window.__TAURI__", async () => {
  const invoked = [];
  const listened = [];
  const sandbox = {
    navigator: {},
    __JARVIS_CORE_TRANSPORT__: Object.freeze({
      invoke(command, payload) {
        invoked.push([command, payload]);
        return Promise.resolve(null);
      },
      listen(event, callback) {
        listened.push([event, callback]);
        return Promise.resolve(() => {});
      },
    }),
  };
  sandbox.window = sandbox;
  Object.defineProperty(sandbox, "__TAURI__", {
    configurable: true,
    get() {
      throw new Error("window.__TAURI__ must not be read");
    },
  });

  vm.runInNewContext(await source("ui/bridge.js"), sandbox, {
    filename: "ui/bridge.js",
  });

  assert.equal(typeof sandbox.jarvis.getState, "function");
  assert.equal(Object.isFrozen(sandbox.jarvis), true);
  assert.equal("__JARVIS_CORE_TRANSPORT__" in sandbox, false);
  await sandbox.jarvis.getState();
  assert.deepEqual(invoked, [["state_get", undefined]]);
  assert.deepEqual(listened, []);
});

test("bridge subscriptions expose registration promises and preserve event payload behavior", async () => {
  const listened = [];
  const stateRegistration = Promise.resolve(() => {});
  const shownRegistration = Promise.resolve(() => {});
  const registrations = new Map([
    ["state", stateRegistration],
    ["panel-shown", shownRegistration],
  ]);
  const sandbox = {
    navigator: {},
    __JARVIS_CORE_TRANSPORT__: Object.freeze({
      invoke() {
        return Promise.resolve(null);
      },
      listen(event, callback) {
        listened.push([event, callback]);
        return registrations.get(event);
      },
    }),
  };
  sandbox.window = sandbox;

  vm.runInNewContext(await source("ui/bridge.js"), sandbox, {
    filename: "ui/bridge.js",
  });

  const states = [];
  const shownCalls = [];
  const stateResult = sandbox.jarvis.onState((value) => states.push(value));
  const shownResult = sandbox.jarvis.onShown((...args) =>
    shownCalls.push(args),
  );
  assert.equal(stateResult, stateRegistration);
  assert.equal(shownResult, shownRegistration);

  listened[0][1]({ payload: ["live state"] });
  listened[1][1]({ payload: "not forwarded" });
  assert.deepEqual(states, [["live state"]]);
  assert.deepEqual(shownCalls, [[]]);
});

test("panel shown animates immediately, waits for state refresh, then reconciles and index wires the sync first", async () => {
  const refresh = deferred();
  const harness = rendererHarness(refresh);
  const context = vm.createContext(harness.sandbox);
  vm.runInContext(await source("ui/renderer.js"), context, {
    filename: "ui/renderer.js",
  });
  vm.runInContext("view = 'chat'; chatSessionId = 'missing';", context);
  harness.events.length = 0;

  const shown = harness.handlers.get("onShown");
  assert.equal(typeof shown, "function");
  const shownResult = shown();
  assert.deepEqual(harness.events, [
    "animation:remove:entering",
    "animation:reflow",
    "animation:add:entering",
    "sync:refresh",
  ]);
  assert.equal(
    vm.runInContext("view", context),
    "chat",
    "stale-session reconciliation waits for the refresh",
  );
  assert.equal(
    harness.events.includes("render:list"),
    false,
    "the hidden stale state is not rendered before refresh",
  );

  harness.events.push("refresh:resolved");
  refresh.resolve(true);
  await shownResult;
  assert.equal(vm.runInContext("view", context), "list");
  assert.ok(
    harness.events.indexOf("render:list") >
      harness.events.indexOf("refresh:resolved"),
    "the refreshed state is reconciled and rendered after refresh",
  );

  const failedRefresh = deferred();
  const failedHarness = rendererHarness(failedRefresh);
  const failedContext = vm.createContext(failedHarness.sandbox);
  vm.runInContext(await source("ui/renderer.js"), failedContext, {
    filename: "ui/renderer.js",
  });
  vm.runInContext("view = 'chat'; chatSessionId = 'missing';", failedContext);
  failedHarness.events.length = 0;

  const failedShown = failedHarness.handlers.get("onShown")();
  failedRefresh.resolve(false);
  await failedShown;
  assert.equal(
    vm.runInContext("view", failedContext),
    "chat",
    "a failed or superseded refresh cannot evict the open chat",
  );
  assert.equal(failedHarness.events.includes("render:list"), false);

  const scripts = scriptSources(await source("ui/index.html"));
  const syncIndex = scripts.indexOf("./state-sync.js");
  const rendererIndex = scripts.indexOf("./renderer.js");
  assert.notEqual(syncIndex, -1, "index loads the state sync");
  assert.equal(
    syncIndex + 1,
    rendererIndex,
    "state sync initializes immediately before renderer",
  );
});

test("all trusted documents load one transport immediately before its consumer", async () => {
  for (const [document, consumer] of trustedDocuments) {
    const html = await source(document);
    const scripts = scriptSources(html);
    assert.equal(
      scripts.filter((src) => src === "./generated/tauri-transport.js").length,
      1,
      `${document} loads the core transport exactly once`,
    );
    assert.equal(
      scripts.filter((src) => src === consumer).length,
      1,
      `${document} loads ${consumer} exactly once`,
    );
    const transportIndex = scripts.indexOf("./generated/tauri-transport.js");
    assert.equal(
      scripts[transportIndex + 1],
      consumer,
      `${document} consumes and deletes the transport without an intervening script`,
    );
    if (document === "ui/onboarding.html") {
      assert.equal(
        scripts[transportIndex - 1],
        "onboarding-state.js",
        "onboarding state initializes before the transport bootstrap",
      );
    }
  }
});

test("script scan preserves Unicode indices while folding ASCII tag names only", () => {
  const fixtures = [
    [trustedHtml('İ<script src="./first.js"></script>'), ["./first.js"]],
    [
      trustedHtml(
        '<script src="./first.js"></script>İ<script src="./second.js"></script>',
      ),
      ["./first.js", "./second.js"],
    ],
    [
      trustedHtml(
        'Привет🙂<ScRiPt SRC = "./first.js"></sCrIpT>世界<script src="./second.js"></script>',
      ),
      ["./first.js", "./second.js"],
    ],
  ];

  for (const [html, expected] of fixtures) {
    assert.deepEqual(
      parseClassicExternalScripts(html, "unicode fixture").map(
        (script) => script.src,
      ),
      expected,
    );
  }

  assert.deepEqual(
    parseClassicExternalScripts(
      trustedHtml('<ѕcript src="./lookalike.js"></ѕcript>'),
      "unicode lookalike",
    ),
    [],
  );
  assert.throws(
    () =>
      parseClassicExternalScripts(
        trustedHtml('İ<ScRiPt src="./unsafe.js" AsYnC="false"></sCrIpT>'),
        "unicode unsafe attribute",
      ),
    (error) =>
      error.code === "tauri_acl_script_tag_invalid" &&
      error.message.includes(
        "script must have exactly one non-empty src attribute",
      ),
  );
});

test("transport and consumer reject execution-changing or malformed attributes", () => {
  const transport = "./generated/tauri-transport.js";
  const consumer = "./bridge.js";
  const body = `<script src="${transport}"></script>
<script src="${consumer}"></script>`;
  const base = trustedHtml(body);
  const attributes = [
    " async",
    ' AsYnC="false"',
    " defer",
    " DeFeR='defer'",
    ' type="module"',
    " TYPE='text/javascript'",
    " nomodule",
    ' NoMoDuLe="false"',
    ' integrity="sha256-test"',
  ];

  for (const src of [transport, consumer]) {
    for (const attribute of attributes) {
      const unsafe = base.replace(
        `<script src="${src}">`,
        `<script src="${src}"${attribute}>`,
      );
      assert.throws(
        () => parseClassicExternalScripts(unsafe, src),
        (error) => error.code === "tauri_acl_script_tag_invalid",
        `${src}${attribute}`,
      );
    }

    for (const tag of [
      `<script src="${src}" SRC="${src}">`,
      `<script src=${src}>`,
      "<script src>",
      `<script src="${src}>`,
    ]) {
      const malformed = base.replace(`<script src="${src}">`, tag);
      assert.throws(
        () => parseClassicExternalScripts(malformed, src),
        (error) => error.code === "tauri_acl_script_tag_invalid",
        tag,
      );
    }
  }

  const onboardingState =
    trustedHtml(`<script src="onboarding-state.js" defer></script>
<script src="${transport}"></script>
<script src="onboarding.js"></script>`);
  assert.throws(
    () => parseClassicExternalScripts(onboardingState, "onboarding-state"),
    (error) => error.code === "tauri_acl_script_tag_invalid",
  );
});

test("trusted scripts cannot hide in inert, raw-text, comment, or foreign contexts", () => {
  const pair =
    '<script src="./generated/tauri-transport.js"></script><script src="./bridge.js"></script>';
  const wrappers = [
    `<textarea>${pair}</textarea>`,
    `<template>${pair}</template>`,
    `<title>${pair}</title>`,
    `<style>${pair}</style>`,
    `<xmp>${pair}</xmp>`,
    `<iframe>${pair}</iframe>`,
    `<noframes>${pair}</noframes>`,
    `<noscript>${pair}</noscript>`,
    `<plaintext>${pair}`,
    `<!--${pair}-->`,
    `<svg>${pair}</svg>`,
    `<div>${pair}</div>`,
  ];

  for (const wrapped of wrappers) {
    assert.throws(
      () => parseClassicExternalScripts(trustedHtml(wrapped), wrapped),
      (error) => error.code === "tauri_acl_script_tag_invalid",
      wrapped,
    );
  }
});

test("malformed trusted HTML fails closed before script ordering", () => {
  for (const malformed of [
    '<html><body><script src="./x.js"></script></body></html>',
    '<!doctype html><html><body><script src="./x.js"></body></html>',
    '<!doctype html><html><body><script src="./x.js"/></body></html>',
    '<!doctype html><html><body><script src="./x.js"></script>',
  ]) {
    assert.throws(
      () => parseClassicExternalScripts(malformed, "malformed fixture"),
      (error) => error.code === "tauri_acl_script_tag_invalid",
    );
  }
});

test("all trusted documents use the same explicit IPC-capable CSP", async () => {
  for (const document of trustedDocuments.keys()) {
    const html = await source(document);
    assert.equal(
      html.match(/http-equiv="Content-Security-Policy"/g)?.length,
      1,
      `${document} has exactly one CSP`,
    );
    assert.match(
      html,
      new RegExp(
        `<meta http-equiv="Content-Security-Policy" content="${expectedCsp.replaceAll(
          /[.*+?^${}()|[\]\\]/g,
          "\\$&",
        )}"\\s*/?>`,
      ),
      document,
    );
  }
});

test("trusted documents do not load remote scripts or assets", async () => {
  const remoteAsset =
    /<(?:script|link|img|iframe|source|audio|video)\b[^>]*\b(?:src|href|srcset)\s*=\s*(?:"(?:https?:)?\/\/|'(?:https?:)?\/\/)/i;
  for (const document of trustedDocuments.keys()) {
    assert.doesNotMatch(await source(document), remoteAsset, document);
  }
});

test("production UI contains no global Tauri fallback", async () => {
  for (const file of [
    "ui/bridge.js",
    "ui/toast-bridge.js",
    "ui/onboarding.js",
    "ui/agent-chat.js",
  ]) {
    assert.doesNotMatch(await source(file), /__TAURI__/, file);
  }
});

test("generated transport is committed without an inline sourcemap", async () => {
  const generated = await source("ui/generated/tauri-transport.js");
  assert.match(generated, /__JARVIS_CORE_TRANSPORT__/);
  assert.match(generated, /Object\.freeze/);
  assert.doesNotMatch(generated, /sourceMappingURL/);
});
