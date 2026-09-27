#!/usr/bin/env bash
# dist/community-config.sh: what a development build and a release accept.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
tool=$repo_root/dist/community-config.sh
placeholder=$repo_root/os/usr/share/tilo/community.toml
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT

b64url() { printf '%s' "$1" | base64 | tr -d '\n=' | tr '/+' '_-'; }
key=$(printf '5e%.0s' {1..32})
standing=$(b64url '{"room_id":"r","standing": true}')
single=$(b64url '{"room_id":"r","standing":false}')

render() {
    printf '[community]\ninvite = "%s"\n\n[team]\nuser_public_key = "%s"\nlabel = "Tilo team"\n' "$1" "$2"
}
write() { render "$1" "$2" > "$work/community.toml"; }
pass() { "$tool" "$@" >/dev/null 2>&1 || { echo "community-config refused: $*" >&2; exit 1; }; }
fail() {
    if "$tool" "$@" >/dev/null 2>&1; then echo "community-config accepted: $*" >&2; exit 1; fi
}

# The checked-in placeholder: a development build ships it, a release refuses it.
pass check "$placeholder"
fail check --require "$placeholder"
fail check "$work/missing.toml"

write "$standing" "$key"
pass check --require "$work/community.toml"
write "$single" "$key"
fail check "$work/community.toml"
write "not base64!" "$key"
fail check "$work/community.toml"
write "$standing" abcd
fail check "$work/community.toml"
write "$standing" "$(printf '0%.0s' {1..64})"
fail check --require "$work/community.toml"
write "$standing" ""
pass check "$work/community.toml"
fail check --require "$work/community.toml"
printf '[community]\ninvitation = "x"\n' > "$work/community.toml"
fail check "$work/community.toml"

# CI provides the file: materialize writes it over the destination, then checks.
cp "$placeholder" "$work/dest.toml"
TILO_COMMUNITY_TOML=$(render "$standing" "$key") pass materialize --require "$work/dest.toml"
grep -Fq "invite = \"$standing\"" "$work/dest.toml"
cp "$placeholder" "$work/dest.toml"
TILO_COMMUNITY_TOML="" fail materialize --require "$work/dest.toml"
pass --help

echo "community config contract passed"
