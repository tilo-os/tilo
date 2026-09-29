#!/usr/bin/env bash
# Steps of .forgejo/workflows/build-buildroots.yml, the only CI boundary allowed
# to resolve kernel and WPE build dependencies from Fedora.
#
#   buildroot-snapshot.sh changed BEFORE AFTER    JSON list of kinds whose inputs changed
#   buildroot-snapshot.sh audit KIND ARCH DIGEST  audit a pushed buildroot, export its ref
#   buildroot-snapshot.sh record KIND ARCH        verify its signature, write refs/ARCH.env
#   buildroot-snapshot.sh promote KIND            propose both pins through protected main
#
# KIND is kernel, wpe or wpe-stage. kernel and wpe build
# os/Containerfile.KIND-buildroot into registry.tilo.so/tilo/tilo-KIND-buildroot
# and pin both architectures in os/KIND-buildroot-snapshot.env. wpe-stage
# compiles the WPE engine (os/Containerfile.wpe, target wpe-stage) on the pinned
# WPE buildroot into registry.tilo.so/tilo/tilo-wpe-stage, pinned in
# os/wpe-stage-snapshot.env, so OS image builds never compile it.
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
wpe_inputs=(
  os/Containerfile.wpe-buildroot
)
wpe_stage_inputs=(
  os/Containerfile.wpe
  os/dev/build-wpe.sh
  os/dev/wpe-stage-key.sh
  os/wpe-buildroot-snapshot.env
  os/dev/cache-github-assets.sh
)

# The asset manifest contains many unrelated OS and Windows downloads. Only
# these three pinned archives enter Containerfile.wpe; unrelated rows must not
# start another hour-long signed stage build or reject its in-flight promotion.
wpe_source_records() {
  git show "$1:os/external-assets/github-assets.tsv" | awk -F '\t' '
    BEGIN {
      names[1]="libwpe-1.16.3.tar.xz"
      names[2]="wpebackend-fdo-1.16.1.tar.xz"
      names[3]="wpewebkit-2.52.6.tar.xz"
    }
    $2 == names[1] || $2 == names[2] || $2 == names[3] {
      rows[$2]=$0; count[$2]++
    }
    END {
      for (i=1; i<=3; i++) {
        if (count[names[i]] != 1) exit 1
        print rows[names[i]]
      }
    }
  '
}

kind_vars() {
  case "$1" in
    kernel) key=KERNEL_BUILDROOT label=kernel promote_kind=kernel-buildroot
            inputs=("${common_inputs[@]}" "${kernel_inputs[@]}") ;;
    wpe) key=WPE_BUILDROOT label=WPE promote_kind=wpe
         inputs=("${common_inputs[@]}" "${wpe_inputs[@]}") ;;
    wpe-stage) key=WPE_STAGE label="WPE engine" promote_kind=wpe-stage
         inputs=("${common_inputs[@]}" "${wpe_stage_inputs[@]}")
         image=registry.tilo.so/tilo/tilo-wpe-stage pins=os/wpe-stage-snapshot.env
         return ;;
    *) die "unknown buildroot kind: $1" ;;
  esac
  image="registry.tilo.so/tilo/tilo-$1-buildroot"
  pins="os/$1-buildroot-snapshot.env"
}

# The key OS builds compare with a pinned engine's label: the engine's inputs.
# build-image.yml computes the same (os/dev/wpe-stage-key.sh).
wpe_stage_key() { "$(dirname "${BASH_SOURCE[0]}")/wpe-stage-key.sh" "$1"; }

arch_key() {
  case "$1" in
    x86_64) printf '%s_X86_64_REF' "$key" ;;
    aarch64) printf '%s_AARCH64_REF' "$key" ;;
    *) die "unknown architecture: $1" ;;
  esac
}

