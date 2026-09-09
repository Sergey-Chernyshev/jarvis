import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
import test from "node:test";

const packageDir = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const repoRoot = path.dirname(path.dirname(packageDir));
const fixturePath = path.join(
  repoRoot,
  "crates/jarvis-plugin-protocol/tests/fixtures/ui-contracts-v1.json",
);

test("generated contracts type-check public wire fixtures", () => {
  const result = spawnSync(
    path.join(repoRoot, "node_modules/.bin/tsc"),
    ["--noEmit", "-p", path.join(packageDir, "tsconfig.contracts.json")],
    { cwd: repoRoot, encoding: "utf8" },
  );

  assert.equal(result.status, 0, `${result.stdout}${result.stderr}`);
});

test("Rust and Node consume the same exact bridge golden frames", async () => {
  const fixture = JSON.parse(await readFile(fixturePath, "utf8"));

  assert.deepEqual(
    [
      fixture.bridgeRequest.type,
      fixture.bridgeWelcome.type,
      fixture.bridgeError.type,
    ],
    ["request", "welcome", "error"],
  );
  assert.deepEqual(Object.keys(fixture.bridgeError).sort(), [
    "code",
    "correlationId",
    "generation",
    "id",
    "type",
    "v",
  ]);
  assert.equal("pluginId" in fixture.bridgeRequest, false);
  assert.equal(fixture.bridgeError.code, "grant_scope_denied");
  assert.equal(fixture.bridgeError.correlationId, "correlation/01");
});
