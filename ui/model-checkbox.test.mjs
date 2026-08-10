import assert from "node:assert/strict";
import { createRequire } from "node:module";
import test from "node:test";

const require = createRequire(import.meta.url);
let ModelCheckbox = null;
try {
  ModelCheckbox = require("./model-checkbox.js");
} catch {
  // RED state: the shared control does not exist yet.
}

class FakeNode {
  constructor(tagName) {
    this.tagName = tagName;
    this.attributes = {};
    this.children = [];
    this.listeners = {};
    this.checked = false;
    this.className = "";
    this.type = "";
  }

  append(...children) {
    this.children.push(...children);
  }

  setAttribute(name, value) {
    this.attributes[name] = String(value);
  }

  addEventListener(name, callback) {
    this.listeners[name] = callback;
  }

  dispatch(name) {
    this.listeners[name]?.({ target: this });
  }
}

const fakeDocument = {
  createElement: (tagName) => new FakeNode(tagName),
};

test("model checkbox keeps native semantics behind a custom visual and label hit area", () => {
  assert.ok(ModelCheckbox, "shared model checkbox control is available");
  const changes = [];
  const control = ModelCheckbox.create(fakeDocument, {
    label: "Выбрать Whisper",
    checked: true,
    onChange: (checked) => changes.push(checked),
  });

  assert.equal(control.node.tagName, "label");
  assert.equal(control.node.className, "model-check");
  assert.equal(control.input.tagName, "input");
  assert.equal(control.input.type, "checkbox");
  assert.equal(control.input.checked, true);
  assert.equal(control.input.attributes["aria-label"], "Выбрать Whisper");
  assert.equal(control.input.className, "model-check-input");
  assert.equal(control.mark.className, "model-check-mark");
  assert.equal(control.mark.attributes["aria-hidden"], "true");
  assert.deepEqual(control.node.children, [control.input, control.mark]);

  control.input.checked = false;
  control.input.dispatch("change");
  assert.deepEqual(changes, [false]);
});