changed() {
  local before=$1 after=$2 kinds=() kind before_wpe after_wpe
  for kind in kernel wpe wpe-stage; do
    kind_vars "$kind"
    if [[ ! $before =~ ^[0-9a-f]{40}$ || $before =~ ^0+$ ]] \
       || ! git diff --quiet "$before" "$after" -- "${inputs[@]}"; then
      kinds+=("\"$kind\"")
    elif [ "$kind" = wpe-stage ]; then
      before_wpe="$(wpe_source_records "$before")" \
        || die "invalid WPE source records at $before"
      after_wpe="$(wpe_source_records "$after")" \
        || die "invalid WPE source records at $after"
      [ "$before_wpe" = "$after_wpe" ] || kinds+=("\"$kind\"")
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
  if [ "$kind" = wpe-stage ]; then
    # A scratch image: copy its markers out instead of running it.
    local container probe
    probe="$(mktemp -d)"
    container="$(docker create --entrypoint /none "$ref")"
    docker cp "$container:/usr/lib/tilo/wpe-stage-arch" "$probe/arch"
    # Containerfile.wpe removes /wpe/usr/lib when it is a symlink; Fedora's
    # WebKit libraries are installed in the retained /wpe/usr/lib64 tree.
    docker cp "$container:/wpe/usr/lib64" "$probe/lib"
    docker rm "$container" >/dev/null
    test "$(cat "$probe/arch")" = "$arch"
    find "$probe/lib" -name 'libWPEWebKit-2.0.so*' -print -quit | grep -q .
    rm -rf "$probe"
    test "$(docker image inspect --format '{{index .Config.Labels "org.tilo.wpe.inputs"}}' "$ref")" \
      = "$(wpe_stage_key "$arch")"
    printf 'TILO_BUILDROOT_REF=%s\n' "$ref" >> "$GITHUB_ENV"
    return 0
  fi
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
  local kind=$1 arch=$2 ref=${TILO_BUILDROOT_REF:?} name pinned unchanged=false
  kind_vars "$kind"
  name="$(arch_key "$arch")"
  cosign verify --key keys/cosign.pub "$ref" >/dev/null
  mkdir -p refs
  printf '%s=%s\n' "$name" "$ref" > "refs/$arch.env"
  if [ "$kind" = wpe ]; then
    # The buildroot resolves Fedora afresh and is never byte-reproducible, so
    # every run yields a new digest, and a new pin would discard the OS image's
    # WPE engine layer cache (over an hour per architecture). Record whether it
    # installs exactly the packages of the pinned one; the promotion keeps the
    # pins when neither architecture changed.
    pinned="$(sed -n "s/^$name=//p" "$pins")"
    if [[ $pinned =~ ^registry\.tilo\.so/tilo/tilo-wpe-buildroot@sha256:[0-9a-f]{64}$ ]] \
       && docker run --rm --entrypoint cat "$pinned" /usr/lib/tilo/wpe-buildroot-packages.txt > pinned-packages.txt \
       && docker run --rm --entrypoint cat "$ref" /usr/lib/tilo/wpe-buildroot-packages.txt > new-packages.txt \
       && cmp -s pinned-packages.txt new-packages.txt; then
      unchanged=true
    fi
    printf '%s_UNCHANGED=%s\n' "$name" "$unchanged" >> "refs/$arch.env"
    echo "::notice::$name=$ref (packages unchanged from the pin: $unchanged)"
  else
    echo "::notice::$name=$ref"
  fi
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
  local x86_same="${x86_name}_UNCHANGED" arm_same="${arm_name}_UNCHANGED"
  if [[ ${!x86_same:-false} == true && ${!arm_same:-false} == true ]]; then
    echo "::notice::both buildroots install exactly the pinned packages; keeping the pins and the $label layer cache"
    return 0
  fi
  git fetch --no-tags origin main
  if ! git diff --quiet "$GITHUB_SHA..origin/main" -- "${inputs[@]}"; then
    die "$label buildroot inputs changed after this run started; refusing stale promotion"
  fi
  if [ "$kind" = wpe-stage ]; then
    local original_wpe current_wpe
    original_wpe="$(wpe_source_records "$GITHUB_SHA")" \
      || die "invalid WPE source records at $GITHUB_SHA"
    current_wpe="$(wpe_source_records origin/main)" \
      || die "invalid WPE source records at origin/main"
    [ "$original_wpe" = "$current_wpe" ] \
      || die "WPE source records changed after this run started; refusing stale promotion"
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
  # The branch name open-snapshot-pr.py verifies: a wpe-stage pin is not a
  # buildroot's.
  if [ "$kind" = wpe-stage ]; then
    git commit -m "build(os): pin the native $label stages"
    branch="automation/wpe-stage-snapshot-${snapshot_digest:0:16}"
  else
    git commit -m "build(os): refresh native $label buildroot snapshots"
    branch="automation/$kind-buildroot-snapshot-${snapshot_digest:0:16}"
  fi
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
