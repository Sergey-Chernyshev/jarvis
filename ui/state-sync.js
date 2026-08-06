(function (root, factory) {
  const api = factory();
  if (typeof module === "object" && module.exports) module.exports = api;
  else root.JarvisStateSync = api;
})(typeof globalThis !== "undefined" ? globalThis : this, function () {
  "use strict";

  const SESSION_STATUSES = new Set([
    "working",
    "waiting",
    "done",
    "idle",
    "limit",
  ]);
  const SESSION_AGENTS = new Set(["claude", "codex"]);
  const OPTIONAL_STRINGS = [
    "project",
    "cwd",
    "detail",
    "title",
    "task",
    "summary",
    "lastPrompt",
    "branch",
    "model",
    "tmuxPane",
    "tmuxName",
    "host",
    "app",
    "transcript",
  ];

  function normalizeSessions(value) {
    if (!Array.isArray(value)) return null;
    const sessions = new Map();
    for (const candidate of value) {
      if (
        !candidate ||
        typeof candidate !== "object" ||
        Array.isArray(candidate)
      )
        continue;
      const id = typeof candidate.id === "string" ? candidate.id.trim() : "";
      if (!id) continue;

      const session = { ...candidate, id };
      for (const key of OPTIONAL_STRINGS) {
        if (key in session && typeof session[key] !== "string")
          delete session[key];
      }
      session.updatedAt = Number.isFinite(session.updatedAt)
        ? session.updatedAt
        : 0;
      if (!SESSION_STATUSES.has(session.status)) session.status = "idle";
      if (typeof session.agent === "string")
        session.agent = session.agent.toLowerCase();
      if (!SESSION_AGENTS.has(session.agent)) delete session.agent;

      const previous = sessions.get(id);
      if (!previous || session.updatedAt >= previous.updatedAt)
        sessions.set(id, session);
    }
    return [...sessions.values()];
  }

  function create({ subscribe, read, apply, onError = () => {} }) {
    let pushEpoch = 0;
    let requestEpoch = 0;

    function report(error) {
      try {
        const result = onError(error);
        if (result && typeof result.then === "function") {
          Promise.resolve(result).catch(() => {});
        }
      } catch {}
    }

    function applyNow(value) {
      try {
        const result = apply(value);
        if (result && typeof result.then === "function") {
          Promise.resolve(result).catch(report);
        }
      } catch (error) {
        report(error);
      }
    }

    function onPush(value) {
      pushEpoch += 1;
      applyNow(value);
    }

    let listenerSettlement;
    try {
      listenerSettlement = Promise.resolve(subscribe(onPush)).catch(report);
    } catch (error) {
      report(error);
      listenerSettlement = Promise.resolve();
    }

    async function requestSnapshot() {
      const request = ++requestEpoch;
      const push = pushEpoch;
      await listenerSettlement;

      let snapshot;
      try {
        snapshot = await read();
      } catch (error) {
        report(error);
        return false;
      }

      if (request !== requestEpoch) return false;
      if (push !== pushEpoch) return true;
      applyNow(snapshot);
      return true;
    }

    return Object.freeze({
      start: requestSnapshot,
      refresh: requestSnapshot,
    });
  }

  return Object.freeze({ create, normalizeSessions });
});
