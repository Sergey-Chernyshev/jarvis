#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fixture_root="$(mktemp -d "${TMPDIR:-/tmp}/jarvis-plugin-boundary.XXXXXX")"

cleanup() {
  case "$fixture_root" in
    "${TMPDIR:-/tmp}"/jarvis-plugin-boundary.*) rm -rf -- "$fixture_root" ;;
    *) echo "refusing unexpected fixture path: $fixture_root" >&2 ;;
  esac
}
trap cleanup EXIT

write_clean_fixture() {
  rm -rf -- \
    "$fixture_root/crates" \
    "$fixture_root/plugins" \
    "$fixture_root/src-tauri"
  mkdir -p \
    "$fixture_root/crates/jarvis-package/src" \
    "$fixture_root/crates/jarvis-plugin-protocol/src" \
    "$fixture_root/plugins/agent-vm/src" \
    "$fixture_root/plugins/community/src" \
    "$fixture_root/src-tauri"
  printf '%s\n' \
    '[package]' \
    'name = "jarvis-plugin-protocol"' \
    'version = "0.1.0"' \
    > "$fixture_root/crates/jarvis-plugin-protocol/Cargo.toml"
  printf '%s\n' '#![forbid(unsafe_code)]' \
    > "$fixture_root/crates/jarvis-plugin-protocol/src/lib.rs"
  printf '%s\n' \
    '[package]' \
    'name = "jarvis-agent-vm-plugin"' \
    'version = "0.1.0"' \
    '[dependencies]' \
    'jarvis-secret-store = { path = "../../crates/jarvis-secret-store" }' \
    > "$fixture_root/plugins/agent-vm/Cargo.toml"
  printf '%s\n' \
    '[package]' \
    'name = "community-plugin"' \
    'version = "0.1.0"' \
    > "$fixture_root/plugins/community/Cargo.toml"
  printf '%s\n' \
    '[package]' \
    'name = "jarvis-package"' \
    'version = "0.1.0"' \
    'edition = "2021"' \
    'rust-version = "1.77.2"' \
    'publish = false' \
    > "$fixture_root/crates/jarvis-package/Cargo.toml"
  printf '%s\n' \
    '#![deny(unsafe_code)]' \
    '' \
    '#[cfg(target_os = "macos")]' \
    '#[allow(unsafe_code)]' \
    'mod macos_dir;' \
    > "$fixture_root/crates/jarvis-package/src/lib.rs"
  printf '%s\n' \
    'pub(crate) fn read() {' \
    '    unsafe { std::ptr::read_volatile(&0_u8); }' \
    '}' \
    > "$fixture_root/crates/jarvis-package/src/macos_dir.rs"
  printf '%s\n' \
    '[package]' \
    'name = "jarvis-host"' \
    'version = "0.1.0"' \
    '[dependencies]' \
    'jarvis-package = { path = "../crates/jarvis-package" }' \
    > "$fixture_root/src-tauri/Cargo.toml"
}

expect_rejected() {
  local expected="$1"
  local output
  if output="$(bash "$repo_root/scripts/check-plugin-boundaries.sh" "$fixture_root" 2>&1)"; then
    echo "boundary gate accepted forbidden fixture: $expected" >&2
    exit 1
  fi
  if [[ "$output" != *"$expected"* ]]; then
    echo "boundary gate did not identify $expected" >&2
    echo "$output" >&2
    exit 1
  fi
}

write_clean_fixture
bash "$repo_root/scripts/check-plugin-boundaries.sh" "$fixture_root" >/dev/null

printf '%s\n' \
  '[package]' \
  'name = "jarvis-plugin-protocol"' \
  'version = "0.1.0"' \
  '[dependencies]' \
  "core = { path = '../../src-tauri' }" \
  > "$fixture_root/crates/jarvis-plugin-protocol/Cargo.toml"
expect_rejected "src-tauri"

write_clean_fixture
printf '%s\n' \
  '[package]' \
  'name = "community-plugin"' \
  'version = "0.1.0"' \
  '[dependencies]' \
  'store = { package = "jarvis-secret-store", path = "../../crates/jarvis-secret-store" }' \
  > "$fixture_root/plugins/community/Cargo.toml"
