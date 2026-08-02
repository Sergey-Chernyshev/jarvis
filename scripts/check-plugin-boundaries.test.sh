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

run_fixture_trust_scan() {
  printf '%s\0' "$@" \
    | node "$repo_root/scripts/scan-rust-unsafe-boundary.mjs" \
      --trust-roots \
      "$fixture_root/crates" \
      "$fixture_root/plugins" \
      "$fixture_root/src-tauri" \
      --target-sources-stdin0
}

run_fixture_source_scan() {
  node "$repo_root/scripts/scan-rust-unsafe-boundary.mjs" \
    --trust-roots \
    "$fixture_root/crates" \
    "$fixture_root/plugins" \
    "$fixture_root/src-tauri"
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

provenance_root="$fixture_root/provenance"
provenance_package_root="$provenance_root/member"
provenance_manifest="$provenance_package_root/Cargo.toml"
provenance_lock="$provenance_root/Cargo.lock"
mkdir -p "$provenance_package_root/src"
printf '%s\n' \
  '[workspace]' \
  'members = ["member"]' \
  'resolver = "2"' \
  > "$provenance_root/Cargo.toml"
printf '%s\n' \
  '[package]' \
  'name = "provenance-fixture"' \
  'version = "0.1.0"' \
  > "$provenance_manifest"

write_provenance_lock() {
  local identities=("$@")
  printf '%s\n' \
    'version = 3' \
    '' \
    '[[package]]' \
    'name = "provenance-fixture"' \
    'version = "0.1.0"' \
    > "$provenance_lock"
  local identity
  for identity in "${identities[@]}"; do
    IFS='|' read -r package version source checksum <<< "$identity"
    printf '%s\n' \
      '' \
      '[[package]]' \
      "name = \"$package\"" \
      "version = \"$version\"" \
      "source = \"$source\"" \
      "checksum = \"$checksum\"" \
      >> "$provenance_lock"
  done
}

run_provenance_contract() {
  local dependency_name="$1"
  local dependency_alias="$2"
  local declared_source="$3"
  local resolved_name="$4"
  local resolved_version="$5"
  local resolved_source="$6"
  local scenario="${7:-exact}"
  local resolved_manifest_path="${8:-$serde_json_manifest_path}"
  node -e '
      const manifestPath = process.argv[1];
      const workspaceRoot = require("node:path").dirname(
        require("node:path").dirname(manifestPath),
      );
      const dependencyName = process.argv[2];
      const dependencyAlias = process.argv[3];
      const declaredSource = process.argv[4] || null;
      const resolvedName = process.argv[5];
      const resolvedVersion = process.argv[6];
      const resolvedSource = process.argv[7] || null;
      const scenario = process.argv[8];
      const resolvedManifestPath = process.argv[9];
      const rootId = `path+file://${manifestPath}#provenance-fixture@0.1.0`;
      const dependencyId =
        `${resolvedSource ?? `path+file://${workspaceRoot}/patched`}` +
        `#${resolvedName}@${resolvedVersion}`;
      const dependencyPackage = {
        name: resolvedName,
        version: resolvedVersion,
        id: dependencyId,
        source: resolvedSource,
        manifest_path: resolvedManifestPath,
        dependencies: [],
        targets: [],
      };
      const packages = [{
        name: "provenance-fixture",
        version: "0.1.0",
        id: rootId,
        source: null,
        manifest_path: manifestPath,
        dependencies: [{
          name: dependencyName,
          source: declaredSource,
          req: "^1",
          kind: null,
          rename: dependencyAlias === dependencyName ? null : dependencyAlias,
          optional: false,
          uses_default_features: true,
          features: [],
          target: null,
          registry: null,
          path: null,
        }],
        targets: [],
      }, dependencyPackage];
      const rootDependencies =
        scenario === "missing-resolve"
          ? []
          : [{
              name: dependencyAlias.replaceAll("-", "_"),
              pkg: dependencyId,
              dep_kinds: [{ kind: null, target: null }],
            }];
      const nodes = [{
        id: rootId,
        dependencies: rootDependencies.map((dependency) => dependency.pkg),
        deps: rootDependencies,
        features: [],
      }, {
        id: dependencyId,
        dependencies: [],
        deps: [],
        features: [],
      }];
      if (scenario === "missing-package") {
        packages.pop();
      }
      if (scenario === "ambiguous-direct") {
        const secondId =
          `${resolvedSource}#${resolvedName}@1.0.150`;
        packages.push({
          ...dependencyPackage,
          version: "1.0.150",
          id: secondId,
        });
        rootDependencies.push({
          name: dependencyAlias.replaceAll("-", "_"),
          pkg: secondId,
          dep_kinds: [{ kind: null, target: "cfg(target_os = \"macos\")" }],
        });
        nodes.push({
          id: secondId,
          dependencies: [],
          deps: [],
          features: [],
        });
      }
      if (scenario === "transitive-second") {
        const secondId =
          `${resolvedSource}#${resolvedName}@1.0.150`;
        const carrierId =
          "registry+https://github.com/rust-lang/crates.io-index#carrier@1.0.0";
        packages[0].dependencies.push({
          name: "carrier",
          source: "registry+https://github.com/rust-lang/crates.io-index",
          req: "^1",
          kind: null,
          rename: null,
          optional: false,
          uses_default_features: true,
          features: [],
          target: null,
          registry: null,
          path: null,
        });
        packages.push({
          ...dependencyPackage,
          version: "1.0.150",
          id: secondId,
        }, {
          name: "carrier",
          version: "1.0.0",
          id: carrierId,
          source: "registry+https://github.com/rust-lang/crates.io-index",
          manifest_path: `${workspaceRoot}/registry/carrier/Cargo.toml`,
          dependencies: [],
          targets: [],
        });
        rootDependencies.push({
          name: "carrier",
          pkg: carrierId,
          dep_kinds: [{ kind: null, target: null }],
        });
        nodes.push({
          id: carrierId,
          dependencies: [secondId],
          deps: [{
            name: resolvedName.replaceAll("-", "_"),
            pkg: secondId,
            dep_kinds: [{ kind: null, target: null }],
          }],
          features: [],
        }, {
          id: secondId,
          dependencies: [],
          deps: [],
          features: [],
        });
      }
      process.stdout.write(JSON.stringify({
        packages,
        workspace_root: workspaceRoot,
        workspace_members: [rootId],
        workspace_default_members: [rootId],
        resolve:
          scenario === "null-resolve"
            ? null
            : {
                root: null,
                nodes,
              },
      }));
    ' \
    "$provenance_manifest" \
    "$dependency_name" \
    "$dependency_alias" \
    "$declared_source" \
    "$resolved_name" \
    "$resolved_version" \
    "$resolved_source" \
    "$scenario" \
    "$resolved_manifest_path" \
    | node "$repo_root/scripts/resolve-cargo-macro-provenance.mjs" \
      "$provenance_manifest"
}

expect_provenance_rejected() {
  local expected="$1"
  shift
  local output
  if output="$(run_provenance_contract "$@" 2>&1)"; then
    echo "Cargo macro provenance accepted forbidden identity: $expected" >&2
    exit 1
  fi
  if [[ "$output" != *"$expected"* ]]; then
    echo "Cargo macro provenance did not identify: $expected" >&2
    echo "$output" >&2
    exit 1
  fi
}

registry_source='registry+https://github.com/rust-lang/crates.io-index'
serde_1_0_150_checksum='e8014e44b4736ed0538adeecded0fce2a272f22dc9578a7eb6b2d9993c74cfb9'
serde_1_0_151_checksum='c841b55ecdae098c80dcae9cf767f6f8a0c2cdb3416bbef72181df4d0fe73f14'
serde_json_manifest_path="$(
  cargo metadata \
    --all-features \
    --format-version=1 \
    --locked \
    --offline \
    --manifest-path "$repo_root/crates/jarvis-package/Cargo.toml" \
    | node -e '
      let source = "";
      process.stdin.setEncoding("utf8");
      process.stdin.on("data", (chunk) => { source += chunk; });
      process.stdin.on("end", () => {
        const metadata = JSON.parse(source);
        const matches = metadata.packages.filter(
          (candidate) =>
            candidate.name === "serde_json" &&
            candidate.version === "1.0.151",
        );
        if (matches.length !== 1) process.exit(1);
        process.stdout.write(matches[0].manifest_path);
      });
    '
)"
write_provenance_lock \
  "serde_json|1.0.151|$registry_source|$serde_1_0_151_checksum"
