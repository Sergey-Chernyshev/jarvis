export type Theme = 'dark' | 'light' | 'midnight';
export type Paint = 'clover' | 'raspberry' | 'coal';
export type Agent = 'claude' | 'codex';
export type AsyncStatus = 'idle' | 'loading' | 'success' | 'error';

export type SessionStatus =
  | 'working'
  | 'waiting'
  | 'done'
  | 'interrupted'
  | 'error';

export interface Attachment {
  id: string;
  name: string;
  kind: 'image' | 'file';
  size: number;
}

export interface Session {
  id: string;
  projectId: string;
  project: string;
  cwd: string;
  branch?: string;
  agent: Agent;
  model: string;
  effort: 'low' | 'medium' | 'high' | 'max';
  host: string;
  status: SessionStatus;
  summary: string;
  detail: string;
  createdAt: number;
  doneAt?: number;
  pinned: boolean;
  usage: {
    tokens: number;
    cost: number;
    billing: 'plan' | 'api';
  };
  question?: AgentQuestionSet;
  board?: TaskBoard;
}

export interface ChatMessage {
  id: string;
  role: 'user' | 'assistant' | 'tool';
  text: string;
  timestamp: number;
  tool?: {
    kind: 'read' | 'edit' | 'shell' | 'search' | 'web' | 'task';
    label: string;
    count?: number;
  };
  attachments?: Attachment[];
}

export interface TurnFile {
  path: string;
  note: string;
  kind: 'created' | 'edited' | 'read';
}

export interface TurnSummary {
  id: string;
  summary: string;
  files: TurnFile[];
  commands: string[];
}

export interface QuestionOption {
  id: string;
  label: string;
  description?: string;
}

export interface AgentQuestion {
  id: string;
  header: string;
  question: string;
  multiSelect: boolean;
  options: QuestionOption[];
  allowCustom: boolean;
}

export interface AgentQuestionSet {
  current: number;
  questions: AgentQuestion[];
}

export type TaskStatus =
  | 'pending'
  | 'in_progress'
  | 'completed'
  | 'interrupted';

export interface AgentTask {
  id: string;
  number: number;
  title: string;
  status: TaskStatus;
  model: string;
  startedAt?: number;
  durationMs?: number;
}

export interface TaskBoard {
  tasks: AgentTask[];
  subagents: Array<{
    id: string;
    name: string;
    model: string;
    startedAt: number;
    stoppedAt?: number;
  }>;
  stopped: boolean;
}

export type VmState = 'running' | 'stopped' | 'booting' | 'error' | 'none';

export interface ProjectChat {
  id: string;
  title: string;
  agent: Agent;
  when: string;
  day: string;
  run: 'terminal' | 'vm-working' | 'vm-done' | 'vm-failed';
  detail?: string;
}

export interface Project {
  id: string;
  name: string;
  path: string;
  vmId?: string;
  vmState: VmState;
  chats: ProjectChat[];
}

export interface VmMount {
  id: string;
  host: string;
  guest: string;
  mode: 'ro' | 'rw';
  description: string;
  removable: boolean;
}

export interface FileNode {
  name: string;
  kind: 'directory' | 'file';
  size?: string;
  note?: string;
  children?: FileNode[];
}

export interface VirtualMachine {
  id: string;
  projectId: string;
  project: string;
  name: string;
  state: VmState;
  busy: boolean;
  cpus: number;
  ramUsed: number;
  ramTotal: number;
  diskUsed: number;
  diskMax: number;
  uptime?: string;
  autostart: boolean;
  mounts: VmMount[];
  home: FileNode[];
}

export interface UsagePoint {
  date: string;
  claudeTokens: number;
  codexTokens: number;
  sessions: number;
}

export interface UsageSummary {
  period: 'today' | '7d' | '30d';
  totalTokens: number;
  planTokens: number;
  apiCost: number;
  sessions: number;
  points: UsagePoint[];
  limits: Array<{
    id: string;
    label: string;
    usedPercent: number;
    resetAt: string;
  }>;
}

