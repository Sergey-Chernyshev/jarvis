import {
  BarChart3,
  FolderKanban,
  MessageCircleMore,
  Mic2,
  Settings2,
} from 'lucide-react';
import type { AppRoute, AppSection } from '../core/client/types';
import { useAppStore } from './app-store';

const items: Array<{
  id: AppSection;
  label: string;
  key: string;
  icon: typeof MessageCircleMore;
  route: AppRoute;
}> = [
  {
    id: 'chats',
    label: 'Чаты',
    key: '⌘1',
    icon: MessageCircleMore,
    route: { section: 'chats', view: 'list' },
  },
  {
    id: 'projects',
    label: 'Проекты',
    key: '⌘2',
    icon: FolderKanban,
    route: { section: 'projects', view: 'catalog' },
  },
  {
    id: 'stats',
    label: 'Статистика',
    key: '⌘3',
    icon: BarChart3,
    route: { section: 'stats', view: 'overview' },
  },
  {
    id: 'voice',
    label: 'Голос',
    key: '⌘4',
    icon: Mic2,
    route: { section: 'voice', view: 'workspace' },
  },
  {
    id: 'settings',
    label: 'Настройки',
    key: '⌘,',
    icon: Settings2,
    route: { section: 'settings', view: 'settings', pane: 'appearance' },
  },
];

export function Navigation() {
  const route = useAppStore((state) => state.route);
  const navigate = useAppStore((state) => state.navigate);

  return (
    <nav className="navigation" aria-label="Основные разделы">
      {items.map((item) => {
        const Icon = item.icon;
        const active = item.id === route.section;
        return (
          <button
            type="button"
            key={item.id}
            className={active ? 'is-active' : ''}
            aria-current={active ? 'page' : undefined}
            onClick={() => navigate(item.route)}
          >
            <Icon size={15} strokeWidth={1.8} />
            <span>{item.label}</span>
            <kbd>{item.key}</kbd>
          </button>
        );
      })}
    </nav>
  );
}
