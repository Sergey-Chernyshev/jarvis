import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import crypto from 'node:crypto';
import { TARGETS, fnv1a64, sourceState, checkElf, readManifest, verify } from './prepare-node-bundle.mjs';

const SOURCES = {
  'src-tauri/Cargo.lock': 'version = 4\n',
  'src-tauri/node/Cargo.toml': '[package]\nname = "jarvis-node"\nversion = "9.8.7"\n',
  'src-tauri/node/src/main.rs': 'mod worker;\nfn main() {}\n',
  'src-tauri/node/src/nested/worker.rs': 'pub const LABEL: &str = "привет";\n',
  'src-tauri/shared/Cargo.toml': '[package]\nname = "jarvis-node-shared"\nversion = "9.8.7"\n',
  'src-tauri/shared/src/lib.rs': 'pub mod codex_hooks;\npub mod terminal_stream;\n',
  'src-tauri/shared/src/codex_hooks.rs': 'pub const HOOK: u8 = 1;\n',
  'src-tauri/shared/src/terminal_stream.rs': 'pub const STREAM: u8 = 1;\n',
};
// Independently calculated standard FNV-1a vector: sorted UTF-8 path, NUL,
// file bytes, NUL for each source, exactly as build_node_bundle.rs consumes it.
const EXPECTED_STATE = { version: '9.8.7', sourceFingerprint: 'c71c1356ce2fc0ed', files: Object.keys(SOURCES).sort() };
const sha256 = bytes => crypto.createHash('sha256').update(bytes).digest('hex');
const fingerprint = bytes => fnv1a64(bytes).toString(16).padStart(16, '0');

function temporary(t) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "jarvis-bundle-test's "));
  t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
  return directory;
}
function sourceFixture(t) {
  const root = temporary(t);
  // Reverse insertion order prevents directory enumeration from defining the hash.
  for (const [relative, body] of Object.entries(SOURCES).reverse()) {
    const file = path.join(root, relative);
    fs.mkdirSync(path.dirname(file), { recursive: true }); fs.writeFileSync(file, body);
  }
  return root;
}

// ELF64 little-endian executable with a real-sized PT_LOAD table. These are
// inert fixtures; no executable or external build tool is ever launched.
function elf(target, { dynamic = false } = {}) {
  const bytes = Buffer.alloc(384);
  bytes.set([0x7f, 0x45, 0x4c, 0x46, 2, 1, 1]);
  bytes.writeUInt16LE(2, 16); // ET_EXEC
  bytes.writeUInt16LE(target.startsWith('x86_64') ? 62 : 183, 18);
  bytes.writeUInt32LE(1, 20);
  bytes.writeBigUInt64LE(0x400100n, 24);
  bytes.writeBigUInt64LE(64n, 32);
  bytes.writeUInt16LE(64, 52);
  bytes.writeUInt16LE(56, 54);
  bytes.writeUInt16LE(dynamic ? 2 : 1, 56);
  bytes.writeUInt32LE(1, 64); // PT_LOAD, executable/readable
  bytes.writeUInt32LE(5, 68);
  bytes.writeBigUInt64LE(0x400000n, 80);
  bytes.writeBigUInt64LE(0x400000n, 88);
  bytes.writeBigUInt64LE(BigInt(bytes.length), 96);
  bytes.writeBigUInt64LE(BigInt(bytes.length), 104);
  bytes.writeBigUInt64LE(4096n, 112);
  if (dynamic) {
    const interpreter = Buffer.from('/lib/ld-musl-fixture.so.1\0');
    bytes.writeUInt32LE(3, 120); // PT_INTERP in the second header
    bytes.writeUInt32LE(4, 124);
    bytes.writeBigUInt64LE(256n, 128);
    bytes.writeBigUInt64LE(BigInt(interpreter.length), 152);
    bytes.writeBigUInt64LE(BigInt(interpreter.length), 160);
    bytes.writeBigUInt64LE(1n, 168);
    interpreter.copy(bytes, 256);
  }
  return bytes;
}

