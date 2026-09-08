import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

const repo = 'Sergey-Chernyshev/jarvis';
const gh = (...args) => execFileSync('gh', [...args, '--repo', repo], { encoding: 'utf8' });

export function validateRelease(tag, release, manifest) {
  assert.match(tag, /^v\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/);
  assert.equal(manifest.version.replace(/^v/, ''), tag.slice(1), 'Updater version differs from tag');
  assert.equal(release.isPrerelease, tag.includes('-'), 'Incorrect prerelease flag');
  const assets = new Map(release.assets.map(asset => [asset.name, asset]));
  assert(release.assets.some(asset => asset.name.endsWith('.dmg') && asset.size > 0), 'Missing DMG');
  for (const name of ['latest.json', 'manifest.json', 'jarvis-node-x86_64-unknown-linux-musl', 'jarvis-node-aarch64-unknown-linux-musl']) {
    assert(assets.get(name)?.size > 0, `Missing asset: ${name}`);
  }
  assert(manifest.platforms?.['darwin-aarch64'], 'Missing Apple Silicon updater');
  for (const platform of Object.values(manifest.platforms)) {
    assert.equal(typeof platform.signature, 'string');
    assert(platform.signature.trim().length > 40, 'Missing updater signature');
    const prefix = `https://github.com/${repo}/releases/download/${tag}/`;
    assert(platform.url.startsWith(prefix), 'Updater must use this immutable version tag');
    const name = decodeURIComponent(platform.url.slice(prefix.length));
    assert(assets.get(name)?.size > 0, `Updater archive is absent: ${name}`);
    assert(assets.get(`${name}.sig`)?.size > 0, `Detached signature is absent: ${name}`);
  }
}

function main() {
  const [tag, mode = '--check'] = process.argv.slice(2);
  assert(['--check', '--publish'].includes(mode), 'Use TAG [--check|--publish]');
  assert.match(tag ?? '', /^v\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/);
  const dir = mkdtempSync(join(tmpdir(), 'jarvis-release-'));
  try {
    const release = JSON.parse(gh('release', 'view', tag, '--json', 'isDraft,isPrerelease,assets'));
    gh('release', 'download', tag, '--pattern', 'latest.json', '--dir', dir);
    const path = join(dir, 'latest.json');
    const manifest = JSON.parse(readFileSync(path, 'utf8'));
    validateRelease(tag, release, manifest);
    console.log(`${tag}: DMG, signed updater and both Linux nodes are present`);
    if (mode !== '--publish') return;

    // Publish immutable version assets before exposing their URL in the update feed.
    gh('release', 'edit', tag, '--draft=false', `--prerelease=${tag.includes('-')}`, `--latest=${!tag.includes('-')}`);
    const releases = JSON.parse(gh('release', 'list', '--limit', '100', '--json', 'tagName'));
    const notes = `Update manifest for the Jarvis beta channel. Current version: [${tag}](https://github.com/${repo}/releases/tag/${tag}). Install the DMG from that version for the first beta installation. This channel also advances to stable versions when they are published.`;
    if (!releases.some(release => release.tagName === 'beta')) {
      gh('release', 'create', 'beta', '--target', execFileSync('git', ['rev-parse', `${tag}^{commit}`], { encoding: 'utf8' }).trim(), '--prerelease', '--latest=false', '--title', 'Jarvis beta update channel', '--notes', notes);
    }
    gh('release', 'upload', 'beta', path, '--clobber');
    gh('release', 'edit', 'beta', '--prerelease', '--latest=false', '--notes', notes);
    console.log(`Published ${tag} and refreshed the beta update channel`);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) main();
