#!/usr/bin/env node
/** Build current Linux nodes or merge their CI artifacts before bundling Jarvis.
 * No remote host is contacted. Cross-build tools live in an isolated temp folder.
 *   node scripts/prepare-node-bundle.mjs              # both Linux musl targets
 *   node scripts/prepare-node-bundle.mjs --verify     # offline, no writes
 *   node scripts/prepare-node-bundle.mjs --from DIR   # verified CI artifacts
 *   node scripts/prepare-node-bundle.mjs --native --target TARGET --out DIR
 */
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import crypto from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
export const TARGETS = ['x86_64-unknown-linux-musl', 'aarch64-unknown-linux-musl'];
const OFFSET = 0xcbf29ce484222325n;
const PRIME = 1099511628211n;
const MASK = (1n << 64n) - 1n;

export function fnv1a64(bytes, state = OFFSET) {
  for (const byte of bytes) state = ((state ^ BigInt(byte)) * PRIME) & MASK;
  return state;
}
const hex = value => value.toString(16).padStart(16, '0');
const sha256 = data => crypto.createHash('sha256').update(data).digest('hex');

export function sourceState(repo = REPO) {
  const files = ['src-tauri/node/Cargo.toml', 'src-tauri/shared/Cargo.toml', 'src-tauri/Cargo.lock'];
  function collect(relative) {
    for (const entry of fs.readdirSync(path.join(repo, relative), { withFileTypes: true })) {
      const child = `${relative}/${entry.name}`;
      if (entry.isSymbolicLink()) throw new Error(`Node source must not be a symlink: ${child}`);
      if (entry.isDirectory()) collect(child);
      else if (entry.isFile() && child.endsWith('.rs')) files.push(child);
    }
  }
  collect('src-tauri/node/src');
  collect('src-tauri/shared/src');
  files.sort();
  let hash = OFFSET;
  for (const file of files) {
    hash = fnv1a64(Buffer.from(file, 'utf8'), hash);
    hash = fnv1a64(Buffer.from([0]), hash);
    hash = fnv1a64(fs.readFileSync(path.join(repo, file)), hash);
    hash = fnv1a64(Buffer.from([0]), hash);
  }
  const version = fs.readFileSync(path.join(repo, 'src-tauri/node/Cargo.toml'), 'utf8').match(/^version\s*=\s*"([^"]+)"/m)?.[1];
  if (!version) throw new Error('Could not read jarvis-node version');
  return { version, sourceFingerprint: hex(hash), files };
}

export function checkElf(bytes, target) {
  if (!TARGETS.includes(target)) throw new Error(`Unsupported bundled target: ${target}`);
  const machine = target.startsWith('x86_64') ? 62 : 183;
  if (bytes.length < 64 || !bytes.subarray(0, 4).equals(Buffer.from([127, 69, 76, 70])) || bytes[4] !== 2 || bytes[5] !== 1 || bytes.readUInt16LE(18) !== machine) {
    throw new Error(`Artifact is not a 64-bit Linux ELF for ${target}`);
  }
  const offset = Number(bytes.readBigUInt64LE(32));
  const size = bytes.readUInt16LE(54), count = bytes.readUInt16LE(56);
  if (!count || size < 56 || !Number.isSafeInteger(offset) || offset + count * size > bytes.length) throw new Error(`Invalid ELF program headers: ${target}`);
  for (let index = 0; index < count; index++) {
    if (bytes.readUInt32LE(offset + index * size) === 3) throw new Error(`Bundled musl node must be static (found ELF interpreter): ${target}`);
  }
}

export function readManifest(directory, state = sourceState()) {
  const manifest = JSON.parse(fs.readFileSync(path.join(directory, 'manifest.json'), 'utf8'));
  if (manifest.version !== state.version || manifest.sourceFingerprint !== state.sourceFingerprint) throw new Error(`Stale node artifacts in ${directory}; rebuild from the current sources and Cargo.lock`);
  if (!Array.isArray(manifest.artifacts) || !manifest.artifacts.length) throw new Error(`Empty artifact manifest: ${directory}`);
  const seen = new Set();
  for (const entry of manifest.artifacts) {
    if (!TARGETS.includes(entry.target) || seen.has(entry.target) || entry.file !== `jarvis-node-${entry.target}`) throw new Error(`Invalid artifact entry: ${directory}`);
    seen.add(entry.target);
    const file = path.join(directory, entry.file);
    if (!fs.lstatSync(file).isFile()) throw new Error(`Artifact must be a regular file: ${file}`);
    const bytes = fs.readFileSync(file);
    if (bytes.length > 64 * 1024 * 1024 || sha256(bytes) !== entry.sha256) throw new Error(`Artifact checksum mismatch: ${file}`);
    if (entry.fileFingerprint && hex(fnv1a64(bytes)) !== entry.fileFingerprint) throw new Error(`Artifact fingerprint mismatch: ${file}`);
    checkElf(bytes, entry.target);
  }
  return manifest;
}