function bundleFixture(t, targets = TARGETS) {
  const directory = temporary(t);
  const manifest = { version: EXPECTED_STATE.version, sourceFingerprint: EXPECTED_STATE.sourceFingerprint, artifacts: [] };
  for (const target of targets) {
    const bytes = elf(target), file = `jarvis-node-${target}`;
    fs.writeFileSync(path.join(directory, file), bytes);
    manifest.artifacts.push({ target, file, sha256: sha256(bytes), fileFingerprint: fingerprint(bytes) });
  }
  const writeManifest = () => fs.writeFileSync(path.join(directory, 'manifest.json'), JSON.stringify(manifest));
  const replaceArtifact = (index, bytes) => {
    const entry = manifest.artifacts[index];
    fs.writeFileSync(path.join(directory, entry.file), bytes);
    entry.sha256 = sha256(bytes); entry.fileFingerprint = fingerprint(bytes); writeManifest();
  };
  writeManifest();
  return { directory, manifest, writeManifest, replaceArtifact };
}

test('FNV uses the standard offset and known UTF-8 input vector', () => {
  assert.equal(fnv1a64(Buffer.alloc(0)), 0xcbf29ce484222325n);
  assert.equal(fnv1a64(Buffer.from('hello')), 0xa430d84680aabd0bn);
});

test('sourceState matches the sorted path/NUL/bytes/NUL build fingerprint', t => {
  const root = sourceFixture(t);
  fs.writeFileSync(path.join(root, 'src-tauri/node/src/ignored.md'), 'not a Rust input');
  assert.deepEqual(sourceState(root), EXPECTED_STATE);
});

test('every source, Cargo.lock and source path participates in invalidation', t => {
  const root = sourceFixture(t);
  for (const relative of EXPECTED_STATE.files) {
    const file = path.join(root, relative);
    fs.appendFileSync(file, '\n');
    assert.notEqual(sourceState(root).sourceFingerprint, EXPECTED_STATE.sourceFingerprint, relative);
    fs.writeFileSync(file, SOURCES[relative]);
  }
  fs.renameSync(path.join(root, 'src-tauri/node/src/nested/worker.rs'), path.join(root, 'src-tauri/node/src/nested/renamed.rs'));
  assert.notEqual(sourceState(root).sourceFingerprint, EXPECTED_STATE.sourceFingerprint, 'same content at a different path');
});

test('sourceState rejects symlinked Rust input and a missing package version', t => {
  const root = sourceFixture(t);
  const alias = path.join(root, 'src-tauri/node/src/alias.rs');
  fs.symlinkSync(path.join(root, 'src-tauri/node/src/main.rs'), alias);
  assert.throws(() => sourceState(root), /source must not be a symlink/);
  fs.unlinkSync(alias);
  fs.writeFileSync(path.join(root, 'src-tauri/node/Cargo.toml'), '[package]\nname = "jarvis-node"\n');
  assert.throws(() => sourceState(root), /Could not read jarvis-node version/);
});

test('matching static artifacts for both required targets pass manifest and bundle verification', t => {
  const f = bundleFixture(t);
  assert.deepEqual(readManifest(f.directory, EXPECTED_STATE), f.manifest);
  assert.deepEqual(verify(f.directory, TARGETS, EXPECTED_STATE), f.manifest);
  for (const target of TARGETS) assert.doesNotThrow(() => checkElf(elf(target), target));
});

test('malformed JSON and malformed manifest shapes fail closed', t => {
  const directory = temporary(t), file = path.join(directory, 'manifest.json');
  for (const malformed of ['{', 'null', '[]', '{}']) {
    fs.writeFileSync(file, malformed);
    assert.throws(() => readManifest(directory, EXPECTED_STATE), undefined, malformed);
  }
});

test('manifest version and source fingerprint independently reject stale artifacts', t => {
  const f = bundleFixture(t);
  for (const field of ['version', 'sourceFingerprint']) {
    const previous = f.manifest[field]; f.manifest[field] = 'stale'; f.writeManifest();
    assert.throws(() => readManifest(f.directory, EXPECTED_STATE), /Stale node artifacts/, field);
    f.manifest[field] = previous;
  }
});

