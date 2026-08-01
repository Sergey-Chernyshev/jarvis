#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
helper_manifest="$repo_root/crates/jarvis-power-helper/Cargo.toml"
host_manifest="$repo_root/src-tauri/Cargo.toml"
helper_native="$repo_root/crates/jarvis-power-helper/native/xpc_server.m"
client_native="$repo_root/src-tauri/native/power_helper_client.m"
daemon_plist="$repo_root/src-tauri/PowerHelper/app.jarvis.monitor.power-helper.plist"
entitlements="$repo_root/src-tauri/PowerHelper/helper.entitlements.plist"

for required in \
  "$helper_native" \
  "$client_native" \
  "$daemon_plist" \
  "$entitlements"; do
  if [[ ! -f "$required" ]]; then
    echo "missing Task5 artifact: $required" >&2
    exit 1
  fi
done

cargo test --locked --manifest-path "$helper_manifest" xpc
cargo test --locked --manifest-path "$host_manifest" power::helper:: --no-default-features

test_root="$(mktemp -d "${TMPDIR:-/tmp}/jarvis-power-xpc-test.XXXXXX")"
cleanup() {
  case "$test_root" in
    "${TMPDIR:-/tmp}"/jarvis-power-xpc-test.*) rm -rf -- "$test_root" ;;
    *) echo "refusing unexpected fixture path: $test_root" >&2 ;;
  esac
}
trap cleanup EXIT

deployment_target="${MACOSX_DEPLOYMENT_TARGET:-13.0}"
if [[ "$deployment_target" != "13.0" ]]; then
  echo "Task5 native tests require MACOSX_DEPLOYMENT_TARGET=13.0" >&2
  exit 1
fi

xcrun --sdk macosx clang \
  -fobjc-arc \
  -fblocks \
  -mmacosx-version-min=13.0 \
  -I"$repo_root/crates/jarvis-power-helper/native" \
  -c "$helper_native" \
  -o "$test_root/xpc_server.o"
xcrun --sdk macosx clang \
  -fobjc-arc \
  -fblocks \
  -mmacosx-version-min=13.0 \
  -I"$repo_root/src-tauri/native" \
  -c "$client_native" \
  -o "$test_root/power_helper_client.o"

nm -u "$test_root/xpc_server.o" | rg -q '_SecCodeCreateWithXPCMessage$'
nm -u "$test_root/xpc_server.o" | rg -q '_SecCodeCheckValidity$'
nm -u "$test_root/power_helper_client.o" | rg -q '_OBJC_CLASS_\$_SMAppService$'

plutil -lint "$daemon_plist" "$entitlements" >/dev/null
node - "$daemon_plist" "$helper_native" "$client_native" <<'NODE'
const { readFileSync } = require("node:fs");

const [plistPath, helperPath, clientPath] = process.argv.slice(2);
const plist = readFileSync(plistPath, "utf8");
const helper = readFileSync(helperPath, "utf8");
const client = readFileSync(clientPath, "utf8");

const requiredPlist = [
  "<key>Label</key>",
  "<string>app.jarvis.monitor.power-helper</string>",
  "<key>BundleProgram</key>",
  "<string>Contents/Library/LaunchDaemons/app.jarvis.monitor.power-helper</string>",
  "<key>MachServices</key>",
  "<key>ThrottleInterval</key>",
  "<integer>1</integer>",
];
for (const token of requiredPlist) {
  if (!plist.includes(token)) throw new Error(`missing plist contract: ${token}`);
}
for (const forbidden of [
  "<key>Program</key>",
  "<key>ProgramArguments</key>",
  "<key>KeepAlive</key>",
  "com.apple.security.network.client",
  "com.apple.security.network.server",
]) {
  if (plist.includes(forbidden)) throw new Error(`forbidden plist contract: ${forbidden}`);
}

for (const token of [
  "SecCodeCreateWithXPCMessage",
  "kSecCSStrictValidate | kSecCSCheckAllArchitectures",
  "SecCodeCheckValidity",
  "xpc_dictionary_get_remote_connection",
  "proc_pidinfo",
]) {
  if (!helper.includes(token)) throw new Error(`missing native attestation contract: ${token}`);
}
for (const token of [
  "daemonServiceWithPlistName",
  "unregisterWithCompletionHandler",
  "xpc_connection_send_message_with_reply",
]) {
  if (!client.includes(token)) throw new Error(`missing native lifecycle contract: ${token}`);
}
NODE

echo "power-helper XPC contract checks passed"
