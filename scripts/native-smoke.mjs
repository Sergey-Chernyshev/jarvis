#!/usr/bin/env node
// Run a real debug Tauri bundle in a disposable, explicitly guarded profile.
// Never builds, restarts, or signals any existing Jarvis process.
// Fullscreen/foreground checks are distorted by concurrent user gestures. Use
// --wait-idle 15 --idle-timeout 60 to require 15 seconds of HID inactivity before
// preparing a fixture and recheck immediately before launch. This observes only
// IOHIDSystem's idle duration; it never records input or changes permissions.
import { mkdtemp, mkdir, readFile, writeFile, cp } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { spawn, execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const args = process.argv.slice(2);
const option = name => { const at = args.indexOf(name); return at < 0 ? undefined : args[at + 1]; };
const app = option('--app'), scenario = option('--scenario');
if (!app || !scenario || process.platform !== 'darwin') {
  console.error('Usage: node scripts/native-smoke.mjs --app /absolute/Jarvis.app --scenario /absolute/scenario.js [--editor] [--fullscreen-policy accessory|regular] [--timeout 90] [--wait-idle 0..60] [--idle-timeout 1..600]');
  process.exit(2);
}
const timeout = Number(option('--timeout') || 90);
if (!Number.isFinite(timeout) || timeout < 1 || timeout > 600) throw new Error('timeout must be between 1 and 600 seconds');
const fullscreenPolicy = option('--fullscreen-policy') || 'accessory';
if (!['accessory', 'regular'].includes(fullscreenPolicy)) throw new Error('--fullscreen-policy must be accessory or regular');
const waitIdle = args.includes('--wait-idle') ? Number(option('--wait-idle')) : 0;
const idleTimeout = args.includes('--idle-timeout') ? Number(option('--idle-timeout')) : 60;
if (!Number.isInteger(waitIdle) || waitIdle < 0 || waitIdle > 60) throw new Error('--wait-idle must be an integer between 0 and 60 seconds');
if (!Number.isInteger(idleTimeout) || idleTimeout < 1 || idleTimeout > 600) throw new Error('--idle-timeout must be an integer between 1 and 600 seconds');
const source = resolve(app);
if (!source.endsWith('.app')) throw new Error('--app must name a debug .app bundle');
// One deadline covers both gates. Preparation does not authorize a later
// launch if the user has resumed interacting in the meantime.
const idleDeadline = Date.now() + idleTimeout * 1000;
const idleFailure = (stage, status, failure) => {
  console.error(JSON.stringify({ ok:false, status, stage, appLaunched:false, failure }));
  process.exit(1);
};
async function requireIdle(stage) {
  if (waitIdle === 0) return;
  for (;;) {
    let output;
    try {
      output = execFileSync('/usr/sbin/ioreg', ['-r', '-c', 'IOHIDSystem', '-d', '1', '-l'], {
        encoding:'utf8', stdio:['ignore', 'pipe', 'pipe'], timeout:3000, maxBuffer:2 * 1024 * 1024,
      });
    } catch {
      idleFailure(stage, 'idle-check-failed', 'Could not read IOHIDSystem HIDIdleTime; native UI was not launched');
    }
    const match = output.match(/"HIDIdleTime"\s*=\s*(\d+)\b/);
    if (!match) idleFailure(stage, 'idle-check-failed', 'IOHIDSystem did not expose HIDIdleTime; native UI was not launched');
    const idleSeconds = Number(BigInt(match[1])) / 1e9;
    if (!Number.isFinite(idleSeconds)) idleFailure(stage, 'idle-check-failed', 'IOHIDSystem returned an invalid idle duration; native UI was not launched');
    if (idleSeconds >= waitIdle) return;
    const remaining = idleDeadline - Date.now();
    if (remaining <= 0) idleFailure(stage, 'busy', `Input stayed active: required ${waitIdle}s of HID inactivity within ${idleTimeout}s; native UI was not launched`);
    await new Promise(resolve => setTimeout(resolve, Math.min(1000, remaining)));
  }
}
await requireIdle('before-preparation');
const root = await mkdtemp(join(tmpdir(), 'jarvis-native-smoke-'));
const data = join(root, 'data');
await mkdir(data, { mode: 0o700 });
await writeFile(join(root, 'marker.json'), JSON.stringify({ kind:'jarvis-native-smoke', version:1 }), { mode: 0o600 });
await writeFile(join(root, 'scenario.js'), await readFile(resolve(scenario)), { mode: 0o600 });
await writeFile(join(data, 'settings.json'), JSON.stringify({
  mode:'window', theme:'dark', autoUpdate:false, diagnostics:true, nativeSmokeFullscreenPolicy:fullscreenPolicy,
  voice:{ mute:true, duckOthers:false }, stt:{ engine:'whisper-turbo', mute:true }, wake:{ enabled:false }
}), { mode: 0o600 });

// A separate bundle identity keeps native window discovery and TCC separate.
// cp copies regular files; modifying/signing this copy never rewrites the source.
const bundle = join(root, 'JarvisNativeSmoke.app');
await cp(source, bundle, { recursive:true });
const plist = join(bundle, 'Contents', 'Info.plist');
const plistBuddy = '/usr/libexec/PlistBuddy';
const executable = execFileSync(plistBuddy, ['-c', 'Print :CFBundleExecutable', plist], { encoding:'utf8' }).trim();
if (!/^[A-Za-z0-9_.-]+$/.test(executable)) throw new Error('Unexpected bundle executable name');
execFileSync(plistBuddy, ['-c', `Set :CFBundleIdentifier app.jarvis.native-smoke.${process.pid}`, plist]);
execFileSync(plistBuddy, ['-c', 'Set :CFBundleName JarvisNativeSmoke', plist]);
const repo = resolve(dirname(fileURLToPath(import.meta.url)), '..');
if (args.includes('--editor')) {
  execFileSync('/usr/bin/clang', ['-fobjc-arc', '-framework', 'AppKit', '-framework', 'CoreGraphics', join(repo, 'scripts/qa/native-editor.m'), '-o', join(root, 'qa-editor')], { stdio:'pipe' });
}
execFileSync('/usr/bin/codesign', ['--force', '--deep', '--sign', '-', '--entitlements', join(repo, 'src-tauri', 'entitlements.plist'), bundle], { stdio:'pipe' });

const output = [];
await requireIdle('before-launch');
const child = spawn(join(bundle, 'Contents', 'MacOS', executable), ['--native-smoke', root], {
  env:{ ...process.env, JARVIS_DIR:data, JARVIS_SOCK:join(data, 'run.sock'), JARVIS_DEV:'1' },
  stdio:['ignore', 'pipe', 'pipe'],
});
console.log(`Native smoke PID ${child.pid}; profile ${root}`);
// A companion fixture can abort its wrapper; reap only this wrapper's child.
process.on('SIGTERM', () => child.kill('SIGTERM'));
process.on('SIGINT', () => child.kill('SIGTERM'));
child.stdout.on('data', chunk => output.push(chunk));
child.stderr.on('data', chunk => output.push(chunk));
let timedOut = false;
const timer = setTimeout(() => { timedOut = true; child.kill('SIGTERM'); setTimeout(() => child.kill('SIGKILL'), 3000).unref(); }, timeout * 1000);
const outcome = await new Promise((accept, reject) => { child.once('error', reject); child.once('exit', (code, signal) => accept({ code, signal })); });
clearTimeout(timer);
await writeFile(join(root, 'process.log'), Buffer.concat(output));
let report;
try { report = JSON.parse(await readFile(join(root, 'report.json'), 'utf8')); }
catch { report = { ok:false, failure:timedOut ? `Native smoke timed out after ${timeout}s` : 'Native app exited without a report', outcome }; await writeFile(join(root, 'report.json'), JSON.stringify(report,null,2)); }
console.log(JSON.stringify({ profile:root, report:join(root,'report.json'), ok:report.ok, steps:report.steps, errors:report.errors, failure:report.failure, outcome }, null, 2));
process.exitCode = report.ok && outcome.code === 0 ? 0 : 1;
