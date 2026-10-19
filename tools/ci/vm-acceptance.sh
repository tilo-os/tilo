#!/usr/bin/env bash
# The OS image's acceptance scenarios in tilo-vm, as t's vm-acceptance check
# runs them on a privileged x86-64 node with KVM (t.toml). The tree under test
# is the check's own copy of the sources: its components are swapped onto the
# dev disk of the newest main commit at or before it that has a published dev
# disk or dev image, then scripts/vm-acceptance/run.py drives the checks.
#
# Inputs (t dispatch vm-acceptance --input name=value; every other run gets
# the defaults):
#   swap     default (shell plus the apps the tree changed), none, all, or a
#            comma-separated list (components, app:<dir>)
#   only     comma-separated check ids or areas; empty: every check, or on a
#            working tree the checks its changes select
#   outputs  the slot's outputs
#   gl       virgl: auto, on or off
#
# The report and the performance records land in /target/vm-acceptance/report.
#
# TILO_ACCEPTANCE_IMAGE_DIR names a dev image built from this very tree
# (os-dev-image-x86_64's output: image.oci.tar, digest, revision) to boot instead
# of a published one; t's dev-trust-acceptance check runs it so, for checks that
# need the tree's own image (the trust area: the image's merge path itself).
set -euo pipefail

arch=x86_64
work=/var/tmp/vm-acceptance
report=${CARGO_TARGET_DIR:-/target}/vm-acceptance/report
run_user=acceptance
behind_warn=20

# The node's own Fedora: the tools the run needs come from dnf.
dnf --setopt=retries=3 --setopt=install_weak_deps=False install -y \
  libvirt-daemon-driver-qemu libvirt-client passt qemu-kvm qemu-img edk2-ovmf \
  ipxe-roms-qemu openssh-clients rsync python3-pillow binutils btrfs-progs dosfstools \
  e2fsprogs fuse-overlayfs podman jq skopeo slirp4netns socat sudo systemd \
  shadow-utils util-linux bash-devel readline-devel git curl python3 rustup gcc
command -v virsh qemu-system-x86_64 qemu-img passt ssh podman skopeo systemd-repart sudo >/dev/null
python3 -c 'import PIL'
# Rust as rust-toolchain.toml pins it, kept with the build directory.
# Its own homes: the session user builds with them too.
export RUSTUP_HOME=${CARGO_TARGET_DIR:-/target}/rustup
export CARGO_HOME=${CARGO_TARGET_DIR:-/target}/cargo-home
export PATH=$CARGO_HOME/bin:$PATH
command -v rustup >/dev/null 2>&1 && [[ -x $CARGO_HOME/bin/cargo ]] \
  || rustup-init -y --no-modify-path --default-toolchain none --profile minimal >/dev/null
cargo --version

./os/dev/assert-native-arch.sh "$arch"
python3 os/dev/probe-kvm.py "$arch" || {
  echo 'this node cannot create a KVM VM; the acceptance suite never runs under TCG' >&2
  exit 1
}


rm -rf "$work" "$report"
mkdir -p "$work" "$report"
# The swaps build as the session user, in a copy of the tree it owns.
src=$work/src
cp -a --reflink=auto "$PWD" "$src"

# The tree under test, its base disk, the swaps and the checks.
target=$(git rev-parse HEAD)
commit="" disk_url="" image="" image_archive=""
packages=https://code.tilo.so/api/packages/tilo/generic/tilo-vm-disk
auth=()
[[ -z ${REGISTRY_TOKEN:-} ]] || auth=(-H "Authorization: Bearer $REGISTRY_TOKEN")
tree_image=${TILO_ACCEPTANCE_IMAGE_DIR:-}
if [[ -n $tree_image ]]; then
  [[ $(cat "$tree_image/revision") == "$target" ]] \
    || { echo "$tree_image is the dev image of $(cat "$tree_image/revision"), not of ${target:0:12}" >&2; exit 1; }
  commit=$target image_archive=$tree_image/image.oci.tar
  image="localhost/tilo-vm@$(cat "$tree_image/digest")"
