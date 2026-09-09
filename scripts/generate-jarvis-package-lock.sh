#!/usr/bin/env bash
set -euo pipefail

repo_root="${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
cargo_bin="${CARGO_BIN:-cargo}"
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

"$cargo_bin" \
  --config 'resolver.incompatible-rust-versions="fallback"' \
  generate-lockfile \
  --manifest-path "$repo_root/crates/jarvis-package/Cargo.toml"

bash "$script_dir/check-package-lock-contract.sh" "$repo_root"