verified_provenance="$(
  run_provenance_contract \
    "serde_json" \
    "serde_json" \
    "$registry_source" \
    "serde_json" \
    "1.0.151" \
    "$registry_source"
)"
printf '%s\n' "$verified_provenance" \
  | node -e '
    let source = "";
    process.stdin.setEncoding("utf8");
    process.stdin.on("data", (chunk) => { source += chunk; });
    process.stdin.on("end", () => {
      const record = JSON.parse(source);
      const identity = record.aliases?.serde_json;
      if (
        identity?.package !== "serde_json" ||
        identity?.version !== "1.0.151" ||
        identity?.checksum !==
          "c841b55ecdae098c80dcae9cf767f6f8a0c2cdb3416bbef72181df4d0fe73f14"
      ) {
        process.exit(1);
      }
    });
  '

renamed_provenance="$(
  run_provenance_contract \
    "serde_json" \
    "json_codec" \
    "$registry_source" \
    "serde_json" \
    "1.0.151" \
    "$registry_source"
)"
printf '%s\n' "$renamed_provenance" \
  | node -e '
    let source = "";
    process.stdin.setEncoding("utf8");
    process.stdin.on("data", (chunk) => { source += chunk; });
    process.stdin.on("end", () => {
      const record = JSON.parse(source);
      if (
        Object.keys(record.aliases ?? {}).join(",") !== "json_codec" ||
        record.aliases.json_codec.package !== "serde_json"
      ) {
        process.exit(1);
      }
    });
  '

expect_provenance_rejected \
  "audited Cargo macro alias serde_json resolves to unaudited package lookalike" \
  "lookalike" \
  "serde_json" \
  "$registry_source" \
  "lookalike" \
  "1.0.0" \
  "$registry_source"

expect_provenance_rejected \
  "audited Cargo macro package serde_json must use the crates.io registry" \
  "serde_json" \
  "serde_json" \
  "$registry_source" \
  "serde_json" \
  "1.0.151" \
  "" \
  "exact"

expect_provenance_rejected \
  "audited Cargo macro package serde_json must use the crates.io registry" \
  "serde_json" \
  "serde_json" \
  "$registry_source" \
  "serde_json" \
  "1.0.151" \
  "git+https://example.invalid/serde_json" \
  "exact"

write_provenance_lock \
  "serde_json|1.0.151|$registry_source|0000000000000000000000000000000000000000000000000000000000000000"
expect_provenance_rejected \
  "serde_json 1.0.151 does not match an audited checksum" \
  "serde_json" \
  "serde_json" \
  "$registry_source" \
  "serde_json" \
  "1.0.151" \
  "$registry_source"

