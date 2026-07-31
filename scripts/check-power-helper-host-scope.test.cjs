"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { spawnSync } = require("node:child_process");
const test = require("node:test");

const {
  changedScopeWarnings,
  hostRelative,
} = require("./check-power-helper-host-scope.cjs");

function compilerWarning(fileName, message) {
  return JSON.stringify({
    reason: "compiler-message",
    message: {
      level: "warning",
      message,
      rendered: `${message}\n`,
      spans: [{ file_name: fileName, is_primary: true }],
    },
  });
}

test("hostRelative handles absolute, host-prefixed, and relative Cargo spans", () => {
  assert.equal(
    hostRelative(
      "/private/tmp/repository/src-tauri/src/power/helper/client.rs",
    ),
    "src/power/helper/client.rs",
  );
  assert.equal(
    hostRelative("src-tauri/src/power/helper/dev_uds.rs"),
    "src/power/helper/dev_uds.rs",
  );
  assert.equal(
    hostRelative("src/power/helper/client.rs"),
    "src/power/helper/client.rs",
  );
  assert.equal(
    hostRelative(
      String.raw`C:\repository\src-tauri\src\power\helper\client.rs`,
    ),
    "src/power/helper/client.rs",
  );
});

test("changed-scope warnings fail while unrelated host warnings are allowed", () => {
  const fixture = [
    compilerWarning(
      "/private/tmp/repository/src-tauri/src/power/helper/client.rs",
      "absolute changed-scope warning",
    ),
    compilerWarning(
      "src/power/helper/dev_uds.rs",
      "relative changed-scope warning",
    ),
    compilerWarning(
      "/private/tmp/repository/src-tauri/src/stt/hub.rs",
      "unrelated warning",
    ),
  ].join("\n");

  assert.deepEqual(changedScopeWarnings(fixture), [
    "absolute changed-scope warning\n",
    "relative changed-scope warning\n",
  ]);
  assert.deepEqual(
    changedScopeWarnings(
      compilerWarning("src/config_health.rs", "known unrelated warning"),
    ),
    [],
  );
});

test("parser executable rejects changed scope and accepts unrelated fixture", () => {
  const temp = fs.mkdtempSync(path.join(os.tmpdir(), "jarvis-power-guard-"));
  try {
    const fixturePath = path.join(temp, "diagnostics.jsonl");
    fs.writeFileSync(
      fixturePath,
      compilerWarning(
        "/private/tmp/repository/src-tauri/src/power/helper/client.rs",
        "must fail",
      ),
    );
    const rejected = spawnSync(
      process.execPath,
      [require.resolve("./check-power-helper-host-scope.cjs"), fixturePath],
      { encoding: "utf8" },
    );
    assert.equal(rejected.status, 1);
    assert.match(rejected.stderr, /must fail/);

    fs.writeFileSync(
      fixturePath,
      compilerWarning(
        "/private/tmp/repository/src-tauri/src/stt/hub.rs",
        "allowed baseline",
      ),
    );
    const accepted = spawnSync(
      process.execPath,
      [require.resolve("./check-power-helper-host-scope.cjs"), fixturePath],
      { encoding: "utf8" },
    );
    assert.equal(accepted.status, 0);
    assert.match(accepted.stdout, /changed-scope clippy: clean/);
  } finally {
    fs.rmSync(temp, { recursive: true, force: true });
  }
});
