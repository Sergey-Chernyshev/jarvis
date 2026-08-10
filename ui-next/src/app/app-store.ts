import { create } from 'zustand';
import type {
  AppRoute,
  AppSnapshot,
  Attachment,
  Paint,
  Theme,
  ToastMessage,
} from '../core/client/types';
import type { JarvisClient } from '../core/client/jarvis-client';
import { MockJarvisClient } from '../core/client/mock-jarvis-client';

const client: JarvisClient = new MockJarvisClient();

interface AppState {
  client: JarvisClient;
  snapshot: AppSnapshot | null;
  loading: boolean;
  route: AppRoute;
  search: string;
  configErrorVisible: boolean;
  repairingConfig: boolean;
  toasts: ToastMessage[];
  bootProgress: Record<string, number>;
  load: () => Promise<void>;
  refresh: () => Promise<void>;
  navigate: (route: AppRoute) => void;
  setSearch: (search: string) => void;
  setTheme: (theme: Theme) => Promise<void>;
  setPaint: (paint: Paint) => Promise<void>;
  patchSnapshot: (recipe: (snapshot: AppSnapshot) => void) => void;
  updateSettings: (patch: Partial<AppSnapshot['settings']>) => Promise<void>;
  sendMessage: (
    sessionId: string,
    text: string,
    attachments: Attachment[],
  ) => Promise<void>;
  showConfigError: () => void;
  repairConfig: () => Promise<void>;
  toast: (message: Omit<ToastMessage, 'id'>) => void;
  dismissToast: (id: string) => void;
  setBootProgress: (vmId: string, progress: number) => void;
}

const initialRoute: AppRoute = { section: 'projects', view: 'catalog' };

function routeToHash(route: AppRoute): string {
  switch (route.section) {
    case 'chats':
      return route.view === 'session' ? `#/chats/${route.sessionId}` : '#/chats';
    case 'projects':
      if (route.view === 'history') return `#/projects/${route.projectId}/history`;
      if (route.view === 'workspace')
        return `#/projects/${route.projectId}/workspace`;
      if (route.view === 'environments') return '#/projects/environments';
      if (route.view === 'environment-files')
        return `#/projects/environments/${route.vmId}/files`;
      return '#/projects';
    case 'stats':
      return '#/stats';
    case 'voice':
      return '#/voice';
    case 'settings':
      return `#/settings/${route.pane ?? 'appearance'}`;
  }
}

export function parseHash(hash: string): AppRoute {
  const parts = hash.replace(/^#\/?/, '').split('/').filter(Boolean);
  const [section, first, second] = parts;
  if (section === 'chats' && first) {
    return { section: 'chats', view: 'session', sessionId: first };
  }
  if (section === 'chats') return { section: 'chats', view: 'list' };
  if (section === 'projects' && first === 'environments' && second) {
    return { section: 'projects', view: 'environment-files', vmId: second };
  }
  if (section === 'projects' && first === 'environments') {
    return { section: 'projects', view: 'environments' };
  }
  if (section === 'projects' && first && second === 'workspace') {
    return { section: 'projects', view: 'workspace', projectId: first };
  }
  if (section === 'projects' && first && second === 'history') {
    return { section: 'projects', view: 'history', projectId: first };
  }
  if (section === 'projects') return { section: 'projects', view: 'catalog' };
  if (section === 'stats') return { section: 'stats', view: 'overview' };
  if (section === 'voice') return { section: 'voice', view: 'workspace' };
  if (section === 'settings') {
    return { section: 'settings', view: 'settings', pane: first ?? 'appearance' };
  }
  return initialRoute;
}

export const useAppStore = create<AppState>((set, get) => ({
  client,
  snapshot: null,
  loading: true,
  route:
    typeof window === 'undefined' ? initialRoute : parseHash(window.location.hash),
  search: '',
  configErrorVisible: false,
  repairingConfig: false,
  toasts: [],
  bootProgress: {},

  load: async () => {
    set({ loading: true });
    const snapshot = await client.getSnapshot();
    set({ snapshot, loading: false });
  },
  refresh: async () => {
    const snapshot = await client.getSnapshot();
    set({ snapshot });
  },
  navigate: (route) => {
    if (typeof window !== 'undefined') {
      const hash = routeToHash(route);
      if (window.location.hash !== hash) window.history.pushState(null, '', hash);
    }
    set({ route, search: '' });
  },
  setSearch: (search) => set({ search }),
  setTheme: async (theme) => {
    const settings = await client.setTheme(theme);
    set((state) =>
      state.snapshot
        ? { snapshot: { ...state.snapshot, settings } }
        : {},
    );
  },
  setPaint: async (paint) => {
    const settings = await client.setPaint(paint);
    set((state) =>
      state.snapshot
        ? { snapshot: { ...state.snapshot, settings } }
        : {},
    );
  },
  patchSnapshot: (recipe) =>
    set((state) => {
      if (!state.snapshot) return {};
      const snapshot = structuredClone(state.snapshot);
      recipe(snapshot);
      return { snapshot };
    }),
  updateSettings: async (patch) => {
    const settings = await client.updateSettings(patch);
    set((state) =>
      state.snapshot
        ? { snapshot: { ...state.snapshot, settings } }
        : {},
    );
  },
  sendMessage: async (sessionId, text, attachments) => {
    await client.sendMessage(sessionId, text, attachments);
    await get().refresh();
  },
  showConfigError: () => {
    set({ configErrorVisible: true });
    get().toast({
      kind: 'error',
      title: 'Конфигурация требует внимания',
      body: 'В settings.json найден конфликтующий блок permissions.',
    });
  },
  repairConfig: async () => {
    set({ repairingConfig: true });
    await client.repairConfig();
    set({ repairingConfig: false, configErrorVisible: false });
    get().toast({
      kind: 'success',
      title: 'Конфигурация исправлена',
      body: 'Резервная копия сохранена рядом с исходным файлом.',
    });
  },
  toast: (message) => {
    const id = crypto.randomUUID();
    set((state) => ({
      toasts: [...state.toasts, { ...message, id }].slice(-4),
    }));
    window.setTimeout(() => get().dismissToast(id), 4200);
  },
  dismissToast: (id) =>
    set((state) => ({
      toasts: state.toasts.filter((message) => message.id !== id),
    })),
  setBootProgress: (vmId, progress) =>
    set((state) => ({
      bootProgress: { ...state.bootProgress, [vmId]: progress },
    })),
}));