write_provenance_lock \
  "serde_json|1.0.150|$registry_source|$serde_1_0_150_checksum" \
  "serde_json|1.0.151|$registry_source|$serde_1_0_151_checksum"
expect_provenance_rejected \
  "Cargo macro alias serde_json is ambiguous" \
  "serde_json" \
  "serde_json" \
  "$registry_source" \
  "serde_json" \
  "1.0.151" \
  "$registry_source" \
  "ambiguous-direct"

transitive_provenance="$(
  run_provenance_contract \
    "serde_json" \
    "serde_json" \
    "$registry_source" \
    "serde_json" \
    "1.0.151" \
    "$registry_source" \
    "transitive-second"
)"
if [[ "$transitive_provenance" != *'"version":"1.0.151"'* ]]; then
  echo "unrelated transitive Cargo version changed direct macro identity" >&2
  exit 1
fi

write_provenance_lock \
  "serde_json|1.0.151|$registry_source|$serde_1_0_151_checksum"
expect_provenance_rejected \
  "Cargo resolve is missing audited alias serde_json" \
  "serde_json" \
  "serde_json" \
  "$registry_source" \
  "serde_json" \
  "1.0.151" \
  "$registry_source" \
  "missing-resolve"

expect_provenance_rejected \
  "Cargo resolve package identity is missing" \
  "serde_json" \
  "serde_json" \
  "$registry_source" \
  "serde_json" \
  "1.0.151" \
  "$registry_source" \
  "missing-package"

expect_provenance_rejected \
  "Cargo metadata resolve graph is missing" \
  "serde_json" \
  "serde_json" \
  "$registry_source" \
  "serde_json" \
  "1.0.151" \
  "$registry_source" \
  "null-resolve"

substituted_registry="$provenance_root/substituted/registry"
substituted_index="index.crates.io-1949cf8c6b5b557f"
substituted_artifact="serde_json-1.0.151"
substituted_source_root="$substituted_registry/src/$substituted_index"
substituted_cache_root="$substituted_registry/cache/$substituted_index"
serde_json_source_root="$(dirname "$serde_json_manifest_path")"
serde_json_registry_root="$(
  dirname "$(dirname "$(dirname "$serde_json_source_root")")"
)"
mkdir -p "$substituted_source_root" "$substituted_cache_root"
cp -R "$serde_json_source_root" "$substituted_source_root/$substituted_artifact"
cp \
  "$serde_json_registry_root/cache/$substituted_index/$substituted_artifact.crate" \
  "$substituted_cache_root/$substituted_artifact.crate"
printf '\n# local source replacement\n' \
  >> "$substituted_source_root/$substituted_artifact/Cargo.toml"
expect_provenance_rejected \
  "audited Cargo source file differs from its archive" \
  "serde_json" \
  "serde_json" \
  "$registry_source" \
  "serde_json" \
  "1.0.151" \
  "$registry_source" \
  "exact" \
  "$substituted_source_root/$substituted_artifact/Cargo.toml"

source_override_output=""
if source_override_output="$(
  CARGO_SOURCE_CRATES_IO_REPLACE_WITH=mirror \
    run_provenance_contract \
      "serde_json" \
      "serde_json" \
      "$registry_source" \
      "serde_json" \
      "1.0.151" \
      "$registry_source" \
      2>&1
)"; then
  echo "Cargo provenance accepted a source override environment" >&2
  exit 1
fi
if [[ "$source_override_output" != *"Cargo source override environment is forbidden"* ]]; then
  echo "Cargo provenance did not identify a source override environment" >&2
  echo "$source_override_output" >&2
  exit 1
fi

tauri_checksum='437404997acf375d85f1177afa7e11bb971f274ed6a7b83a2a3e339015f4cc28'
tauri_macros_checksum='ae6cb4e3896c21d2f6da5b31251d2faea0153bba56ed0e970f918115dbee4924'
read -r tauri_manifest_path tauri_macros_manifest_path < <(
  cargo metadata \
    --all-features \
    --format-version=1 \
    --locked \
    --offline \
    --manifest-path "$repo_root/src-tauri/Cargo.toml" \
    | node -e '
      let source = "";
      process.stdin.setEncoding("utf8");
      process.stdin.on("data", (chunk) => { source += chunk; });
      process.stdin.on("end", () => {
        const metadata = JSON.parse(source);
        const pathFor = (name, version) => {
          const matches = metadata.packages.filter(
            (candidate) =>
              candidate.name === name && candidate.version === version,
          );
          if (matches.length !== 1) process.exit(1);
          return matches[0].manifest_path;
        };
        process.stdout.write(
          `${pathFor("tauri", "2.11.2")} ` +
          `${pathFor("tauri-macros", "2.6.2")}\n`,
        );
      });
    '
)
run_tauri_provenance_contract() {
  local provider_source="$1"
  node -e '
    const manifestPath = process.argv[1];
    const workspaceRoot = require("node:path").dirname(
      require("node:path").dirname(manifestPath),
    );
    const registry = process.argv[2];
    const providerSource = process.argv[3] || null;
    const rootId = `path+file://${manifestPath}#provenance-fixture@0.1.0`;
    const tauriId = `${registry}#tauri@2.11.2`;
    const macrosId =
      `${providerSource ?? `path+file://${workspaceRoot}/patched`}` +
      "#tauri-macros@2.6.2";
    process.stdout.write(JSON.stringify({
      packages: [{
        name: "provenance-fixture",
        version: "0.1.0",
        id: rootId,
        source: null,
        manifest_path: manifestPath,
        dependencies: [{
          name: "tauri",
          source: registry,
          req: "^2.11.2",
          kind: null,
          rename: null,
          optional: false,
          uses_default_features: true,
          features: [],
          target: null,
          registry: null,
          path: null,
        }],
        targets: [],
      }, {
        name: "tauri",
        version: "2.11.2",
        id: tauriId,
        source: registry,
        manifest_path: process.argv[4],
        dependencies: [],
        targets: [],
      }, {
        name: "tauri-macros",
        version: "2.6.2",
        id: macrosId,
        source: providerSource,
        manifest_path: process.argv[5],
        dependencies: [],
        targets: [],
      }],
      workspace_root: workspaceRoot,
      workspace_members: [rootId],
      workspace_default_members: [rootId],
      resolve: {
        root: null,
        nodes: [{
          id: rootId,
          dependencies: [tauriId],
          deps: [{
            name: "tauri",
            pkg: tauriId,
            dep_kinds: [{ kind: null, target: null }],
          }],
          features: [],
        }, {
          id: tauriId,
          dependencies: [macrosId],
          deps: [{
            name: "tauri_macros",
            pkg: macrosId,
            dep_kinds: [{ kind: null, target: null }],
          }],
          features: [],
        }, {
          id: macrosId,
          dependencies: [],
          deps: [],
          features: [],
        }],
      },
    }));
  ' \
    "$provenance_manifest" \
    "$registry_source" \
    "$provider_source" \
    "$tauri_manifest_path" \
    "$tauri_macros_manifest_path" \
    | node "$repo_root/scripts/resolve-cargo-macro-provenance.mjs" \
      "$provenance_manifest"
}

