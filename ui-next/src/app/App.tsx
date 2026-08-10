import { useEffect } from 'react';
import { useAppStore } from './app-store';
import { AppShell } from './AppShell';

export function App() {
  const load = useAppStore((state) => state.load);
  const loading = useAppStore((state) => state.loading);

  useEffect(() => {
    void load();
  }, [load]);

  if (loading) {
    return (
      <main className="app-loading">
        <span className="brand-orbit" aria-hidden="true">
          J
        </span>
        <div>
          <strong>Jarvis</strong>
          <span>собирает локальный контекст…</span>
        </div>
      </main>
    );
  }

  return <AppShell />;
}
