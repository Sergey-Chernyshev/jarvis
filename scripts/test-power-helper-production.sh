#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
build_script="$repo_root/scripts/build-power-helper.sh"
config="$repo_root/src-tauri/tauri.conf.json"
info_plist="$repo_root/src-tauri/Info.plist"
fixture_tools="$repo_root/scripts/fixtures/power-helper-signing"
helper_binary="$repo_root/crates/jarvis-power-helper/target/release/jarvis-power-helper"
minimum_build_floor=340
expected_build="$(
  node -e \
    'process.stdout.write(require(process.argv[1]).bundle.macOS.bundleVersion)' \
    "$config"
)"
expected_team=ABCDEFGHIJ

test_root="$(mktemp -d "${TMPDIR:-/tmp}/jarvis-power-production-test.XXXXXX")"
had_helper=0
if [[ -f "$helper_binary" ]]; then
  cp -p "$helper_binary" "$test_root/original-helper"
  had_helper=1
fi
cleanup() {
  if [[ "$had_helper" == 1 ]]; then
    cp -p "$test_root/original-helper" "$helper_binary"
  else
    rm -f -- "$helper_binary"
  fi
  case "$test_root" in
    "${TMPDIR:-/tmp}"/jarvis-power-production-test.*) rm -rf -- "$test_root" ;;
    *) echo "refusing unexpected fixture path: $test_root" >&2 ;;
  esac
}
trap cleanup EXIT

failures=0
record_failure() {
  echo "FAIL: $1" >&2
  failures=$((failures + 1))
}

if ! node - "$config" "$info_plist" "$minimum_build_floor" <<'NODE'
const { execFileSync } = require("node:child_process");
const { readFileSync } = require("node:fs");

const [configPath, plistPath, expected] = process.argv.slice(2);
const config = JSON.parse(readFileSync(configPath, "utf8"));
const displayVersion = config.version;
const bundleVersion = config.bundle?.macOS?.bundleVersion;
if (!/^\d+$/.test(String(bundleVersion)) || BigInt(bundleVersion) <= 0n) {
  throw new Error("bundle.macOS.bundleVersion must be a positive decimal");
}
if (BigInt(bundleVersion) < BigInt(expected)) {
  throw new Error(`bundleVersion must not regress below ${expected}`);
}
if (!/^\d+\.\d+\.\d+(?:[-+].+)?$/.test(String(displayVersion))) {
  throw new Error("display version must remain semantic");
}
if (displayVersion === bundleVersion) {
  throw new Error("semantic display version and monotonic build number are distinct");
}
const plistBuild = execFileSync(
  "/usr/libexec/PlistBuddy",
  ["-c", "Print:CFBundleVersion", plistPath],
  { encoding: "utf8" },
).trim();
if (plistBuild !== bundleVersion) {
  throw new Error(`Info.plist build ${plistBuild} != config build ${bundleVersion}`);
}
NODE
then
  record_failure "canonical app build number is missing or inconsistent"
fi

mismatch_log="$test_root/build-mismatch.log"
mismatched_build=$((expected_build + 1))
if APPLE_TEAM_ID="$expected_team" \
  JARVIS_APP_BUILD="$mismatched_build" \
  MACOSX_DEPLOYMENT_TARGET=13.0 \
  cargo check --quiet --locked \
    --manifest-path "$repo_root/crates/jarvis-power-helper/Cargo.toml" \
    --no-default-features \
    --features production-xpc \
    --bin jarvis-power-helper >"$mismatch_log" 2>&1; then
  record_failure "helper build accepted a build number different from Tauri config"
elif ! rg -F -q \
  'JARVIS_APP_BUILD must equal bundle.macOS.bundleVersion' \
  "$mismatch_log"; then
  record_failure "helper build rejected mismatched build for an unrelated reason"
fi

if ! APPLE_TEAM_ID="$expected_team" \
  JARVIS_APP_BUILD="$expected_build" \
  MACOSX_DEPLOYMENT_TARGET=13.0 \
  cargo check --quiet --locked \
    --manifest-path "$repo_root/crates/jarvis-power-helper/Cargo.toml" \
    --no-default-features \
    --features production-xpc \
    --bin jarvis-power-helper; then
  record_failure "helper build rejected the canonical app build number"
fi

run_mock_production() {
  local output="$1"
  local identity="$2"
  local signed_team="$3"
  local mode="$4"
  local log_file="$5"
  PATH="$fixture_tools:/opt/homebrew/bin:/usr/bin:/bin" \
    JARVIS_TEST_REPO_ROOT="$repo_root" \
    JARVIS_TEST_TOOL_LOG="$log_file" \
    MOCK_CODESIGN_TEAM_ID="$signed_team" \
    MOCK_CODESIGN_MODE="$mode" \
    APPLE_TEAM_ID="$expected_team" \
    JARVIS_APP_BUILD="$expected_build" \
    POWER_HELPER_SIGNING_IDENTITY="$identity" \
    POWER_HELPER_OUTPUT="$output" \
    MACOSX_DEPLOYMENT_TARGET=13.0 \
    bash "$build_script" --production
}

