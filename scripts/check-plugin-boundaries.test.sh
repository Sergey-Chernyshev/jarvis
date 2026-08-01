#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fixture_root="$(mktemp -d "${TMPDIR:-/tmp}/jarvis-plugin-boundary.XXXXXX")"
cargo_bin="${CARGO_BIN:-cargo}"

cleanup() {
  case "$fixture_root" in
    "${TMPDIR:-/tmp}"/jarvis-plugin-boundary.*) rm -rf -- "$fixture_root" ;;
    *) echo "refusing unexpected fixture path: $fixture_root" >&2 ;;
  esac
}
trap cleanup EXIT

run_fixture_boundary() {
  JARVIS_BOUNDARY_ALLOW_UNLOCKED_FIXTURES=1 \
    CARGO_BIN="$cargo_bin" \
    bash "$repo_root/scripts/check-plugin-boundaries.sh" "$fixture_root"
}

write_clean_fixture() {
  rm -rf -- \
    "$fixture_root/cargo-target" \
    "$fixture_root/crates" \
    "$fixture_root/external-source" \
    "$fixture_root/plugins" \
    "$fixture_root/src-tauri"
  mkdir -p \
    "$fixture_root/crates/jarvis-package/src" \
    "$fixture_root/crates/jarvis-plugin-protocol/src" \
    "$fixture_root/plugins/agent-vm/src" \
    "$fixture_root/plugins/community/src" \
    "$fixture_root/src-tauri/src"
  printf '%s\n' 'target/' > "$fixture_root/.gitignore"
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
  printf '%s\n' 'pub fn agent_vm() {}' \
    > "$fixture_root/plugins/agent-vm/src/lib.rs"
  printf '%s\n' \
    '[package]' \
    'name = "community-plugin"' \
    'version = "0.1.0"' \
    > "$fixture_root/plugins/community/Cargo.toml"
  printf '%s\n' 'pub fn community() {}' \
    > "$fixture_root/plugins/community/src/lib.rs"
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
    'pub trait PackageTrustVerifier {}' \
    'struct ProductionVerifier;' \
    '' \
    '#[cfg(target_os = "macos")]' \
    '#[allow(unsafe_code)]' \
    'mod macos_dir;' \
    '' \
    '#[cfg(test)]' \
    'mod tests {' \
    '    struct FixtureVerifier;' \
    '    impl super::PackageTrustVerifier for FixtureVerifier {}' \
    '}' \
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
    'edition = "2021"' \
    '[dependencies]' \
    'jarvis-package = { path = "../crates/jarvis-package" }' \
    > "$fixture_root/src-tauri/Cargo.toml"
  printf '%s\n' 'pub fn host() {}' \
    > "$fixture_root/src-tauri/src/lib.rs"
}

expect_rejected() {
  local expected="$1"
  local output
  if output="$(run_fixture_boundary 2>&1)"; then
    echo "boundary gate accepted forbidden fixture: $expected" >&2
    exit 1
  fi
  if [[ "$output" != *"$expected"* ]]; then
    echo "boundary gate did not identify $expected" >&2
    echo "$output" >&2
    exit 1
  fi
}

expect_cargo_accepts_private_source() {
  RUSTFLAGS="-Awarnings" \
    CARGO_TARGET_DIR="$fixture_root/cargo-target" \
    "$cargo_bin" check --quiet --offline \
      --manifest-path "$fixture_root/crates/jarvis-package/Cargo.toml"
}

expect_cargo_accepts_host_source() {
  RUSTFLAGS="-Awarnings" \
    CARGO_TARGET_DIR="$fixture_root/cargo-target" \
    "$cargo_bin" check --quiet --offline \
      --manifest-path "$fixture_root/src-tauri/Cargo.toml"
}

expect_cargo_dependency() {
  local manifest="$1"
  "$cargo_bin" read-manifest --manifest-path "$manifest" \
    | node -e '
      let source = "";
      process.stdin.setEncoding("utf8");
      process.stdin.on("data", (chunk) => { source += chunk; });
      process.stdin.on("end", () => {
        const packageRecord = JSON.parse(source);
        if (!packageRecord.dependencies.some(
          (dependency) => dependency.name === "jarvis-package",
        )) {
          process.exit(1);
        }
      });
    '
}

write_clean_fixture
run_fixture_boundary >/dev/null

