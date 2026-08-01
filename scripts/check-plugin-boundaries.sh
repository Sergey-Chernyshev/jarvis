#!/usr/bin/env bash
set -euo pipefail

repo_root="${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cargo_bin="${CARGO_BIN:-cargo}"
failed=0

repo_root_resolved="$(cd "$repo_root" && pwd -P)"
boundary_fixture_mode=0
if [[ "${JARVIS_BOUNDARY_ALLOW_UNLOCKED_FIXTURES:-0}" == "1" ]]; then
  tmp_root_resolved="$(cd "${TMPDIR:-/tmp}" && pwd -P)"
  case "$repo_root_resolved" in
    "$tmp_root_resolved"/jarvis-plugin-boundary.*)
      boundary_fixture_mode=1
      ;;
    *)
      echo "unlocked Cargo boundary mode is restricted to temporary fixtures: $repo_root" >&2
      exit 1
      ;;
  esac
fi

semantic_manifest_json() {
  local manifest="$1"
  if [[ "$boundary_fixture_mode" == "1" ]]; then
    "$cargo_bin" read-manifest --manifest-path "$manifest"
  else
    "$cargo_bin" metadata \
      --no-deps \
      --format-version=1 \
      --locked \
      --offline \
      --manifest-path "$manifest"
  fi
}

report_matches() {
  local message="$1"
  local matches="$2"
  if [[ -n "$matches" ]]; then
    echo "$message" >&2
    echo "$matches" >&2
    failed=1
  fi
}

public_manifests="$(
  rg --files "$repo_root/crates" 2>/dev/null \
    | rg '/jarvis-plugin-[^/]+/Cargo\.toml$' \
    || true
)"

while IFS= read -r manifest; do
  [[ -z "$manifest" ]] && continue
  matches="$(
    rg -n --no-heading "path\\s*=\\s*['\"][^'\"]*src-tauri(?:/|['\"])" \
      "$manifest" \
      || true
  )"
  report_matches "public plugin crate must not depend on src-tauri:" "$matches"

  matches="$(
    rg -n --no-heading \
      "^\\s*(jarvis|jarvis-core)\\s*=|^\\s*\\[[^]]*dependencies\\.(jarvis|jarvis-core)\\]\\s*$|package\\s*=\\s*['\"](jarvis|jarvis-core)['\"]" \
      "$manifest" \
      || true
  )"
  report_matches "public plugin crate must not depend on Jarvis Core:" "$matches"

  crate_root="$(dirname "$manifest")"
  if ! rg -q '^#!\[forbid\(unsafe_code\)\]$' "$crate_root/src/lib.rs"; then
    echo "public plugin crate must forbid unsafe code: $crate_root/src/lib.rs" >&2
    failed=1
  fi

  matches="$(
    rg -n --no-heading '(extern\s+crate\s+jarvis|use\s+jarvis::)' \
      "$crate_root/src" -g '*.rs' \
      || true
  )"
  report_matches "public plugin crate source must not import Jarvis Core:" "$matches"
done <<< "$public_manifests"

package_root="$repo_root/crates/jarvis-package"
package_manifest="$package_root/Cargo.toml"
package_lib="$package_root/src/lib.rs"
allowed_trust_verifier="$repo_root_resolved/src-tauri/src/plugins/trust/package.rs"
allowed_private_test_verifier_root="$repo_root_resolved/crates/jarvis-package"