write_provenance_lock \
  "tauri|2.11.2|$registry_source|$tauri_checksum" \
  "tauri-macros|2.6.2|$registry_source|$tauri_macros_checksum"
tauri_provenance="$(run_tauri_provenance_contract "$registry_source")"
if [[ "$tauri_provenance" != *'"tauri-macros"'* ]]; then
  echo "Cargo provenance omitted the audited Tauri proc-macro provider" >&2
  exit 1
fi
tauri_provider_output=""
if tauri_provider_output="$(run_tauri_provenance_contract "" 2>&1)"; then
  echo "Cargo provenance accepted a patched Tauri proc-macro provider" >&2
  exit 1
fi
if [[ "$tauri_provider_output" != *"Cargo macro provider tauri-macros must use the crates.io registry"* ]]; then
  echo "Cargo provenance did not identify the patched Tauri macro provider" >&2
  echo "$tauri_provider_output" >&2
  exit 1
fi

write_provenance_lock \
  "serde_json|1.0.151|$registry_source|$serde_1_0_151_checksum"
provenance_records="$provenance_root/provenance.ndjson"
printf '%s\n' "$verified_provenance" > "$provenance_records"
printf '%s\n' \
  'pub fn audited() {' \
  '    let _ = serde_json::json!({ "safe": true });' \
  '}' \
  > "$provenance_package_root/src/lib.rs"
audited_macro_scan="$(
  node "$repo_root/scripts/scan-rust-unsafe-boundary.mjs" \
    "$provenance_package_root" \
    --cargo-provenance-file "$provenance_records"
)"
if [[ "$audited_macro_scan" == *$'source\t'* ]]; then
  echo "scanner rejected Cargo-bound audited macro identity" >&2
  echo "$audited_macro_scan" >&2
  exit 1
fi

missing_provenance_scan="$(
  node "$repo_root/scripts/scan-rust-unsafe-boundary.mjs" \
    "$provenance_package_root"
)"
if [[ "$missing_provenance_scan" != *$'source\t'* ]]; then
  echo "scanner accepted audited macro text without Cargo provenance" >&2
  exit 1
fi

mkdir -p "$provenance_package_root/nested/src"
printf '%s\n' \
  '[package]' \
  'name = "nested-without-provenance"' \
  'version = "0.1.0"' \
  > "$provenance_package_root/nested/Cargo.toml"
printf '%s\n' \
  'pub fn nested() {' \
  '    let _ = serde_json::json!({ "safe": true });' \
  '}' \
  > "$provenance_package_root/nested/src/lib.rs"
scoped_provenance_scan="$(
  node "$repo_root/scripts/scan-rust-unsafe-boundary.mjs" \
    "$provenance_package_root" \
    --cargo-provenance-file "$provenance_records"
)"
if [[ "$scoped_provenance_scan" != *$'source\t'* ]]; then
  echo "scanner leaked an enclosing package macro identity into a nested crate" >&2
  exit 1
fi
rm -rf -- "$provenance_package_root/nested"

mkdir -p "$provenance_package_root/nested/src"
printf '%s\n' \
  '[package]' \
  'name = "nested-with-provenance"' \
  'version = "0.1.0"' \
  > "$provenance_package_root/nested/Cargo.toml"
printf '%s\n' \
  'pub fn nested() {' \
  '    let _ = serde_json::json!({ "safe": true });' \
  '}' \
  > "$provenance_package_root/nested/src/shared.rs"
printf '%s\n' \
  'include!("../nested/src/shared.rs");' \
  > "$provenance_package_root/src/lib.rs"
printf '%s\n' "$verified_provenance" > "$provenance_records"
node -e '
  const record = JSON.parse(process.argv[1]);
  const { realpathSync } = require("node:fs");
  const { join } = require("node:path");
  record.packageRoot = realpathSync(process.argv[2]);
  record.manifestPath = realpathSync(
    join(record.packageRoot, "Cargo.toml"),
  );
  process.stdout.write(`${JSON.stringify(record)}\n`);
' \
  "$verified_provenance" \
  "$provenance_package_root/nested" \
  >> "$provenance_records"
