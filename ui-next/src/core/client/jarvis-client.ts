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

export interface JarvisClient {
  getSnapshot(): Promise<AppSnapshot>;
  updateSettings(patch: Partial<AppSettings>): Promise<AppSettings>;
  setTheme(theme: Theme): Promise<AppSettings>;
  setPaint(paint: Paint): Promise<AppSettings>;
  pinSession(sessionId: string, pinned: boolean): Promise<void>;
  clearFinishedSessions(): Promise<void>;
  sendMessage(
    sessionId: string,
    text: string,
    attachments: Attachment[],
  ): Promise<void>;
  answerQuestion(
    sessionId: string,
    answers: Record<string, string[]>,
  ): Promise<void>;
  setSessionModel(sessionId: string, model: string): Promise<void>;
  setSessionEffort(
    sessionId: string,
    effort: 'low' | 'medium' | 'high' | 'max',
  ): Promise<void>;
  launchSession(projectId: string, agent: Agent, inVm: boolean): Promise<void>;
  setVmState(vmId: string, state: VmState): Promise<void>;
  updateUsagePeriod(period: UsageSummary['period']): Promise<UsageSummary>;
  repairConfig(): Promise<void>;
  reset(): Promise<AppSnapshot>;
}