write_clean_fixture
printf '%s\n' \
  'impl PackageTrustVerifier for ProductionVerifier {}' \
  >> "$fixture_root/crates/jarvis-package/src/lib.rs"
expect_rejected "PackageTrustVerifier production implementation outside host trust adapter"

write_clean_fixture
printf '%s\n' \
  '#[cfg(not(test))]' \
  'mod production {' \
  '    struct HiddenVerifier;' \
  '    impl super::PackageTrustVerifier for HiddenVerifier {}' \
  '}' \
  >> "$fixture_root/crates/jarvis-package/src/lib.rs"
expect_rejected "PackageTrustVerifier production implementation outside host trust adapter"

write_clean_fixture
printf '%s\n' \
  '#[cfg(any(test, feature = "permissive"))]' \
  'mod production {' \
  '    struct FeatureVerifier;' \
  '    impl super::PackageTrustVerifier for FeatureVerifier {}' \
  '}' \
  >> "$fixture_root/crates/jarvis-package/src/lib.rs"
expect_rejected "PackageTrustVerifier production implementation outside host trust adapter"

write_clean_fixture
mkdir -p "$fixture_root/src-tauri/src/plugins"
printf '%s\n' \
  'struct WrongVerifier;' \
  'impl PackageTrustVerifier for WrongVerifier {}' \
  > "$fixture_root/src-tauri/src/plugins/wrong.rs"
expect_rejected "PackageTrustVerifier production implementation outside host trust adapter"

write_clean_fixture
mkdir -p "$fixture_root/src-tauri/src/plugins"
printf '%s\n' \
  'use jarvis_package::PackageTrustVerifier as Trust;' \
  'struct AliasedVerifier;' \
  'impl Trust for AliasedVerifier {}' \
  > "$fixture_root/src-tauri/src/plugins/wrong.rs"
expect_rejected "PackageTrustVerifier production implementation outside host trust adapter"

write_clean_fixture
printf '%s\n' \
  'use jarvis_package::PackageTrustVerifier as r#VerifierAlias;' \
  'struct RawAliasedVerifier;' \
  'impl r#VerifierAlias for RawAliasedVerifier {}' \
  > "$fixture_root/src-tauri/src/lib.rs"
expect_cargo_accepts_host_source
expect_rejected "PackageTrustVerifier production implementation outside host trust adapter"

write_clean_fixture
printf '%s\n' \
  'use jarvis_package::PackageTrustVerifier as Vérifier;' \
  'struct CanonicallyAliasedVerifier;' \
  'impl Vérifier for CanonicallyAliasedVerifier {}' \
  > "$fixture_root/src-tauri/src/lib.rs"
expect_cargo_accepts_host_source
expect_rejected "PackageTrustVerifier production implementation outside host trust adapter"

write_clean_fixture
printf '%s\n' \
  'use jarvis_package::PackageTrustVerifier as Vérifier;' \
  'trait Verifier {}' \
  'struct NonEquivalentVerifier;' \
  'impl Verifier for NonEquivalentVerifier {}' \
  > "$fixture_root/src-tauri/src/lib.rs"
expect_cargo_accepts_host_source
run_fixture_boundary >/dev/null

write_clean_fixture
mkdir -p "$fixture_root/src-tauri/src/plugins"
printf '%s\n' \
  'macro_rules! implement_verifier {' \
  '    ($trait:path, $type:ty) => { impl $trait for $type {} };' \
  '}' \
  'struct MacroVerifier;' \
  'implement_verifier!(PackageTrustVerifier, MacroVerifier);' \
  > "$fixture_root/src-tauri/src/plugins/wrong.rs"
expect_rejected "PackageTrustVerifier production implementation outside host trust adapter"

write_clean_fixture
mkdir -p "$fixture_root/src-tauri/src/plugins/trust"
printf '%s\n' \
  'pub use jarvis_package::PackageTrustVerifier as CrossRootTrust;' \
  > "$fixture_root/src-tauri/src/plugins/trust/package.rs"
printf '%s\n' \
  'struct CrossRootAliasedVerifier;' \
  'impl CrossRootTrust for CrossRootAliasedVerifier {}' \
  > "$fixture_root/plugins/community/src/lib.rs"
expect_rejected "PackageTrustVerifier production implementation outside host trust adapter"

