import { useEffect } from 'react';
import { AnimatePresence, motion, useReducedMotion } from 'motion/react';
import { Search, Sparkles } from 'lucide-react';
import { Navigation } from './Navigation';
import { GlobalBanners, ToastHost } from './GlobalBanners';
import { parseHash, useAppStore } from './app-store';
import { FeatureRouter } from './FeatureRouter';

export function AppShell() {
  const snapshot = useAppStore((state) => state.snapshot);
  const route = useAppStore((state) => state.route);
  const search = useAppStore((state) => state.search);
  const setSearch = useAppStore((state) => state.setSearch);
  const navigate = useAppStore((state) => state.navigate);
  const reduceMotion = useReducedMotion();

  useEffect(() => {
    if (!snapshot) return;
    document.documentElement.dataset.theme = snapshot.settings.theme;
    if (snapshot.settings.paint === 'clover') {
      delete document.documentElement.dataset.paint;
    } else {
      document.documentElement.dataset.paint = snapshot.settings.paint;
    }
    localStorage.setItem('jarvis-ui-theme', snapshot.settings.theme);
    localStorage.setItem('jarvis-ui-paint', snapshot.settings.paint);
  }, [snapshot]);

  useEffect(() => {
    const onHash = () => {
      navigate(parseHash(window.location.hash));
    };
    const onKey = (event: KeyboardEvent) => {
      if (!(event.metaKey || event.ctrlKey)) return;
      const routeByKey = {
        '1': { section: 'chats', view: 'list' },
        '2': { section: 'projects', view: 'catalog' },
        '3': { section: 'stats', view: 'overview' },
        '4': { section: 'voice', view: 'workspace' },
        ',': { section: 'settings', view: 'settings', pane: 'appearance' },
      } as const;
      const next = routeByKey[event.key as keyof typeof routeByKey];
      if (next) {
        event.preventDefault();
        navigate(next);
      }
    };
    window.addEventListener('popstate', onHash);
    window.addEventListener('keydown', onKey);
    return () => {
      window.removeEventListener('popstate', onHash);
      window.removeEventListener('keydown', onKey);
    };
  }, [navigate]);

  if (!snapshot) return null;

  const searchPlaceholder =
    route.section === 'chats'
      ? 'Найти активный чат…'
      : route.section === 'projects'
        ? 'Найти проект или сессию…'
        : route.section === 'voice'
          ? 'Поиск по надиктованному…'
          : 'Быстрый поиск…';

  const hideSearch =
    (route.section === 'chats' && route.view === 'session') ||
    route.section === 'settings';

  return (
    <div className="stage">
      <motion.section
        className="app-window"
        initial={reduceMotion ? { opacity: 0 } : { opacity: 0, y: 18, scale: 0.985 }}
        animate={{ opacity: 1, y: 0, scale: 1 }}
        transition={{ duration: 0.44, ease: [0.16, 1, 0.3, 1] }}
      >
        <header className="titlebar">
          <span className="traffic-lights" aria-hidden="true">
            <i />
            <i />
            <i />
          </span>
          <span className="wordmark">
            Jarvis <i>next</i>
          </span>
          <span className="titlebar-spacer" />
          <span className="live-pill">
            <span className="live-wave">
              <i />
              <i />
              <i />
            </span>
            {snapshot.sessions.filter((session) => session.status === 'working').length}{' '}
            агента работают
          </span>
        </header>

        <div className="command-row">
          <Navigation />
          {!hideSearch && (
            <label className="global-search">
              <Search size={14} />
              <input
                value={search}
                onChange={(event) => setSearch(event.target.value)}
                placeholder={searchPlaceholder}
              />
              <kbd>⌘K</kbd>
            </label>
          )}
        </div>

        <GlobalBanners />

        <div className="app-content" style={{ gridRow: 4 }}>
          <AnimatePresence mode="wait" initial={false}>
            <motion.div
              key={`${route.section}:${route.view}`}
              className="route-frame"
              initial={reduceMotion ? { opacity: 0 } : { opacity: 0, x: 10 }}
              animate={{ opacity: 1, x: 0 }}
              exit={reduceMotion ? { opacity: 0 } : { opacity: 0, x: -8 }}
              transition={{ duration: 0.19, ease: [0.16, 1, 0.3, 1] }}
            >
              <FeatureRouter />
            </motion.div>
          </AnimatePresence>
        </div>

        <footer className="app-footer" style={{ gridRow: 5 }}>
          <span className="footer-status">
            <i />
            демон активен
          </span>
          <span>{snapshot.sessions.length} сессии · 3 VM · локально</span>
          <span className="footer-hint">
            <Sparkles size={13} />
            <kbd>/</kbd> команды
            <kbd>esc</kbd> назад
          </span>
        </footer>
      </motion.section>
      <ToastHost />
    </div>
  );
}