assert_rejected_preserves_final() {
  local case_name="$1"
  local identity="$2"
  local signed_team="$3"
  local mode="$4"
  local accepted_message="$5"
  local output="$test_root/$case_name-helper"
  local expected_output="$test_root/$case_name.expected"
  local log_file="$test_root/$case_name.log"

  printf 'existing verified artifact\0case=%s\n' "$case_name" >"$expected_output"
  cp "$expected_output" "$output"
  if run_mock_production \
    "$output" \
    "$identity" \
    "$signed_team" \
    "$mode" \
    "$log_file"; then
    record_failure "$accepted_message"
  fi
  if ! cmp -s "$expected_output" "$output"; then
    record_failure "$case_name rejection changed the previous final output"
  fi
  if find "$test_root" \
    -maxdepth 1 \
    -name ".$case_name-helper.stage.*" \
    -print \
    -quit |
    rg -q .; then
    record_failure "$case_name rejection left staging or requirement residue"
  fi
}

adhoc_log="$test_root/adhoc.log"
if run_mock_production \
  "$test_root/adhoc-helper" \
  "-" \
  "$expected_team" \
  valid \
  "$adhoc_log"; then
  record_failure "production signing accepted the ad-hoc identity"
fi

assert_rejected_preserves_final \
  "wrong-team" \
  "Developer ID Application: Wrong Team (ZZZZZZZZZZ)" \
  ZZZZZZZZZZ \
  "valid" \
  "production signing accepted a different TeamIdentifier"

assert_rejected_preserves_final \
  "wrong-chain" \
  "Developer ID Application: Jarvis Test ($expected_team)" \
  "$expected_team" \
  "wrong-chain" \
  "production signing accepted a helper with the wrong certificate chain"

assert_rejected_preserves_final \
  "requirement-mismatch" \
  "Developer ID Application: Jarvis Test ($expected_team)" \
  "$expected_team" \
  "requirement-mismatch" \
  "production signing accepted a mismatched embedded designated requirement"

assert_rejected_preserves_final \
  "post-sign-adhoc" \
  "Developer ID Application: Jarvis Test ($expected_team)" \
  "$expected_team" \
  "adhoc-display" \
  "production signing accepted post-sign ad-hoc evidence"

preserved_output="$test_root/preserved-helper"
printf 'existing verified artifact\n' >"$preserved_output"
sign_failure_log="$test_root/sign-failure.log"
if run_mock_production \
  "$preserved_output" \
  "Developer ID Application: Jarvis Test ($expected_team)" \
  "$expected_team" \
  sign-fail \
  "$sign_failure_log"; then
  record_failure "production signing reported success after signing failure"
fi
if [[ "$(cat "$preserved_output")" != "existing verified artifact" ]]; then
  record_failure "failed signing overwrote the final output before verification"
fi
if find "$test_root" -maxdepth 1 -name '.*.stage.*' -print -quit | rg -q .; then
  record_failure "failed signing left staging residue"
fi

symlink_target="$test_root/symlink-target"
printf 'do not replace through symlink\n' >"$symlink_target"
symlink_output="$test_root/symlink-helper"
ln -s "$symlink_target" "$symlink_output"
symlink_log="$test_root/symlink.log"
if run_mock_production \
  "$symlink_output" \
  "Developer ID Application: Jarvis Test ($expected_team)" \
  "$expected_team" \
  valid \
  "$symlink_log"; then
  record_failure "production publish accepted a symlink output"
fi
if [[ "$(cat "$symlink_target")" != "do not replace through symlink" ]]; then
  record_failure "production publish modified a symlink target"
fi

valid_log="$test_root/valid.log"
valid_output="$test_root/verified-helper"
if ! run_mock_production \
  "$valid_output" \
  "Developer ID Application: Jarvis Test ($expected_team)" \
  "$expected_team" \
  valid \
  "$valid_log"; then
  record_failure "exact Developer ID fixture was not published"
elif [[ ! -f "$valid_output" ]]; then
  record_failure "verified helper output is missing"
fi
for required_log in \
  "cargo JARVIS_APP_BUILD=$expected_build" \
  'csreq ' \
  'codesign --force' \
  'codesign --display' \
  'codesign --verify' \
  ' -R='; do
  if ! rg -F -q -- "$required_log" "$valid_log"; then
    record_failure "production flow omitted evidence step: $required_log"
  fi
done
if find "$test_root" -maxdepth 1 -name '.*.stage.*' -print -quit | rg -q .; then
  record_failure "successful signing left staging residue"
fi

if [[ "$failures" -ne 0 ]]; then
  echo "$failures production power-helper contract failure(s)" >&2
  exit 1
fi
echo "production power-helper build/signing contracts passed"