if [[ -f "$package_manifest" ]]; then
  if ! rg -q '^\s*publish\s*=\s*false\s*$' "$package_manifest"; then
    echo "jarvis-package must set publish = false: $package_manifest" >&2
    failed=1
  fi
  if ! rg -q '^\s*edition\s*=\s*"2021"\s*$' "$package_manifest"; then
    echo "jarvis-package must set edition = \"2021\": $package_manifest" >&2
    failed=1
  fi
  if ! rg -q '^\s*rust-version\s*=\s*"1\.77\.2"\s*$' "$package_manifest"; then
    echo "jarvis-package must set rust-version = \"1.77.2\": $package_manifest" >&2
    failed=1
  fi
  if [[ ! -f "$package_lib" ]] || ! rg -q '^#!\[deny\(unsafe_code\)\]$' "$package_lib"; then
    echo "jarvis-package crate root must deny unsafe code: $package_lib" >&2
    failed=1
  fi

  package_manifest_json=""
  if ! package_manifest_json="$(semantic_manifest_json "$package_manifest")"; then
    echo "failed to parse private package targets semantically: $package_manifest" >&2
    failed=1
  fi
  package_target_sources=()
  if [[ -n "$package_manifest_json" ]]; then
    package_target_source_lines=""
    if ! package_target_source_lines="$(
      printf '%s\n' "$package_manifest_json" \
        | node -e '
          const { realpathSync } = require("node:fs");
          let source = "";
          process.stdin.setEncoding("utf8");
          process.stdin.on("data", (chunk) => { source += chunk; });
          process.stdin.on("end", () => {
            const payload = JSON.parse(source);
            const packages = payload.packages ?? [payload];
            const wantedManifest = realpathSync(process.argv[1]);
            const packageRecord = packages.find(
              (candidate) => realpathSync(candidate.manifest_path) === wantedManifest,
            );
            if (!packageRecord) process.exit(2);
            for (const target of packageRecord.targets) {
              process.stdout.write(`${target.src_path}\n`);
            }
          });
        ' "$package_manifest"
    )"; then
      echo "failed to select private package target metadata: $package_manifest" >&2
      failed=1
    else
      while IFS= read -r target_source; do
        [[ -n "$target_source" ]] && package_target_sources+=("$target_source")
      done <<< "$package_target_source_lines"
    fi
  fi
  unsafe_scan=""
  if ! unsafe_scan="$(
    node \
      "$script_dir/scan-rust-unsafe-boundary.mjs" \
      "$package_root" \
      "${package_target_sources[@]}"
  )"; then
    echo "failed to scan jarvis-package Rust syntax: $package_root" >&2
    failed=1
  fi
  unsafe_allows="$(
    printf '%s\n' "$unsafe_scan" \
      | awk -F '\t' '$1 == "allow" { print $2 }'
  )"
  unsafe_allow_count=0
  if [[ -n "$unsafe_allows" ]]; then
    unsafe_allow_count="$(printf '%s\n' "$unsafe_allows" | wc -l | tr -d '[:space:]')"
  fi
  if [[ "$unsafe_allow_count" -ne 1 ]] || ! awk '
    $0 == "#[cfg(target_os = \"macos\")]" {
      if ((getline allow) > 0 && (getline module) > 0 &&
          allow == "#[allow(unsafe_code)]" && module == "mod macos_dir;") {
        found += 1
      }
    }
    END { exit(found == 1 ? 0 : 1) }
  ' "$package_lib"; then
    echo "jarvis-package unsafe allow must be exactly scoped to macos_dir: $package_lib" >&2
    [[ -n "$unsafe_allows" ]] && echo "$unsafe_allows" >&2
    failed=1
  fi

  unsafe_syntax="$(
    printf '%s\n' "$unsafe_scan" \
      | awk -F '\t' '$1 == "unsafe" { print $2 }'
  )"
  allowed_unsafe_path="$(cd "$package_root/src" && pwd -P)/macos_dir.rs"
  disallowed_unsafe=""
  while IFS= read -r match; do
    [[ -z "$match" ]] && continue
    match_path="${match%%:*}"
    if [[ "$match_path" != "$allowed_unsafe_path" ]]; then
      disallowed_unsafe+="${disallowed_unsafe:+$'\n'}$match"
    fi
  done <<< "$unsafe_syntax"
  report_matches "jarvis-package unsafe syntax outside macos_dir.rs:" "$disallowed_unsafe"
  source_escapes="$(
    printf '%s\n' "$unsafe_scan" \
      | awk -F '\t' '$1 == "source" { print $2 " (" $3 ")" }'
  )"
  report_matches "jarvis-package source discovery escape:" "$source_escapes"