cross_package_include_scan="$(
  node "$repo_root/scripts/scan-rust-unsafe-boundary.mjs" \
    --trust-roots \
    "$provenance_package_root" \
    --cargo-provenance-file "$provenance_records"
)"
if [[ "$cross_package_include_scan" != *$'source\t'* ]]; then
  echo "scanner accepted a cross-package include under the wrong namespace" >&2
  exit 1
fi
rm -rf -- "$provenance_package_root/nested"

printf '%s\n' "$verified_provenance" > "$provenance_records"
printf '%s\n' \
  'extern crate self as serde_json;' \
  'pub fn rebound_absolute() {' \
  '    let _ = ::serde_json::json!({ "safe": true });' \
  '}' \
  > "$provenance_package_root/src/lib.rs"
absolute_rebind_scan="$(
  node "$repo_root/scripts/scan-rust-unsafe-boundary.mjs" \
    "$provenance_package_root" \
    --cargo-provenance-file "$provenance_records"
)"
if [[ "$absolute_rebind_scan" != *$'source\t'* ]]; then
  echo "scanner accepted an absolute Cargo alias rebound by extern crate" >&2
  exit 1
fi

printf '%s\n' "$renamed_provenance" > "$provenance_records"
printf '%s\n' \
  'pub fn renamed() {' \
  '    let _ = json_codec::json!({ "safe": true });' \
  '}' \
  > "$provenance_package_root/src/lib.rs"
renamed_macro_scan="$(
  node "$repo_root/scripts/scan-rust-unsafe-boundary.mjs" \
    "$provenance_package_root" \
    --cargo-provenance-file "$provenance_records"
)"
if [[ "$renamed_macro_scan" == *$'source\t'* ]]; then
  echo "scanner rejected legitimate Cargo macro rename" >&2
  echo "$renamed_macro_scan" >&2
  exit 1
fi

printf '%s\n' "$verified_provenance" > "$provenance_records"
printf '%s\n' \
  'mod serde_json {' \
  '    macro_rules! json {' \
  '        ($($token:tt)*) => { () };' \
  '    }' \
  '    pub(crate) use json;' \
  '}' \
  'pub fn shadowed() {' \
  '    let _ = serde_json::json!({ "safe": true });' \
  '}' \
  > "$provenance_package_root/src/lib.rs"
shadowed_macro_scan="$(
  node "$repo_root/scripts/scan-rust-unsafe-boundary.mjs" \
    "$provenance_package_root" \
    --cargo-provenance-file "$provenance_records"
)"
if [[ "$shadowed_macro_scan" != *$'source\t'* ]]; then
  echo "scanner accepted a locally shadowed Cargo macro alias" >&2
  exit 1
fi

printf '%s\n' \
  'mod serde_json {' \
  '    macro_rules! json {' \
  '        ($($token:tt)*) => { () };' \
  '    }' \
  '    pub(crate) use json;' \
  '}' \
  'pub fn absolute() {' \
  '    let _ = ::serde_json::json!({ "safe": true });' \
  '}' \
  > "$provenance_package_root/src/lib.rs"
absolute_macro_scan="$(
  node "$repo_root/scripts/scan-rust-unsafe-boundary.mjs" \
    "$provenance_package_root" \
    --cargo-provenance-file "$provenance_records"
)"
if [[ "$absolute_macro_scan" == *$'source\t'* ]]; then
  echo "scanner rejected an absolute Cargo-bound macro path" >&2
  echo "$absolute_macro_scan" >&2
  exit 1
fi

rm -rf -- "$provenance_root"

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
printf '%s\n' \
  '' \
  '[lib]' \
  'path = "src/hidden.inc"' \
  >> "$fixture_root/crates/jarvis-plugin-protocol/Cargo.toml"
printf '%s\n' \
  'struct HiddenCrateVerifier;' \
  'impl PackageTrustVerifier for HiddenCrateVerifier {}' \
  > "$fixture_root/crates/jarvis-plugin-protocol/src/hidden.inc"
expect_rejected "PackageTrustVerifier production implementation outside host trust adapter"

write_clean_fixture
printf '%s\n' \
  '' \
  '[lib]' \
  'path = "src/hidden.inc"' \
  >> "$fixture_root/plugins/community/Cargo.toml"
printf '%s\n' \
  'struct HiddenPluginVerifier;' \
  'impl PackageTrustVerifier for HiddenPluginVerifier {}' \
  > "$fixture_root/plugins/community/src/hidden.inc"
expect_rejected "PackageTrustVerifier production implementation outside host trust adapter"

write_clean_fixture
printf '%s\n' \
  '' \
  '[[bin]]' \
  'name = "hidden-host"' \
  'path = "src/hidden.inc"' \
  >> "$fixture_root/src-tauri/Cargo.toml"
printf '%s\n' \
  'struct HiddenHostVerifier;' \
  'impl PackageTrustVerifier for HiddenHostVerifier {}' \
  > "$fixture_root/src-tauri/src/hidden.inc"
expect_rejected "PackageTrustVerifier production implementation outside host trust adapter"

write_clean_fixture
mkdir -p "$fixture_root/external-source"
printf '%s\n' \
  'pub fn outside() {}' \
  > "$fixture_root/external-source/outside.rs"
printf '%s\n' \
  '' \
  '[lib]' \
  'path = "../../external-source/outside.rs"' \
  >> "$fixture_root/plugins/community/Cargo.toml"
expect_rejected "PackageTrustVerifier source discovery escape"

write_clean_fixture
mkdir -p "$fixture_root/plugins/community/target"
printf '%s\n' \
  'pub fn generated() {}' \
  > "$fixture_root/plugins/community/target/generated.rs"
