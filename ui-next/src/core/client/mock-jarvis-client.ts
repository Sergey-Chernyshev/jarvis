import type { JarvisClient } from './jarvis-client';
import type {
  Agent,
  AppSettings,
  AppSnapshot,
  Attachment,
  Paint,
  Theme,
  UsageSummary,
  VmState,
} from './types';
import { initialSnapshot } from '../mock/fixtures';

const clone = <T,>(value: T): T => structuredClone(value);
const delay = (ms = 180) => new Promise((resolve) => window.setTimeout(resolve, ms));

export class MockJarvisClient implements JarvisClient {
  private snapshot: AppSnapshot = clone(initialSnapshot);

  async getSnapshot(): Promise<AppSnapshot> {
    await delay(120);
    return clone(this.snapshot);
  }

  async updateSettings(patch: Partial<AppSettings>): Promise<AppSettings> {
    await delay();
    this.snapshot.settings = { ...this.snapshot.settings, ...patch };
    return clone(this.snapshot.settings);
  }

  setTheme(theme: Theme): Promise<AppSettings> {
    return this.updateSettings({ theme });
  }

  setPaint(paint: Paint): Promise<AppSettings> {
    return this.updateSettings({ paint });
  }

  async pinSession(sessionId: string, pinned: boolean): Promise<void> {
    await delay(80);
    const session = this.snapshot.sessions.find((item) => item.id === sessionId);
    if (session) session.pinned = pinned;
  }

  async clearFinishedSessions(): Promise<void> {
    await delay();
    this.snapshot.sessions = this.snapshot.sessions.filter(
      (session) => session.status !== 'done' && session.status !== 'interrupted',
    );
  }

  async sendMessage(
    sessionId: string,
    text: string,
    attachments: Attachment[],
  ): Promise<void> {
    await delay();
    const messages = this.snapshot.messages[sessionId] ?? [];
    messages.push({
      id: crypto.randomUUID(),
      role: 'user',
      text,
      attachments,
      timestamp: Date.now(),
    });
    this.snapshot.messages[sessionId] = messages;
    const session = this.snapshot.sessions.find((item) => item.id === sessionId);
    if (session) {
      session.status = 'working';
      session.detail = 'Агент отвечает…';
    }
  }

  async answerQuestion(
    sessionId: string,
    _answers: Record<string, string[]>,
  ): Promise<void> {
    await delay();
    const session = this.snapshot.sessions.find((item) => item.id === sessionId);
    if (session) {
      delete session.question;
      session.status = 'working';
      session.detail = 'Продолжает после ответа';
    }
  }

  async setSessionModel(sessionId: string, model: string): Promise<void> {
    await delay(100);
    const session = this.snapshot.sessions.find((item) => item.id === sessionId);
    if (session) session.model = model;
  }

  async setSessionEffort(
    sessionId: string,
    effort: 'low' | 'medium' | 'high' | 'max',
  ): Promise<void> {
    await delay(100);
    const session = this.snapshot.sessions.find((item) => item.id === sessionId);
    if (session) session.effort = effort;
  }

  async launchSession(
    projectId: string,
    agent: Agent,
    inVm: boolean,
  ): Promise<void> {
    await delay();
    const project = this.snapshot.projects.find((item) => item.id === projectId);
    if (project && inVm && project.vmState === 'stopped') {
      project.vmState = 'booting';
      const vm = this.snapshot.vms.find((item) => item.projectId === projectId);
      if (vm) vm.state = 'booting';
    }
    if (project) {
      project.chats.unshift({
        id: crypto.randomUUID(),
        title: `Новая сессия ${agent === 'claude' ? 'Claude' : 'Codex'}`,
        agent,
        when: 'сейчас',
        day: 'Сегодня',
        run: inVm ? 'vm-working' : 'terminal',
        ...(inVm ? { detail: 'машина запускается' } : {}),
      });
    }
  }

  async setVmState(vmId: string, state: VmState): Promise<void> {
    await delay(140);
    const vm = this.snapshot.vms.find((item) => item.id === vmId);
    if (vm) {
      vm.state = state;
      vm.busy = state === 'running';
      vm.ramUsed = state === 'running' ? Math.max(vm.ramUsed, 1.2) : 0;
    }
    const project = vm
      ? this.snapshot.projects.find((item) => item.id === vm.projectId)
      : undefined;
    if (project) project.vmState = state;
  }

  async updateUsagePeriod(
    period: UsageSummary['period'],
  ): Promise<UsageSummary> {
    await delay();
    const factor = period === 'today' ? 0.18 : period === '30d' ? 3.6 : 1;
    this.snapshot.usage = {
      ...this.snapshot.usage,
      period,
      totalTokens: Math.round(2_840_000 * factor),
      planTokens: Math.round(2_166_000 * factor),
      apiCost: Number((18.42 * factor).toFixed(2)),
      sessions: Math.max(1, Math.round(64 * factor)),
    };
    return clone(this.snapshot.usage);
  }

  async repairConfig(): Promise<void> {
    await delay(900);
  }

  async reset(): Promise<AppSnapshot> {
    await delay(80);
    this.snapshot = clone(initialSnapshot);
    return clone(this.snapshot);
  }
}
