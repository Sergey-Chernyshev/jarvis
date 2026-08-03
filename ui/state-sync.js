(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.JarvisStateSync = api;
})(typeof globalThis !== 'undefined' ? globalThis : this, function () {
  'use strict';

  function create({ subscribe, read, apply, onError = () => {} }) {
    let pushEpoch = 0;
    let requestEpoch = 0;

    function report(error) {
      try {
        const result = onError(error);
        if (result && typeof result.then === 'function') {
          Promise.resolve(result).catch(() => {});
        }
      } catch {}
    }

    function applyNow(value) {
      try {
        const result = apply(value);
        if (result && typeof result.then === 'function') {
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

  return Object.freeze({ create });
});
