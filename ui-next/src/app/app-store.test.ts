import { describe, expect, it } from 'vitest';
import { parseHash } from './app-store';

describe('parseHash', () => {
  it('restores deep project and environment routes', () => {
    expect(parseHash('#/projects/jarvis/workspace')).toEqual({
      section: 'projects',
      view: 'workspace',
      projectId: 'jarvis',
    });
    expect(parseHash('#/projects/environments/vm-jarvis/files')).toEqual({
      section: 'projects',
      view: 'environment-files',
      vmId: 'vm-jarvis',
    });
  });

  it('opens appearance as the default settings pane', () => {
    expect(parseHash('#/settings')).toEqual({
      section: 'settings',
      view: 'settings',
      pane: 'appearance',
    });
  });
});
