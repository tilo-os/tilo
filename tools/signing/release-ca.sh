#!/usr/bin/env bash
# The release root ceremony: create Tilo's offline root certificate and issue
# the short-lived operational certificates CI signs under. keys/README.md has
# the owner's exact steps; this script is the only tool they need besides
# OpenSSL 3.
#
#   release-ca.sh init-root   --key KEY --out root.crt [--years N]
#   release-ca.sh issue       --root root.crt --key KEY --purpose PURPOSE \
#                             --public-key op.pub --out op.crt [--days N] [--epoch N]
#   release-ca.sh verify      --root root.crt --purpose PURPOSE [--epoch N] op.crt
#   release-ca.sh inspect     CERT
#   release-ca.sh revoke      (raises the epoch in keys/release/trust.env)
#
# KEY is the root's private key: a PKCS#11 URI (`pkcs11:token=...;object=...`)
# for a hardware token through OpenSSL's pkcs11 provider, or the path of a
# passphrase-protected PEM file on an offline medium. The root key never has to
# exist anywhere else, and nothing here writes it: `openssl genpkey` (or the
# token's own tool) creates it, see keys/README.md.
#
# An operational certificate binds one KMS public key (`cosign public-key
# --key awskms:///...` prints it, on any connected machine) to one purpose:
#
#   os-images             OS images and the legacy guest (containers-policy)
#   extension-manifests   downloaded system-extension manifests
#
# Its identity is <purpose>@TILO_RELEASE_IDENTITY_DOMAIN and its OIDC-issuer
# extension TILO_RELEASE_ISSUER_PREFIX<epoch> (keys/release/trust.env); that
# pair is all containers-policy's `fulcio` requirement and Tilo's own verifiers
# match, so a certificate for one purpose never verifies another, and raising
# the epoch retires every certificate issued before.
set -euo pipefail

repo=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
trust_env=${TILO_RELEASE_TRUST_ENV:-$repo/keys/release/trust.env}
PURPOSES=(os-images extension-manifests)

die() { printf 'release-ca: %s\n' "$*" >&2; exit 1; }

trust_value() {  # trust_value NAME -> the value of NAME in trust.env
  local value
  value=$(sed -n "s/^$1=\\(.*\\)\$/\\1/p" "$trust_env")
  [[ -n $value ]] || die "$trust_env sets no $1"
  printf '%s\n' "$value"
}

openssl_key_args() {  # openssl_key_args OPTION KEY -> arguments selecting KEY
  case $2 in
    pkcs11:*) printf '%s\n' -provider pkcs11 -provider default "$1" "$2" ;;
    *) [[ -f $2 ]] || die "no private key at $2"; printf '%s\n' "$1" "$2" ;;
  esac
}

valid_purpose() {
  local purpose
  for purpose in "${PURPOSES[@]}"; do [[ $1 == "$purpose" ]] && return 0; done
  die "unknown purpose '$1' (one of: ${PURPOSES[*]})"
}

serial() { printf '0x%s\n' "$(openssl rand -hex 16)"; }

fingerprint() {
  openssl x509 -in "$1" -outform DER | sha256sum | cut -d' ' -f1
}

# The extension config for an operational certificate. The OIDC-issuer
# extensions are what sigstore verifiers (containers-policy `fulcio`, cosign
# --certificate-oidc-issuer) compare: 1.3.6.1.4.1.57264.1.1 holds the raw
# issuer bytes, 1.3.6.1.4.1.57264.1.8 the same value as a DER UTF8String.
leaf_config() {  # leaf_config PURPOSE EPOCH
  local issuer identity
  issuer="$(trust_value TILO_RELEASE_ISSUER_PREFIX)$2"
  identity="$1@$(trust_value TILO_RELEASE_IDENTITY_DOMAIN)"
  cat <<EOF
[leaf]
basicConstraints = critical, CA:FALSE
keyUsage = critical, digitalSignature
extendedKeyUsage = codeSigning
subjectAltName = email:$identity
subjectKeyIdentifier = hash
authorityKeyIdentifier = keyid:always
1.3.6.1.4.1.57264.1.1 = DER:$(printf '%s' "$issuer" | od -An -tx1 -v | tr -d ' \n')
1.3.6.1.4.1.57264.1.8 = ASN1:UTF8String:$issuer
EOF
}

root_config() {
  cat <<'EOF'
[root]
basicConstraints = critical, CA:TRUE, pathlen:0
keyUsage = critical, keyCertSign, cRLSign
subjectKeyIdentifier = hash
EOF
}

init_root() {
  local key='' out='' years=10
  while (($#)); do
    case $1 in
      --key) key=$2; shift 2 ;;
      --out) out=$2; shift 2 ;;
      --years) years=$2; shift 2 ;;
      *) die "init-root: unexpected argument $1" ;;
    esac
  done
  [[ -n $key && -n $out ]] || die 'init-root needs --key and --out'
  [[ $years =~ ^[0-9]+$ ]] && ((years >= 1 && years <= 25)) || die '--years must be 1 to 25'
  [[ ! -e $out ]] || die "$out exists; a root is created once"
  local config
  config=$(mktemp)
  trap 'rm -f -- "$config"' RETURN
  root_config > "$config"
  local -a key_args
  mapfile -t key_args < <(openssl_key_args -key "$key")
  openssl x509 -new "${key_args[@]}" -sha384 \
    -subj "/O=Tilo/CN=Tilo Release Root $(date -u +%Y)" \
    -days $((years * 366)) -set_serial "$(serial)" \
    -extfile "$config" -extensions root -out "$out"
  printf 'root certificate %s\nSHA-256 %s\n' "$out" "$(fingerprint "$out")"
}

