#!/usr/bin/env bash
set -euo pipefail

repo_root="${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
failed=0

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

  unsafe_scan=""
  if ! unsafe_scan="$(node "$script_dir/scan-rust-unsafe-boundary.mjs" "$package_root")"; then
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
fi

package_root_resolved=""
if [[ -d "$package_root" ]]; then
  package_root_resolved="$(cd "$package_root" && pwd -P)"
fi
all_manifests="$(
  rg --files "$repo_root" 2>/dev/null \
    | rg '/Cargo\.toml$' \
    | rg -v '/target/' \
    || true
)"
while IFS= read -r manifest; do
  [[ -z "$manifest" ]] && continue
  dependency_matches="$(
    rg -n --no-heading \
      '^\s*(jarvis-package|["'"'"']jarvis-package["'"'"'])\s*=|^\s*\[[^]]*dependencies\.(jarvis-package|["'"'"']jarvis-package["'"'"'])\]\s*$|package\s*=\s*["'"'"']jarvis-package["'"'"']' \
      "$manifest" \
      || true
  )"

  path_matches="$(rg -n --no-heading 'path\s*=\s*["'"'"'][^"'"'"']+["'"'"']' "$manifest" || true)"
  while IFS= read -r path_match; do
    [[ -z "$path_match" ]] && continue
    path_line="${path_match#*:}"
    path_line="${path_line#*:}"
    dependency_path="$(
      printf '%s\n' "$path_line" \
        | sed -E 's/.*path[[:space:]]*=[[:space:]]*["'"'"']([^"'"'"']+)["'"'"'].*/\1/'
    )"
    resolved_path=""
    if [[ -d "$(dirname "$manifest")/$dependency_path" ]]; then
      resolved_path="$(cd "$(dirname "$manifest")/$dependency_path" && pwd -P)"
    fi
    if [[ -n "$package_root_resolved" ]] && [[ "$resolved_path" == "$package_root_resolved" ]]; then
      dependency_matches+="${dependency_matches:+$'\n'}$path_match"
    fi
  done <<< "$path_matches"

  if [[ -n "$dependency_matches" ]] && [[ "$manifest" != "$repo_root/src-tauri/Cargo.toml" ]]; then
    if [[ "$manifest" == "$repo_root"/crates/jarvis-plugin-*/Cargo.toml ]] \
      || [[ "$manifest" == "$repo_root"/plugins/*/Cargo.toml ]]; then
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
