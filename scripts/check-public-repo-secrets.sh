#!/bin/bash
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root"

scanner_path="scripts/check-public-repo-secrets.sh"
failed=0

report() {
  printf 'public-repo guard: %s in %s\n' "$1" "$2" >&2
  failed=1
}

scan_file() {
  local file=$1
  [ -f "$file" ] || return
  [ "$file" = "$scanner_path" ] && return

  case "/$file" in
    */.env|*/auth.json|*/.credentials.json|*/tokens.json)
      report "forbidden credential filename" "$file"
      ;;
  esac

  LC_ALL=C grep -Iq . "$file" || return 0

  local brand_re='t''bank|tin''koff|т''-банк|тинь''кофф'
  local token_re='(sk|rk|pk)-[A-Za-z0-9_-]{20,}|ghp_[A-Za-z0-9]{20,}|glpat-[A-Za-z0-9_-]{20,}|AKIA[A-Z0-9]{16}'
  local private_key_re='BEGIN (RSA |EC |OPENSSH )?PRIVATE KEY'
  local url_userinfo_re='https?://[^[:space:]/:@]+:[^[:space:]/@]+@'

  LC_ALL=C grep -Eiq -- "$brand_re" "$file" &&
    report "employer-specific marker" "$file"
  LC_ALL=C grep -Eq -- "$token_re" "$file" &&
    report "credential-like token" "$file"
  LC_ALL=C grep -Eiq -- "$private_key_re" "$file" &&
    report "private key material" "$file"
  case "$file" in
    docs/superpowers/specs/2026-06-26-model-manager-design.md|\
    src-tauri/src/claude_bin.rs|\
    src-tauri/src/log.rs|\
    src-tauri/src/settings.rs|\
    ui/onboarding.js|\
    ui/settings2.js)
      # Pre-existing synthetic proxy/redaction examples in the public repository.
      ;;
    *)
      LC_ALL=C grep -Eiq -- "$url_userinfo_re" "$file" &&
        report "URL containing userinfo" "$file"
      ;;
  esac
  return 0
}

if [ "$#" -gt 0 ]; then
  for file in "$@"; do
    scan_file "$file"
  done
else
  while IFS= read -r -d '' file; do
    scan_file "$file"
  done < <(git ls-files --cached --others --exclude-standard -z)
fi

if [ "$failed" -ne 0 ]; then
  printf 'public-repo guard failed; no content was printed\n' >&2
  exit 1
fi

printf 'public-repo guard: clean\n'