issue() {
  local root='' key='' purpose='' public='' out='' days='' epoch=''
  while (($#)); do
    case $1 in
      --root) root=$2; shift 2 ;;
      --key) key=$2; shift 2 ;;
      --purpose) purpose=$2; shift 2 ;;
      --public-key) public=$2; shift 2 ;;
      --out) out=$2; shift 2 ;;
      --days) days=$2; shift 2 ;;
      --epoch) epoch=$2; shift 2 ;;
      *) die "issue: unexpected argument $1" ;;
    esac
  done
  [[ -n $root && -n $key && -n $purpose && -n $public && -n $out ]] \
    || die 'issue needs --root, --key, --purpose, --public-key and --out'
  valid_purpose "$purpose"
  local max
  max=$(trust_value TILO_RELEASE_OPERATIONAL_MAX_DAYS)
  days=${days:-$max}
  [[ $days =~ ^[0-9]+$ ]] && ((days >= 1 && days <= max)) \
    || die "--days must be 1 to $max (TILO_RELEASE_OPERATIONAL_MAX_DAYS)"
  epoch=${epoch:-$(trust_value TILO_RELEASE_EPOCH)}
  [[ $epoch =~ ^[1-9][0-9]*$ ]] || die '--epoch must be a positive integer'
  openssl pkey -pubin -in "$public" -noout 2>/dev/null || die "$public is not a public key"
  # An operational key is never the root key: a certificate for the root's own
  # key would let whoever holds the operational key act as the root.
  if cmp -s <(openssl pkey -pubin -in "$public" -outform DER) \
            <(openssl x509 -in "$root" -noout -pubkey | openssl pkey -pubin -outform DER); then
    die 'the operational public key is the root key'
  fi
  local config
  config=$(mktemp)
  trap 'rm -f -- "$config"' RETURN
  leaf_config "$purpose" "$epoch" > "$config"
  local -a key_args
  mapfile -t key_args < <(openssl_key_args -CAkey "$key")
  openssl x509 -new -force_pubkey "$public" -sha384 \
    -subj "/O=Tilo/CN=Tilo $purpose signing (epoch $epoch)" \
    -CA "$root" "${key_args[@]}" -days "$days" -set_serial "$(serial)" \
    -extfile "$config" -extensions leaf -out "$out"
  verify --root "$root" --purpose "$purpose" --epoch "$epoch" "$out"
}

verify() {
  local root='' purpose='' epoch='' cert=''
  while (($#)); do
    case $1 in
      --root) root=$2; shift 2 ;;
      --purpose) purpose=$2; shift 2 ;;
      --epoch) epoch=$2; shift 2 ;;
      -*) die "verify: unexpected argument $1" ;;
      *) cert=$1; shift ;;
    esac
  done
  [[ -n $root && -n $purpose && -n $cert ]] || die 'verify needs --root, --purpose and a certificate'
  valid_purpose "$purpose"
  epoch=${epoch:-$(trust_value TILO_RELEASE_EPOCH)}
  openssl verify -x509_strict -CAfile "$root" "$cert" >/dev/null \
    || die "$cert does not chain to $root"
  local text identity issuer
  text=$(openssl x509 -in "$cert" -noout -text)
  identity="$purpose@$(trust_value TILO_RELEASE_IDENTITY_DOMAIN)"
  issuer="$(trust_value TILO_RELEASE_ISSUER_PREFIX)$epoch"
  grep -Fq "email:$identity" <<< "$text" || die "$cert does not name $identity"
  grep -Fq "$issuer" <<< "$text" || die "$cert is not of $issuer"
  grep -Fq 'CA:FALSE' <<< "$text" || die "$cert is a CA certificate"
  grep -Fq 'Code Signing' <<< "$text" || die "$cert is not for code signing"
  printf '%s: %s, %s, until %s\nSHA-256 %s\n' "$cert" "$identity" "$issuer" \
    "$(openssl x509 -in "$cert" -noout -enddate | cut -d= -f2)" "$(fingerprint "$cert")"
}

inspect() {
  [[ $# == 1 ]] || die 'inspect takes one certificate'
  openssl x509 -in "$1" -noout -subject -issuer -serial -startdate -enddate \
    -ext subjectAltName,extendedKeyUsage,basicConstraints
  printf 'SHA-256 %s\n' "$(fingerprint "$1")"
}

revoke() {
  [[ $# == 0 ]] || die 'revoke takes no arguments'
  local epoch next
  epoch=$(trust_value TILO_RELEASE_EPOCH)
  next=$((epoch + 1))
  sed -i "s/^TILO_RELEASE_EPOCH=$epoch\$/TILO_RELEASE_EPOCH=$next/" "$trust_env"
  [[ $(trust_value TILO_RELEASE_EPOCH) == "$next" ]] || die "could not raise the epoch in $trust_env"
  cat <<EOF
Raised TILO_RELEASE_EPOCH from $epoch to $next in $trust_env.
Images built from here on accept only epoch-$next certificates. Before committing,
issue an epoch-$next certificate for every purpose (issue --epoch $next) and keep an
epoch-$epoch bridge certificate for each replacement key (issue --epoch $epoch,
saved as keys/operational/<purpose>.bridge.crt) until installed systems have
updated past this change; keys/README.md, "Revoke an operational key".
EOF
}

command=${1:-}
shift || true
case $command in
  init-root) init_root "$@" ;;
  issue) issue "$@" ;;
  verify) verify "$@" ;;
  inspect) inspect "$@" ;;
  revoke) revoke "$@" ;;
  *) sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'; exit 2 ;;
esac
