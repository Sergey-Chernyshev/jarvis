import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const stateSyncUrl = new URL("./state-sync.js", import.meta.url);

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

async function flushMicrotasks() {
  await Promise.resolve();
  await Promise.resolve();
}

async function loadStateSync() {
  const sandbox = {};
  vm.runInNewContext(await readFile(stateSyncUrl, "utf8"), sandbox, {
    filename: "ui/state-sync.js",
  });
  return sandbox.JarvisStateSync;
}

test("session normalization rejects invalid envelopes and deduplicates by newest update", async () => {
  const JarvisStateSync = await loadStateSync();

  assert.equal(JarvisStateSync.normalizeSessions(null), null);
  assert.equal(JarvisStateSync.normalizeSessions({ sessions: [] }), null);

  const normalized = JarvisStateSync.normalizeSessions([
    { id: "", project: "broken" },
    {
      id: "alpha",
      project: "old",
      updatedAt: 10,
      status: "working",
      agent: "Codex",
    },
    {
      id: "alpha",
      project: "new",
      updatedAt: 20,
      status: "waiting",
      agent: "CODEX",
    },
    {
      id: "beta",
      project: 42,
      updatedAt: "bad",
      status: "future-status",
      agent: "other",
    },
  ]);

  assert.deepEqual(JSON.parse(JSON.stringify(normalized)), [
    {
      id: "alpha",
      project: "new",
      updatedAt: 20,
      status: "waiting",
      agent: "codex",
    },
    { id: "beta", updatedAt: 0, status: "idle" },
  ]);
});

test("initial snapshot waits for listener registration to settle", async () => {
  const JarvisStateSync = await loadStateSync();
  const listener = deferred();
  const events = [];
  const sync = JarvisStateSync.create({
    subscribe() {
      events.push("subscribe");
      return listener.promise;
    },
    async read() {
      events.push("read");
      return ["snapshot"];
    },
    apply(value) {
      events.push(["apply", value]);
    },
  });

  assert.equal(Object.isFrozen(JarvisStateSync), true);
  assert.equal(Object.isFrozen(sync), true);
  assert.deepEqual(events, ["subscribe"], "subscription starts during create");

  const started = sync.start();
  await flushMicrotasks();
  assert.deepEqual(
    events,
    ["subscribe"],
    "read is gated by listener settlement",
  );

  listener.resolve(() => {});
  await started;
  assert.deepEqual(events, ["subscribe", "read", ["apply", ["snapshot"]]]);
});

test("push before listener readiness applies immediately and wins over initial snapshot", async () => {
  const JarvisStateSync = await loadStateSync();
  const listenerReady = deferred();
  const applied = [];
  let push;
  const sync = JarvisStateSync.create({
    subscribe(callback) {
      push = callback;
      return listenerReady.promise;
    },
    async read() {
      return ["stale snapshot"];
    },
    apply(value) {
      applied.push(value);
    },
  });

  const started = sync.start();
  push(["live push"]);
  assert.deepEqual(applied, [["live push"]]);

  listenerReady.resolve(() => {});
  await started;
  assert.deepEqual(applied, [["live push"]]);
});

test("push while a read is in flight applies immediately and makes its snapshot stale", async () => {
  const JarvisStateSync = await loadStateSync();
  const snapshot = deferred();
  const readStarted = deferred();
  const applied = [];
  let push;
  const sync = JarvisStateSync.create({
    subscribe(callback) {
      push = callback;
      return Promise.resolve(() => {});
    },
    read() {
      readStarted.resolve();
      return snapshot.promise;
    },
    apply(value) {
      applied.push(value);
    },
  });

  const started = sync.start();
  await readStarted.promise;

  push(["live push"]);
  assert.deepEqual(applied, [["live push"]]);
  snapshot.resolve(["stale snapshot"]);
  assert.equal(await started, true);
  assert.deepEqual(applied, [["live push"]]);
});

test("a newer refresh wins when refresh responses resolve in reverse order", async () => {
  const JarvisStateSync = await loadStateSync();
  const reads = [deferred(), deferred()];
  const readStarted = [deferred(), deferred()];
  const applied = [];
  let readIndex = 0;
  const sync = JarvisStateSync.create({
    subscribe() {
      return Promise.resolve(() => {});
    },
    read() {
      const index = readIndex++;
      readStarted[index].resolve();
      return reads[index].promise;
    },
    apply(value) {
      applied.push(value);
    },
  });

  const older = sync.refresh();
  await readStarted[0].promise;
  const newer = sync.refresh();
  await readStarted[1].promise;
  assert.equal(readIndex, 2);

  reads[1].resolve(["newer"]);
  assert.equal(await newer, true);
  reads[0].resolve(["older"]);
  assert.equal(await older, false);
  assert.deepEqual(applied, [["newer"]]);
});

test("listener rejection is reported and falls back to the snapshot", async () => {
  const JarvisStateSync = await loadStateSync();
  const listenerError = new Error("listener failed");
  const reported = [];
  const applied = [];
  const sync = JarvisStateSync.create({
    subscribe() {
      return Promise.reject(listenerError);
    },
    async read() {
      return ["snapshot"];
    },
    apply(value) {
      applied.push(value);
    },
    onError(error) {
      reported.push(error);
    },
  });

  await assert.doesNotReject(sync.start());
  assert.deepEqual(reported, [listenerError]);
  assert.deepEqual(applied, [["snapshot"]]);
});

test("read rejection preserves a push and a later refresh can retry", async () => {
  const JarvisStateSync = await loadStateSync();
  const firstRead = deferred();
  const firstReadStarted = deferred();
  const readError = new Error("read failed");
  const reported = [];
  const applied = [];
  let push;
  let readCount = 0;
  const sync = JarvisStateSync.create({
    subscribe(callback) {
      push = callback;
      return Promise.resolve(() => {});
    },
    read() {
      readCount += 1;
      if (readCount === 1) firstReadStarted.resolve();
      return readCount === 1 ? firstRead.promise : Promise.resolve(["retry"]);
    },
    apply(value) {
      applied.push(value);
    },
    onError(error) {
      reported.push(error);
    },
  });

  const started = sync.start();
  await firstReadStarted.promise;
  push(["live push"]);
  firstRead.reject(readError);
  assert.equal(await started, false);
  assert.deepEqual(applied, [["live push"]]);
  assert.deepEqual(reported, [readError]);

  assert.equal(await sync.refresh(), true);
  assert.deepEqual(applied, [["live push"], ["retry"]]);
  assert.equal(readCount, 2);
});
