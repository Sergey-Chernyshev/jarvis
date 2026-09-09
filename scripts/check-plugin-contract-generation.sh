#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
temp_dir=$(mktemp -d "${TMPDIR:-/tmp}/jarvis-plugin-contracts.XXXXXX")
trap 'rm -rf "$temp_dir"' EXIT

schema_dir="$temp_dir/schemas"
typescript_out="$temp_dir/contracts.ts"
mkdir -p "$schema_dir"

cargo run --quiet \
  --manifest-path "$repo_root/crates/jarvis-plugin-protocol/Cargo.toml" \
  --bin export_ui_contracts \
  -- \
  --out-dir "$schema_dir"
node "$repo_root/scripts/generate-plugin-ui-contracts.mjs" \
  --schema-dir "$schema_dir" \
  --typescript-out "$typescript_out"

for filename in \
  plugin-broker-v1.schema.json \
  plugin-ui-bridge-v1.schema.json \
  plugin-contribution-v1.schema.json \
  plugin-settings-v1.schema.json
do
  if ! cmp -s "$repo_root/schemas/$filename" "$schema_dir/$filename"; then
    echo "generated contract differs: schemas/$filename" >&2
    exit 1
  fi
done

if ! cmp -s \
  "$repo_root/packages/jarvis-plugin-ui/src/generated/contracts.ts" \
  "$typescript_out"
then
  echo "generated contract differs: packages/jarvis-plugin-ui/src/generated/contracts.ts" >&2
  exit 1
fi
