#!/bin/sh
# Install the Lounge's app packages into a package store, as an image build
# (the Lounge's Containerfile) and the local stack install one into a store at
# its final path. usage: install-apps.sh PACKAGES STORE
#
# PACKAGES holds the signed `.tapp` packages this build carries
# (tools/ci/publish/lounge-apps.sh); each is installed with `tilo-install` as
# any person's is, over the same signature. The store records what it installed
# under `.image-packages`, so a store that already holds this build's packages
# is left alone and a newer image replaces the older ones.
#
# TILO_INSTALL names the `tilo-install` program (default: beside this script's
# own `bin/`); TILO_RUNTIME_BUNDLE_DIR the runtime bundles the packages run on,
# where they are not at the store's default (/usr/lib/tilo-runtime).
set -eu

usage='usage: install-apps.sh PACKAGES STORE'
packages=${1:?$usage}
store=${2:?$usage}
here=$(cd -- "$(dirname -- "$0")" && pwd)

fail() {
  echo "install-apps: $*" >&2
  exit 1
}

install_bin=${TILO_INSTALL:-}
if [ -z "$install_bin" ]; then
  for candidate in "$here/bin/tilo-install" "$here/tilo-install"; do
    if [ -x "$candidate" ]; then install_bin=$candidate; break; fi
  done
fi
if [ -z "$install_bin" ]; then
  install_bin=$(command -v tilo-install 2>/dev/null || true)
fi
[ -n "$install_bin" ] && [ -x "$install_bin" ] \
  || fail "no tilo-install to install $packages with (set TILO_INSTALL)"
[ -d "$packages" ] || fail "no packages to install at $packages"
set -- "$packages"/*.tapp
[ -f "$1" ] || fail "$packages holds no packages"

# What this build ships, as one name: the same store, installed from the same
# packages, is nothing to do again.
stamp=$(cd "$packages" && sha256sum ./*.tapp | sha256sum | cut -d' ' -f1)
if [ -f "$store/.image-packages" ] && [ "$(cat "$store/.image-packages")" = "$stamp" ]; then
  echo "install-apps: $store already holds these packages"
  exit 0
fi

mkdir -p "$store"
for tapp in "$@"; do
  # A package may name an optional runtime this image does not carry (the
  # Browser's Chromium engine), and installs to say its engine is missing
  # rather than to refuse: the app offers it for download when it runs.
  "$install_bin" --offline-store "$store" install "$tapp" --force --ignore-missing-runtime \
    || fail "could not install $tapp"
done
# The trees are materialised: the installer's staging, content store and
# writer lock serve nothing a launch reads.
rm -rf "$store/tmp" "$store/.lock" "$store/content"
printf '%s\n' "$stamp" > "$store/.image-packages"
echo "install-apps: installed $(($# - 0)) packages into $store"
