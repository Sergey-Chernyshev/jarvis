#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
diagnostics="$(mktemp "${TMPDIR:-/tmp}/jarvis-power-helper-clippy.XXXXXX")"
trap 'rm -f "$diagnostics"' EXIT

cargo clippy \
  --manifest-path "$repo_root/src-tauri/Cargo.toml" \
  --all-targets \
  --locked \
  --no-default-features \
  --features power-helper-dev \
  --message-format=json >"$diagnostics"

node "$repo_root/scripts/check-power-helper-host-scope.cjs" "$diagnostics"
