import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { useAppStore } from '../../app/app-store';
import { ProjectsFeature } from './ProjectsFeature';

describe('EnvironmentList', () => {
  afterEach(cleanup);

  beforeEach(async () => {
    const snapshot = await useAppStore.getState().client.getSnapshot();
    useAppStore.setState({
      snapshot,
      loading: false,
      route: { section: 'projects', view: 'environments' },
      toasts: [],
    });
  });

  it('keeps navigation in the app shell instead of showing a close-page control', () => {
    render(<ProjectsFeature />);

    expect(
      screen.queryByRole('button', { name: 'К проектам' }),
    ).not.toBeInTheDocument();
  });

  it('hides technical resources until the user asks for VM details', () => {
    render(<ProjectsFeature />);

    expect(screen.queryAllByText('cpu')).toHaveLength(0);

    fireEvent.click(
      screen.getByRole('button', { name: 'Подробнее о машине jarvis' }),
    );

    expect(screen.getByText('4 CPU')).toBeInTheDocument();
    expect(screen.getByText('4 ГиБ RAM')).toBeInTheDocument();
  });

  it('keeps cache maintenance behind a secondary page menu', () => {
    render(<ProjectsFeature />);

    expect(
      screen.queryByRole('button', { name: /Очистить кэш/ }),
    ).not.toBeInTheDocument();

    fireEvent.click(
      screen.getByRole('button', { name: 'Обслуживание машин' }),
    );

    expect(
      screen.getByRole('button', { name: 'Очистить кэш · 841 МБ' }),
    ).toBeInTheDocument();
  });

  it('keeps the transferred-context inventory compact until requested', () => {
    render(<ProjectsFeature />);

    expect(screen.queryByText('Транскрипты и кэш')).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Что доступно агенту' }));
    expect(screen.getByText('Транскрипты и кэш')).toBeInTheDocument();
  });
});
