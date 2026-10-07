#!/usr/bin/env bash
# Steps of .forgejo/workflows/build-buildroots.yml, the only CI boundary allowed
# to resolve kernel build dependencies from Fedora.
#
#   buildroot-snapshot.sh changed BEFORE AFTER    JSON list of kinds whose inputs changed
#   buildroot-snapshot.sh audit KIND ARCH DIGEST  audit a pushed buildroot, export its ref
#   buildroot-snapshot.sh record KIND ARCH        verify its signature, write refs/ARCH.env
#   buildroot-snapshot.sh promote KIND            propose both pins through protected main
#
# KIND is kernel: os/Containerfile.KIND-buildroot is built into
# registry.tilo.so/tilo/tilo-KIND-buildroot and both architectures are pinned
# in os/KIND-buildroot-snapshot.env.
set -euo pipefail

die() { printf 'buildroot-snapshot: %s\n' "$*" >&2; exit 1; }

common_inputs=(
  os/base-snapshot.env
  os/dev/assert-native-arch.sh
  os/dev/install-cached-cosign.sh
  os/dev/configure-cosign-kms.sh
  os/dev/require-cosign-authority.sh
  os/dev/open-snapshot-pr.py
  os/dev/buildroot-snapshot.sh
  .forgejo/actions/install-cosign
  keys/cosign.pub
  .forgejo/workflows/build-buildroots.yml
)
kernel_inputs=(os/Containerfile.kernel-buildroot)
kind_vars() {
  case "$1" in
    kernel) key=KERNEL_BUILDROOT label=kernel promote_kind=kernel-buildroot
            inputs=("${common_inputs[@]}" "${kernel_inputs[@]}") ;;
    *) die "unknown buildroot kind: $1" ;;
  esac
  image="registry.tilo.so/tilo/tilo-$1-buildroot"
  pins="os/$1-buildroot-snapshot.env"
}

arch_key() {
  case "$1" in
    x86_64) printf '%s_X86_64_REF' "$key" ;;
    aarch64) printf '%s_AARCH64_REF' "$key" ;;
    *) die "unknown architecture: $1" ;;
  esac
}

changed() {
  local before=$1 after=$2 kinds=() kind
  for kind in kernel; do
    kind_vars "$kind"
    if [[ ! $before =~ ^[0-9a-f]{40}$ || $before =~ ^0+$ ]] \
       || ! git diff --quiet "$before" "$after" -- "${inputs[@]}"; then
      kinds+=("\"$kind\"")
    fi
  done
  local IFS=,
  printf '[%s]\n' "${kinds[*]}"
}

audit() {
  local kind=$1 arch=$2 digest=$3 ref
  kind_vars "$kind"
  [[ $digest =~ ^sha256:[0-9a-f]{64}$ ]] || die "invalid digest: $digest"
  ref="$image@$digest"
  actual_arch="$(docker run --rm --entrypoint cat "$ref" "/usr/lib/tilo/$kind-buildroot-arch")"
  test "$actual_arch" = "$arch"
  # The package manifest, and no repository to fall back to.
  docker run --rm --entrypoint sh "$ref" -euc "
    test -s /usr/lib/tilo/$kind-buildroot-packages.txt
    ! find /etc/yum.repos.d -type f -print -quit | grep -q .
  "
  if [ "$kind" = kernel ]; then
    actual_base="$(docker run --rm --entrypoint cat "$ref" /usr/lib/tilo/kernel-buildroot-base)"
    test "$actual_base" = "$BASE_REF"
    # Exactly one signature-checked kernel SRPM whose recorded checksum still
    # matches, and the build tools.
    docker run --rm --entrypoint sh "$ref" -euc '
      test "$(find /usr/src/tilo-kernel -maxdepth 1 -name "kernel-*.src.rpm" | wc -l)" -eq 1
      cd /usr/src/tilo-kernel
      sed -n 2p /usr/lib/tilo/kernel-buildroot-srpm.txt | sha256sum --check --strict
      command -v createrepo_c rpmbuild gcc clang cargo pahole >/dev/null
    '
    {
      printf '## Kernel buildroot (%s)\n\n```\n' "$arch"
      docker run --rm --entrypoint cat "$ref" /usr/lib/tilo/kernel-buildroot-srpm.txt
      printf '```\n'
    } >> "${GITHUB_STEP_SUMMARY:-/dev/null}"
  fi
  printf 'TILO_BUILDROOT_REF=%s\n' "$ref" >> "$GITHUB_ENV"
}

