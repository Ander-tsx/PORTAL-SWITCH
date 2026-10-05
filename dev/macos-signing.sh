#!/usr/bin/env bash
# Stable local code-signing identity for macOS development builds.
#
# Why: macOS ties Accessibility / Input Monitoring grants to the code identity of the
# process. Ad-hoc signatures change on every build, so the grants are lost. Signing
# every build with the same certificate keeps the grants across rebuilds.
#
# Usage:
#   dev/macos-signing.sh setup            create the identity (once per machine)
#   dev/macos-signing.sh sign <path>      sign an .app bundle or a binary
#   dev/macos-signing.sh status [path]    show identity and, if given, the signature of <path>
#   dev/macos-signing.sh reset <bundle-id>  reset granted privacy permissions for an app
#   dev/macos-signing.sh remove           delete the identity
#
# Environment:
#   PS_SIGN_IDENTITY       certificate common name (default: "PortalSwitch Dev Signing")
#   PS_KEYCHAIN            keychain to use (default: login keychain)
#   PS_KEYCHAIN_PASSWORD   if set, pre-authorizes codesign so no prompt appears
#   PS_ENTITLEMENTS        optional entitlements plist used by `sign`

set -euo pipefail

NAME="${PS_SIGN_IDENTITY:-PortalSwitch Dev Signing}"
KEYCHAIN="${PS_KEYCHAIN:-$HOME/Library/Keychains/login.keychain-db}"

die() { echo "error: $*" >&2; exit 1; }

identity_exists() {
  security find-certificate -c "$NAME" "$KEYCHAIN" >/dev/null 2>&1
}

# SHA-1 of the signing identity. Self-signed identities are reported as "not trusted",
# and codesign only accepts them when addressed by hash.
identity_hash() {
  security find-identity -p codesigning "$KEYCHAIN" \
    | awk -v n="\"$NAME\"" 'index($0, n) { print $2; exit }'
}

# codesign resolves hashes through the keychain search list, so make sure ours is in it
# for the duration of the script (restored on exit).
ORIGINAL_SEARCH_LIST=()
ensure_in_search_list() {
  local current=() line
  while IFS= read -r line; do
    [[ -n "$line" ]] && current+=("$line")
  done < <(security list-keychains -d user | tr -d '"' | sed 's/^ *//')
  if ! printf '%s\n' ${current[@]+"${current[@]}"} | grep -qxF "$KEYCHAIN"; then
    ORIGINAL_SEARCH_LIST=(${current[@]+"${current[@]}"})
    security list-keychains -d user -s "$KEYCHAIN" ${current[@]+"${current[@]}"}
    trap restore_search_list EXIT
  fi
}
restore_search_list() {
  if [[ ${#ORIGINAL_SEARCH_LIST[@]} -gt 0 ]]; then
    security list-keychains -d user -s "${ORIGINAL_SEARCH_LIST[@]}"
  fi
}

cmd_setup() {
  if identity_exists; then
    echo "identity \"$NAME\" already present in $KEYCHAIN"
    return 0
  fi

  local pass
  TMP_DIR="$(mktemp -d)"
  local tmp="$TMP_DIR"
  trap 'rm -rf "${TMP_DIR:-}"' EXIT
  pass="$(openssl rand -hex 16)"

  cat >"$tmp/openssl.cnf" <<CNF
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = $NAME
[ext]
basicConstraints = critical,CA:false
keyUsage = critical,digitalSignature
extendedKeyUsage = critical,codeSigning
CNF

  openssl req -x509 -newkey rsa:2048 -nodes -days 3650 \
    -keyout "$tmp/key.pem" -out "$tmp/cert.pem" -config "$tmp/openssl.cnf" 2>/dev/null
  openssl pkcs12 -export -inkey "$tmp/key.pem" -in "$tmp/cert.pem" \
    -name "$NAME" -out "$tmp/id.p12" -passout "pass:$pass"

  security import "$tmp/id.p12" -k "$KEYCHAIN" -P "$pass" -T /usr/bin/codesign >/dev/null

  if [[ -n "${PS_KEYCHAIN_PASSWORD:-}" ]]; then
    security set-key-partition-list -S apple-tool:,apple:,codesign: -s \
      -k "$PS_KEYCHAIN_PASSWORD" "$KEYCHAIN" >/dev/null
  else
    echo "note: the first time codesign uses the key, macOS may ask for your login password;"
    echo "      choose \"Always Allow\" so it never asks again."
  fi
  echo "created identity \"$NAME\" in $KEYCHAIN"
}

cmd_sign() {
  local target="${1:-}"
  [[ -n "$target" ]] || die "usage: sign <path>"
  [[ -e "$target" ]] || die "not found: $target"
  identity_exists || die "identity \"$NAME\" missing; run: dev/macos-signing.sh setup"

  local hash
  hash="$(identity_hash)"
  [[ -n "$hash" ]] || die "could not resolve identity hash for \"$NAME\""
  ensure_in_search_list

  local args=(--force --sign "$hash" --timestamp=none)
  [[ -d "$target" ]] && args+=(--deep)
  [[ -n "${PS_ENTITLEMENTS:-}" ]] && args+=(--entitlements "$PS_ENTITLEMENTS")
  codesign "${args[@]}" "$target"
  codesign --verify --strict "$target"
  echo "signed $target"
}

cmd_status() {
  if identity_exists; then
    echo "identity: \"$NAME\" present in $KEYCHAIN"
  else
    echo "identity: \"$NAME\" NOT found in $KEYCHAIN"
  fi
  if [[ -n "${1:-}" ]]; then
    codesign -dv --verbose=2 "$1" 2>&1 | grep -E 'Identifier|Authority|Signature|TeamIdentifier' || true
    echo "designated requirement:"
    codesign -d -r- "$1" 2>&1 | grep -E '^designated' || true
  fi
}

cmd_reset() {
  local bundle_id="${1:-}"
  [[ -n "$bundle_id" ]] || die "usage: reset <bundle-id>"
  for svc in Accessibility ListenEvent PostEvent; do
    tccutil reset "$svc" "$bundle_id" || true
  done
}

cmd_remove() {
  security delete-identity -c "$NAME" "$KEYCHAIN" 2>/dev/null \
    || security delete-certificate -c "$NAME" "$KEYCHAIN"
  echo "removed identity \"$NAME\""
}

case "${1:-help}" in
  setup)  cmd_setup ;;
  sign)   shift; cmd_sign "$@" ;;
  status) shift; cmd_status "$@" ;;
  reset)  shift; cmd_reset "$@" ;;
  remove) cmd_remove ;;
  *)      sed -n '2,20p' "$0" ;;
esac