printf '%s\n' \
  '' \
  '[lib]' \
  'path = "target/generated.rs"' \
  >> "$fixture_root/plugins/community/Cargo.toml"
expect_rejected "PackageTrustVerifier source discovery escape"

write_clean_fixture
target_count_output=""
if target_count_output="$(
  node -e '
    process.stdout.on("error", (error) => {
      if (error.code !== "EPIPE") throw error;
    });
    for (let index = 0; index <= 20_000; index += 1) {
      process.stdout.write(`/definitely-missing/target-${index}\0`);
    }
  ' \
    | node "$repo_root/scripts/scan-rust-unsafe-boundary.mjs" \
      --trust-roots \
      "$fixture_root/crates" \
      "$fixture_root/plugins" \
      "$fixture_root/src-tauri" \
      --target-sources-stdin0 \
      2>&1
)"; then
  echo "trust scanner accepted too many semantic Cargo targets" >&2
  exit 1
fi
if [[ "$target_count_output" != *"Cargo target source count exceeds 20000"* ]]; then
  echo "trust scanner did not account target count before filesystem access" >&2
  echo "$target_count_output" >&2
  exit 1
fi
if [[ "$target_count_output" == *"missing Cargo target source"* ]]; then
  echo "trust scanner diagnosed targets before applying the count budget" >&2
  exit 1
fi

target_transport_output=""
if target_transport_output="$(
  node -e '
    process.stdout.on("error", (error) => {
      if (error.code !== "EPIPE") throw error;
    });
    process.stdout.write(Buffer.alloc(8 * 1024 * 1024 + 1, 0x61));
  ' \
    | node "$repo_root/scripts/scan-rust-unsafe-boundary.mjs" \
      --trust-roots \
      "$fixture_root/crates" \
      "$fixture_root/plugins" \
      "$fixture_root/src-tauri" \
      --target-sources-stdin0 \
      2>&1
)"; then
  echo "trust scanner accepted oversized semantic target transport" >&2
  exit 1
fi
if [[ "$target_transport_output" != *"Cargo target source transport exceeds 8388608 bytes"* ]]; then
  echo "trust scanner did not bound semantic target transport before parsing" >&2
  echo "$target_transport_output" >&2
  exit 1
fi

target_diagnostic_output=""
if target_diagnostic_output="$(
  node -e '
    process.stdout.on("error", (error) => {
      if (error.code !== "EPIPE") throw error;
    });
    for (let index = 0; index <= 1_024; index += 1) {
      process.stdout.write(`/definitely-missing/diagnostic-${index}\0`);
    }
  ' \
    | node "$repo_root/scripts/scan-rust-unsafe-boundary.mjs" \
      --trust-roots \
      "$fixture_root/crates" \
      "$fixture_root/plugins" \
      "$fixture_root/src-tauri" \
      --target-sources-stdin0 \
      2>&1
)"; then
  echo "trust scanner accepted unbounded target diagnostics" >&2
  exit 1
fi
if [[ "$target_diagnostic_output" != *"Rust boundary diagnostics exceed 1024 records"* ]]; then
  echo "trust scanner did not cap retained target diagnostics" >&2
  echo "$target_diagnostic_output" >&2
  exit 1
fi
target_diagnostic_bytes="$(
  LC_ALL=C printf '%s' "$target_diagnostic_output" \
    | wc -c \
    | tr -d '[:space:]'
)"
if [[ "$target_diagnostic_bytes" -gt 1048576 ]]; then
  echo "trust scanner emitted oversized target diagnostics: $target_diagnostic_bytes" >&2
  exit 1
fi

if rg -q -- '--target-sources "\$\{[^}]*target[^}]*\[@\]\}"' \
  "$repo_root/scripts/check-plugin-boundaries.sh"; then
  echo "boundary gate still expands semantic Cargo targets into argv" >&2
  exit 1
fi
if ! rg -q -- '--target-sources-stdin0' \
  "$repo_root/scripts/check-plugin-boundaries.sh"; then
  echo "boundary gate does not use bounded stdin transport for Cargo targets" >&2
  exit 1
fi

write_clean_fixture
mkdir -p \
  "$fixture_root/external-source" \
  "$fixture_root/plugins/community/target" \
  "$fixture_root/plugins/community/node_modules"
printf '%s\n' \
  'pub fn outside() {}' \
  > "$fixture_root/external-source/outside.rs"
printf '%s\n' \
  'pub fn generated() {}' \
  > "$fixture_root/plugins/community/target/generated.rs"
printf '%s\n' \
  'pub fn excluded() {}' \
  > "$fixture_root/plugins/community/node_modules/excluded.rs"
ln -s ../../../external-source/outside.rs \
  "$fixture_root/plugins/community/src/linked.rs"
trust_target_contract=""
if ! trust_target_contract="$(
  run_fixture_trust_scan \
    "$fixture_root/external-source/outside.rs" \
    "$fixture_root/plugins/community/target/generated.rs" \
    "$fixture_root/plugins/community/node_modules/excluded.rs" \
    "$fixture_root/plugins/community/src/linked.rs" \
    "$fixture_root/plugins/community/src/missing.rs" \
    "$fixture_root/plugins/community/src"
)"; then
  echo "trust scanner rejected semantic target-source contract invocation" >&2
  exit 1