write_clean_fixture
mkdir -p "$fixture_root/src-tauri/src/plugins/trust"
printf '%s\n' \
  'mod plugins;' \
  > "$fixture_root/src-tauri/src/lib.rs"
printf '%s\n' \
  'pub mod trust;' \
  'mod wrong;' \
  > "$fixture_root/src-tauri/src/plugins/mod.rs"
printf '%s\n' \
  'pub mod package;' \
  > "$fixture_root/src-tauri/src/plugins/trust/mod.rs"
printf '%s\n' \
  '#[macro_export]' \
  'macro_rules! r#type {' \
  '    ($target:ty) => { impl jarvis_package::PackageTrustVerifier for $target {} };' \
  '}' \
  > "$fixture_root/src-tauri/src/plugins/trust/package.rs"
printf '%s\n' \
  'struct RawMacroVerifier;' \
  'crate::r#type!(RawMacroVerifier);' \
  > "$fixture_root/src-tauri/src/plugins/wrong.rs"
expect_cargo_accepts_host_source
expect_rejected "PackageTrustVerifier production implementation outside host trust adapter"

write_clean_fixture
mkdir -p "$fixture_root/src-tauri/src/plugins/trust"
printf '%s\n' \
  'mod plugins;' \
  > "$fixture_root/src-tauri/src/lib.rs"
printf '%s\n' \
  'pub mod trust;' \
  'mod wrong;' \
  > "$fixture_root/src-tauri/src/plugins/mod.rs"
printf '%s\n' \
  'pub mod package;' \
  > "$fixture_root/src-tauri/src/plugins/trust/mod.rs"
printf '%s\n' \
  '#[macro_export]' \
  'macro_rules! vérifier {' \
  '    ($target:ty) => { impl jarvis_package::PackageTrustVerifier for $target {} };' \
  '}' \
  > "$fixture_root/src-tauri/src/plugins/trust/package.rs"
printf '%s\n' \
  'struct CanonicalMacroVerifier;' \
  'crate::vérifier!(CanonicalMacroVerifier);' \
  > "$fixture_root/src-tauri/src/plugins/wrong.rs"
expect_cargo_accepts_host_source
expect_rejected "PackageTrustVerifier production implementation outside host trust adapter"

write_clean_fixture
mkdir -p "$fixture_root/src-tauri/src/plugins/trust"
printf '%s\n' \
  'mod plugins;' \
  > "$fixture_root/src-tauri/src/lib.rs"
printf '%s\n' \
  'pub mod trust;' \
  'mod wrong;' \
  > "$fixture_root/src-tauri/src/plugins/mod.rs"
printf '%s\n' \
  'pub mod package;' \
  > "$fixture_root/src-tauri/src/plugins/trust/mod.rs"
printf '%s\n' \
  '#[macro_export]' \
  'macro_rules! catalog_verifier {' \
  '    ($target:ty) => { impl jarvis_package::PackageTrustVerifier for $target {} };' \
  '}' \
  > "$fixture_root/src-tauri/src/plugins/trust/package.rs"
printf '%s\n' \
  'macro_rules! forward_verifier {' \
  '    ($implementation_macro:path, $target:ty) => {' \
  '        $implementation_macro!($target);' \
  '    };' \
  '}' \
  'struct ForwardedVerifier;' \
  'forward_verifier!(crate::catalog_verifier, ForwardedVerifier);' \
  > "$fixture_root/src-tauri/src/plugins/wrong.rs"
expect_cargo_accepts_host_source
expect_rejected "PackageTrustVerifier production implementation outside host trust adapter"

write_clean_fixture
mkdir -p "$fixture_root/src-tauri/src/plugins/trust"
printf '%s\n' \
  'mod plugins;' \
  > "$fixture_root/src-tauri/src/lib.rs"
printf '%s\n' \
  'pub mod trust;' \
  'mod wrong;' \
  > "$fixture_root/src-tauri/src/plugins/mod.rs"
printf '%s\n' \
  'pub mod package;' \
  > "$fixture_root/src-tauri/src/plugins/trust/mod.rs"
printf '%s\n' \
  '#[macro_export]' \
  'macro_rules! catalog_verifier {' \
  '    ($target:ty) => { impl jarvis_package::PackageTrustVerifier for $target {} };' \
  '}' \
  > "$fixture_root/src-tauri/src/plugins/trust/package.rs"
