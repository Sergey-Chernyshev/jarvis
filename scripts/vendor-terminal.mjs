import { readFileSync, mkdirSync, writeFileSync } from 'node:fs';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const files = {
  '@xterm/xterm/lib/xterm.js': 'xterm.js',
  '@xterm/xterm/css/xterm.css': 'xterm.css',
  '@xterm/xterm/LICENSE': 'LICENSE.xterm',
  '@xterm/addon-search/lib/addon-search.js': 'addon-search.js',
  '@xterm/addon-search/LICENSE': 'LICENSE.search',
  '@xterm/addon-fit/lib/addon-fit.js': 'addon-fit.js',
  '@xterm/addon-fit/LICENSE': 'LICENSE.fit',
};
const check = process.argv.includes('--check');
for (const [source, name] of Object.entries(files)) {
  const bytes = readFileSync(resolve(root, 'node_modules', source));
  const target = resolve(root, 'ui/vendor/xterm', name);
  if (check) {
    if (!readFileSync(target).equals(bytes)) throw new Error(`Outdated terminal asset: ${name}. Run npm run vendor:terminal.`);
  } else {
    mkdirSync(dirname(target), { recursive: true });
    writeFileSync(target, bytes);
  }
}
console.log(check ? 'Terminal assets match installed packages.' : 'Terminal assets copied with MIT licenses.');