fi

trust_roots=()
for trust_root in "$repo_root/crates" "$repo_root/plugins" "$repo_root/src-tauri"; do
  [[ -d "$trust_root" ]] && trust_roots+=("$trust_root")
done
trust_scan=""
if [[ "${#trust_roots[@]}" -gt 0 ]] && ! trust_scan="$(
  node "$script_dir/scan-rust-unsafe-boundary.mjs" \
    --trust-roots "${trust_roots[@]}" \
    | awk -F '\t' '$1 == "trust" || $1 == "trust-test" { print }'
)"; then
  echo "failed to scan PackageTrustVerifier ownership roots" >&2
  failed=1
  trust_scan=""
fi
disallowed_trust=""
while IFS=$'\t' read -r kind match; do
  [[ -z "$match" ]] && continue
  match_path="${match%%:*}"
  if [[ "$match_path" == "$allowed_trust_verifier" ]]; then
    continue
  fi
  if [[ "$kind" == "trust-test" && "$match_path" == "$allowed_private_test_verifier_root"/* ]]; then
    continue
  fi
  disallowed_trust+="${disallowed_trust:+$'\n'}$match"
done <<< "$trust_scan"
report_matches \
  "PackageTrustVerifier production implementation outside host trust adapter:" \
  "$disallowed_trust"

package_root_resolved=""
if [[ -d "$package_root" ]]; then
  package_root_resolved="$(cd "$package_root" && pwd -P)"
fi
if [[ "$boundary_fixture_mode" == "1" ]]; then
  all_manifests="$(
    find "$repo_root" -type f -name Cargo.toml -print \
      | LC_ALL=C sort
  )"
else
  all_manifests="$(
    while IFS= read -r -d '' manifest_relative; do
      printf '%s/%s\n' "$repo_root_resolved" "$manifest_relative"
    done < <(
      git -C "$repo_root_resolved" ls-files \
        --cached \
        --others \
        --exclude-standard \
        -z \
        -- ':(glob)**/Cargo.toml'
    )
  )"