printf '%s\n' \
  'macro_rules! forward_verifier {' \
  '    ($implementation_macro:path, $target:ty) => {' \
  '        $implementation_macro!($target);' \
  '    };' \
  '}' \
  'macro_rules! forward_twice {' \
  '    ($implementation_macro:path, $target:ty) => {' \
  '        forward_verifier!($implementation_macro, $target);' \
  '    };' \
  '}' \
  'struct TransitivelyForwardedVerifier;' \
  'forward_twice!(crate::catalog_verifier, TransitivelyForwardedVerifier);' \
  > "$fixture_root/src-tauri/src/plugins/wrong.rs"
expect_cargo_accepts_host_source
expect_rejected "PackageTrustVerifier production implementation outside host trust adapter"

write_clean_fixture
mkdir -p "$fixture_root/src-tauri/src/plugins/trust"
printf '%s\n' \
  'mod plugins;' \
  > "$fixture_root/src-tauri/src/lib.rs"
printf '%s\n' \
  'pub mod trust;' \
  'mod wrong;' \
  > "$fixture_root/src-tauri/src/plugins/mod.rs"
printf '%s\n' \
  'pub mod package;' \
  > "$fixture_root/src-tauri/src/plugins/trust/mod.rs"
printf '%s\n' \
  '#[macro_export]' \
  'macro_rules! catalog_verifier {' \
  '    ($target:ty) => { impl jarvis_package::PackageTrustVerifier for $target {} };' \
  '}' \
  > "$fixture_root/src-tauri/src/plugins/trust/package.rs"
printf '%s\n' \
  'use crate::catalog_verifier as r#type;' \
  'macro_rules! forward_verifier {' \
  '    ($implementation_macro:path, $target:ty) => {' \
  '        $implementation_macro!($target);' \
  '    };' \
  '}' \
  'struct RawForwardedVerifier;' \
  'forward_verifier!(r#type, RawForwardedVerifier);' \
  > "$fixture_root/src-tauri/src/plugins/wrong.rs"
expect_cargo_accepts_host_source
expect_rejected "PackageTrustVerifier production implementation outside host trust adapter"

write_clean_fixture
mkdir -p "$fixture_root/src-tauri/src/plugins/trust"
printf '%s\n' \
  'mod plugins;' \
  > "$fixture_root/src-tauri/src/lib.rs"
printf '%s\n' \
  'pub mod trust;' \
  'mod wrong;' \
  > "$fixture_root/src-tauri/src/plugins/mod.rs"
printf '%s\n' \
  'pub mod package;' \
  > "$fixture_root/src-tauri/src/plugins/trust/mod.rs"
printf '%s\n' \
  '#[macro_export]' \
  'macro_rules! catalog_verifier {' \
  '    ($target:ty) => { impl jarvis_package::PackageTrustVerifier for $target {} };' \
  '}' \
  > "$fixture_root/src-tauri/src/plugins/trust/package.rs"
printf '%s\n' \
  'use crate::catalog_verifier as vérifier;' \
  'macro_rules! forward_verifier {' \
  '    ($implementation_macro:path, $target:ty) => {' \
  '        $implementation_macro!($target);' \
  '    };' \
  '}' \
  'struct CanonicallyForwardedVerifier;' \
  'forward_verifier!(vérifier, CanonicallyForwardedVerifier);' \
  > "$fixture_root/src-tauri/src/plugins/wrong.rs"
expect_cargo_accepts_host_source
expect_rejected "PackageTrustVerifier production implementation outside host trust adapter"

write_clean_fixture
mkdir -p "$fixture_root/src-tauri/src/plugins/trust"
printf '%s\n' \
  'mod plugins;' \
  > "$fixture_root/src-tauri/src/lib.rs"
printf '%s\n' \
  'pub mod trust;' \
  'mod wrong;' \
  > "$fixture_root/src-tauri/src/plugins/mod.rs"
printf '%s\n' \
  'pub mod package;' \
  > "$fixture_root/src-tauri/src/plugins/trust/mod.rs"
printf '%s\n' \
  '#[macro_export]' \
  'macro_rules! catalog_verifier {' \
  '    ($target:ty) => { impl jarvis_package::PackageTrustVerifier for $target {} };' \
  '}' \
  > "$fixture_root/src-tauri/src/plugins/trust/package.rs"