fi
for c in $( [[ -n $commit ]] || git rev-list --first-parent -n 1000 main); do
  git merge-base --is-ancestor "$c" "$target" || continue
  tag="main-${c:0:7}-ci"
  if curl -fsS "${auth[@]}" -o "$work/disk.json" "$packages/$tag/tilo-vm-disk-$arch.json" 2>/dev/null; then
    commit=$c disk_url="$packages/$tag/tilo-vm-disk-$arch.json"
    break
  fi
  digest=$(skopeo inspect --no-tags --format '{{.Digest}}' "docker://registry.tilo.so/tilo/tilo-vm:$tag-$arch" 2>/dev/null || true)
  if [[ $digest =~ ^sha256:[0-9a-f]{64}$ ]]; then
    commit=$c image="registry.tilo.so/tilo/tilo-vm@$digest"
    break
  fi
done
[[ -n $commit ]] || { echo "no main commit at or before ${target:0:12} (last 1000) has a published dev disk or dev image" >&2; exit 1; }
behind=$(git rev-list --count "$commit..$target")
bridge=$(python3 scripts/vm-acceptance/management.py "$commit" "$target")

# What differs from the base: committed changes, and a working tree's own.
changed() { { git diff --name-only "$@"; git diff --name-only HEAD; } | sort -u; }
swap=${T_INPUT_SWAP:-default}
case $swap in
  none) swap="" ;;
  default)
    swap=""
    from=$(git merge-base main "$target")
    [[ $from != "$target" ]] || from=$commit
    if [[ -n $(changed "$commit" "$target") ]]; then
      swap=shell
      [[ -z $(changed "$commit" "$target" -- crates/dev/tilo-vmd crates/dev/tilo-vm-protocol) ]] || swap+=",vmd"
      apps=""
      for app in $(changed "$from" "$target" -- apps/ | grep '^apps/' | cut -d/ -f2 | sort -u); do
        [[ -f apps/$app/Cargo.toml ]] && apps+=",app:$app"
      done
      if [[ -n $apps && -n $(changed "$commit" "$target" -- Cargo.lock | grep -x Cargo.lock || true) ]]; then
        swap+=",apps+runtime"
      else
        swap+=$apps
      fi
    fi ;;
  all)
    swap=$(python3 -c '
import tomllib
print(",".join(c["name"] for c in tomllib.load(open("os/components.toml", "rb"))["component"]
               if c.get("kind") == "userspace"))') ;;
esac

# A run of someone's tree checks what its changes reach; main checks all.
only=${T_INPUT_ONLY:-}
if [[ -z $only && -z ${T_TRIGGER:-} ]]; then
  mapfile -t paths < <(changed "$(git merge-base main HEAD)" HEAD)
  (( ${#paths[@]} == 0 )) || only=$(python3 scripts/vm-acceptance/ci_select.py "${paths[@]}")
fi
gl=${T_INPUT_GL:-auto}
case $gl in
  auto) compgen -G '/dev/dri/renderD*' >/dev/null && gl=on || gl=off ;;
  on|off) ;;
  *) echo 'gl must be auto, on or off' >&2; exit 1 ;;
esac
outputs=${T_INPUT_OUTPUTS:-1920x1080@1,1280x800@1.25}
echo "testing ${target:0:12} on the dev disk of ${commit:0:12} ($behind commits behind) from ${disk_url:-$image}; swap: ${swap:-nothing}; checks: ${only:-all}; gl=$gl"
(( behind <= behind_warn )) || echo "warning: the base disk is $behind commits behind: only the swapped components ($swap) are this tree's code"

export TILO_TEST_ASSET_CACHE=${TILO_TEST_ASSET_CACHE:-$work/test-assets}
cargo build --locked -p tilo-vm
install -m 0755 "${CARGO_TARGET_DIR:-target}/debug/tilo-vm" /usr/local/bin/tilo-vm

if [[ $bridge == true ]]; then
  git worktree add --detach "$work/base-cli" "$commit"
  (cd "$work/base-cli" && CARGO_TARGET_DIR=$work/base-cli/target cargo build --locked -p tilo-vm)
  install -m 0755 "$work/base-cli/target/debug/tilo-vm" /usr/local/bin/tilo-vm-base
fi

if [[ -n $image ]]; then
  modprobe fuse 2>/dev/null || true
  modprobe loop 2>/dev/null || true
  test -c /dev/fuse && test -c /dev/loop-control
  chmod 0666 /dev/fuse
  losetup --find >/dev/null
  ./os/dev/ci-native-podman-store.sh
  mkdir -p "$work/install" "$work/install-run"
  if [[ -n $image_archive ]]; then
    id=$(podman pull -q "oci-archive:$image_archive")
    podman tag "$id" localhost/tilo-vm:ci
  else
    podman pull "$image"
    podman tag "$image" localhost/tilo-vm:ci
  fi
  TILO_IMAGE=localhost/tilo-vm:ci TILO_SKIP_BUILD=1 TILO_DISTROBOX="" \
    TILO_OUTPUT=$work/install TILO_RUN=$work/install-run TILO_DISK_SIZE=64G \
    TILO_TARGET_IMGREF="registry.tilo.so/tilo/tilo-vm:main-${commit:0:7}-ci" ./os/dev/build-image.sh
  disk="$work/tilo-dev-${commit:0:9}.qcow2"
  mv "$work/install/qcow2/disk.qcow2" "$disk"
  oci_digest=${image#*@}
else
  read -r url sha256 < <(jq -er '.disks[] | select(.format == "qcow2") | "\(.url) \(.sha256)"' "$work/disk.json")
  disk="$work/tilo-dev-${commit:0:9}.qcow2"
  curl -fsS --retry 3 "${auth[@]}" -o "$disk" "$url"
  echo "$sha256  $disk" | sha256sum -c -
  oci_digest=$(jq -r .imageDigest "$work/disk.json")
fi
jq -n --arg commit "$commit" --arg subject "$(git log -1 --format=%s "$commit")" \
  --arg time "$(git log -1 --format=%cI "$commit")" --arg digest "$oci_digest" \
  --arg built "$(date -u +%Y-%m-%dT%H:%M:%S+00:00)" --arg source "${disk_url:-$image}" \
  '{commit:$commit, subject:$subject, commit_time:$time, oci_digest:$digest,
    built_at:$built, built_by:"t vm-acceptance", source:$source}' > "$disk.provenance.json"

# libvirt's session daemon runs as an unprivileged user, as on a workstation.
id "$run_user" >/dev/null 2>&1 || useradd --create-home --shell /bin/bash "$run_user"
uid=$(id -u "$run_user")
install -d -m 0700 -o "$run_user" -g "$run_user" "/run/user/$uid"
chmod 0666 /dev/kvm
install -d -o "$run_user" -g "$run_user" "/home/$run_user/.config/libvirt"
echo 'max_core = 0' | install -m 0644 -o "$run_user" -g "$run_user" /dev/stdin "/home/$run_user/.config/libvirt/qemu.conf"
mkdir -p "$src/target/dev-runs"
chown -R "$run_user:" "$work" "$report"
chown -R "$run_user:" "$CARGO_HOME" "$RUSTUP_HOME"
keep=$(env | sed -n 's/^\(RUSTC_WRAPPER\|SCCACHE_[A-Z0-9_]*\|CARGO_[A-Z0-9_]*\|RUSTUP_HOME\|TILO_[A-Z0-9_]*\)=.*/\1/p' \
  | grep -vx CARGO_TARGET_DIR | paste -sd, -)
as_user() { sudo -n -u "$run_user" -H --preserve-env="$keep" env "XDG_RUNTIME_DIR=/run/user/$uid" "PATH=$PATH" "$@"; }
as_user virsh --connect qemu:///session capabilities | grep -q "<domain type='kvm'"

args=(--disk "$disk" --commit "$commit" --outputs "$outputs" --out "$report"
      --tilo-vm /usr/local/bin/tilo-vm --records "$report/records.jsonl" --runner t)
[[ $bridge != true ]] || args+=(--bootstrap-tilo-vm /usr/local/bin/tilo-vm-base --bootstrap-from "$src" --swap-ref "$target")
[[ -z $swap ]] || args+=(--swap "$swap" --swap-from "$src" --swap-ref "$target")
[[ -z $only ]] || args+=(--only "$only")
[[ $gl != on ]] || args+=(--gl)
status=0
cd "$src"
as_user env GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=safe.directory GIT_CONFIG_VALUE_0='*' \
  python3 scripts/vm-acceptance/run.py "${args[@]}" || status=$?
[[ ! -f $report/report.md ]] || cat "$report/report.md"
[[ ! -f $report/report.json ]] || jq -r '"score: \([.results[] | select(.status == "pass")] | length)/\(.results | length) checks pass (\(.overall))"' "$report/report.json"
exit "$status"