fi
crates_root_resolved="$(cd "$repo_root/crates" && pwd -P)"
plugins_root_resolved="$(cd "$repo_root/plugins" && pwd -P)"
allowed_package_manifest="$(cd "$repo_root/src-tauri" && pwd -P)/Cargo.toml"
while IFS= read -r manifest; do
  [[ -z "$manifest" ]] && continue
  manifest_resolved="$(cd "$(dirname "$manifest")" && pwd -P)/$(basename "$manifest")"
  manifest_json=""
  if ! manifest_json="$(semantic_manifest_json "$manifest")"; then
    echo "failed to parse Cargo manifest semantically: $manifest" >&2
    failed=1
    continue
  fi
  semantic_dependencies=""
  if ! semantic_dependencies="$(
    printf '%s\n' "$manifest_json" \
      | node -e '
        const { realpathSync } = require("node:fs");
        let source = "";
        process.stdin.setEncoding("utf8");
        process.stdin.on("data", (chunk) => { source += chunk; });
        process.stdin.on("end", () => {
          const payload = JSON.parse(source);
          const packages = payload.packages ?? [payload];
          const wantedManifest = realpathSync(process.argv[1]);
          const packageRecord = packages.find(
            (candidate) => realpathSync(candidate.manifest_path) === wantedManifest,
          );
          if (!packageRecord) process.exit(2);
          for (const dependency of packageRecord.dependencies) {
            process.stdout.write(
              `${dependency.name}\t${dependency.rename ?? ""}\t${dependency.path ?? ""}\n`,
            );
          }
        });
      ' "$manifest_resolved"
  )"; then
    echo "failed to select Cargo package metadata: $manifest" >&2
    failed=1
    continue
  fi
  dependency_matches=""
  while IFS=$'\t' read -r dependency_name dependency_rename dependency_path; do
    [[ -z "$dependency_name" ]] && continue
    dependency_path_resolved=""
    if [[ -n "$dependency_path" ]] && [[ -d "$dependency_path" ]]; then
      dependency_path_resolved="$(cd "$dependency_path" && pwd -P)"
    fi
    if [[ "$dependency_name" == "jarvis-package" ]] \
      || [[ -n "$package_root_resolved" \
        && "$dependency_path_resolved" == "$package_root_resolved" ]]; then
      if [[ -n "$dependency_matches" ]]; then
        dependency_matches+=$'\n'
      fi
      dependency_matches+="$manifest: package=$dependency_name rename=${dependency_rename:--} path=${dependency_path:--}"
    fi
  done <<< "$semantic_dependencies"

  if [[ -n "$dependency_matches" ]] \
    && [[ "$manifest_resolved" != "$allowed_package_manifest" ]]; then
    if [[ "$manifest_resolved" == "$crates_root_resolved"/jarvis-plugin-*/Cargo.toml ]] \
      || [[ "$manifest_resolved" == "$plugins_root_resolved"/*/Cargo.toml ]]; then
      report_matches \
        "public or plugin crate depends on jarvis-package:" \
        "$dependency_matches"
    else
      report_matches \
        "only src-tauri may depend on jarvis-package:" \
        "$dependency_matches"
    fi
  fi
done <<< "$all_manifests"

public_schemas="$(
  rg --files "$repo_root/schemas" 2>/dev/null \
    | rg '/plugin-[^/]+\.schema\.json$' \
    || true
)"
while IFS= read -r schema; do
  [[ -z "$schema" ]] && continue
  relative_schema="${schema#"$repo_root"/}"
  case "$relative_schema" in
    schemas/plugin-manifest-v2.schema.json \
      | schemas/plugin-package-v1.schema.json \
      | schemas/plugin-package-signature-v1.schema.json \
      | schemas/plugin-broker-v1.schema.json \
      | schemas/plugin-ui-bridge-v1.schema.json \
      | schemas/plugin-contribution-v1.schema.json \
      | schemas/plugin-settings-v1.schema.json)
      ;;
    *)
      echo "public plugin schema is not allowlisted: $relative_schema" >&2
      failed=1
      ;;
  esac
done <<< "$public_schemas"

plugin_manifests="$(
  rg --files "$repo_root/plugins" 2>/dev/null \
    | rg '/Cargo\.toml$' \
    || true
)"
while IFS= read -r manifest; do
  [[ -z "$manifest" ]] && continue
  matches="$(
    rg -n --no-heading "path\\s*=\\s*['\"][^'\"]*src-tauri(?:/|['\"])" \
      "$manifest" \
      || true
  )"
  report_matches "plugin crate must not depend on src-tauri:" "$matches"
done <<< "$plugin_manifests"

secret_dependencies="$(
  rg -n --no-heading --fixed-strings 'jarvis-secret-store' \
    "$repo_root/plugins" -g 'Cargo.toml' \
    || true
)"
while IFS= read -r dependency; do
  [[ -z "$dependency" ]] && continue
  dependency_path="${dependency%%:*}"
  line_and_content="${dependency#*:}"
  dependency_content="${line_and_content#*:}"
  if [[ "$dependency_path" != "$repo_root/plugins/agent-vm/Cargo.toml" ]] \
    || [[ "$dependency_content" != 'jarvis-secret-store = { path = "../../crates/jarvis-secret-store" }' ]]; then
    echo "new direct jarvis-secret-store plugin dependency is forbidden: $dependency" >&2
    failed=1
  fi
done <<< "$secret_dependencies"

forbidden_plugin_imports="$(
  rg -n --no-heading '(src_tauri|jarvis::daemon|jarvis::plugins)' \
    "$repo_root/plugins" -g '*.rs' \
    || true
)"
report_matches "plugin source imports a Jarvis Core implementation module:" "$forbidden_plugin_imports"

if [[ "$failed" -ne 0 ]]; then
  exit 1
fi

echo "plugin boundary check passed"