printf '%s\n' \
  'macro_rules! catalog_verifer {' \
  '    ($target:ty) => { const _: usize = std::mem::size_of::<$target>(); };' \
  '}' \
  'macro_rules! forward_data {' \
  '    ($implementation_macro:path, $target:ty) => {' \
  '        $implementation_macro!($target);' \
  '    };' \
  '}' \
  'struct UnrelatedForwardTarget;' \
  'forward_data!(catalog_verifer, UnrelatedForwardTarget);' \
  > "$fixture_root/src-tauri/src/plugins/wrong.rs"
expect_cargo_accepts_host_source
run_fixture_boundary >/dev/null

write_clean_fixture
mkdir -p "$fixture_root/src-tauri/src/plugins/trust"
printf '%s\n' \
  '#[macro_export]' \
  'macro_rules! cross_root_verifier {' \
  '    ($type:ty) => { impl PackageTrustVerifier for $type {} };' \
  '}' \
  > "$fixture_root/src-tauri/src/plugins/trust/package.rs"
printf '%s\n' \
  'struct CrossRootMacroVerifier;' \
  'cross_root_verifier!(CrossRootMacroVerifier);' \
  > "$fixture_root/plugins/community/src/lib.rs"
expect_rejected "PackageTrustVerifier production implementation outside host trust adapter"

write_clean_fixture
mkdir -p "$fixture_root/src-tauri/src/plugins/trust"
printf '%s\n' \
  'mod plugins;' \
  > "$fixture_root/src-tauri/src/lib.rs"
printf '%s\n' \
  'pub mod trust;' \
  'mod wrong;' \
  > "$fixture_root/src-tauri/src/plugins/mod.rs"
printf '%s\n' \
  'pub mod package;' \
  > "$fixture_root/src-tauri/src/plugins/trust/mod.rs"
printf '%s\n' \
  '#[macro_export]' \
  'macro_rules! catalog_verifier {' \
  '    ($target:ty) => { impl jarvis_package::PackageTrustVerifier for $target {} };' \
  '}' \
  > "$fixture_root/src-tauri/src/plugins/trust/package.rs"
printf '%s\n' \
  'use crate::catalog_verifier as r#type;' \
  'struct RawDirectMacroAliasVerifier;' \
  'r#type!(RawDirectMacroAliasVerifier);' \
  > "$fixture_root/src-tauri/src/plugins/wrong.rs"
expect_cargo_accepts_host_source
expect_rejected "PackageTrustVerifier production implementation outside host trust adapter"

write_clean_fixture
mkdir -p "$fixture_root/src-tauri/src/plugins/trust"
printf '%s\n' \
  'mod plugins;' \
  > "$fixture_root/src-tauri/src/lib.rs"
printf '%s\n' \
  'pub mod trust;' \
  'mod wrong;' \
  > "$fixture_root/src-tauri/src/plugins/mod.rs"
printf '%s\n' \
  'pub mod package;' \
  > "$fixture_root/src-tauri/src/plugins/trust/mod.rs"
printf '%s\n' \
  '#[macro_export]' \
  'macro_rules! catalog_verifier {' \
  '    ($target:ty) => { impl jarvis_package::PackageTrustVerifier for $target {} };' \
  '}' \
  > "$fixture_root/src-tauri/src/plugins/trust/package.rs"
printf '%s\n' \
  'use crate::catalog_verifier as vérifier;' \
  'struct CanonicalMacroAliasVerifier;' \
  'vérifier!(CanonicalMacroAliasVerifier);' \
  > "$fixture_root/src-tauri/src/plugins/wrong.rs"
expect_cargo_accepts_host_source
expect_rejected "PackageTrustVerifier production implementation outside host trust adapter"

write_clean_fixture
mkdir -p "$fixture_root/src-tauri/src/plugins/trust"
printf '%s\n' \
  '#[macro_export]' \
  'macro_rules! cross_root_verifier {' \
  '    ($type:ty) => { impl PackageTrustVerifier for $type {} };' \
  '}' \
  > "$fixture_root/src-tauri/src/plugins/trust/package.rs"
printf '%s\n' \
  'use host_macros::cross_root_verifier as install_verifier;' \
  'struct DirectMacroAliasVerifier;' \
  'install_verifier!(DirectMacroAliasVerifier);' \
  > "$fixture_root/plugins/community/src/lib.rs"
