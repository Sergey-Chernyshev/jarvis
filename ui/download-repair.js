(function (root, factory) {
  const api = factory();
  if (typeof module === "object" && module.exports) module.exports = api;
  else root.JarvisDownloadRepair = api;
})(typeof globalThis !== "undefined" ? globalThis : this, function () {
  "use strict";

  const TRANSPORT_FAILURE =
    /не удалось подключиться|не удалось соединиться|ошибка соединения|сеть недоступна|error sending request|dns(?: lookup)?(?: failed| error)?|connection (?:refused|reset|timed out)|connect(?:ion)? error|request timed out|timed out|timeout|network (?:is )?unreachable|proxy(?:\s+connect)?\s+(?:failed|error)/i;

  function actionFor(message) {
    return TRANSPORT_FAILURE.test(String(message || "")) ? "proxy" : null;
  }

  function idsForAction(errorsById, action) {
    if (!errorsById || !action) return [];
    return Object.keys(errorsById).filter((id) => {
      const entry = errorsById[id];
      return entry && actionFor(entry.error) === action;
    });
  }

  return Object.freeze({ actionFor, idsForAction });
});
