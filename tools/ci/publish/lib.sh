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

# Under errexit a command that fails without a word (a grep that matched nothing) would end the
# step silently: name it, its status and where it ran, in functions and substitutions too.
set -o errtrace
trap 'failed $? "$BASH_COMMAND" "${BASH_SOURCE[0]:-$0}" "$LINENO"' ERR
failed() {
  if [[ $- == *e* ]]; then printf 'publish: `%s` failed with status %s at %s:%s\n' "$2" "$1" "$3" "$4" >&2; fi
}

# The channel of this run: `dev` on main, `release` on stable and release tags, `branch` on any
# other branch, which publishes only its own build under tags that name the branch (`t dispatch
# --ref BRANCH` of a check that publishes there). A working tree publishes nothing.
channel() {
  case ${T_REF:-} in
    main) echo dev ;;
    stable | tag:*) echo release ;;
    '') die 'T_REF is not set: publishing runs only in trusted runs (on = [...] or t dispatch)' ;;
    *) echo branch ;;
  esac
}

# The branch of a `branch` run as a tag part: lower case, every run of other characters a dash,
# at most 48 characters (tilo-vm's ci.rs ref_slug computes the same).
ref_slug() {
  local slug
  slug=$(printf '%s' "${T_REF:?}" | tr 'A-Z' 'a-z' | tr -cs 'a-z0-9.' '-' | sed 's/^[-.]*//; s/-*$//' | cut -c1-48)
  slug=${slug%-}
  [[ -n $slug ]] || die "no tag can name the branch $T_REF"
  printf '%s' "$slug"
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

# Credentials of the AWS role $1 in region $2, exported as AWS_ACCESS_KEY_ID,
# AWS_SECRET_ACCESS_KEY and AWS_SESSION_TOKEN: STS exchanges t's identity token for them
# (`id_token = ["sts.amazonaws.com"]`), and the role's trust policy decides which references may
# have them. They last minutes, and no key is stored. The token goes in the request body from a
# file, never on a command line.
aws_role() {
  local role=$1 region=$2 sts
  sts=$(mktemp -d "$scratch/sts.XXXXXX")
  (umask 077 && id_token sts.amazonaws.com > "$sts/token")
  curl --fail-with-body --silent --show-error --proto '=https' "https://sts.$region.amazonaws.com/" \
    --data-urlencode Action=AssumeRoleWithWebIdentity --data-urlencode Version=2011-06-15 \
    --data-urlencode "RoleArn=$role" --data-urlencode DurationSeconds=900 \
    --data-urlencode "RoleSessionName=tilo-t-$(printf '%.12s' "${T_COMMIT:-local}")" \
    --data-urlencode "WebIdentityToken@$sts/token" > "$sts/reply.xml" \
    || die "STS refused the identity token: $(sed -n 's:.*<Message>\(.*\)</Message>.*:\1:p' "$sts/reply.xml")"
  unset AWS_PROFILE AWS_DEFAULT_PROFILE AWS_SHARED_CREDENTIALS_FILE AWS_SECURITY_TOKEN
  AWS_ACCESS_KEY_ID=$(tr -d '\n' < "$sts/reply.xml" | sed -n 's:.*<AccessKeyId>\([^<]*\)</AccessKeyId>.*:\1:p')
  AWS_SECRET_ACCESS_KEY=$(tr -d '\n' < "$sts/reply.xml" | sed -n 's:.*<SecretAccessKey>\([^<]*\)</SecretAccessKey>.*:\1:p')
  AWS_SESSION_TOKEN=$(tr -d '\n' < "$sts/reply.xml" | sed -n 's:.*<SessionToken>\([^<]*\)</SessionToken>.*:\1:p')
  rm -rf -- "$sts"
  [[ -n $AWS_ACCESS_KEY_ID && -n $AWS_SECRET_ACCESS_KEY && -n $AWS_SESSION_TOKEN ]] || die 'STS answered without credentials'
  export AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY AWS_SESSION_TOKEN AWS_REGION=$region
}

# Log in to the registry $1 into the auth file $2. registry.tilo.so takes an identity token from t
# (`id_token = ["registry.tilo.so"]`) as the password of the user `oidc`; it lasts minutes, so a
# long push logs in again. The forge's registry, code.tilo.so, takes no identity token: there a
# Forgejo token that may write packages (`secrets = ["REGISTRY_TOKEN"]`), as its owner.
registry_login() {
  local registry=$1 auth=$2 user password
  case $registry in
    code.tilo.so)
      password=${REGISTRY_TOKEN:-}
      [[ -n $password ]] || die 'no token for code.tilo.so: the check needs secrets = ["REGISTRY_TOKEN"]'
      user=$(curl -fsS -H "Authorization: token $password" https://code.tilo.so/api/v1/user \
        | sed -n 's/.*"login"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p') \
        || die 'code.tilo.so does not accept REGISTRY_TOKEN'
      [[ -n $user ]] || die 'code.tilo.so names no owner of REGISTRY_TOKEN' ;;
    *)
      user=oidc
      password=$(id_token "$registry") ;;
  esac
  printf '%s' "$password" | skopeo login --authfile "$auth" -u "$user" --password-stdin "$registry" >/dev/null
}