expect_rejected "PackageTrustVerifier production implementation outside host trust adapter"

write_clean_fixture
mkdir -p "$fixture_root/src-tauri/src/plugins/trust"
printf '%s\n' \
  'mod plugins;' \
  > "$fixture_root/src-tauri/src/lib.rs"
printf '%s\n' \
  'pub mod trust;' \
  'mod wrong;' \
  > "$fixture_root/src-tauri/src/plugins/mod.rs"
printf '%s\n' \
  'pub mod package;' \
  > "$fixture_root/src-tauri/src/plugins/trust/mod.rs"
printf '%s\n' \
  '#[macro_export]' \
  'macro_rules! catalog_verifier {' \
  '    ($target:ty) => { impl jarvis_package::PackageTrustVerifier for $target {} };' \
  '}' \
  > "$fixture_root/src-tauri/src/plugins/trust/package.rs"
printf '%s\n' \
  'use crate::{catalog_verifier as r#match};' \
  'struct RawGroupedMacroAliasVerifier;' \
  'r#match!(RawGroupedMacroAliasVerifier);' \
  > "$fixture_root/src-tauri/src/plugins/wrong.rs"
expect_cargo_accepts_host_source
expect_rejected "PackageTrustVerifier production implementation outside host trust adapter"

write_clean_fixture
mkdir -p "$fixture_root/src-tauri/src/plugins/trust"
printf '%s\n' \
  '#[macro_export]' \
  'macro_rules! cross_root_verifier {' \
  '    ($type:ty) => { impl PackageTrustVerifier for $type {} };' \
  '}' \
  > "$fixture_root/src-tauri/src/plugins/trust/package.rs"
printf '%s\n' \
  'use host_macros::{unrelated, cross_root_verifier as grouped_verifier};' \
  'struct GroupedMacroAliasVerifier;' \
  'grouped_verifier!(GroupedMacroAliasVerifier);' \
  > "$fixture_root/plugins/community/src/lib.rs"
expect_rejected "PackageTrustVerifier production implementation outside host trust adapter"

write_clean_fixture
mkdir -p "$fixture_root/src-tauri/src/plugins/trust"
printf '%s\n' \
  'struct CatalogVerifier;' \
  'impl PackageTrustVerifier for CatalogVerifier {}' \
  > "$fixture_root/src-tauri/src/plugins/trust/package.rs"
run_fixture_boundary >/dev/null

write_clean_fixture
mkdir -p \
  "$fixture_root/src-tauri/src/plugins/trust" \
  "$fixture_root/src-tauri/src/plugins"
printf '%s\n' \
  '#[macro_export]' \
  'macro_rules! catalog_verifier {' \
  '    ($type:ty) => { impl PackageTrustVerifier for $type {} };' \
  '}' \
  > "$fixture_root/src-tauri/src/plugins/trust/package.rs"
printf '%s\n' \
  'struct EscapedVerifier;' \
  'catalog_verifier!(EscapedVerifier);' \
  > "$fixture_root/src-tauri/src/plugins/wrong.rs"
expect_rejected "PackageTrustVerifier production implementation outside host trust adapter"

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
  '["dependencies"."jarvis-package"]' \
  'version = "0.1.0"' \
  > "$fixture_root/crates/jarvis-plugin-protocol/Cargo.toml"
expect_cargo_dependency "$fixture_root/crates/jarvis-plugin-protocol/Cargo.toml"
expect_rejected "public or plugin crate depends on jarvis-package"

write_clean_fixture
printf '%s\n' \
  '[package]' \
  'name = "jarvis-plugin-protocol"' \
  'version = "0.1.0"' \
  "['dependencies'.'jarvis-package']" \
  'version = "0.1.0"' \
  > "$fixture_root/crates/jarvis-plugin-protocol/Cargo.toml"
expect_cargo_dependency "$fixture_root/crates/jarvis-plugin-protocol/Cargo.toml"
expect_rejected "public or plugin crate depends on jarvis-package"

write_clean_fixture
printf '%s\n' \
  '[package]' \
  'name = "jarvis-plugin-protocol"' \
  'version = "0.1.0"' \
  '[dependencies."jarvis-package"]' \
  'version = "0.1.0"' \
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
printf '%s\n' 'pub fn internal() {}' \
  > "$fixture_root/crates/internal-tool/src/lib.rs"
