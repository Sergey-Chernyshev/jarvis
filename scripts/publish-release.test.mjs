import { test } from 'node:test';
import assert from 'node:assert/strict';
import { validateRelease } from './publish-release.mjs';

const tag = 'v1.0.0-beta.1';
function fixture() {
  const names = ['Jarvis.dmg', 'latest.json', 'manifest.json', 'jarvis-node-x86_64-unknown-linux-musl', 'jarvis-node-aarch64-unknown-linux-musl', 'Jarvis.app.tar.gz', 'Jarvis.app.tar.gz.sig'];
  return {
    release: { isPrerelease: true, assets: names.map(name => ({ name, size: 100 })) },
    manifest: { version: '1.0.0-beta.1', platforms: { 'darwin-aarch64': { signature: 'x'.repeat(80), url: `https://github.com/Sergey-Chernyshev/jarvis/releases/download/${tag}/Jarvis.app.tar.gz` } } },
  };
}

test('complete beta and stable releases pass validation', () => {
  const { release, manifest } = fixture();
  validateRelease(tag, release, manifest);
  release.isPrerelease = false;
  manifest.version = '1.0.0';
  manifest.platforms['darwin-aarch64'].url = manifest.platforms['darwin-aarch64'].url.replace(tag, 'v1.0.0');
  validateRelease('v1.0.0', release, manifest);
});

test('incomplete and cross-version updates cannot be published', () => {
  const mutations = [
    ({ release }) => { release.isPrerelease = false; },
    ({ release }) => { release.assets = release.assets.filter(asset => !asset.name.endsWith('.dmg')); },
    ({ release }) => { release.assets = release.assets.filter(asset => !asset.name.endsWith('.sig')); },
    ({ release }) => { release.assets.find(asset => asset.name.includes('x86_64')).size = 0; },
    ({ manifest }) => { manifest.version = '0.3.3'; },
    ({ manifest }) => { manifest.platforms = {}; },
    ({ manifest }) => { manifest.platforms['darwin-aarch64'].signature = ''; },
    ({ manifest }) => { manifest.platforms['darwin-aarch64'].url = 'https://example.com/archive.tar.gz'; },
    ({ manifest }) => { manifest.platforms['darwin-aarch64'].url = manifest.platforms['darwin-aarch64'].url.replace(tag, 'v0.3.3'); },
  ];
  for (const mutate of mutations) {
    const value = fixture();
    mutate(value);
    assert.throws(() => validateRelease(tag, value.release, value.manifest));
  }
});
