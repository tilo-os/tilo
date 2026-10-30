#!/usr/bin/env bash
# Shared by the checks that build (the OS image's actions, the product builds through
# oci-build.sh and oci-run.sh) and, through lib.sh, by those that publish. Sourced, not run. What
# only publishing needs (channels, identity tokens, registries, signatures) is lib.sh's, so a
# change to how publishing works keys no build.

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
scratch=${TMPDIR:-/tmp}
# Build scripts that prepare test fixtures (tilo-fs) write them to this cache; the source tree
# is read-only in t's sandbox, so it lives in the build directory.
export TILO_TEST_ASSET_CACHE=${TILO_TEST_ASSET_CACHE:-${CARGO_TARGET_DIR:-$repo_root/target}/test-assets}

die() { printf 'publish: %s\n' "$*" >&2; exit 1; }

# Under errexit a command that fails without a word (a grep that matched nothing) would end the
# step silently: name it, its status and where it ran, in functions and substitutions too.
set -o errtrace
trap 'failed $? "$BASH_COMMAND" "${BASH_SOURCE[0]:-$0}" "$LINENO"' ERR
failed() {
  if [[ $- == *e* ]]; then printf 'publish: `%s` failed with status %s at %s:%s\n' "$2" "$1" "$3" "$4" >&2; fi
}

# buildah without a daemon, as root in t's sandbox: chroot isolation and the vfs driver need no
# user namespace of their own and no writable root. The store lives under $scratch/NAME; `b`
# runs buildah on it. DRIVER `overlay` works in a micro-VM step (its scratch disk is ext4) and
# doesn't copy every layer whole as vfs does, for images of hundreds of MB.
use_buildah() {
  local store=$scratch/${1:-buildah} driver=${2:-vfs}
  export STORAGE_DRIVER=$driver BUILDAH_ISOLATION=chroot
  mkdir -p "$store"
  # The image's own storage.conf names an additional image store under its read-only root.
  printf '[storage]\ndriver = "%s"\n' "$driver" > "$store/storage.conf"
  export CONTAINERS_STORAGE_CONF=$store/storage.conf
  buildah_root=$store/root
  buildah_runroot=$store/run
}

b() { buildah --root "$buildah_root" --runroot "$buildah_runroot" "$@"; }

# Copy the repository into DEST without what IGNOREFILE (a .dockerignore without `!` lines)
# leaves out. BuildKit applies such a file to every use of the context; buildah only to COPY,
# not to a RUN's bind mount of it, so builds that bind the context get a filtered copy.
stage_context() {
  local ignorefile=$1 dest=$2 pattern excludes=()
  grep -q '^!' "$ignorefile" && die "$ignorefile has exceptions, which stage_context does not apply"
  while IFS= read -r pattern; do
    pattern=${pattern%/}
    case $pattern in
      '' | '#'*) ;;
      '**/'*) excludes+=(--no-anchored "--exclude=${pattern#'**/'}") ;;
      *) excludes+=(--anchored "--exclude=./$pattern") ;;
    esac
  done < "$ignorefile"
  rm -rf "$dest"
  mkdir -p "$dest"
  tar -C "$repo_root" "${excludes[@]}" -cf - . | tar -C "$dest" -xf -
}

# The pinned external assets NAME... (os/external-assets/github-assets.tsv), checksum-verified;
# prints their directory, for --build-context. They come from the os-assets check's outputs,
# which the calling check needs; nothing here downloads them.
pinned_assets() {
  TILO_ASSET_OFFLINE=1 bash "$repo_root/os/dev/cache-github-assets.sh" "$@" >&2
  printf '%s' "$repo_root/os/external-assets/cache"
}

# The board's firmware, into os/hardware/firmware/BOARD (writable_sources), where the image
# builds take it from: the Lenovo's from its pinned artifact, the Orange Pi's Wi-Fi files from
# their pinned public copies.
stage_board_firmware() {
  case $1 in
    lenovo-83ed)
      python3 "$repo_root/os/dev/hardware-firmware-artifact.py" import \
        --reference "$(publication TILO_LENOVO_FIRMWARE_REF)" \
        --manifest-sha256 "$(publication TILO_LENOVO_FIRMWARE_MANIFEST_SHA256)" \
        --profile lenovo-83ed --architecture aarch64 --repo "$repo_root" >/dev/null ;;
    orangepi-3b-v2-1)
      TILO_ASSET_OFFLINE=1 bash "$repo_root/os/dev/fetch-orangepi-wifi-firmware.sh" ;;
    *) die "no firmware source for the board $1" ;;
  esac
}

# A value of os/publication.env, the public configuration of OS publication.
publication() {
  local value
  value=$(sed -n "s/^$1=//p" "$repo_root/os/publication.env")
  [[ -n $value ]] || die "os/publication.env sets no $1"
  printf '%s' "$value"
}

# The pinned Cosign (os/external-assets/github-assets.tsv), checksum-verified, on PATH. It comes
# from the signing-tools check's outputs, which every check that runs this needs.
use_cosign() {
  command -v cosign >/dev/null && return
  local tools=$scratch/publish-tools
  mkdir -p "$tools"
  RUNNER_TEMP=$tools GITHUB_PATH=$tools/path TILO_GITHUB_ASSET_CACHE=/target/signing-tools \
    TILO_ASSET_OFFLINE=1 bash "$repo_root/os/dev/install-cached-cosign.sh" >/dev/null
  PATH=$tools/tilo-cosign-tools:$PATH
}

# The manifest digest of the single image an OCI index.json (on stdin, from $1) names. Read
# with grep, not a JSON parser: the publishing checks run in the skopeo image, which has neither
# python3 nor jq. An index's only "digest" members are its manifests' (descriptors' annotations
# and platforms have none), and whitespace never matters to them, so compact (podman's) and
# spaced (Python's json.dump, as tools/ci/os/oci_layers.py writes layouts) read alike.
index_digest() {
  local digests count
  digests=$(tr -d '[:space:]' | grep -o '"digest":"sha256:[0-9a-f]\{64\}"' | cut -d'"' -f4) || true
  count=$(printf '%s' "$digests" | grep -c .) || true
  [[ $count == 1 ]] || die "$1 names $count images, not one"
  printf '%s' "$digests"
}

# The manifest digest of the single image in an OCI archive, from its index.json.
archive_digest() {
  tar -xOf "$1" index.json | index_digest "$1 index.json"
}

# The resealing bootc a sealed image's composefs digest is computed with
# (os/Containerfile.bootc-helper): the published build of these sources, else built here.
bootc_helper() {
  local key ref
  key=$(cat os/Containerfile.bootc-helper os/external-assets/bootc-install-reseal/*.patch | sha256sum | cut -c1-16)
  ref=registry.tilo.so/tilo/bootc-install-helper:$key-$(uname -m)
  if ! podman pull -q "$ref" >/dev/null 2>&1; then
    printf 'no published %s; building it\n' "$ref" >&2
    podman build -q -f os/Containerfile.bootc-helper -t "$ref" . >/dev/null
  fi
  printf '%s' "$ref"
}