export interface VoiceTranscript {
  id: string;
  text: string;
  enhanced?: string;
  appliedStyle?: string;
  createdAt: number;
  words: number;
}

export interface DictionaryWord {
  id: string;
  word: string;
  note: string;
}

export interface VoiceTransform {
  id: string;
  name: string;
  description: string;
  enabled: boolean;
}

export interface ModelArtifact {
  id: string;
  label: string;
  kind: 'stt' | 'voice' | 'wake' | 'runtime';
  size: string;
  state: 'active' | 'installed' | 'available' | 'downloading' | 'error';
  progress?: number;
}

export interface HotkeyBinding {
  id: string;
  label: string;
  accel: string;
  defaultAccel: string;
  group: 'panel' | 'sound' | 'voice';
}

export interface AppSettings {
  theme: Theme;
  paint: Paint;
  position: 'center' | 'corner';
  openAtLogin: boolean;
  diagnostics: boolean;
  sttEngine: string;
  microphone: string;
  noiseGate: boolean;
  speaker: string;
  voiceRate: 'slow' | 'medium' | 'fast' | 'x-fast';
  voiceMuted: boolean;
  voiceDuck: boolean;
  bluetoothOnly: boolean;
  wakeEnabled: boolean;
  microphoneMuted: boolean;
  wakeThreshold: number;
  notifyDone: boolean;
  notifyWaiting: boolean;
  autoResume: boolean;
  notificationPosition: 'center' | 'corner';
  notificationTtl: 0 | 5 | 8;
  keepAwake: 'off' | '15m' | '1h' | '4h' | 'inf';
  keepWhileAgentsRun: boolean;
  keepDisplayOn: boolean;
  clamshell: 'sleep' | 'keep';
  launchTerminal: 'terminal-app' | 'iterm2' | 'custom';
  launchCustomCommand: string;
  launchProxyCommand: string;
  launchDangerous: boolean;
  serviceBackend: 'auto' | Agent;
  serviceProxy: string;
  codexModel: string;
  codexEffort: 'low' | 'medium' | 'high';
  memoryScope: 'project' | 'all';
  vmCpus: 2 | 4 | 8;
  vmMemory: 4 | 8 | 16;
  vmDisk: number;
  vmAutostart: boolean;
  vmIdleStop: 'off' | '30' | '120';
  quietMode: boolean;
}

export interface IntegrationStatus {
  hooks: boolean;
  shim: boolean;
  tmux: boolean;
  path: boolean;
  foreignHooks: number;
}

export interface AppSnapshot {
  sessions: Session[];
  messages: Record<string, ChatMessage[]>;
  summaries: Record<string, TurnSummary[]>;
  projects: Project[];
  vms: VirtualMachine[];
  usage: UsageSummary;
  transcripts: VoiceTranscript[];
  dictionary: DictionaryWord[];
  transforms: VoiceTransform[];
  scratchpad: string;
  models: ModelArtifact[];
  hotkeys: HotkeyBinding[];
  settings: AppSettings;
  integration: IntegrationStatus;
  skills: string[];
}

export type AppSection = 'chats' | 'projects' | 'stats' | 'voice' | 'settings';

export type AppRoute =
  | { section: 'chats'; view: 'list' }
  | { section: 'chats'; view: 'session'; sessionId: string }
  | { section: 'projects'; view: 'catalog' }
  | { section: 'projects'; view: 'history'; projectId: string }
  | { section: 'projects'; view: 'workspace'; projectId: string }
  | { section: 'projects'; view: 'environments' }
  | { section: 'projects'; view: 'environment-files'; vmId: string }
  | { section: 'stats'; view: 'overview' }
  | { section: 'voice'; view: 'workspace' }
  | { section: 'settings'; view: 'settings'; pane?: string };

export interface ToastMessage {
  id: string;
  kind: 'info' | 'success' | 'warning' | 'error';
  title: string;
  body?: string;
}
