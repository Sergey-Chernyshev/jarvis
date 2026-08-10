import { render, screen } from '@testing-library/react';
import { beforeEach, describe, expect, it } from 'vitest';
import { useAppStore } from '../../app/app-store';
import { SettingsPage } from './SettingsPage';

describe('SettingsPage', () => {
  beforeEach(async () => {
    const snapshot = await useAppStore.getState().client.getSnapshot();
    useAppStore.setState({
      snapshot,
      loading: false,
      route: {
        section: 'settings',
        view: 'settings',
        pane: 'appearance',
      },
    });
  });

  it('preserves the established settings sidebar hierarchy', () => {
    render(<SettingsPage />);

    expect(screen.getByPlaceholderText('Поиск настроек…')).toBeInTheDocument();
    expect(screen.getByText('локально · v0.4.0-next')).toBeInTheDocument();
  });
});
