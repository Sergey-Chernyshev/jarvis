// Rebuild desktop icons from the original SVG. No application build/restart.
// Requires the installed Tauri CLI and rsvg-convert (librsvg).
import { copyFile, mkdtemp, readdir, rm } from 'node:fs/promises';
import { spawnSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const root = fileURLToPath(new URL('../', import.meta.url));
const icons = path.join(root, 'src-tauri/icons');
const svg = path.join(icons, 'jarvis.svg');
const temporary = await mkdtemp(path.join(tmpdir(), 'jarvis-icon-'));
function run(program, args) {
  const result = spawnSync(program, args, { cwd: root, stdio: 'inherit' });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${program} exited with ${result.status}`);
}
try {
  run(process.execPath, [path.join(root, 'node_modules/.bin/tauri'), 'icon', svg, '--output', temporary]);
  const existing = await readdir(icons);
  const generated = new Set(await readdir(temporary));
  for (const name of existing) {
    if (generated.has(name) && /\.(png|ico|icns)$/.test(name)) await copyFile(path.join(temporary, name), path.join(icons, name));
  }
  run('rsvg-convert', ['-w', '1024', '-h', '1024', svg, '-o', path.join(icons, 'icon-source.png')]);
  run('rsvg-convert', ['-w', '64', '-h', '64', svg, '-o', path.join(icons, '64x64.png')]);
  await copyFile(svg, path.join(root, 'ui/onboarding-mark.svg'));
  console.log('Updated desktop PNG/ICO/ICNS icons and the onboarding SVG from icons/jarvis.svg');
} finally {
  await rm(temporary, { recursive: true, force: true });
}
