#!/usr/bin/env bash
# This worktree's dev OS image built and its acceptance suite run on the rcargo builder,
# not on this machine: the builder has /dev/kvm, the cores and the disk, and keeps what a
# build made (its podman store, the OS build's caches, the dev disk of each image) so the
# next run of the same inputs is warm. scripts/vm-acceptance/README.md ("On the rcargo
# builder") explains the flow.
#
#   builder-vm.sh image                  build the dev image of this worktree (tilo-vm image
#                                        build with the worktree's rcargo build image;
#                                        TILO_BUILD_ARGS=--no-cache builds every step again)
#   builder-vm.sh disk                   its dev disk, installed as os-dev-disk installs it,
#                                        inside a VM booted from main's published dev disk
#                                        (the builder has no loop devices and no root)
#   builder-vm.sh acceptance [--only IDS] [--shards S,...]
#                                        vm-acceptance against that disk, one container per
#                                        shard, side by side; reports land here, in
#                                        target/builder-vm/<run>/
#   builder-vm.sh all [--only IDS]       image, disk and acceptance
#   builder-vm.sh clean [--all]          this worktree's run containers and scratch;
#                                        --all also its image, disk and build caches (never
#                                        another worktree's, never the shared store)
#
# Everything heavy runs on the builder under /srv/rcargo/scratch/builder-vm, in rootless
# podman, outside rcargo's care: rcargo stops a worktree's build container after an hour
# without an rcargo command, evicts idle worktrees and prunes its shared podman store above
# the disk watermark, and a two-hour image build or a run's VMs do not survive that. The
# worktree's files go there with rsync; the image build runs in a container from the
# worktree's rcargo build image, with a podman store and the OS build's cargo and sccache
# caches that outlive it; the VMs run in a container made from tools/ci/packages/
# vm-acceptance.txt with /dev/kvm passed through, each in a network namespace of its own,
# so shards never share a port.
set -euo pipefail