test('invalid, empty, duplicate and path-traversing artifact entries are rejected', t => {
  const f = bundleFixture(t), valid = structuredClone(f.manifest.artifacts);
  const variants = [
    [[], /Empty artifact manifest/],
    [{}, /Empty artifact manifest/],
    [[valid[0], valid[0]], /Invalid artifact entry/],
    [[{ ...valid[0], target: 'x86_64-apple-darwin' }], /Invalid artifact entry/],
    [[{ ...valid[0], file: '../outside' }], /Invalid artifact entry/],
  ];
  for (const [entries, error] of variants) {
    f.manifest.artifacts = entries; f.writeManifest();
    assert.throws(() => readManifest(f.directory, EXPECTED_STATE), error);
  }
});

test('tampered artifact bytes and mismatched optional FNV fingerprints are rejected', t => {
  const f = bundleFixture(t), entry = f.manifest.artifacts[0], file = path.join(f.directory, entry.file);
  const original = fs.readFileSync(file), tampered = Buffer.from(original);
  tampered[tampered.length - 1] ^= 1; fs.writeFileSync(file, tampered);
  assert.throws(() => readManifest(f.directory, EXPECTED_STATE), /checksum mismatch/);
  fs.writeFileSync(file, original); entry.fileFingerprint = '0000000000000000'; f.writeManifest();
  assert.throws(() => readManifest(f.directory, EXPECTED_STATE), /fingerprint mismatch/);
  delete entry.fileFingerprint; f.writeManifest();
  assert.doesNotThrow(() => readManifest(f.directory, EXPECTED_STATE));
});

test('a valid checksum cannot disguise an artifact for the wrong architecture', t => {
  const f = bundleFixture(t);
  f.replaceArtifact(0, elf(TARGETS[1]));
  assert.throws(() => readManifest(f.directory, EXPECTED_STATE), /not a 64-bit Linux ELF/);
});

test('a valid checksum cannot disguise a dynamic ELF as a static musl node', t => {
  const f = bundleFixture(t);
  f.replaceArtifact(0, elf(TARGETS[0], { dynamic: true }));
  assert.throws(() => readManifest(f.directory, EXPECTED_STATE), /must be static.*ELF interpreter/);
});

test('missing artifacts and symlinks cannot satisfy a manifest entry', t => {
  const f = bundleFixture(t), file = path.join(f.directory, f.manifest.artifacts[0].file);
  fs.unlinkSync(file);
  assert.throws(() => readManifest(f.directory, EXPECTED_STATE), /ENOENT/);
  const target = path.join(f.directory, 'actual-artifact'); fs.writeFileSync(target, elf(TARGETS[0])); fs.symlinkSync(target, file);
  assert.throws(() => readManifest(f.directory, EXPECTED_STATE), /must be a regular file/);
});

test('a valid partial CI manifest fails verification when a required target is missing', t => {
  const f = bundleFixture(t, [TARGETS[0]]);
  assert.deepEqual(verify(f.directory, [TARGETS[0]], EXPECTED_STATE), f.manifest);
  assert.throws(() => verify(f.directory, TARGETS, EXPECTED_STATE), /Missing bundled target aarch64-unknown-linux-musl/);
});

test('ELF verification rejects unsupported, truncated, wrong-class and wrong-endian files', () => {
  assert.throws(() => checkElf(elf(TARGETS[0]), 'x86_64-unknown-linux-gnu'), /Unsupported bundled target/);
  for (const edit of [bytes => bytes.subarray(0, 63), bytes => { bytes[0] = 0; return bytes; }, bytes => { bytes[4] = 1; return bytes; }, bytes => { bytes[5] = 2; return bytes; }]) {
    assert.throws(() => checkElf(edit(elf(TARGETS[0])), TARGETS[0]), /not a 64-bit Linux ELF/);
  }
});

test('ELF program header bounds are checked before any header is read', () => {
  for (const edit of [
    bytes => bytes.writeUInt16LE(0, 56),
    bytes => bytes.writeUInt16LE(55, 54),
    bytes => bytes.writeUInt16LE(100, 56),
    bytes => bytes.writeBigUInt64LE(380n, 32),
    bytes => bytes.writeBigUInt64LE(9007199254740992n, 32),
  ]) {
    const bytes = elf(TARGETS[0]); edit(bytes);
    assert.throws(() => checkElf(bytes, TARGETS[0]), /Invalid ELF program headers/);
  }
});
