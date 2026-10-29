#!/usr/bin/env bash
# Shared by the publishing scripts. Sourced, not run.
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

# Log in to the registry $1 into the auth file $2. registry.tilo.so takes an OpenID Connect
# identity token as the password of the user `oidc`; it lasts minutes, so a long push logs in
# again. Where there is no identity token, it takes REGISTRY_TOKEN, a Forgejo token that may push
# to this repository, as its owner. The forge's registry, code.tilo.so, takes no identity token:
# there REGISTRY_TOKEN, as its owner.
registry_login() {
  local registry=$1 auth=$2 user password
  if [[ $registry != code.tilo.so && -z ${ACTIONS_ID_TOKEN_REQUEST_URL:-} && -n ${REGISTRY_TOKEN:-} ]]; then
    registry=code.tilo.so
  fi
  case $registry in
    code.tilo.so)
      password=${REGISTRY_TOKEN:-}
      [[ -n $password ]] || die 'no token for code.tilo.so: the check needs secrets = ["REGISTRY_TOKEN"]'
      user=$(curl -fsS -H "Authorization: token $password" https://code.tilo.so/api/v1/user \
        | sed -n 's/.*"login"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p') \
        || die 'code.tilo.so does not accept REGISTRY_TOKEN'
      [[ -n $user ]] || die 'code.tilo.so names no owner of REGISTRY_TOKEN' ;;
    *)
      # A Forgejo Actions run logs in with its job token, as its docker/login steps do.
      if [[ -n ${REGISTRY_PASSWORD:-} ]]; then
        user=${REGISTRY_USER:?REGISTRY_PASSWORD needs REGISTRY_USER}
        password=$REGISTRY_PASSWORD
      else
        user=oidc
        password=$(id_token "$registry")
      fi ;;
  esac
  printf '%s' "$password" | skopeo login --authfile "$auth" -u "$user" --password-stdin "$1" >/dev/null
}

# What a copy of the OCI layout SRC (oci:DIR[:REF]) to DST (docker://REGISTRY/REPO:TAG) sends: each
# blob its manifests name with its size, and whether DST's repository has it already (a HEAD
# with the anonymous pull token the registry's challenge names, as registry.tilo.so wants from
# inside the cluster), so a push log says how many bytes were new. Other sources are not measured.
push_plan() {
  local src=$1 dst=$2 dir repo url digest size have challenge realm service auth=() code
  local new=0 new_bytes=0 old=0 old_bytes=0 blobs
  [[ $src == oci:* ]] || return 0
  dir=${src#oci:}
  dir=${dir%%:*}
  repo=${dst#docker://}
  repo=${repo%[:@]*}
  url="https://${repo%%/*}/v2/${repo#*/}/blobs"
  blobs=$(layout_blobs "$dir" "$dir/index.json" | sort -u)
  [[ -n $blobs ]] || { printf 'push: %s: no blobs found in %s\n' "$dst" "$dir"; return 0; }
  challenge=$(curl -sSI "$url/${blobs%% *}" 2>/dev/null | tr -d '\r' | sed -n 's/^[Ww]ww-[Aa]uthenticate: *Bearer //p')
  if [[ -n $challenge ]]; then
    realm=$(sed -n 's/.*realm="\([^"]*\)".*/\1/p' <<< "$challenge")
    service=$(sed -n 's/.*service="\([^"]*\)".*/\1/p' <<< "$challenge")
    auth=(-H "Authorization: Bearer $(curl -fsS "$realm?service=$service&scope=repository:${repo#*/}:pull" 2>/dev/null \
      | sed -n 's/.*"token" *: *"\([^"]*\)".*/\1/p')")
  fi
  while read -r digest size; do
    code=$(curl -sS -o /dev/null -w '%{http_code}' -I "${auth[@]}" "$url/$digest" 2>/dev/null) || code=000
    case $code in
      200 | 307) have=present; old=$((old + 1)); old_bytes=$((old_bytes + size)) ;;
      *) have=new; new=$((new + 1)); new_bytes=$((new_bytes + size)) ;;
    esac
    printf 'push: blob %s %12s %s\n' "$digest" "$size" "$have"
  done <<< "$blobs"
  printf 'push: %s: %s new blobs (%s MB), %s present (%s MB)\n' "$dst" "$new" "$((new_bytes / 1000000))" \
    "$old" "$((old_bytes / 1000000))"
}

# The blobs (digest and size) the manifests and indexes below the index or manifest FILE of the
# layout DIR name, recursively; read with sed, since the skopeo image has no jq. Every key order
# occurs (podman's, Python's, sorted), and a descriptor's nested objects (annotations, platform)
# hold none of its digest, media type and size, so splitting at braces keeps each descriptor's
# three in one piece.
layout_blobs() {
  local dir=$1 file=$2 piece type digest size
  while IFS= read -r piece; do
    digest=$(sed -n 's/.*"digest":"\(sha256:[0-9a-f]\{64\}\)".*/\1/p' <<< "$piece")
    size=$(sed -n 's/.*"size":\([0-9]*\).*/\1/p' <<< "$piece")
    [[ -n $digest && -n $size ]] || continue
    type=$(sed -n 's/.*"mediaType":"\([^"]*\)".*/\1/p' <<< "$piece")
    case $type in
      *manifest* | *index*) layout_blobs "$dir" "$dir/blobs/${digest/://}" ;;
      *) printf '%s %s\n' "$digest" "$size" ;;
    esac
  done < <(tr -d ' \n' < "$file" | tr '{}' '\n\n')
}

