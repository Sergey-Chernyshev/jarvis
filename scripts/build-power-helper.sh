#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
manifest="$repo_root/crates/jarvis-power-helper/Cargo.toml"
tauri_config="$repo_root/src-tauri/tauri.conf.json"
deployment_target="${MACOSX_DEPLOYMENT_TARGET:-13.0}"

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "production power-helper builds require macOS" >&2
  exit 1
fi
if [[ "$deployment_target" != "13.0" ]]; then
  echo "production power-helper requires MACOSX_DEPLOYMENT_TARGET=13.0" >&2
  exit 1
fi
configured_build="$(/usr/bin/plutil -extract bundle.macOS.bundleVersion raw -o - "$tauri_config")"
if [[ ! "$configured_build" =~ ^[1-9][0-9]*$ ]]; then
  echo "bundle.macOS.bundleVersion must be a canonical positive decimal" >&2
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
    : "${POWER_HELPER_SIGNING_IDENTITY:?POWER_HELPER_SIGNING_IDENTITY is required}"
    : "${POWER_HELPER_OUTPUT:?POWER_HELPER_OUTPUT is required}"
    if [[ ! "$APPLE_TEAM_ID" =~ ^[A-Z0-9]{10}$ ]]; then
      echo "APPLE_TEAM_ID must be exactly 10 uppercase letters or digits" >&2
      exit 1
    fi
    if [[ "$POWER_HELPER_SIGNING_IDENTITY" == "-" ]]; then
      echo "ad-hoc signing is forbidden for the production power-helper" >&2
      exit 1
    fi
    if [[ -n "${JARVIS_APP_BUILD:-}" && "$JARVIS_APP_BUILD" != "$configured_build" ]]; then
      echo "JARVIS_APP_BUILD must equal configured bundleVersion $configured_build" >&2
      exit 1
    fi
    JARVIS_APP_BUILD="$configured_build"
    if [[ ! "$POWER_HELPER_OUTPUT" = /* ]]; then
      echo "POWER_HELPER_OUTPUT must be an absolute file path" >&2
      exit 1
    fi
    output_parent_input="$(dirname -- "$POWER_HELPER_OUTPUT")"
    output_name="$(basename -- "$POWER_HELPER_OUTPUT")"
    if [[ -z "$output_name" || "$output_name" == "." || "$output_name" == ".." ||
          ! -d "$output_parent_input" ]]; then
      echo "POWER_HELPER_OUTPUT must name a file inside an existing directory" >&2
      exit 1
    fi
    output_parent="$(cd -P -- "$output_parent_input" && pwd)"
    final_output="$output_parent/$output_name"
    if [[ -L "$final_output" || -d "$final_output" ||
          ( -e "$final_output" && ! -f "$final_output" ) ]]; then
      echo "POWER_HELPER_OUTPUT must be absent or a regular non-symlink file" >&2
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
    staging="$(mktemp "$output_parent/.${output_name}.stage.XXXXXX")"
    requirement_blob="$staging.requirements"
    cleanup_staging() {
      if [[ -n "${staging:-}" && "$staging" == "$output_parent/.${output_name}.stage."* ]]; then
        rm -f -- "$staging" "$requirement_blob"
      fi
    }
    trap cleanup_staging EXIT HUP INT TERM
    install -m 0755 "$binary" "$staging"

    helper_identifier="app.jarvis.monitor.power-helper"
    designated_requirement="designated => anchor apple generic and identifier \"$helper_identifier\" and certificate leaf[subject.OU] = \"$APPLE_TEAM_ID\" and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists"
    csreq -r="$designated_requirement" -b "$requirement_blob"
    canonical_requirement="$(csreq -r "$requirement_blob" -t)"
    codesign \
      --force \
      --options runtime \
      --timestamp \
      --identifier "$helper_identifier" \
      --requirements "$requirement_blob" \
      --sign "$POWER_HELPER_SIGNING_IDENTITY" \
      --entitlements "$repo_root/src-tauri/PowerHelper/helper.entitlements.plist" \
      "$staging"
    codesign \
      --verify \
      --strict \
      --verbose=4 \
      -R="$designated_requirement" \
      "$staging"
    signing_details="$(codesign \
      --display \
      --verbose=4 \
      --requirements - \
      "$staging" 2>&1)"
    signed_team="$(
      printf '%s\n' "$signing_details" |
        /usr/bin/awk -F= '$1 == "TeamIdentifier" { print substr($0, index($0, "=") + 1) }'
    )"
    signed_identifier="$(
      printf '%s\n' "$signing_details" |
        /usr/bin/awk -F= '$1 == "Identifier" { print substr($0, index($0, "=") + 1) }'
    )"
    if [[ "$signed_team" != "$APPLE_TEAM_ID" ||
          "$signed_identifier" != "$helper_identifier" ]]; then
      echo "signed helper identity does not match the configured Team ID and identifier" >&2
      exit 1
    fi
    for evidence in \
      "Authority=Developer ID Application:" \
      "Authority=Developer ID Certification Authority" \
      "Authority=Apple Root CA"; do
      if ! /usr/bin/grep -F -q -- "$evidence" <<<"$signing_details"; then
        echo "signed helper is missing required evidence: $evidence" >&2
        exit 1
      fi
    done
    if ! /usr/bin/grep -F -x -q -- "$canonical_requirement" <<<"$signing_details"; then
      echo "signed helper designated requirement does not match compiled policy" >&2
      exit 1
    fi
    if /usr/bin/grep -F -q -- "Signature=adhoc" <<<"$signing_details"; then
      echo "ad-hoc production helper signature is forbidden" >&2
      exit 1
    fi
    build_info="$(xcrun vtool -show-build "$staging")"
    rg -q -- 'minos 13\.0' <<<"$build_info"
    if [[ -L "$final_output" || -d "$final_output" ||
          ( -e "$final_output" && ! -f "$final_output" ) ]]; then
      echo "POWER_HELPER_OUTPUT changed to an unsafe file before publish" >&2
      exit 1
    fi
    /bin/mv -f -- "$staging" "$final_output"
    staging=""
    rm -f -- "$requirement_blob"
    trap - EXIT HUP INT TERM
    ;;
  *)
    echo "usage: $0 --unsigned-test | --production" >&2
    exit 64
    ;;
esac
