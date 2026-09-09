#!/usr/bin/env bash
set -euo pipefail

repo_root="${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
private_manifest="$repo_root/crates/jarvis-package/Cargo.toml"
private_lock="$repo_root/crates/jarvis-package/Cargo.lock"
host_manifest="$repo_root/src-tauri/Cargo.toml"
host_lock="$repo_root/src-tauri/Cargo.lock"
failed=0

report() {
  echo "$1" >&2
  failed=1
}

lock_records() {
  awk '
    function emit() {
      if (name != "") {
        print name "\t" version "\t" checksum "\t" source
      }
    }
    /^\[\[package\]\]$/ {
      emit()
      name = ""
      version = ""
      checksum = ""
      source = ""
      next
    }
    /^name = "/ {
      name = $0
      sub(/^name = "/, "", name)
      sub(/"$/, "", name)
      next
    }
    /^version = "/ {
      version = $0
      sub(/^version = "/, "", version)
      sub(/"$/, "", version)
      next
    }
    /^checksum = "/ {
      checksum = $0
      sub(/^checksum = "/, "", checksum)
      sub(/"$/, "", checksum)
      next
    }
    /^source = "/ {
      source = $0
      sub(/^source = "/, "", source)
      sub(/"$/, "", source)
      next
    }
    END { emit() }
  ' "$1"
}

lock_dependencies() {
  local lock="$1"
  local wanted_name="$2"
  local wanted_version="$3"
  awk -v wanted_name="$wanted_name" -v wanted_version="$wanted_version" '
    function emit() {
      if (name == wanted_name && version == wanted_version) {
        for (dependency_index = 1; dependency_index <= dependency_count; dependency_index += 1) {
          print dependencies[dependency_index]
        }
      }
    }
    /^\[\[package\]\]$/ {
      emit()
      name = ""
      version = ""
      in_dependencies = 0
      dependency_count = 0
      delete dependencies
      next
    }
    /^name = "/ {
      name = $0
      sub(/^name = "/, "", name)
      sub(/"$/, "", name)
      next
    }
    /^version = "/ {
      version = $0
      sub(/^version = "/, "", version)
      sub(/"$/, "", version)
      next
    }
    /^dependencies = \[$/ {
      in_dependencies = 1
      next
    }
    in_dependencies && /^\]$/ {
      in_dependencies = 0
      next
    }
    in_dependencies {
      dependency = $0
      sub(/^[[:space:]]*"/, "", dependency)
      sub(/",[[:space:]]*$/, "", dependency)
      dependencies[++dependency_count] = dependency
    }
    END { emit() }
  ' "$lock" | LC_ALL=C sort
}

normal_dependencies() {
  awk '
    /^\[/ {
      in_normal_dependencies = ($0 == "[dependencies]")
      next
    }
    in_normal_dependencies { print }
  ' "$1"
}

manifest_package_version() {
  awk '
    /^\[/ {
      in_package = ($0 == "[package]")
      next
    }
    in_package && /^version = "/ {
      version = $0
      sub(/^version = "/, "", version)
      sub(/"$/, "", version)
      print version
      exit
    }
  ' "$1"
}

private_normal_dependencies="$(
  normal_dependencies "$private_manifest"
)"
if ! printf '%s\n' "$private_normal_dependencies" | rg -q \
  '^\s*getrandom\s*=\s*\{[^}]*version\s*=\s*"=0\.3\.4"[^}]*\}\s*$' \
  -; then
  report "private getrandom dependency must be a normal exact 0.3.4 dependency"
fi
if ! printf '%s\n' "$private_normal_dependencies" | rg -q \
  '^\s*tempfile\s*=\s*\{[^}]*version\s*=\s*"=3\.27\.0"[^}]*\}\s*$' \
  -; then
  report "private tempfile dependency must be pinned to 3.27.0"
fi

# This contract deliberately supports only the two audited runtime locations.
# Collect declarations from every section first, so a valid-looking line cannot
# hide a second definition in a dev/build/unsupported-target table.
host_package_declarations="$(
  awk -v macos_table="[target.'cfg(target_os = \"macos\")'.dependencies]" '
    /^[[:space:]]*\[/ {
      section = $0
      sub(/^[[:space:]]*/, "", section)
      sub(/[[:space:]]*$/, "", section)
      if (section ~ /jarvis-package/) {
        print "unsupported\t" section
      }
      next
    }
    /^[[:space:]]*["\047]?jarvis-package["\047]?[[:space:]]*=/ {
      scope = (section == "[dependencies]" || section == macos_table) ? "runtime" : "unsupported"
      print scope "\t" $0
      next
    }
    /package[[:space:]]*=[[:space:]]*["\047]jarvis-package["\047]/ {
      print "unsupported\t" $0
    }
  ' "$host_manifest"
)"
host_package_count="$(printf '%s\n' "$host_package_declarations" | awk 'NF { count++ } END { print count+0 }')"
if [[ "$host_package_count" -eq 0 ]]; then
  report "host jarvis-package dependency must be a normal dependency"
elif [[ "$host_package_count" -ne 1 ]]; then
  report "host jarvis-package dependency definitions are ambiguous"
elif [[ "$host_package_declarations" != runtime$'\t'* ]]; then
  report "host jarvis-package dependency must be a normal dependency in the global or supported macOS table"
else
  host_package_dependency="${host_package_declarations#*$'\t'}"
  if ! printf '%s\n' "$host_package_dependency" | rg -q \
    '^\s*jarvis-package\s*=\s*\{\s*path\s*=\s*"\.\./crates/jarvis-package"\s*\}\s*$' -; then
    report "host jarvis-package dependency must use the exact private path without overrides"
  fi
fi

private_records="$(lock_records "$private_lock")"
private_package_record="$(
  printf '%s\n' "$private_records" \
    | awk -F '\t' '$1 == "jarvis-package" { print $2 "\t" $3 "\t" $4 }'
)"
if [[ "$private_package_record" != $'0.1.0\t\t' ]]; then
  report "private lock must contain one path-only jarvis-package 0.1.0 record"
fi
private_package_dependencies="$(lock_dependencies "$private_lock" "jarvis-package" "0.1.0")"
expected_private_package_dependencies="$(
  printf '%s\n' \
    base64 \
    caseless \
    getrandom \
    jarvis-plugin-protocol \
    libc \
    rustix \
    serde \
    serde_json \
    serde_json_canonicalizer \
    sha2 \
    tar \
    tempfile \
    unicode-normalization \
    | LC_ALL=C sort
)"
if [[ "$private_package_dependencies" != "$expected_private_package_dependencies" ]]; then
  report "private jarvis-package dependency block changed"
fi

host_records="$(lock_records "$host_lock")"
host_package_record="$(
  printf '%s\n' "$host_records" \
    | awk -F '\t' '$1 == "jarvis-package" { print $2 "\t" $3 "\t" $4 }'
)"
if [[ "$host_package_record" != $'0.1.0\t\t' ]]; then
  report "host lock must contain one path-only jarvis-package 0.1.0 record"
fi
host_jarvis_version="$(manifest_package_version "$host_manifest")"
host_jarvis_dependencies="$(lock_dependencies "$host_lock" "jarvis" "$host_jarvis_version")"
if ! printf '%s\n' "$host_jarvis_dependencies" \
  | rg -q '^jarvis-package(?: 0\.1\.0)?$' -; then
  report "host jarvis lock record must depend on jarvis-package"
fi

private_tempfile="$(
  printf '%s\n' "$private_records" \
    | awk -F '\t' '$1 == "tempfile" { print $2 "\t" $3 }'
)"
if [[ "$private_tempfile" != \
  $'3.27.0\t32497e9a4c7b38532efcdebeef879707aa9f794296a4f0244f6f69e9bc8574bd' ]]; then
  report "private lock must contain exact tempfile 3.27.0"
fi
private_tempfile_dependencies="$(lock_dependencies "$private_lock" "tempfile" "3.27.0")"
expected_private_tempfile_dependencies="$(
  printf '%s\n' fastrand getrandom once_cell rustix windows-sys | LC_ALL=C sort
)"
if [[ "$private_tempfile_dependencies" != "$expected_private_tempfile_dependencies" ]]; then
  report "private tempfile dependency block changed"
fi
private_getrandom="$(
  printf '%s\n' "$private_records" \
    | awk -F '\t' '$1 == "getrandom" { print $2 }'
)"
if [[ "$private_getrandom" != "0.3.4" ]]; then
  report "private lock must contain getrandom 0.3.4 and no 0.4 release"
fi

host_tempfile="$(
  printf '%s\n' "$host_records" \
    | awk -F '\t' '$1 == "tempfile" { print $2 "\t" $3 }'
)"
if [[ "$host_tempfile" != \
  $'3.27.0\t32497e9a4c7b38532efcdebeef879707aa9f794296a4f0244f6f69e9bc8574bd' ]]; then
  report "host tempfile registry block changed"
fi
host_tempfile_dependencies="$(lock_dependencies "$host_lock" "tempfile" "3.27.0")"
expected_host_tempfile_dependencies="$(
  printf '%s\n' \
    fastrand \
    'getrandom 0.4.2' \
    once_cell \
    rustix \
    'windows-sys 0.61.2' \
    | LC_ALL=C sort
)"
if [[ "$host_tempfile_dependencies" != "$expected_host_tempfile_dependencies" ]]; then
  report "host tempfile dependency block changed"
fi
host_getrandom_04="$(
  printf '%s\n' "$host_records" \
    | awk -F '\t' '$1 == "getrandom" && $2 ~ /^0\.4\./ { print $2 "\t" $3 }'
)"
if [[ "$host_getrandom_04" != \
  $'0.4.2\t0de51e6874e94e7bf76d726fc5d13ba782deca734ff60d5bb2fb2607c7406555' ]]; then
  report "host getrandom registry block changed"
fi
host_getrandom_dependencies="$(lock_dependencies "$host_lock" "getrandom" "0.4.2")"
expected_host_getrandom_dependencies="$(
  printf '%s\n' cfg-if libc 'r-efi 6.0.0' wasip2 wasip3 | LC_ALL=C sort
)"
if [[ "$host_getrandom_dependencies" != "$expected_host_getrandom_dependencies" ]]; then
  report "host getrandom dependency block changed"
fi

global_resolver_config="$(
  rg -n --no-heading 'incompatible-rust-versions' \
    "$repo_root/.cargo" -g '*.toml' -g 'config' 2>/dev/null \
    || true
)"
if [[ -n "$global_resolver_config" ]]; then
  report "package resolver fallback must not be stored in repository Cargo config"
  echo "$global_resolver_config" >&2
fi

if [[ "$failed" -ne 0 ]]; then
  exit 1
fi

echo "package lock contract check passed"
