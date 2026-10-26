#!/usr/bin/env bash
# Shared by the publishing checks in t.toml (docs/publishing-on-t.md). Sourced, not run.
#
# A publishing check learns what it publishes for from t: T_REF is the branch (`main`,
# `stable`) or `tag:<name>` of the trusted run, T_COMMIT its commit. Everyone pushes to main,
# so main only ever publishes development artifacts; releases come from stable and release
# tags, which only the stabilizer pushes.

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
scratch=${TMPDIR:-/tmp}
# Build scripts that prepare test fixtures (tilo-fs) write them to this cache; the source tree
# is read-only in t's sandbox, so it lives in the build directory.
export TILO_TEST_ASSET_CACHE=${TILO_TEST_ASSET_CACHE:-${CARGO_TARGET_DIR:-$repo_root/target}/test-assets}

die() { printf 'publish: %s\n' "$*" >&2; exit 1; }

# The channel of this run: `dev` on main, `release` on stable and release tags. Anything else
# (a working tree, an unknown branch) publishes nothing.
channel() {
  case ${T_REF:-} in
    main) echo dev ;;
    stable | tag:*) echo release ;;
    '') die 'T_REF is not set: publishing runs only in trusted runs (on = [...] or t dispatch)' ;;
    *) die "no channel for $T_REF" ;;
  esac
}

require_commit() {
  [[ ${T_COMMIT:-} =~ ^[0-9a-f]{40}$ ]] || die "T_COMMIT is not a commit: ${T_COMMIT:-unset}"
}

# An identity token from t for audience $1 (`id_token = ["$1"]` in the check), on stdout: a JWT
# naming this repository, reference and commit, valid for minutes. Parsed with sed because the
# skopeo image has curl but no jq.
id_token() {
  [[ -n ${ACTIONS_ID_TOKEN_REQUEST_URL:-} && -n ${ACTIONS_ID_TOKEN_REQUEST_TOKEN:-} ]] \
    || die "no identity token for $1: the check needs id_token = [\"$1\"] and a trusted run"
  local token
  token=$(curl -fsS -H "Authorization: bearer $ACTIONS_ID_TOKEN_REQUEST_TOKEN" \
    "$ACTIONS_ID_TOKEN_REQUEST_URL&audience=$1" \
    | sed -n 's/.*"value"[[:space:]]*:[[:space:]]*"\([A-Za-z0-9._-]*\)".*/\1/p') \
    || die "t gave no identity token for $1"
  [[ -n $token ]] || die "t gave no identity token for $1"
  printf '%s' "$token"
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

# The manifest digest of the single image in an OCI archive, from its index.json.
archive_digest() {
  tar -xOf "$1" index.json | grep -o '"digest":"sha256:[0-9a-f]\{64\}"' | head -1 | cut -d'"' -f4
}

# The frozen RPM snapshot a build installs from (rpm-snapshot-ref.sh wrote SNAPSHOT): the
# published snapshot in SNAPSHOT/rpms-ref, or, where this package set is not published yet (off
# stable, before stable publishes it), this run's image-rpms-ARCH in RPMS, loaded unsigned into
# the store `b` runs on (use_buildah first). Sets rpms_ref, what the Containerfile's FROM takes,
# and rpms_source, what the build records beside its output: the published reference, or
# "unsigned image-rpms-ARCH DIGEST", which require_published_snapshot refuses, so a build of an
# unpublished package set is tested everywhere and signed or published nowhere.
snapshot_for_build() {
  local snapshot=$1 rpms=$2 arch=$3 digest
  if [[ -f $snapshot/rpms-ref ]]; then
    rpms_ref=$(cat "$snapshot/rpms-ref")
    [[ $rpms_ref =~ ^registry\.tilo\.so/tilo/rpms@sha256:[0-9a-f]{64}$ ]] || die "unexpected snapshot $rpms_ref"
    rpms_source=$rpms_ref
    return
  fi
  [[ -f $snapshot/unpublished ]] || die "$snapshot names no snapshot: neither rpms-ref nor unpublished"
  digest=$(cat "$rpms/digest")
  [[ $(archive_digest "$rpms/image.oci.tar") == "$digest" ]] || die "$rpms/image.oci.tar is not the build $digest"
  rpms_ref=localhost/tilo-rpms:unpublished-$arch
  b tag "$(b pull -q "oci-archive:$rpms/image.oci.tar")" "$rpms_ref"
  rpms_source="unsigned image-rpms-$arch $digest"
  printf 'the RPM snapshot %s is not published: building from image-rpms-%s (%s), unsigned; nothing signs or publishes this build\n' \
    "$(cat "$snapshot/unpublished")" "$arch" "$digest"
}

# Refuse a build whose recorded snapshot (FILE, as snapshot_for_build set rpms_source) is not
# the published, signed one.
require_published_snapshot() {
  local source
  source=$(cat "$1")
  [[ $source =~ ^registry\.tilo\.so/tilo/rpms@sha256:[0-9a-f]{64}$ ]] \
    || die "$1: built from $source, not the published RPM snapshot: refusing to sign or publish it"
}

# The simple-signing payload Cosign signs for an image: its repository and manifest digest.
signing_payload() {
  printf '{"critical":{"identity":{"docker-reference":"%s"},"image":{"docker-manifest-digest":"%s"},"type":"cosign container image signature"},"optional":null}' "$1" "$2"
}

# Repositories installed systems verify through /etc/containers/policy.json
# (tools/signing/release_trust.py, SIGNED_REPOSITORIES). Once the release root exists
# (keys/release/root.crt), their releases also carry certified signatures: oci-sign.sh writes
# OUT/certified/N.{sig,bundle,crt,epoch}, oci-publish.sh attaches and verifies them.
device_verified() {
  case $1 in
    registry.tilo.so/tilo/tilo | code.tilo.so/tilo/tilo | registry.tilo.so/tilo/legacy-guest) return 0 ;;
    *) return 1 ;;
  esac
}