fi
outside_target="$(
  node -e 'console.log(require("node:path").resolve(process.argv[1]))' \
    "$fixture_root/external-source/outside.rs"
)"
build_target="$(
  node -e 'console.log(require("node:path").resolve(process.argv[1]))' \
    "$fixture_root/plugins/community/target/generated.rs"
)"
excluded_target="$(
  node -e 'console.log(require("node:path").resolve(process.argv[1]))' \
    "$fixture_root/plugins/community/node_modules/excluded.rs"
)"
symlink_target="$(
  node -e 'console.log(require("node:path").resolve(process.argv[1]))' \
    "$fixture_root/plugins/community/src/linked.rs"
)"
missing_target="$(
  node -e 'console.log(require("node:path").resolve(process.argv[1]))' \
    "$fixture_root/plugins/community/src/missing.rs"
)"
directory_target="$(
  node -e 'console.log(require("node:path").resolve(process.argv[1]))' \
    "$fixture_root/plugins/community/src"
)"
for expected_source_record in \
  "$outside_target:1"$'\t'"Cargo target source outside trust roots" \
  "$build_target:1"$'\t'"Cargo target source inside build output" \
  "$excluded_target:1"$'\t'"Cargo target source inside excluded directory" \
  "$symlink_target:1"$'\t'"symlink Cargo target source" \
  "$missing_target:1"$'\t'"missing Cargo target source" \
  "$directory_target:1"$'\t'"Cargo target source is not a regular file"
do
  if [[ "$trust_target_contract" != *$'source\t'"$expected_source_record"* ]]; then
    echo "trust scanner missed semantic target source: $expected_source_record" >&2
    echo "$trust_target_contract" >&2
    exit 1
  fi
done

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
  '#[allow(unsafe_code)]' \
  'pub fn escaped() { unsafe { std::ptr::read_volatile(&0_u8); } }' \
  > "$fixture_root/crates/jarvis-package/src/escaped.inc"
printf '%s\n' \
  'use std::include as hidden_source;' \
  'hidden_source!("escaped.inc");' \
  >> "$fixture_root/crates/jarvis-package/src/lib.rs"
expect_cargo_accepts_private_source
expect_rejected "jarvis-package source discovery escape"

write_clean_fixture
printf '%s\n' \
  '#[allow(unsafe_code)]' \
  'pub fn escaped() { unsafe { std::ptr::read_volatile(&0_u8); } }' \
  > "$fixture_root/crates/jarvis-package/src/escaped.inc"
printf '%s\n' \
  'use std::{include as grouped_source};' \
  'grouped_source!("escaped.inc");' \
  >> "$fixture_root/crates/jarvis-package/src/lib.rs"
expect_cargo_accepts_private_source
expect_rejected "jarvis-package source discovery escape"

write_clean_fixture
printf '%s\n' \
  '#[allow(unsafe_code)]' \
  'pub fn escaped() { unsafe { std::ptr::read_volatile(&0_u8); } }' \
  > "$fixture_root/crates/jarvis-package/src/escaped.inc"
printf '%s\n' \
  'use std::include as first_source;' \
  'use first_source as transitive_source;' \
  'transitive_source!("escaped.inc");' \
  >> "$fixture_root/crates/jarvis-package/src/lib.rs"
expect_cargo_accepts_private_source
expect_rejected "jarvis-package source discovery escape"

write_clean_fixture
printf '%s\n' \
  '#[allow(unsafe_code)]' \
  'pub fn escaped() { unsafe { std::ptr::read_volatile(&0_u8); } }' \
  > "$fixture_root/crates/jarvis-package/src/escaped.inc"
printf '%s\n' \
  'use std::include as r#type;' \
  'r#type!("escaped.inc");' \
  >> "$fixture_root/crates/jarvis-package/src/lib.rs"
expect_cargo_accepts_private_source
expect_rejected "jarvis-package source discovery escape"

write_clean_fixture
printf '%s\n' \
  '#[allow(unsafe_code)]' \
  'pub fn escaped() { unsafe { std::ptr::read_volatile(&0_u8); } }' \
  > "$fixture_root/crates/jarvis-package/src/escaped.inc"
printf '%s\n' \
  'use std::include as inclúde;' \
  'inclúde!("escaped.inc");' \
  >> "$fixture_root/crates/jarvis-package/src/lib.rs"
expect_cargo_accepts_private_source
expect_rejected "jarvis-package source discovery escape"

write_clean_fixture
printf '%s\n' \
  '#[allow(unsafe_code)]' \
  'pub fn escaped() { unsafe { std::ptr::read_volatile(&0_u8); } }' \
  > "$fixture_root/crates/jarvis-package/src/escaped.inc"
printf '%s\n' \
  'macro_rules! alias {' \
  '    ($macro_name:ident, $alias_name:ident) => {' \
  '        use std::$macro_name as $alias_name;' \
  '    };' \
  '}' \
  'alias!(include, hidden_source);' \
  'hidden_source!("escaped.inc");' \
  >> "$fixture_root/crates/jarvis-package/src/lib.rs"
expect_cargo_accepts_private_source
expect_rejected "jarvis-package source discovery escape"

write_clean_fixture
mkdir -p "$fixture_root/external-source/src"
printf '%s\n' \
  '[package]' \
  'name = "source-alias"' \
  'version = "0.1.0"' \
  'edition = "2021"' \
  > "$fixture_root/external-source/Cargo.toml"
printf '%s\n' \
  'pub use std::include as hidden_source;' \
  > "$fixture_root/external-source/src/lib.rs"
printf '%s\n' \
  '#[allow(unsafe_code)]' \
  'pub fn escaped() { unsafe { std::ptr::read_volatile(&0_u8); } }' \
  > "$fixture_root/crates/jarvis-package/src/escaped.inc"
printf '%s\n' \
  '' \
  '[dependencies]' \
  'source-alias = { path = "../../external-source" }' \
  >> "$fixture_root/crates/jarvis-package/Cargo.toml"
