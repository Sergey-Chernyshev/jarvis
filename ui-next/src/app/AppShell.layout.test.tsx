import { render } from '@testing-library/react';
import { beforeEach, describe, expect, it } from 'vitest';
import { AppShell } from './AppShell';
import { useAppStore } from './app-store';
import '../styles/global.css';

describe('AppShell layout', () => {
  beforeEach(async () => {
    const snapshot = await useAppStore.getState().client.getSnapshot();
    useAppStore.setState({
      snapshot,
      loading: false,
      configErrorVisible: false,
      route: { section: 'voice', view: 'workspace' },
    });
  });

  it('keeps the footer in its compact bottom row when the optional banner is hidden', () => {
    const { container } = render(<AppShell />);

    const footer = container.querySelector<HTMLElement>('.app-footer');
    const content = container.querySelector<HTMLElement>('.app-content');
    expect(footer).not.toBeNull();
    expect(content).not.toBeNull();
    expect(content?.style.gridRow).toBe('4');
    expect(footer?.style.gridRow).toBe('5');
  });
});