expect_rejected "jarvis-secret-store"

write_clean_fixture
printf '%s\n' \
  '[package]' \
  'name = "community-plugin"' \
  'version = "0.1.0"' \
  '[dependencies.store]' \
  'package = "jarvis-secret-store"' \
  'path = "../../crates/jarvis-secret-store"' \
  > "$fixture_root/plugins/community/Cargo.toml"
expect_rejected "jarvis-secret-store"

write_clean_fixture
printf '%s\n' \
  '[package]' \
  'name = "jarvis-plugin-protocol"' \
  'version = "0.1.0"' \
  '[dependencies]' \
  'core = { package = "jarvis", version = "0.1" }' \
  > "$fixture_root/crates/jarvis-plugin-protocol/Cargo.toml"
expect_rejected "Jarvis Core"

write_clean_fixture
printf '%s\n' \
  '[package]' \
  'name = "jarvis-plugin-protocol"' \
  'version = "0.1.0"' \
  '[dependencies]' \
  'package-engine = { package = "jarvis-package", path = "../jarvis-package" }' \
  > "$fixture_root/crates/jarvis-plugin-protocol/Cargo.toml"
expect_rejected "public or plugin crate depends on jarvis-package"

write_clean_fixture
printf '%s\n' \
  '[package]' \
  'name = "jarvis-plugin-protocol"' \
  'version = "0.1.0"' \
  '[dependencies]' \
  '"jarvis-package" = "0.1.0"' \
  > "$fixture_root/crates/jarvis-plugin-protocol/Cargo.toml"
expect_rejected "public or plugin crate depends on jarvis-package"

write_clean_fixture
printf '%s\n' \
  '[package]' \
  'name = "community-plugin"' \
  'version = "0.1.0"' \
  '[dependencies.package-engine]' \
  'package = "jarvis-package"' \
  'path = "../../crates/jarvis-package"' \
  > "$fixture_root/plugins/community/Cargo.toml"
expect_rejected "public or plugin crate depends on jarvis-package"

write_clean_fixture
mkdir -p "$fixture_root/crates/internal-tool/src"
printf '%s\n' \
  '[package]' \
  'name = "internal-tool"' \
  'version = "0.1.0"' \
  '[dependencies]' \
  'engine = { path = "../jarvis-package", package = "jarvis-package" }' \
  > "$fixture_root/crates/internal-tool/Cargo.toml"
expect_rejected "only src-tauri may depend on jarvis-package"

write_clean_fixture
sed -i '' 's/publish = false/publish = true/' \
  "$fixture_root/crates/jarvis-package/Cargo.toml"
expect_rejected "jarvis-package must set publish = false"

write_clean_fixture
printf '%s\n' '#[allow(unsafe_code)]' \
  >> "$fixture_root/crates/jarvis-package/src/lib.rs"
expect_rejected "jarvis-package unsafe allow must be exactly scoped"

write_clean_fixture
printf '%s\n' \
  '#![allow(unsafe_code)]' \
  'pub fn escaped() { unsafe { std::ptr::read_volatile(&0_u8); } }' \
  > "$fixture_root/crates/jarvis-package/src/escaped.rs"
expect_rejected "jarvis-package unsafe syntax outside macos_dir.rs"

write_clean_fixture
mkdir -p "$fixture_root/crates/jarvis-package/tests"
printf '%s\n' \
  '#[test]' \
  'fn escaped() { unsafe { std::ptr::read_volatile(&0_u8); } }' \
  > "$fixture_root/crates/jarvis-package/tests/escaped.rs"
expect_rejected "jarvis-package unsafe syntax outside macos_dir.rs"

write_clean_fixture
printf '%s\n' \
  'fn main() { unsafe { std::ptr::read_volatile(&0_u8); } }' \
  > "$fixture_root/crates/jarvis-package/build.rs"
expect_rejected "jarvis-package unsafe syntax outside macos_dir.rs"

echo "plugin boundary negative fixtures passed"
