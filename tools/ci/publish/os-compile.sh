#!/usr/bin/env bash
# os-compile-ARCH (t.toml, the os-layers block): the OS image's compiled tree (os/image/compile.sh),
# with the apps and desktop a release published wherever the run has them.
#
#   os-compile.sh ARCH OUT
#
# On stable, os-compile-ARCH needs publish-apps and publish-desktop-payload (`optional_needs`: they
# run on stable only), so the pointer files they set are in place: the `app-collection` pointer
# (TILO_APPS_POINTER) and the `desktop-payload` pointer (TILO_DESKTOP_POINTER). The image then
# embeds exactly what stable published, as a device gets it from the registry: tools/ci/publish/
# os_inputs.py downloads and verifies the collection and payload the pointers name, and the compile
# takes them instead of compiling the apps and the desktop. A pointer that names nothing for ARCH,
# or anything os_inputs.py refuses, fails the build: a release never falls back to apps or a
# desktop nobody published. publish-apps' result is keyed by the app inputs, so a cached one is the
# collection of an earlier stable commit with this commit's app inputs.
#
# On main and for a working tree neither runs: the compile builds the apps and the desktop of the
# commit, as a development image carries them.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"
arch=${1:?usage: os-compile.sh ARCH OUT} out=${2:?usage: os-compile.sh ARCH OUT}
apps_pointer=${TILO_APPS_POINTER:-/target/apps-release/channel/channel.json}
desktop_pointer=${TILO_DESKTOP_POINTER:-/target/publish/desktop/payload.json}
[[ $(uname -m) == "$arch" ]] || die "the $arch tree compiles natively, not on $(uname -m)"

# The pointer files are taken out of the build directory once read, and the tree is made anew:
# a later build on this one's build directory (t's snapshots) must not find a release's inputs
# and embed them as its own.
staged=$scratch/os-release-inputs
rm -rf "$staged" "$out"
mkdir -p "$staged"
take() {
  local pointer=$1 what=$2
  [[ -f $pointer ]] || return 1
  cp "$pointer" "$staged/$what.pointer.json"
  rm -f "$pointer"
  python3 -I tools/ci/publish/os_inputs.py "$what" "$arch" "$staged" --pointer "$staged/$what.pointer.json" \
    || die "the published $what can't be embedded"
}
if take "$apps_pointer" apps; then
  export TILO_PREBUILT_APPS=1 TILO_APP_ARTIFACTS=$staged/app-artifacts TILO_APP_RUNTIME=$staged/app-runtime
fi
if take "$desktop_pointer" desktop; then
  export TILO_PREBUILT_DESKTOP=1 TILO_DESKTOP_ARTIFACTS=$staged/desktop-artifacts
fi
mkdir -p "$out"
bash os/image/compile.sh "$out"
# What the tree took, for the image's provenance and for whoever reads `t outputs`.
for record in "$staged"/app-collection.json "$staged"/desktop-payload.json; do
  [[ ! -f $record ]] || cp "$record" "$out/"
done
