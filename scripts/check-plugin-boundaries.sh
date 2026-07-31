#!/usr/bin/env bash
set -euo pipefail

repo_root="${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
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
