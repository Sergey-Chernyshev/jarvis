#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fixture_root="$(mktemp -d "${TMPDIR:-/tmp}/jarvis-package-lock-contract.XXXXXX")"

cleanup() {
  case "$fixture_root" in
    "${TMPDIR:-/tmp}"/jarvis-package-lock-contract.*) rm -rf -- "$fixture_root" ;;
    *) echo "refusing unexpected fixture path: $fixture_root" >&2 ;;
  esac
}
trap cleanup EXIT

write_clean_fixture() {
  rm -rf -- "$fixture_root/crates" "$fixture_root/src-tauri"
  mkdir -p "$fixture_root/crates/jarvis-package" "$fixture_root/src-tauri"
  printf '%s\n' \
    '[package]' \
    'name = "jarvis-package"' \
    'version = "0.1.0"' \
    'edition = "2021"' \
    'rust-version = "1.77.2"' \
    'publish = false' \
    '' \
    '[dependencies]' \
    'getrandom = { version = "=0.3.4", default-features = false }' \
    'tempfile = { version = "=3.27.0", default-features = false, features = ["getrandom"] }' \
    > "$fixture_root/crates/jarvis-package/Cargo.toml"
  printf '%s\n' \
    '[package]' \
    'name = "jarvis"' \
    'version = "0.3.3"' \
    '' \
    '[dependencies]' \
    'jarvis-package = { path = "../crates/jarvis-package" }' \
    > "$fixture_root/src-tauri/Cargo.toml"
  printf '%s\n' \
    'version = 4' \
    '' \
    '[[package]]' \
    'name = "jarvis-package"' \
    'version = "0.1.0"' \
    'dependencies = [' \
    '  "unicode-normalization",' \
    '  "tempfile",' \
    '  "tar",' \
    '  "sha2",' \
    '  "serde_json_canonicalizer",' \
    '  "serde_json",' \
    '  "serde",' \
    '  "rustix",' \
    '  "libc",' \
    '  "jarvis-plugin-protocol",' \
    '  "getrandom",' \
    '  "caseless",' \
    '  "base64",' \
    ']' \
    '' \
    '[[package]]' \
    'name = "getrandom"' \
    'version = "0.3.4"' \
    'source = "registry+https://github.com/rust-lang/crates.io-index"' \
    'checksum = "abcdef"' \
    '' \
    '[[package]]' \
    'name = "tempfile"' \
    'version = "3.27.0"' \
    'source = "registry+https://github.com/rust-lang/crates.io-index"' \
    'checksum = "32497e9a4c7b38532efcdebeef879707aa9f794296a4f0244f6f69e9bc8574bd"' \
    'dependencies = [' \
    ' "fastrand",' \
    ' "getrandom",' \
    ' "once_cell",' \
    ' "rustix",' \
    ' "windows-sys",' \
    ']' \
    > "$fixture_root/crates/jarvis-package/Cargo.lock"
  printf '%s\n' \
    'version = 4' \
    '' \
    '[[package]]' \
    'name = "jarvis"' \
    'version = "0.3.3"' \
    'dependencies = [' \
    ' "jarvis-package",' \
    ']' \
    '' \
    '[[package]]' \
    'name = "jarvis-package"' \
    'version = "0.1.0"' \
    '' \
    '[[package]]' \
    'name = "getrandom"' \
    'version = "0.4.2"' \
    'source = "registry+https://github.com/rust-lang/crates.io-index"' \
    'checksum = "0de51e6874e94e7bf76d726fc5d13ba782deca734ff60d5bb2fb2607c7406555"' \
    'dependencies = [' \
    ' "cfg-if",' \
    ' "libc",' \
    ' "r-efi 6.0.0",' \
    ' "wasip2",' \
    ' "wasip3",' \
    ']' \
    '' \
    '[[package]]' \
    'name = "tempfile"' \
    'version = "3.27.0"' \
    'source = "registry+https://github.com/rust-lang/crates.io-index"' \
    'checksum = "32497e9a4c7b38532efcdebeef879707aa9f794296a4f0244f6f69e9bc8574bd"' \
    'dependencies = [' \
    ' "fastrand",' \
    ' "getrandom 0.4.2",' \
    ' "once_cell",' \
    ' "rustix",' \
    ' "windows-sys 0.61.2",' \
    ']' \
    > "$fixture_root/src-tauri/Cargo.lock"
}

expect_rejected() {
  local expected="$1"
  local output
  if output="$(bash "$repo_root/scripts/check-package-lock-contract.sh" "$fixture_root" 2>&1)"; then
    echo "package lock gate accepted forbidden fixture: $expected" >&2
    exit 1
  fi
  if [[ "$output" != *"$expected"* ]]; then
    echo "package lock gate did not identify $expected" >&2
    echo "$output" >&2
    exit 1
  fi
}

