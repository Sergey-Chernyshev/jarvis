#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
manifest="$repo_root/crates/jarvis-power-helper/Cargo.toml"
deployment_target="${MACOSX_DEPLOYMENT_TARGET:-13.0}"

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "production power-helper builds require macOS" >&2
  exit 1
fi
if [[ "$deployment_target" != "13.0" ]]; then
  echo "production power-helper requires MACOSX_DEPLOYMENT_TARGET=13.0" >&2
  exit 1
fi

mode="${1:-}"
case "$mode" in
  --unsigned-test)
    MACOSX_DEPLOYMENT_TARGET=13.0 \
      cargo build \
        --locked \
        --release \
        --manifest-path "$manifest" \
        --no-default-features \
        --features unsigned-test \
        --bin jarvis-power-helper
    binary="$repo_root/crates/jarvis-power-helper/target/release/jarvis-power-helper"
    binary_strings="$(strings "$binary")"
    if ! rg -q \
      -- \
      'unsigned-test power-helper builds are compile-only and cannot serve requests' \
      <<<"$binary_strings"; then
      echo "unsigned-test binary is missing its mandatory runtime refusal" >&2
      exit 1
    fi
    if rg -q \
      -- \
      'power-helper-dev\.sock|\.power-helper-dev\.cleanup-residue' \
      <<<"$binary_strings"; then
      echo "production helper inherited development UDS artifacts" >&2
      exit 1
    fi
    build_info="$(xcrun vtool -show-build "$binary")"
    rg -q -- 'minos 13\.0' <<<"$build_info"
    echo "unsigned-test power-helper compiled without registration or execution"
    ;;
  --production)
    : "${APPLE_TEAM_ID:?APPLE_TEAM_ID is required}"
    : "${JARVIS_APP_BUILD:?JARVIS_APP_BUILD is required}"
    : "${POWER_HELPER_SIGNING_IDENTITY:?POWER_HELPER_SIGNING_IDENTITY is required}"
    : "${POWER_HELPER_OUTPUT:?POWER_HELPER_OUTPUT is required}"
    if [[ ! "$POWER_HELPER_OUTPUT" = /* ]]; then
      echo "POWER_HELPER_OUTPUT must be an absolute file path" >&2
      exit 1
    fi
    MACOSX_DEPLOYMENT_TARGET=13.0 \
      APPLE_TEAM_ID="$APPLE_TEAM_ID" \
      JARVIS_APP_BUILD="$JARVIS_APP_BUILD" \
      cargo build \
        --locked \
        --release \
        --manifest-path "$manifest" \
        --no-default-features \
        --features production-xpc \
        --bin jarvis-power-helper
    binary="$repo_root/crates/jarvis-power-helper/target/release/jarvis-power-helper"
    install -m 0755 "$binary" "$POWER_HELPER_OUTPUT"
    codesign \
      --force \
      --options runtime \
      --sign "$POWER_HELPER_SIGNING_IDENTITY" \
      --entitlements "$repo_root/src-tauri/PowerHelper/helper.entitlements.plist" \
      "$POWER_HELPER_OUTPUT"
    codesign --verify --strict --verbose=2 "$POWER_HELPER_OUTPUT"
    build_info="$(xcrun vtool -show-build "$POWER_HELPER_OUTPUT")"
    rg -q -- 'minos 13\.0' <<<"$build_info"
    ;;
  *)
    echo "usage: $0 --unsigned-test | --production" >&2
    exit 64
    ;;
esac
