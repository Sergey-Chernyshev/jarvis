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

mkdir -p \
  "$fixture_root/crates/jarvis-plugin-protocol/src" \
  "$fixture_root/plugins/agent-vm/src" \
  "$fixture_root/plugins/community/src"

write_clean_fixture() {
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

echo "plugin boundary negative fixtures passed"