write_clean_fixture
bash "$repo_root/scripts/check-package-lock-contract.sh" "$fixture_root" >/dev/null

sed -i '' '/^  "getrandom",$/d' "$fixture_root/crates/jarvis-package/Cargo.lock"
expect_rejected "private jarvis-package dependency block changed"

write_clean_fixture
sed -i '' '/^  "tempfile",$/d' "$fixture_root/crates/jarvis-package/Cargo.lock"
expect_rejected "private jarvis-package dependency block changed"

write_clean_fixture
sed -i '' '/^ "jarvis-package",$/d' "$fixture_root/src-tauri/Cargo.lock"
expect_rejected "host jarvis lock record must depend on jarvis-package"

write_clean_fixture
sed -i '' 's#path = "../crates/jarvis-package"#path = "../crates/not-jarvis-package"#' \
  "$fixture_root/src-tauri/Cargo.toml"
expect_rejected "host jarvis-package dependency must use the exact private path"

write_clean_fixture
sed -i '' 's/\[dependencies\]/[dev-dependencies]/' \
  "$fixture_root/src-tauri/Cargo.toml"
expect_rejected "host jarvis-package dependency must be a normal dependency"

write_clean_fixture
sed -i '' 's/=3.27.0/=3.24.0/' "$fixture_root/crates/jarvis-package/Cargo.toml"
expect_rejected "private tempfile dependency must be pinned to 3.27.0"

write_clean_fixture
sed -i '' 's/\[dependencies\]/[dev-dependencies]/' \
  "$fixture_root/crates/jarvis-package/Cargo.toml"
expect_rejected "private getrandom dependency must be a normal exact 0.3.4 dependency"

write_clean_fixture
sed -i '' 's/version = \"0.3.4\"/version = \"0.4.2\"/' \
  "$fixture_root/crates/jarvis-package/Cargo.lock"
expect_rejected "private lock must contain getrandom 0.3.4 and no 0.4 release"

write_clean_fixture
sed -i '' 's/version = \"3.27.0\"/version = \"3.24.0\"/' \
  "$fixture_root/src-tauri/Cargo.lock"
expect_rejected "host tempfile registry block changed"

write_clean_fixture
sed -i '' 's/32497e9a4c7b38532efcdebeef879707aa9f794296a4f0244f6f69e9bc8574bd/bad/' \
  "$fixture_root/src-tauri/Cargo.lock"
expect_rejected "host tempfile registry block changed"

write_clean_fixture
sed -i '' 's/version = \"0.4.2\"/version = \"0.4.3\"/' \
  "$fixture_root/src-tauri/Cargo.lock"
expect_rejected "host getrandom registry block changed"

write_clean_fixture
sed -i '' 's/getrandom 0.4.2/getrandom 0.3.4/' \
  "$fixture_root/src-tauri/Cargo.lock"
expect_rejected "host tempfile dependency block changed"

write_clean_fixture
capture="$fixture_root/cargo-args"
fake_cargo="$fixture_root/fake-cargo"
printf '%s\n' \
  '#!/usr/bin/env bash' \
  'printf "%s\n" "$@" > "$CAPTURE_PATH"' \
  > "$fake_cargo"
chmod +x "$fake_cargo"
CAPTURE_PATH="$capture" CARGO_BIN="$fake_cargo" \
  bash "$repo_root/scripts/generate-jarvis-package-lock.sh" "$fixture_root"
expected_args="$(
  printf '%s\n' \
    '--config' \
    'resolver.incompatible-rust-versions="fallback"' \
    'generate-lockfile' \
    '--manifest-path' \
    "$fixture_root/crates/jarvis-package/Cargo.toml"
)"
if [[ "$(cat "$capture")" != "$expected_args" ]]; then
  echo "package lock generator did not use the approved current-Cargo contract" >&2
  cat "$capture" >&2
  exit 1
fi

write_clean_fixture
sed -i '' 's/version = \"0.3.4\"/version = \"0.4.2\"/' \
  "$fixture_root/crates/jarvis-package/Cargo.lock"
if CAPTURE_PATH="$capture" CARGO_BIN="$fake_cargo" \
  bash "$repo_root/scripts/generate-jarvis-package-lock.sh" "$fixture_root" >/dev/null 2>&1; then
  echo "package lock generator accepted an incompatible generated private lock" >&2
  exit 1
fi

echo "package lock contract negative fixtures passed"