# skopeo copy SRC DST (docker://REGISTRY/...), with the auth file AUTH and further skopeo flags,
# logged in afresh before each attempt: an identity token lasts minutes, so a push that outlives
# it (a multi-GB image) fails to authenticate its next blob; the next attempt logs in again, and
# the blobs already pushed are not sent twice. DOCKER_CONFIG, when set, gets the same login.
registry_copy() {
  local auth=$1 src=$2 dst=$3 registry attempt
  shift 3
  registry=${dst#docker://}
  registry=${registry%%/*}
  for attempt in 1 2 3 4 5; do
    registry_login "$registry" "$auth"
    [[ -z ${DOCKER_CONFIG:-} ]] || cp "$auth" "$DOCKER_CONFIG/config.json"
    skopeo copy --preserve-digests --retry-times 3 --authfile "$auth" "$@" "$src" "$dst" && return
    printf 'publish: copy attempt %s to %s failed; logging in again\n' "$attempt" "$dst" >&2
  done
  die "could not copy $src to $dst"
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

# The image a build left in OUT, as an OCI archive (OUT/image.oci.tar) or an OCI layout
# (OUT/oci, as the OS image layers compose one): a reference skopeo copies from, and the
# manifest digest of its single image.
built_image() {
  if [[ -f $1/image.oci.tar ]]; then printf 'oci-archive:%s/image.oci.tar' "$1"; else printf 'oci:%s/oci' "$1"; fi
}
built_digest() {
  if [[ -f $1/image.oci.tar ]]; then
    archive_digest "$1/image.oci.tar"
  else
    index_digest "$1/oci/index.json" < "$1/oci/index.json"
  fi
}

# Whether a moving development tag that now names commit $1 (nothing, this commit, or an
# ancestor of it) may move to T_COMMIT: main's builds may finish out of order, and an older one
# never takes a tag back.
moves_forward() {
  local current=$1 ahead
  [[ -z $current || $current == "<no value>" || $current == "$T_COMMIT" ]] && return 0
  # The commits the named one has that this one lacks: none, when it is an ancestor.
  ahead=$(curl -fsS "https://code.tilo.so/api/v1/repos/tilo/tilo/compare/$T_COMMIT...$current" \
    | sed -n 's/.*"total_commits":\([0-9]*\).*/\1/p') || ahead=
  [[ $ahead == 0 ]] && return 0
  printf 'it names %s, %s; leaving it\n' "$current" \
    "${ahead:+which is not an ancestor of $T_COMMIT}${ahead:-whose history could not be compared}" >&2
  return 1
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