# skopeo copy SRC DST (docker://REGISTRY/...), with the auth file AUTH and further skopeo flags,
# logged in afresh before each attempt. skopeo takes the credentials once: t's identity token lasts
# 5 minutes and the registry token it buys 10, and when that runs out skopeo asks again with the
# expired identity token, so a copy that takes longer fails its next blob. One that sends a GB or
# more takes longer (the first push of a new package set, kernel or base, at the 1-2 MB/s the
# registry takes in); the next attempt logs in again and sends only the blobs not yet there.
# DOCKER_CONFIG, when set, gets the same login.
registry_copy() {
  local auth=$1 src=$2 dst=$3 registry attempt started
  shift 3
  registry=${dst#docker://}
  registry=${registry%%/*}
  push_plan "$src" "$dst"
  started=$SECONDS
  for attempt in 1 2 3 4 5; do
    registry_login "$registry" "$auth"
    [[ -z ${DOCKER_CONFIG:-} ]] || cp "$auth" "$DOCKER_CONFIG/config.json"
    if skopeo copy --preserve-digests --retry-times 3 --authfile "$auth" "$@" "$src" "$dst"; then
      printf 'push: %s copied in %s s\n' "$dst" "$((SECONDS - started))"
      return
    fi
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
# (OUT/oci, as the OS image layers compose one): the manifest digest of its single image.
built_digest() {
  if [[ -f $1/image.oci.tar ]]; then
    archive_digest "$1/image.oci.tar"
  else
    index_digest "$1/oci/index.json" < "$1/oci/index.json"
  fi
}

# The image a build left in OUT as an OCI layout to copy from, in `layout`: OUT/oci, or
# OUT/image.oci.tar unpacked once. Copying from oci-archive: has skopeo unpack the whole archive
# into TMPDIR and read it back while it sends, a write and a read of every layer more on the
# step's disk; this unpacks it into /dev/shm where it fits beside what the step's memory already
# holds, else into the scratch directory. `layout_unpacked` names what to remove afterwards.
built_layout() {
  local out=$1 size room dir=$scratch
  layout_unpacked=
  if [[ ! -f $out/image.oci.tar ]]; then
    [[ -f $out/oci/index.json ]] || die "$out holds no image: neither image.oci.tar nor oci/"
    layout=$out/oci
    return
  fi
  size=$(stat -c %s "$out/image.oci.tar")
  # Half a GiB to spare for skopeo and cosign themselves.
  if room=$(memory_room) && ((size + 536870912 < room)); then dir=/dev/shm; fi
  layout=$(mktemp -d "$dir/layout.XXXXXX")
  layout_unpacked=$layout
  tar -xf "$out/image.oci.tar" -C "$layout"
}

# Bytes a file on /dev/shm may still take: what the tmpfs has free, at most what the step's
# memory (its cgroup's limit, the machine's available memory) has left. Fails without a tmpfs
# there.
memory_room() {
  local room limit used available
  [[ -d /dev/shm && $(stat -f -c %T /dev/shm) == tmpfs ]] || return 1
  room=$(($(stat -f -c '%a * %S' /dev/shm)))
  available=$(sed -n 's/^MemAvailable: *\([0-9]*\) kB$/\1/p' /proc/meminfo)
  [[ -n $available ]] || return 1
  ((available * 1024 < room)) && room=$((available * 1024))
  if [[ -r /sys/fs/cgroup/memory.max && -r /sys/fs/cgroup/memory.current ]]; then
    limit=$(cat /sys/fs/cgroup/memory.max)
    used=$(cat /sys/fs/cgroup/memory.current)
    if [[ $limit =~ ^[0-9]+$ ]] && ((limit - used < room)); then room=$((limit - used)); fi
  fi
  printf '%s' "$room"
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