printf '%s\n' \
  'use source_alias::hidden_source;' \
  'hidden_source!("escaped.inc");' \
  >> "$fixture_root/crates/jarvis-package/src/lib.rs"
expect_cargo_accepts_private_source
expect_rejected "jarvis-package source discovery escape"

write_clean_fixture
printf '%s\n' \
  '#[allow(unsafe_code)]' \
  'pub fn escaped() { unsafe { std::ptr::read_volatile(&0_u8); } }' \
  > "$fixture_root/crates/jarvis-package/src/escaped.inc"
for ((alias_index = 2999; alias_index > 0; alias_index -= 1)); do
  printf 'use alias_%d as alias_%d;\n' \
    "$((alias_index - 1))" \
    "$alias_index" \
    >> "$fixture_root/crates/jarvis-package/src/lib.rs"
done
printf '%s\n' \
  'use std::include as alias_0;' \
  'alias_2999!("escaped.inc");' \
  >> "$fixture_root/crates/jarvis-package/src/lib.rs"
reverse_alias_scan="$(run_fixture_source_scan)"
if [[ "$reverse_alias_scan" != *$'\tinclude! source expansion'* ]]; then
  echo "trust scanner missed the bounded reverse alias stress chain" >&2
  echo "$reverse_alias_scan" >&2
  exit 1
fi

write_clean_fixture
for ((alias_index = 0; alias_index <= 4096; alias_index += 1)); do
  printf 'use source_%d as alias_%d;\n' \
    "$alias_index" \
    "$alias_index" \
    >> "$fixture_root/crates/jarvis-package/src/lib.rs"
done
alias_edge_budget_output=""
if alias_edge_budget_output="$(run_fixture_source_scan 2>&1)"; then
  echo "trust scanner accepted an oversized use-alias graph" >&2
  exit 1
fi
if [[ "$alias_edge_budget_output" != *"Rust use-alias graph exceeds 4096 edges"* ]]; then
  echo "trust scanner did not fail closed at the use-alias edge budget" >&2
  echo "$alias_edge_budget_output" >&2
  exit 1
fi

write_clean_fixture
for ((alias_index = 4094; alias_index >= 0; alias_index -= 1)); do
  printf 'use alias_%d as alias_%d;\n' \
    "$alias_index" \
    "$((alias_index + 1))" \
    >> "$fixture_root/crates/jarvis-package/src/lib.rs"
done
printf '%s\n' 'use std::include as alias_0;' \
  >> "$fixture_root/crates/jarvis-package/src/lib.rs"
alias_work_budget_output=""
if alias_work_budget_output="$(run_fixture_source_scan 2>&1)"; then
  echo "trust scanner accepted an oversized use-alias closure" >&2
  exit 1
fi
if [[ "$alias_work_budget_output" != *"Rust use-alias closure exceeds 8192 work units"* ]]; then
  echo "trust scanner did not fail closed at the use-alias work budget" >&2
  echo "$alias_work_budget_output" >&2
  exit 1
fi

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

node -e '
  const { readFileSync } = require("node:fs");
  const workflow = readFileSync(process.argv[1], "utf8");
  const boundaryScript = readFileSync(process.argv[2], "utf8");
  const prepareMarker =
    "      - name: Prepare plugin boundary Cargo metadata";
  const boundaryMarker = "      - name: Plugin source boundary";
  const prepareIndex = workflow.indexOf(prepareMarker);
  const boundaryIndex = workflow.indexOf(boundaryMarker);
  if (
    prepareIndex === -1 ||
    boundaryIndex === -1 ||
    prepareIndex >= boundaryIndex
  ) {
    throw new Error(
      "CI must prepare Cargo metadata before the offline plugin boundary",
    );
  }
  const preparation = workflow.slice(prepareIndex, boundaryIndex);
  const manifests = [
    "crates/jarvis-package/Cargo.toml",
    "crates/jarvis-plugin-protocol/Cargo.toml",
    "crates/jarvis-plugin-sdk/Cargo.toml",
    "crates/jarvis-plugin-test-host/Cargo.toml",
    "crates/jarvis-power-core/Cargo.toml",
    "crates/jarvis-power-helper/Cargo.toml",
    "crates/jarvis-secret-store/Cargo.toml",
    "plugins/agent-vm/Cargo.toml",
    "src-tauri/Cargo.toml",
  ];
  const fetches = [
    ...preparation.matchAll(
      /cargo fetch --locked --manifest-path ([^\s]+)/g,
    ),
  ].map((match) => match[1]);
  if (
    fetches.length !== manifests.length ||
    new Set(fetches).size !== fetches.length ||
    manifests.some((manifest) => !fetches.includes(manifest))
  ) {
    throw new Error(
      "CI Cargo metadata preparation must fetch each workspace root once",
    );
  }
  for (const flag of ["--all-features", "--locked", "--offline"]) {
    if (!boundaryScript.includes(flag)) {
      throw new Error(`plugin boundary metadata is missing ${flag}`);
    }
  }
  if (
    !boundaryScript.includes("--cargo-provenance-fd 9") ||
    boundaryScript.includes(
      "--cargo-provenance-file \"$cargo_provenance_file\"",
    )
  ) {
    throw new Error(
      "plugin boundary must hand scanner the already-open provenance inode",
    );
  }
' \
  "$repo_root/.github/workflows/ci.yml" \
  "$repo_root/scripts/check-plugin-boundaries.sh"

mkdir -p "$fixture_root/schemas"
printf '%s\n' '{}' > "$fixture_root/schemas/plugin-private-v1.schema.json"
expect_rejected "public plugin schema is not allowlisted"

echo "plugin boundary negative fixtures passed"
