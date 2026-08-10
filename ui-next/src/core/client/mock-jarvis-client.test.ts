import { describe, expect, it } from 'vitest';
import { MockJarvisClient } from './mock-jarvis-client';

describe('MockJarvisClient', () => {
  it('returns a complete typed snapshot', async () => {
    const client = new MockJarvisClient();
    const snapshot = await client.getSnapshot();

    expect(snapshot.projects.length).toBeGreaterThan(3);
    expect(snapshot.sessions.some((session) => session.status === 'waiting')).toBe(
      true,
    );
    expect(snapshot.settings.theme).toBe('dark');
    expect(snapshot.vms.some((vm) => vm.state === 'running')).toBe(true);
  });
});
