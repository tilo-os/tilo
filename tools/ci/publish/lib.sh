#!/usr/bin/env bash
# Shared by the publishing checks in t.toml (docs/publishing-on-t.md). Sourced, not run.
#
# A publishing check learns what it publishes for from t: T_REF is the branch (`main`,
# `stable`) or `tag:<name>` of the trusted run, T_COMMIT its commit. Everyone pushes to main,
# so main only ever publishes development artifacts; releases come from stable and release
# tags, which only the stabilizer pushes.

# What a build needs as well (die, the scratch directory, buildah, pinned assets, image digests)
# is build-lib.sh's, which builds source alone, so they are not keyed on how publishing works.
. "$(dirname -- "${BASH_SOURCE[0]}")/build-lib.sh"

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
# logged in afresh before each attempt. skopeo takes the credentials once: t's identity token lasts
# 5 minutes and the registry token it buys 10, and when that runs out skopeo asks again with the
# expired identity token, so a copy that takes longer fails its next blob. One that sends a GB or
# more takes longer (the first push of a new package set, kernel or base, at the 1-2 MB/s the
# registry takes in); the next attempt logs in again and sends only the blobs not yet there.
# DOCKER_CONFIG, when set, gets the same login.
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
