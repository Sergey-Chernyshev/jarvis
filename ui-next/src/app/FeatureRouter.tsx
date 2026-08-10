import { ChatsFeature } from '../features/chats/ChatsFeature';
import { ProjectsFeature } from '../features/projects/ProjectsFeature';
import { StatsPage } from '../features/stats/StatsPage';
import { VoicePage } from '../features/voice/VoicePage';
import { SettingsPage } from '../features/settings/SettingsPage';
import { useAppStore } from './app-store';

export function FeatureRouter() {
  const section = useAppStore((state) => state.route.section);

  if (section === 'chats') return <ChatsFeature />;
  if (section === 'projects') return <ProjectsFeature />;
  if (section === 'stats') return <StatsPage />;
  if (section === 'voice') return <VoicePage />;
  return <SettingsPage />;
}