cmd=${1:-}
[[ -n $cmd ]] || { sed -n '2,30p' "$0"; exit 2; }
shift
repo=$(git rev-parse --show-toplevel)
cd "$repo"
local_path=$(pwd -P)
scratch=/srv/rcargo/scratch/builder-vm
# This worktree's own part of the scratch, by its path.
key=$(printf '%s' "$local_path" | sha256sum | cut -c1-12)
src=$scratch/src/$key
label=sh.tilo.builder-vm
host_image_inputs=(tools/ci/install-packages.sh tools/ci/packages/vm-acceptance.txt)
builder=$(python3 -c '
import os, tomllib
b = tomllib.load(open(os.path.expanduser("~/.config/rcargo/config.toml"), "rb"))["builder"]
print(b.get("user", "fedora") + "@" + b["host"])')

on_builder() { rcargo builder ssh -- "$@"; }
note() { printf 'builder-vm: %s\n' "$*" >&2; }
stamp() { date -u +%s; }

# Run the script on stdin on the builder as a job of its own, detached from this connection,
# and follow its log to its end; a dropped connection (an hour-long build outlives some) is
# picked up again where the log was. Exits with the job's status.
builder_job() {
  local name=$1 dir=$scratch/jobs/$key-$1 seen=0 copy status
  # Each line of the log carries the seconds the job had run when it was written, so a slow
  # step of a build (podman prints no times of its own) shows itself in the log.
  on_builder "set -e; rm -rf $dir; mkdir -p $dir; cat > $dir/run.sh
    setsid nohup bash -c 'set -o pipefail; bash $dir/run.sh 2>&1 | awk -v t0=\$(date +%s) \"{ printf \\\"%5ds %s\\n\\\", systime() - t0, \\\$0; fflush() }\" > $dir/log; echo \$? > $dir/status' < /dev/null > /dev/null 2>&1 &"
  copy=$(mktemp)
  while :; do
    on_builder "until [ -e $dir/log ]; do sleep 1; done; tail -c +$((seen + 1)) -f $dir/log & t=\$!
      until [ -e $dir/status ]; do sleep 2; done; sleep 1; kill \$t" 2>/dev/null | tee -a "$copy" >&2 || true
    seen=$(stat -c %s "$copy")
    status=$(on_builder "cat $dir/status 2>/dev/null" 2>/dev/null) && [[ -n $status ]] && break
  done
  rm -f "$copy"
  return "$status"
}

# The worktree's files as they are here, tracked and untracked but not ignored, mirrored
# into its scratch on the builder: an rsync of what changed, seconds after an edit.
sync_src() {
  on_builder "mkdir -p $src"
  git ls-files -z --cached --others --exclude-standard \
    | rsync -a --delete-missing-args --from0 --files-from=- -e 'ssh -o BatchMode=yes' \
        ./ "$builder:$src/" >&2
}

# The worktree's directory on the builder as rcargo keeps it (its meta.json names this
# checkout), whose build container's image the OS build runs in.
rcargo_tree() {
  local found
  found=$(on_builder "grep -l '\"local_path\": \"$local_path\"' /srv/rcargo/p/*/w/*/meta.json | head -1")
  [[ -n $found ]] || { note "rcargo has no builder worktree for $local_path"; exit 1; }
  printf '%s\n' "${found%/meta.json}"
}

# The tilo-vm the VMs are driven with, built by rcargo from this tree's tilo-vm sources and
# kept by them, so a run whose tilo-vm did not change builds nothing.
tilo_vm() {
  local sources dir tree
  sources=$({ git ls-files -s -- crates/dev/tilo-vm crates/dev/tilo-vm-protocol Cargo.lock
              git diff HEAD -- crates/dev/tilo-vm crates/dev/tilo-vm-protocol Cargo.lock; } | sha256sum | cut -c1-16)
  dir=$scratch/bin/$sources
  if ! on_builder "test -x $dir/tilo-vm"; then
    note "building tilo-vm ($sources)"
    rcargo build -p tilo-vm --quiet >&2
    git checkout -q -- Cargo.lock 2>/dev/null || true
    tree=$(rcargo_tree)
    on_builder "mkdir -p $dir.part && cp $tree/target/debug/tilo-vm $dir.part/ && rm -rf $dir && mv $dir.part $dir"
  fi
  printf '%s\n' "$dir/tilo-vm"
}

# The container the VMs run in: Fedora with what vm-acceptance.sh needs, tagged by the
# package list and installer, so it is built once per list and kept in the builder's store.
host_image() {
  local tag image
  tag=$(cat "${host_image_inputs[@]}" | sha256sum | cut -c1-16)
  image=localhost/tilo-builder-vm-host:$tag
  if ! on_builder "podman image exists $image"; then
    note "building the VM host image $image"
    tar -c "${host_image_inputs[@]}" | on_builder "set -e; mkdir -p $scratch/tmp; d=\$(mktemp -d $scratch/tmp/host.XXXX); cd \$d; tar -x
      printf '%s\n' 'FROM registry.fedoraproject.org/fedora:44' 'COPY tools /tools' \
        'RUN bash /tools/ci/install-packages.sh /tools/ci/packages/vm-acceptance.txt && dnf clean all' > Containerfile
      podman build -q -t $image --label $label=host . >/dev/null; cd /; rm -rf \$d" >&2
  fi
  printf '%s\n' "$image"
}

image_dir() { printf '%s/images/%s\n' "$scratch" "$1"; }

cmd_image() {
  local t0 digest bin rcargo_image out=$scratch/out/$key
  t0=$(stamp)
  note "building the dev image of $(git rev-parse --short HEAD)$(git diff --quiet HEAD || echo ' + local changes') on the builder"
  sync_src
  bin=$(tilo_vm)
  # The worktree's rcargo build image: the toolchain, podman and the OS build's tools.
  rcargo check -p tilo-vm-protocol --quiet >&2
  git checkout -q -- Cargo.lock 2>/dev/null || true
  rcargo_image=$(on_builder "podman inspect --format '{{.ImageName}}' rcargo-$(basename "$(rcargo_tree)")")
  # With the rcargo container's capabilities, the environment rcargo's config gives the OS
  # build, and a local tag of this worktree's own (rcargo 0.1.0 passes its config's
  # `TILO_LOCAL_TAG = "wt-{worktree}"` on without expanding the placeholder). The compile
  # jobs are bounded: the build counts the node's cores (377), not the builder's share of
  # them, and that many compilers at once fail to spawn; neither container limits its
  # processes, as podman's default of 2048 does, which a compile of 32 jobs outgrows.
  # Every worktree's build shares the store's images and layers, but keeps its containers
  # (the compile's, the image build's steps) in its own run directory (transient_store): a
  # podman starting in another worktree's build container takes the shared container
  # database for one left from before a reboot and resets it, which unmounted the running
  # compile of this one ("Failed to create temp dir", a linker killed mid-link).
  builder_job image <<<"set -e
    mkdir -p $scratch/store/tmp $scratch/store/tilo-external-assets $out
    printf '[containers]\npids_limit = 0\n' > $scratch/store/containers.conf
    printf '[storage]\ndriver = \"overlay\"\ngraphroot = \"/var/lib/containers/storage\"\nrunroot = \"/run/containers/storage\"\ntransient_store = true\n[storage.options]\nadditionalimagestores = [\"/usr/lib/containers/storage\"]\n' \
      > $scratch/store/storage.conf
    podman run --rm --name bvm-image-$key --label $label=image --label $label.tree=$key --pids-limit=-1 \
      -e CONTAINERS_CONF_OVERRIDE=/var/lib/containers/containers.conf \
      -e CONTAINERS_STORAGE_CONF=/var/lib/containers/storage.conf \
      --cap-add SYS_ADMIN --cap-add NET_ADMIN --security-opt unmask=ALL --device /dev/fuse \
      -v $scratch/store:/var/lib/containers -v /srv/rcargo/ci-cache:/srv/rcargo/ci-cache:ro \
      -v $src:'$local_path' -v $out:/out -v $bin:/usr/local/bin/tilo-vm:ro \
      -w '$local_path' -e TILO_LOCAL_TAG=wt-$key -e SOURCE_DATE_EPOCH=$(git log -1 --format=%ct) \
      -e TILO_CARGO_BUILD_JOBS=32 -e TILO_DEV_CARGO_BUILD_JOBS=32 -e TILO_BUILD_ARGS='${TILO_BUILD_ARGS:-}' \
      -e TILO_ASSET_STORE=/var/lib/containers/tilo-external-assets -e TMPDIR=/var/lib/containers/tmp \
      -e TILO_IMAGE_SCCACHE_URL=https://code.tilo.so/ci-cache/r/tilo/tilo/ \
      -e TILO_IMAGE_SCCACHE_TOKEN_FILE=/srv/rcargo/ci-cache/token \
      $rcargo_image tilo-vm image build --out /out/tilo-vm"
  digest=$(on_builder "cat $out/tilo-vm/image.digest")
  # One copy per image (reflinks on the builder's XFS), named by its digest, so the next
  # build of this worktree does not replace what a disk was made from.
  on_builder "set -e; d=$(image_dir "${digest#sha256:}"); if [ ! -e \$d/image.digest ]; then
      mkdir -p \$d.part; cp -a --reflink=auto $out/tilo-vm/image $out/tilo-vm/image.facts \
        $out/tilo-vm/image.digest \$d.part/; rm -rf \$d; mv \$d.part \$d; fi
    mkdir -p $scratch/worktrees; printf '%s\n' '$digest' > $scratch/worktrees/$key.image"
  note "dev image $digest in $(( $(stamp) - t0 )) s"
}

current_digest() {
  on_builder "cat $scratch/worktrees/$key.image 2>/dev/null" \
    || { note "no dev image built for this worktree: builder-vm.sh image"; exit 1; }
}

# Main's newest published dev disk, fetched once and kept by its tag: the worker the disk
# step installs in.
base_disk() {
  local tag json url sha
  tag=$(curl -fsSL "https://code.tilo.so/api/v1/packages/tilo?type=generic&q=tilo-vm-disk&limit=50" \
    | jq -r '[.[] | select(.version | startswith("main-"))] | sort_by(.created_at) | last | .version')
  [[ -n $tag && $tag != null ]] || { note "main has published no dev disk"; exit 1; }
  json=$(curl -fsSL "https://code.tilo.so/api/packages/tilo/generic/tilo-vm-disk/$tag/tilo-vm-disk-x86_64.json")
  url=$(jq -er '.disks[] | select(.format == "qcow2" and .arch == "x86_64") | .url' <<<"$json")
  sha=$(jq -er '.disks[] | select(.format == "qcow2" and .arch == "x86_64") | .sha256' <<<"$json")
  printf '%s\n' "$json" | on_builder "set -e; d=$scratch/base/$tag; if [ ! -s \$d/disk.qcow2 ]; then mkdir -p \$d.part
      cat > \$d.part/manifest.json
      curl -fsSL --retry 3 -o \$d.part/disk.qcow2 '$url'
      echo '$sha  '\$d.part/disk.qcow2 | sha256sum -c --quiet
      rm -rf \$d; mv \$d.part \$d; fi" >&2
  printf '%s\n' "$scratch/base/$tag"
}

cmd_disk() {
  local t0 digest base image disks bin
  t0=$(stamp)
  digest=$(current_digest)
  disks=$scratch/disks/${digest#sha256:}
  if on_builder "test -s $disks/disk.qcow2"; then
    note "dev disk of $digest kept from an earlier run"
    return
  fi
  sync_src
  bin=$(tilo_vm)
  base=$(base_disk)
  image=$(host_image)
  note "installing $digest onto a disk in a VM booted from main's dev disk ($(basename "$base"))"
  builder_job disk <<<"set -e; rm -rf $disks.part $scratch/work/$key
    mkdir -p $disks.part $scratch/work/$key
    podman run --rm --name bvm-disk-$key --label $label=disk --label $label.tree=$key \
      --device /dev/kvm -e SOURCE_DATE_EPOCH=$(git log -1 --format=%ct) \
      -v $base:/base:ro -v $disks.part:/out -v $scratch/work/$key:/work \
      -v $(image_dir "${digest#sha256:}"):/layout:ro -v $bin:/usr/local/bin/tilo-vm:ro \
      -v $src:/src:ro $image bash /src/tools/ci/builder-vm-guest.sh disk
    rm -rf $disks $scratch/work/$key; mv $disks.part $disks"
  note "dev disk of $digest in $(( $(stamp) - t0 )) s"
}

cmd_acceptance() {
  local only="" shards="" t0 digest disks image run commit out bin
  while (($#)); do
    case $1 in
      --only) only=$2; shift 2 ;;
      --shards) shards=${2//,/ }; shift 2 ;;
      *) note "unknown argument $1"; exit 2 ;;
    esac
  done
  t0=$(stamp)
  digest=$(current_digest)
  disks=$scratch/disks/${digest#sha256:}
  on_builder "test -s $disks/disk.qcow2" || { note "no dev disk of $digest: builder-vm.sh disk"; exit 1; }
  sync_src
  bin=$(tilo_vm)
  image=$(host_image)
  [[ -n $shards ]] || shards=$(python3 -c '
import importlib.util
spec = importlib.util.spec_from_file_location("run", "scripts/vm-acceptance/run.py")
run = importlib.util.module_from_spec(spec)
spec.loader.exec_module(run)
print(" ".join(run.SHARDS))')
  commit=$(git rev-parse HEAD)
  run=$(date -u +%Y%m%dT%H%M%SZ)-$key
  note "vm-acceptance of $digest, run $run: shards $shards${only:+, checks $only}"
  # One container per shard, all at once: each boots its own VM from the disk. The run's
  # directory is the container's /target, where vm-acceptance.sh writes its reports (it
  # empties /target/vm-acceptance, which a mount of its own would refuse).
  builder_job "run-$run" <<<"set -e; r=$scratch/runs/$run; mkdir -p \$r
    for shard in $shards; do
      mkdir -p \$r/\$shard
      podman run -d --name bvm-$run-\$shard --label $label=run --label $label.tree=$key \\
        --label $label.run=$run --device /dev/kvm \\
        -e T_SHARDS=\$shard -e T_TRIGGER=builder -e T_TEST_FILTER='${only//,/|}' -e TILO_ACCEPTANCE_COMMIT=$commit \\
        -v \$r/\$shard:/target -v $disks:/target/publish/os-dev-disk/x86_64/disk:ro \\
        -v $bin:/target/tilo-vm-bin/tilo-vm:ro -v $src:/src-ro:ro \\
        $image bash /src-ro/tools/ci/builder-vm-guest.sh acceptance >/dev/null
    done
    for shard in $shards; do
      status=\$(podman wait bvm-$run-\$shard 2>&1 || true)
      podman logs bvm-$run-\$shard > \$r/\$shard.log 2>&1 || true
      echo \"builder-vm: the \$shard container exited with status \$status\" >> \$r/\$shard.log
      podman rm -f bvm-$run-\$shard >/dev/null || true
      grep -h '^t-shard:' \$r/\$shard.log || echo \"t-shard: \$shard failed (it said nothing of itself; \$shard.log)\"
    done"
  out=target/builder-vm/$run
  mkdir -p "$out"
  on_builder "cd $scratch/runs/$run && tar -c --exclude='*.qcow2' --exclude=publish --exclude=tilo-vm-bin ." | tar -x -C "$out"
  note "vm-acceptance in $(( $(stamp) - t0 )) s; reports in $out"
  python3 - "$out" $shards <<'EOF'
import json, pathlib, re, sys
out, shards = pathlib.Path(sys.argv[1]), sys.argv[2:]
failed, unfinished = [], []
for shard in shards:
    log = out / f"{shard}.log"
    text = log.read_text(errors="replace") if log.exists() else ""
    if not re.search(rf"^t-shard: {re.escape(shard)} passed$", text, re.M):
        unfinished.append(shard)
    for report in sorted((out / shard / "vm-acceptance").glob("*/report/report.json")):
        for r in json.loads(report.read_text())["results"]:
            if r["status"] == "fail":
                failed.append(f'{shard:12s} {r["id"]} {r.get("seconds") or 0:6.1f}s  {r["title"]}')
print("failed checks:" if failed else "no failed check reported", *failed, sep="\n  ")
if unfinished:
    print("shards that did not pass:", " ".join(unfinished))
sys.exit(1 if unfinished else 0)
EOF
}

cmd_clean() {
  local all=0 digest
  [[ ${1:-} == --all ]] && all=1
  on_builder "podman ps -aq --filter label=$label.tree=$key | xargs -r podman rm -f >/dev/null
    podman unshare rm -rf $scratch/runs/*-$key $scratch/jobs/$key-* $scratch/work/$key $scratch/src/$key"
  if ((all)); then
    digest=$(on_builder "cat $scratch/worktrees/$key.image 2>/dev/null" || true)
    on_builder "podman unshare rm -rf $scratch/out/$key $scratch/worktrees/$key.image ${digest:+$(image_dir "${digest#sha256:}") $scratch/disks/${digest#sha256:}}"
  fi
  note "cleaned this worktree's runs and scratch$( ((all)) && echo ', its image, disk and build caches')"
}

case $cmd in
  image) cmd_image ;;
  disk) cmd_disk ;;
  acceptance) cmd_acceptance "$@" ;;
  all) cmd_image; cmd_disk; cmd_acceptance "$@" ;;
  clean) cmd_clean "$@" ;;
  *) note "unknown command $cmd"; sed -n '2,30p' "$0"; exit 2 ;;
esac