export function verify(directory, targets = TARGETS, state = sourceState()) {
  const manifest = readManifest(directory, state);
  for (const target of targets) if (!manifest.artifacts.some(entry => entry.target === target)) throw new Error(`Missing bundled target ${target}`);
  return manifest;
}

function run(command, args, options = {}) {
  const result = spawnSync(command, args, { cwd: REPO, stdio: 'inherit', ...options });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${path.basename(command)} exited with ${result.status ?? result.signal}`);
  return result;
}
function output(command, args) { return run(command, args, { stdio: ['ignore', 'pipe', 'pipe'], encoding: 'utf8', maxBuffer: 1024 * 1024 }).stdout.trim(); }
function atomic(file, bytes, mode = 0o644) { fs.mkdirSync(path.dirname(file), { recursive: true }); const temp = `${file}.tmp-${process.pid}`; fs.writeFileSync(temp, bytes, { mode }); fs.renameSync(temp, file); }

async function download(url, file) {
  const partial = `${file}.part`;
  console.log(`Downloading ${path.basename(file)} (progress: ${partial})`);
  // Restart curl per attempt so resume uses the latest on-disk offset; curl's
  // internal retry can rewind a partially transferred file to the initial offset.
  for (let attempt = 0; ; attempt++) {
    try {
      run('curl', ['--fail', '--location', '--silent', '--show-error', '--connect-timeout', '20', '--max-time', '180', '--continue-at', '-', '--output', partial, url]);
      break;
    } catch (error) {
      if (attempt === 2) throw error;
      const bytes = fs.existsSync(partial) ? fs.statSync(partial).size : 0;
      console.log(`Resuming ${path.basename(file)} after ${bytes} downloaded bytes`);
    }
  }
  fs.renameSync(partial, file);
}

async function crossToolchain(work, targets) {
  fs.mkdirSync(work, { recursive: true });
  const version = output('rustc', ['--version']).match(/^rustc ([0-9]+\.[0-9]+\.[0-9]+)/)?.[1];
  if (!version) throw new Error('A stable Rust compiler is required for the Linux node cross-build');
  const sysroot = path.join(work, `sysroot-${version}`);
  async function component(kind, target) {
    const name = `${kind}-${version}-${target}`;
    const archive = path.join(work, `${name}.tar.xz`), checksum = `${archive}.sha256`;
    const url = `https://static.rust-lang.org/dist/${name}.tar.xz`;
    await download(`${url}.sha256`, checksum);
    const expected = fs.readFileSync(checksum, 'utf8').match(/^[a-f0-9]{64}/)?.[0];
    if (!expected) throw new Error(`Invalid Rust checksum for ${name}`);
    if (!fs.existsSync(archive) && fs.existsSync(`${archive}.part`) && sha256(fs.readFileSync(`${archive}.part`)) === expected) fs.renameSync(`${archive}.part`, archive);
    if (!fs.existsSync(archive) || sha256(fs.readFileSync(archive)) !== expected) await download(url, archive);
    if (sha256(fs.readFileSync(archive)) !== expected) throw new Error(`Rust component checksum mismatch for ${name}`);
    const unpack = path.join(work, `${name}-unpack`);
    fs.mkdirSync(unpack, { recursive: true });
    run('tar', ['-xJf', archive, '-C', unpack]);
    return path.join(unpack, name, kind === 'rustc' ? 'rustc' : `rust-std-${target}`);
  }
  // Distribution-patched compilers (including Homebrew) can reject official
  // std metadata despite an identical rustc version and commit. Keep compiler
  // and every std component from the same official release, only under /tmp.
  const host = output('rustc', ['-vV']).match(/^host: (.+)$/m)?.[1];
  if (!host || !/^[a-zA-Z0-9_-]+$/.test(host)) throw new Error('Could not determine the Rust host target');
  const rustc = path.join(sysroot, 'bin', 'rustc');
  if (!fs.existsSync(rustc)) fs.cpSync(await component('rustc', host), sysroot, { recursive: true });
  for (const target of [...targets, host]) {
    const std = path.join(sysroot, 'lib', 'rustlib', target, 'lib');
    if (!fs.existsSync(std)) {
      const unpack = await component('rust-std', target);
      fs.cpSync(path.join(unpack, 'lib', 'rustlib', target), path.dirname(std), { recursive: true });
    }
  }

  const linker = path.join(sysroot, 'lib', 'rustlib', host, 'bin', 'rust-lld');
  if (!fs.existsSync(linker)) throw new Error('The official Rust toolchain is missing its bundled rust-lld linker');
  return { linker, sysroot, rustc };
}

