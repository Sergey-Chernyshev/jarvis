import { mkdir } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { build } from 'esbuild';

const repositoryRoot = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  '..',
);
const outputDirectory = path.join(repositoryRoot, 'ui/generated');

await mkdir(outputDirectory, { recursive: true });
await build({
  absWorkingDir: repositoryRoot,
  entryPoints: ['ui/core/tauri-transport.ts'],
  outfile: 'ui/generated/tauri-transport.js',
  bundle: true,
  charset: 'utf8',
  format: 'iife',
  legalComments: 'none',
  logLevel: 'warning',
  minify: true,
  platform: 'browser',
  sourcemap: false,
  target: ['safari15'],
});