record() {
  local kind=$1 arch=$2 ref=${TILO_BUILDROOT_REF:?} name
  kind_vars "$kind"
  name="$(arch_key "$arch")"
  cosign verify --key keys/cosign.pub "$ref" >/dev/null
  mkdir -p refs
  printf '%s=%s\n' "$name" "$ref" > "refs/$arch.env"
  echo "::notice::$name=$ref"
}

promote() {
  local kind=$1 x86_name arm_name x86 arm ref
  kind_vars "$kind"
  x86_name="$(arch_key x86_64)" arm_name="$(arch_key aarch64)"
  x86="$(find refs -type f -name x86_64.env -print -quit)"
  arm="$(find refs -type f -name aarch64.env -print -quit)"
  test -n "$x86" && test -n "$arm"
  # shellcheck disable=SC1090
  . "$x86"; . "$arm"
  x86=${!x86_name} arm=${!arm_name}
  for ref in "$x86" "$arm"; do
    [[ $ref =~ ^${image//./\\.}@sha256:[0-9a-f]{64}$ ]] \
      || die "not an immutable $label buildroot reference: $ref"
    cosign verify --key keys/cosign.pub "$ref" >/dev/null
  done
  [ "$x86" != "$arm" ] || die "native $label builds unexpectedly produced one shared manifest"
  git fetch --no-tags origin main
  if ! git diff --quiet "$GITHUB_SHA..origin/main" -- "${inputs[@]}"; then
    die "$label buildroot inputs changed after this run started; refusing stale promotion"
  fi
  git checkout --detach origin/main
  python3 os/dev/open-snapshot-pr.py pins "$promote_kind" --x86-ref "$x86" --arm-ref "$arm" > "$pins"
  if git diff --quiet -- "$pins"; then
    echo "$label buildroot pins are already current"
    return 0
  fi
  git config user.name 'Tilo CI'
  git config user.email 'ci@tilo.invalid'
  git add "$pins"
  snapshot_digest="$(sha256sum "$pins" | awk '{print $1}')"
  # The branch name open-snapshot-pr.py verifies.
  git commit -m "build(os): refresh native $label buildroot snapshots"
  branch="automation/$kind-buildroot-snapshot-${snapshot_digest:0:16}"
  remote_tip="$(git ls-remote --heads origin "refs/heads/$branch" | awk '{print $1}')"
  git push --force-with-lease="refs/heads/$branch:$remote_tip" origin "HEAD:refs/heads/$branch"
  python3 os/dev/open-snapshot-pr.py \
    --server "$GITHUB_SERVER_URL" --repository "$GITHUB_REPOSITORY" \
    --branch "$branch" --snapshot-digest "$snapshot_digest" \
    "$promote_kind" --x86-ref "$x86" --arm-ref "$arm" | tee -a "${GITHUB_STEP_SUMMARY:-/dev/null}"
}

command=${1:-}
shift || true
case "$command" in
  changed) [ $# -eq 2 ] || die "usage: changed BEFORE AFTER"; changed "$@" ;;
  audit) [ $# -eq 3 ] || die "usage: audit KIND ARCH DIGEST"; audit "$@" ;;
  record) [ $# -eq 2 ] || die "usage: record KIND ARCH"; record "$@" ;;
  promote) [ $# -eq 1 ] || die "usage: promote KIND"; promote "$@" ;;
  *) die "usage: changed|audit|record|promote ..." ;;
esac
