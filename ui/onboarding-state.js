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
    };

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

  function selectedPlan(selection, capabilities) {
    const value = selection || {};
    const ids = [];
    if (value.whisper) ids.push('whisper-turbo');
    if (value.qwen) ids.push(value.qwenSize === 'qwen3-1.7b' ? 'qwen3-1.7b' : 'qwen3-0.6b');
    if (value.wake) ids.push('hey_jarvis');
    if (value.silero) ids.push('silero');
    if (!Array.isArray(capabilities)) return ids;
    return ids.filter((id) => {
      const capability = capabilities.find((item) => item.id === (id.startsWith('qwen3-') ? 'qwen3-runtime' : id));
      return capability && capability.available !== false && !capability.ready;
    });
  }

  function mergeSnapshot(current, incoming) {
    if (!incoming || typeof incoming !== 'object') return current;
    const next = incoming.coreReady !== undefined ? incoming : { ...current, job: incoming };
    if (!current || !current.job || !next.job) return next;
    const previous = current.job;
    const candidate = next.job;
    const older = Number(candidate.id || 0) < Number(previous.id || 0);
    const regressed = Number(candidate.id || 0) === Number(previous.id || 0)
      && ['done', 'failed'].includes(previous.state) && candidate.state === 'running';
    return older || regressed ? { ...next, job: previous } : next;
  }

  function stepProgress(job) {
    const steps = Array.isArray(job?.steps) ? job.steps : [];
    // Percentages belong to one real installer stage, never to the whole job.
    const running = steps.filter((step) => ['start', 'info'].includes(step.state));
    const latest = running[running.length - 1] || steps[steps.length - 1] || null;
    const pct = ['start', 'info'].includes(latest?.state) && typeof latest?.pct === 'number' && Number.isFinite(latest.pct)
      ? Math.max(0, Math.min(100, latest.pct)) : null;
    return { latest, pct, steps, done: steps.filter((step) => step.state === 'done').length };
  }

  function navigation(screen, snapshot, failureDismissed = false, allowVoiceOnly = false) {
    const derived = derive(snapshot);
    if (!snapshot) return 'checking';
    if (derived.screen === 'installing') return 'installing';
    if (derived.screen === 'degraded' && !failureDismissed) return 'degraded';
    if (['capabilities', 'ready'].includes(screen) && !snapshot.coreReady && !allowVoiceOnly) return 'agents';
    return ['welcome', 'agents', 'capabilities', 'ready'].includes(screen) ? screen : 'agents';
  }

  function failureCopy(kind) {
    return {
      network: ['Загрузка остановилась', 'Проверь подключение к интернету. Если нужен прокси, укажи его и повтори установку.'],
      disk: ['Нужно больше места', 'Освободи место на диске и повтори установку. Скачанные и проверенные части останутся.'],
      permission: ['Нужен доступ', 'Проверь разрешения и сообщение ниже. После подтверждения можно повторить проверку.'],
      hooks: ['Не удалось подключить агента', 'Повтори настройку интеграции. Jarvis изменяет только собственные подключения.'],
      unknown: ['Подготовка остановилась', 'Уже готовые возможности сохранятся. Причину можно посмотреть ниже и повторить шаг.'],
    }[kind] || ['Не получен ответ', 'Проверь состояние ещё раз. Если установка уже началась, она продолжится в фоне.'];
  }

  return Object.freeze({ derive, selectedPlan, classifyFailure, mergeSnapshot, stepProgress, navigation, failureCopy });
});
