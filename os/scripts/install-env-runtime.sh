#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# Ship the published env-runtime environment image in a read-only system app
# store, so the first environment opens without a network.
#
#   install-env-runtime.sh SOURCE STORE DATA_DIR INSTALL_BIN
#
#   SOURCE       the published image's /env-runtime directory: one
#                env-runtime-<arch>.tapp, .id and SHA256SUMS per architecture
#                and publisher.hex (.forgejo/workflows/build-env-runtime.yml)
#   STORE        store root the image's tree goes into (pkgs/<id>/): the OS
#                image's /usr/share/tilo-apps, the RPM's /usr/lib/tilo-apps
#   DATA_DIR     the system store's registry directory, where the runtime's
#                publisher is recorded in `image-publishers` so later runtimes
#                and tools from the same publisher are trusted too
#   INSTALL_BIN  tilo-install
#
# The runtime goes through `tilo-install image add`, the path every image
# takes: its signature, its publisher and its tree are checked exactly as on a
# device. A SOURCE without a runtime for this architecture (a build with no
# runtime published yet, ENV_RUNTIME_REF=scratch) is reported and skipped
# unless TILO_REQUIRE_ENV_RUNTIME=1; environments then fetch the runtime from
# the registry on first use.
# ---------------------------------------------------------------------------
set -euo pipefail

msg() { printf '[install-env-runtime] %s\n' "$*" >&2; }
die() { msg "ERROR: $*"; exit 1; }

[ "$#" -eq 4 ] || die "usage: $0 SOURCE STORE DATA_DIR INSTALL_BIN"
source_dir=$1
store=$2
data_dir=$3
install_bin=$4
arch=$(uname -m)
tapp="$source_dir/env-runtime-$arch.tapp"

if [ ! -f "$tapp" ]; then
    [ "${TILO_REQUIRE_ENV_RUNTIME:-0}" != 1 ] \
        || die "no env-runtime for $arch in $source_dir"
    msg "warning: no published env-runtime for $arch; environments fetch it on first use"
    exit 0
fi
[ -x "$install_bin" ] || die "$install_bin not found or not executable"

(cd "$source_dir" && grep " \./env-runtime-$arch\.tapp\$" SHA256SUMS | sha256sum --check --strict) \
    || die "$tapp does not match the published SHA256SUMS"
publisher=$(cat "$source_dir/publisher.hex")
[[ $publisher =~ ^[0-9a-f]{64}$ ]] || die "publisher.hex is not a 64-character hex key"
expected=$(cat "$source_dir/env-runtime-$arch.id")
[[ $expected =~ ^tapp:[0-9a-f]{64}$ ]] || die "env-runtime-$arch.id is not a package id"

mkdir -p "$store" "$data_dir"
added=$(TILO_APPS_DATA_DIR="$store" "$install_bin" image add "$tapp" --publisher "$publisher")
read -r id kind _ <<<"$added"
[ "$id" = "$expected" ] || die "installed $id, but the image publishes $expected"
[ "$kind" = env-runtime ] || die "$tapp is a $kind package"
[ -x "$store/pkgs/${id#tapp:}/usr/bin/bash" ] || die "the installed runtime has no usr/bin/bash"

publishers="$data_dir/image-publishers"
grep -qx "$publisher" "$publishers" 2>/dev/null || printf '%s\n' "$publisher" >> "$publishers"

# The tree is materialised and the store is read-only: the installer's content
# store, staging and writer lock would only double the bytes shipped.
rm -rf -- "$store/content" "$store/tmp" "$store/.lock"
msg "shipped env-runtime $id ($arch) from publisher ed25519:$publisher"
