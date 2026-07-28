#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(/usr/bin/dirname -- "$0")/.." && /bin/pwd)
target_triple=${TAURI_ENV_TARGET_TRIPLE:-}

if [ -z "$target_triple" ]; then
  target_triple=$(rustc -vV | /usr/bin/sed -n 's/^host: //p')
fi

if [ -z "$target_triple" ]; then
  echo "Cannot determine Rust target triple" >&2
  exit 1
fi

cargo build \
  --release \
  --manifest-path "$repo_root/plugins/agent-vm/Cargo.toml" \
  --target "$target_triple"

source_binary="$repo_root/plugins/agent-vm/target/$target_triple/release/jarvis-agent-vm-plugin"
destination_dir="$repo_root/src-tauri/binaries"
destination_binary="$destination_dir/jarvis-agent-vm-plugin-$target_triple"

/bin/mkdir -p "$destination_dir"
/bin/cp "$source_binary" "$destination_binary"
/bin/chmod 755 "$destination_binary"