printf '%s\n' \
  '[package]' \
  'name = "internal-tool"' \
  'version = "0.1.0"' \
  '[dependencies]' \
  'engine = { path = "../jarvis-package", package = "jarvis-package" }' \
  > "$fixture_root/crates/internal-tool/Cargo.toml"
expect_rejected "only src-tauri may depend on jarvis-package"

write_clean_fixture
mkdir -p "$fixture_root/crates/target/internal-tool/src"
printf '%s\n' 'pub fn internal() {}' \
  > "$fixture_root/crates/target/internal-tool/src/lib.rs"
printf '%s\n' \
  '[package]' \
  'name = "target-internal-tool"' \
  'version = "0.1.0"' \
  '[dependencies]' \
  'jarvis-package = { path = "../../jarvis-package" }' \
  > "$fixture_root/crates/target/internal-tool/Cargo.toml"
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
  '#[allow(unsafe_code, dead_code)]' \
  'pub fn escaped() { unsafe { std::ptr::read_volatile(&0_u8); } }' \
  > "$fixture_root/crates/jarvis-package/src/escaped.rs"
expect_rejected "jarvis-package unsafe allow must be exactly scoped"

write_clean_fixture
printf '%s\n' \
  '#[allow(' \
  '    unsafe_code' \
  ')]' \
  'pub fn escaped() { unsafe { std::ptr::read_volatile(&0_u8); } }' \
  > "$fixture_root/crates/jarvis-package/src/escaped.rs"
expect_rejected "jarvis-package unsafe allow must be exactly scoped"

write_clean_fixture
printf '%s\n' \
  '#[allow(unsafe_code)]' \
  'pub fn escaped() {' \
  '    unsafe' \
  '    {' \
  '        std::ptr::read_volatile(&0_u8);' \
  '    }' \
  '}' \
  > "$fixture_root/crates/jarvis-package/src/escaped.rs"
expect_rejected "jarvis-package unsafe syntax outside macos_dir.rs"

write_clean_fixture
printf '%s\n' \
  'pub const TEXT: &str = "unsafe { not Rust syntax }";' \
  'pub const RAW: &str = r#"#[allow(unsafe_code)]"#;' \
  'pub const SOURCE_TEXT: &str = "include!(\"escaped.inc\") #[path = \"escaped.rs\"]";' \
  'pub fn r#unsafe() {}' \
  'pub fn r#include() {}' \
  'pub fn r#path() {}' \
  'pub fn data_macros() {' \
  '    let _ = include_str!("text_mentions.rs");' \
  '    let _ = include_bytes!("text_mentions.rs");' \
  '}' \
  '#[doc = "path = ignored"]' \
  'pub fn documented() {}' \
  '// unsafe {' \
  '// include!("escaped.inc");' \
  '/* #[allow(' \
  ' * unsafe_code' \
  ' *)] */' \
  > "$fixture_root/crates/jarvis-package/src/text_mentions.rs"
run_fixture_boundary >/dev/null

write_clean_fixture
printf '%s\n' \
  '#[path = "covered.rs"]' \
  'mod covered;' \
  > "$fixture_root/plugins/community/src/lib.rs"
printf '%s\n' \
  'pub fn covered() {}' \
  > "$fixture_root/plugins/community/src/covered.rs"
run_fixture_boundary >/dev/null

write_clean_fixture
mkdir -p "$fixture_root/crates/jarvis-package/src/target"
printf '%s\n' \
  '#![allow(unsafe_code)]' \
  'pub fn escaped() { unsafe { std::ptr::read_volatile(&0_u8); } }' \
  > "$fixture_root/crates/jarvis-package/src/target/mod.rs"
printf '%s\n' 'mod target;' \
  >> "$fixture_root/crates/jarvis-package/src/lib.rs"
expect_cargo_accepts_private_source
expect_rejected "jarvis-package unsafe syntax outside macos_dir.rs"

write_clean_fixture
printf '%s\n' \
  '#[allow(unsafe_code)]' \
  'pub fn escaped() { unsafe { std::ptr::read_volatile(&0_u8); } }' \
  > "$fixture_root/crates/jarvis-package/src/escaped.inc"
printf '%s\n' 'include!("escaped.inc");' \
  >> "$fixture_root/crates/jarvis-package/src/lib.rs"