function pack(outputDirectory, artifacts, before) {
  if (sourceState().sourceFingerprint !== before.sourceFingerprint) throw new Error('Node sources or Cargo.lock changed while building; repeat preparation');
  fs.mkdirSync(outputDirectory, { recursive: true });
  const entries = [];
  for (const [target, binary] of artifacts) {
    const bytes = fs.readFileSync(binary);
    checkElf(bytes, target);
    const file = `jarvis-node-${target}`;
    if (path.resolve(binary) !== path.join(outputDirectory, file)) atomic(path.join(outputDirectory, file), bytes, 0o755);
    entries.push({ target, file, sha256: sha256(bytes), fileFingerprint: hex(fnv1a64(bytes)) });
  }
  entries.sort((a, b) => a.target.localeCompare(b.target));
  atomic(path.join(outputDirectory, 'manifest.json'), JSON.stringify({ version: before.version, sourceFingerprint: before.sourceFingerprint, artifacts: entries }, null, 2) + '\n');
  return verify(outputDirectory, entries.map(entry => entry.target), before);
}

async function build(options, before) {
  const cross = options.native ? null : await crossToolchain(options.work, options.targets);
  const targetDirectory = path.join(options.work, 'target');
  const artifacts = [];
  for (const target of options.targets) {
    const env = { ...process.env };
    if (cross) {
      env.RUSTC = cross.rustc;
      env[`CARGO_TARGET_${target.replaceAll('-', '_').toUpperCase()}_LINKER`] = cross.linker;
      env.CARGO_ENCODED_RUSTFLAGS = ['--sysroot', cross.sysroot, '-C', 'target-feature=+crt-static', '-C', 'linker-flavor=ld.lld'].join('\x1f');
    }
    console.log(`Building jarvis-node ${target} from ${before.sourceFingerprint}`);
    fs.mkdirSync(options.work, { recursive: true });
    const log = path.join(options.work, `build-${target}.log`);
    const fd = fs.openSync(log, 'w');
    try {
      run('cargo', ['build', '--locked', '--release', '--manifest-path', path.join(REPO, 'src-tauri/Cargo.toml'), '-p', 'jarvis-node', '--target', target, '--target-dir', targetDirectory], { env, stdio: ['ignore', fd, fd] });
    } catch (error) {
      console.error(fs.readFileSync(log, 'utf8').slice(-16000));
      throw new Error(`${error.message}; complete build log: ${log}`);
    } finally { fs.closeSync(fd); }
    artifacts.push([target, path.join(targetDirectory, target, 'release', 'jarvis-node')]);
  }
  return pack(options.out, artifacts, before);
}

function merge(directory, options, before) {
  const manifests = [];
  function collect(dir, depth = 0) {
    if (depth > 3) return;
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      if (entry.isFile() && entry.name === 'manifest.json') manifests.push(dir);
      else if (entry.isDirectory()) collect(path.join(dir, entry.name), depth + 1);
    }
  }
  collect(directory);
  const artifacts = new Map();
  for (const dir of manifests) {
    for (const entry of readManifest(dir, before).artifacts) {
      if (artifacts.has(entry.target)) throw new Error(`Duplicate CI artifact for ${entry.target}`);
      artifacts.set(entry.target, path.join(dir, entry.file));
    }
  }
  for (const target of options.targets) if (!artifacts.has(target)) throw new Error(`CI artifacts missing ${target}`);
  return pack(options.out, [...artifacts], before);
}

export async function main(argv = process.argv.slice(2)) {
  const options = { out: path.join(REPO, 'src-tauri/node-binaries'), work: path.join(os.tmpdir(), 'jarvis-node-cross'), targets: [...TARGETS], native: false, verify: false, from: null };
  for (let index = 0; index < argv.length; index++) {
    const arg = argv[index];
    if (arg === '--verify') options.verify = true;
    else if (arg === '--native') options.native = true;
    else if (['--out','--work','--from','--target'].includes(arg)) {
      const value = argv[++index]; if (!value) throw new Error(`${arg} requires a value`);
      if (arg === '--target') { if (!TARGETS.includes(value)) throw new Error(`Unsupported target ${value}`); options.targets = [value]; }
      else options[arg.slice(2)] = path.resolve(value);
    } else throw new Error(`Unknown option ${arg}`);
  }
  const before = sourceState();
  const manifest = options.verify ? verify(options.out, options.targets, before) : options.from ? merge(options.from, options, before) : await build(options, before);
  console.log(`Linux node bundle ready: ${manifest.artifacts.map(entry => entry.target).join(', ')}; sources ${manifest.sourceFingerprint}`);
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch(error => { console.error(`Node bundle: ${error.message}\nRun node scripts/prepare-node-bundle.mjs to prepare current Linux nodes.`); process.exitCode = 1; });
}
