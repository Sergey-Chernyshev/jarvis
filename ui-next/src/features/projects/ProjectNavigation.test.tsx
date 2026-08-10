import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { useAppStore } from '../../app/app-store';
import '../../styles/global.css';
import { ProjectsFeature } from './ProjectsFeature';

describe('project navigation', () => {
  beforeEach(async () => {
    const snapshot = await useAppStore.getState().client.getSnapshot();
    useAppStore.setState({
      snapshot,
      loading: false,
      route: { section: 'projects', view: 'catalog' },
      toasts: [],
    });
  });

  afterEach(cleanup);

  it('names the environments destination in plain language', () => {
    render(<ProjectsFeature />);

    fireEvent.click(screen.getByRole('button', { name: 'Управление VM' }));

    expect(useAppStore.getState().route).toEqual({
      section: 'projects',
      view: 'environments',
    });
  });

  it('separates history days with whitespace instead of horizontal rules', () => {
    useAppStore.setState({
      route: {
        section: 'projects',
        view: 'history',
        projectId: 'jarvis',
      },
    });
    const { container } = render(<ProjectsFeature />);

    const list = container.querySelector<HTMLElement>('.dated-list');
    const separators = container.querySelectorAll<HTMLElement>('.date-separator');

    expect(list).toHaveClass('line-free-list');
    expect(separators).toHaveLength(2);
    separators.forEach((separator) => {
      expect(separator).toHaveClass('line-free-separator');
    });
  });

  it('keeps project rows distinct without divider lines', () => {
    const { container } = render(<ProjectsFeature />);

    const list = container.querySelector<HTMLElement>('.entity-list');
    const rows = container.querySelectorAll<HTMLElement>('.project-row');

    expect(list).toHaveClass('line-free-list');
    expect(rows).toHaveLength(4);
    rows.forEach((row) => {
      expect(row).toHaveClass('line-free-row');
    });
  });

  it('keeps launch-menu selection marks separate from flexible text', () => {
    useAppStore.setState({
      route: {
        section: 'projects',
        view: 'history',
        projectId: 'jarvis',
      },
    });
    const { container } = render(<ProjectsFeature />);

    fireEvent.click(
      screen.getByRole('button', { name: 'Выбрать агента и место' }),
    );

    const menu = container.querySelector<HTMLElement>('.launch-menu');
    const copies = menu?.querySelectorAll('.launch-menu__copy');
    const marks = menu?.querySelectorAll('.selection-mark');

    expect(copies).toHaveLength(5);
    expect(marks).toHaveLength(4);
    marks?.forEach((mark) => {
      expect(mark).not.toHaveClass('launch-menu__copy');
    });
  });

  it('shows agent results instead of making an embedded terminal the workspace', () => {
    useAppStore.setState({
      route: {
        section: 'projects',
        view: 'workspace',
        projectId: 'jarvis',
      },
    });
    const { container } = render(<ProjectsFeature />);

    expect(container.querySelector('.terminal-surface')).not.toBeInTheDocument();
    expect(
      screen.getByRole('region', { name: 'Результат агента' }),
    ).toBeInTheDocument();
    expect(screen.getByText('17 файлов изменено')).toBeInTheDocument();
    expect(screen.getByText('42 теста прошли')).toBeInTheDocument();
    expect(
      screen.getByRole('button', { name: 'Скопировать команду входа' }),
    ).toBeInTheDocument();
    expect(
      screen.getByText('jarvis vm shell jarvis --resume'),
    ).toBeInTheDocument();
  });

  it('keeps the workspace body and composer in stable grid rows', () => {
    useAppStore.setState({
      route: {
        section: 'projects',
        view: 'workspace',
        projectId: 'jarvis',
      },
    });
    const { container } = render(<ProjectsFeature />);

    const body = container.querySelector<HTMLElement>('.workspace-body');
    const composer = container.querySelector<HTMLElement>('.workspace-composer');

    expect(body?.style.gridRow).toBe('3');
    expect(composer?.style.gridRow).toBe('4');
  });
});
