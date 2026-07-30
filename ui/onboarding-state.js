(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.JarvisOnboardingState = api;
})(typeof globalThis !== 'undefined' ? globalThis : this, function () {
  'use strict';

  function classifyFailure(failures) {
    const text = (failures || []).join(' ').toLowerCase();
    if (/network|proxy|timeout|timed out|http|dns|соедин|сеть/.test(text)) return 'network';
    if (/no space|disk|device full|readonly|read-only|места на диске/.test(text)) return 'disk';
    if (/permission|denied|trust|access|доступ|прав/.test(text)) return 'permission';
    if (/\bhome\b|\bpath\b|environment|окружен/.test(text)) return 'environment';
    if (/hook/.test(text)) return 'hooks';
    return 'unknown';
  }

  function derive(snapshot) {
    if (!snapshot) {
      return {
        snapshot: null,
        screen: 'checking',
        primaryAction: 'wait',
        readyCapabilities: 0,
        totalCapabilities: 0,
        failures: [],
        failureKind: null,
        runtimeState: 'checking',
      };
    }
    const data = snapshot;
    const job = data.job || { state: 'idle', failures: [] };
    const capabilities = Array.isArray(data.capabilities) ? data.capabilities : [];
    const agents = Array.isArray(data.agents) ? data.agents : [];
    const configHealth = data.configHealth || { status: 'healthy', issues: [] };
    const socket = (Array.isArray(data.transport) ? data.transport : [])
      .find((item) => item && item.id === 'socket');
    const readyCapabilities = capabilities.filter((item) => item && item.ready).length;
    const base = {
      snapshot: data,
      readyCapabilities,
      totalCapabilities: capabilities.length,
      failures: Array.isArray(job.failures) ? job.failures.slice() : [],
      failureKind: null,
      runtimeState: data.coreReady ? (socket && socket.ready ? 'online' : 'warming') : 'offline',
      readyAgents: agents.filter((item) => item && item.ready).length,
      totalAgents: agents.filter((item) => item && item.available !== false).length,
      configHealth,
      configIssues: Array.isArray(configHealth.issues) ? configHealth.issues.slice() : [],
    };

    if (configHealth.status === 'error') {
      return { ...base, screen: 'config', primaryAction: 'repair-config' };
    }
    if (job.state === 'running') {
      return { ...base, screen: 'installing', primaryAction: 'wait' };
    }
    if (job.state === 'failed') {
      return {
        ...base,
        screen: 'degraded',
        primaryAction: 'retry',
        failureKind: classifyFailure(base.failures),
      };
    }
    if (!data.coreReady) {
      return { ...base, screen: 'agents', primaryAction: 'repair' };
    }
    return { ...base, screen: 'capabilities', primaryAction: 'continue' };
  }

  function selectedPlan(selection) {
    const value = selection || {};
    const ids = [];
    if (value.whisper) ids.push('whisper-turbo');
    if (value.qwen) ids.push(value.qwenSize === 'qwen3-1.7b' ? 'qwen3-1.7b' : 'qwen3-0.6b');
    if (value.wake) ids.push('hey_jarvis');
    if (value.silero) ids.push('silero');
    return ids;
  }

  return Object.freeze({ derive, selectedPlan, classifyFailure });
});