expect_cargo_accepts_private_source
expect_rejected "jarvis-package source discovery escape"

write_clean_fixture
printf '%s\n' \
  '#[allow(unsafe_code)]' \
  'pub fn escaped() { unsafe { std::ptr::read_volatile(&0_u8); } }' \
  > "$fixture_root/crates/jarvis-package/src/escaped.inc"
printf '%s\n' 'r#include!("escaped.inc");' \
  >> "$fixture_root/crates/jarvis-package/src/lib.rs"
expect_cargo_accepts_private_source
expect_rejected "jarvis-package source discovery escape"

write_clean_fixture
printf '%s\n' \
  '#![allow(unsafe_code)]' \
  'pub fn escaped() { unsafe { std::ptr::read_volatile(&0_u8); } }' \
  > "$fixture_root/crates/jarvis-package/src/escaped.inc"
printf '%s\n' \
  '#[path = "escaped.inc"]' \
  'mod escaped;' \
  >> "$fixture_root/crates/jarvis-package/src/lib.rs"
expect_cargo_accepts_private_source
expect_rejected "jarvis-package source discovery escape"

write_clean_fixture
printf '%s\n' \
  '#![allow(unsafe_code)]' \
  'pub fn escaped() { unsafe { std::ptr::read_volatile(&0_u8); } }' \
  > "$fixture_root/crates/jarvis-package/src/escaped.inc"
printf '%s\n' \
  '#[r#path = "escaped.inc"]' \
  'mod escaped;' \
  >> "$fixture_root/crates/jarvis-package/src/lib.rs"
expect_cargo_accepts_private_source
expect_rejected "jarvis-package source discovery escape"

write_clean_fixture
printf '%s\n' \
  '' \
  '[lib]' \
  'path = "src/escaped.inc"' \
  >> "$fixture_root/crates/jarvis-package/Cargo.toml"
printf '%s\n' \
  '#![allow(unsafe_code)]' \
  'pub fn escaped() { unsafe { std::ptr::read_volatile(&0_u8); } }' \
  > "$fixture_root/crates/jarvis-package/src/escaped.inc"
expect_cargo_accepts_private_source
expect_rejected "jarvis-package unsafe syntax outside macos_dir.rs"

write_clean_fixture
mkdir -p "$fixture_root/external-source"
printf '%s\n' \
  '#![allow(unsafe_code)]' \
  'pub fn escaped() { unsafe { std::ptr::read_volatile(&0_u8); } }' \
  > "$fixture_root/external-source/mod.rs"
ln -s ../../../external-source \
  "$fixture_root/crates/jarvis-package/src/linked"
printf '%s\n' 'mod linked;' \
  >> "$fixture_root/crates/jarvis-package/src/lib.rs"
expect_cargo_accepts_private_source
expect_rejected "jarvis-package source discovery escape"

write_clean_fixture
mkdir -p "$fixture_root/external-source"
printf '%s\n' \
  'pub struct HiddenSource;' \
  > "$fixture_root/external-source/hidden.rs"
ln -s ../../../external-source/hidden.rs \
  "$fixture_root/plugins/community/src/linked.rs"
expect_rejected "PackageTrustVerifier source discovery escape"

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

repository_lock_digests() {
  local lock
  while IFS= read -r lock; do
    [[ -n "$lock" ]] && shasum -a 256 "$repo_root/$lock"
  done < <(
    git -C "$repo_root" ls-files -- '*Cargo.lock' \
      | LC_ALL=C sort
  )
}

lock_digests_before="$(repository_lock_digests)"
CARGO_BIN=cargo CARGO_NET_OFFLINE=true \
  bash "$repo_root/scripts/check-plugin-boundaries.sh" "$repo_root" >/dev/null
lock_digests_after="$(repository_lock_digests)"
if [[ "$lock_digests_before" != "$lock_digests_after" ]]; then
  echo "plugin boundary check mutated a committed Cargo lock" >&2
  diff -u <(printf '%s\n' "$lock_digests_before") \
    <(printf '%s\n' "$lock_digests_after") >&2 \
    || true
  exit 1
fi

mkdir -p "$fixture_root/schemas"
printf '%s\n' '{}' > "$fixture_root/schemas/plugin-private-v1.schema.json"
expect_rejected "public plugin schema is not allowlisted"

echo "plugin boundary negative fixtures passed"
