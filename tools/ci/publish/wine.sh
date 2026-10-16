#!/usr/bin/env bash
# The build halves of the x86 Windows guest (docs/windows-compatibility.md): traditional Wine
# rebuilt with Mock for x86-64 and i686, paired, signed with a key made for the run, and
# assembled into the guest. The steps run as root in micro-VM steps whose userspace is the
# wine-tools image built in the same run (image-wine-tools), so Mock, podman and the
# producer scripts run directly instead of in containers of a Docker daemon. They hold no
# secrets and push nothing: each image goes into an OCI archive with its digest beside it
# (DIR/image.oci.tar, DIR/digest), and what later steps record (contracts, provenance) names it
# by the registry digest it is published with, since pushing keeps digests.
#
# usage: wine.sh source OUT
#        wine.sh resolve TARGET OUT
#        wine.sh build TARGET OUT
#        wine.sh pair OUT
#        wine.sh guest OUT
# TARGET is x86_64 or i686. Inputs are at the paths the checks put them (/target/windows-compat).
set -euo pipefail
here=$(dirname -- "${BASH_SOURCE[0]}")
. "$here/lib.sh"
inputs=/target/windows-compat
tools_out=/target/publish/wine-tools
work=/var/tmp/tilo-wine
registry=registry.tilo.so/tilo

[[ $(uname -m) == x86_64 ]] || die 'Wine is built natively on x86_64'
[[ $(id -u) == 0 ]] || die 'runs as root in a micro-VM step (isolation = "vm")'

# The wine-tools image this step runs in, by the digest it is published as.
runtime() {
  local digest
  digest=$(cat "$tools_out/digest")
  [[ $digest =~ ^sha256:[0-9a-f]{64}$ ]] || die "no wine-tools digest in $tools_out"
  printf '%s/wine-tools@%s' "$registry" "$digest"
}

# podman as root without systemd, for this script and for Mock's bootstrap image: cgroups and
# events by hand, storage as the image configures it.
mkdir -p /etc/containers/containers.conf.d
printf '[engine]\ncgroup_manager = "cgroupfs"\nevents_logger = "file"\n' \
  > /etc/containers/containers.conf.d/tilo-micro-vm.conf
p() { podman "$@"; }

# Write IMAGE (in podman's store) to DIR/image.oci.tar with its digest, and load that archive
# back under REPOSITORY, so builds take it FROM REPOSITORY@<digest>, the reference it is
# published as. Prints that reference.
archive() {
  local image=$1 dir=$2 repository=$3 digest id
  mkdir -p "$dir"
  rm -f "$dir/image.oci.tar"
  p push --quiet --compression-format gzip --force-compression "$image" "oci-archive:$dir/image.oci.tar" >&2
  digest=$(archive_digest "$dir/image.oci.tar")
  [[ $digest =~ ^sha256:[0-9a-f]{64}$ ]] || die "no manifest digest in $dir/image.oci.tar"
  printf '%s\n' "$digest" > "$dir/digest"
  load "$dir" "$repository"
}

# Load DIR/image.oci.tar under REPOSITORY and print REPOSITORY@<digest>, checking that the
# reference resolves to it locally.
load() {
  local dir=$1 repository=$2 digest id
  digest=$(cat "$dir/digest")
  [[ $(archive_digest "$dir/image.oci.tar") == "$digest" ]] || die "$dir/image.oci.tar is not its build"
  id=$(p pull --quiet "oci-archive:$dir/image.oci.tar")
  p tag "$id" "$repository:local" >&2
  p image inspect "$repository@$digest" >/dev/null || die "$repository@$digest does not resolve to $dir"
  printf '%s@%s' "$repository" "$digest"
}

# The tree PATH under DIR as an image, as `docker import` made them: `wine-output` holds
# /wine-output, `.` the directory's contents at the root.
import_tree() {
  local dir=$1 path=$2 tag=$3
  tar -C "$dir" -cf - "$path" | p import --quiet --os linux --arch amd64 - "$tag" >/dev/null
}

# Mock in a VM without systemd: the simple chroot isolation it picks inside containers, and a
# root on the step's scratch disk instead of the image's.
prepare_mock() {
  printf "\n# A micro-VM step has no systemd for nspawn, like the containers this ran in.\nconfig_opts['isolation'] = 'simple'\n" \
    >> /etc/mock/site-defaults.cfg
  mkdir -p "$work/mock"
  chown root:mock "$work/mock"
  chmod 2775 "$work/mock"
  mount --bind "$work/mock" /var/lib/mock
}

config() {
  case $1 in x86_64) echo fedora-44-x86_64 ;; i686) echo fedora-44-i386 ;; *) die "unknown target $1" ;; esac
}

# The producer, as CI ran it: sudo from the unprivileged member of the mock group.
producer() { runuser -u tilo-build -- sudo -n /usr/bin/python3 /tilo/wine-mock-producer.py "$@"; }

# Mock compiles with this many jobs; resolution records it and the build must match.
jobs=16

cmd=${1:?usage: wine.sh source|resolve|build|pair|guest ...}
shift
mkdir -p "$work"
case $cmd in
  source)
    out=${1:?usage: wine.sh source OUT}
    rm -rf "$out"
    mkdir -p "$out/evidence"
    runtime > "$out/evidence/runtime-image.txt"
    python3 /tilo/fetch-wine-source.py "$out/source"
    cp -a /usr/lib/tilo/. "$out/evidence/"
    ;;
  resolve)
    target=${1:?usage: wine.sh resolve TARGET OUT}
    out=${2:?}
    prepare_mock
    cp -a "$inputs/wine-source/source" "$work/source"
    trap 'status=$?; ((status)) && for f in "$work"/bundle/*.log; do echo "== $f"; tail -n 60 "$f"; done; exit $status' EXIT
    producer resolve --target "$target" --runtime-image "$(runtime)" --pin /tilo/wine-source.json \
      --source "$work/source" --config "$(config "$target")" --output "$work/bundle" --jobs "$jobs"
    mkdir -p "$out"
    tar -C "$work/bundle" -cf "$out/bundle.tar" .
    ;;
  build)
    target=${1:?usage: wine.sh build TARGET OUT}
    out=${2:?}
    prepare_mock
    rm -rf "$out"
    mkdir -p "$out/evidence" "$work/inputs/bundle" "$work/output"
    tar -C "$work/inputs/bundle" -xf "$inputs/wine-resolve-$target/bundle.tar"
    # The resolution as the image Forgejo pushed: what the RPMs' contract names.
    import_tree "$work/inputs/bundle" . localhost/wine-resolution:build
    snapshot=$(archive localhost/wine-resolution:build "$out/resolution" "$registry/wine-resolution")
    printf '%s\n' "$snapshot" > "$out/evidence/resolution-image.txt"
    # Only the checked bundle crosses into the offline build: read-only, with no network but
    # loopback (the producer checks /sys/class/net, so sysfs is mounted for the new namespace).
    mount --bind -o ro "$work/inputs" "$work/inputs"
    trap 'status=$?; ((status)) && { tail -n 200 "$work/output/hermetic-build.log" 2>/dev/null; cp -a "$work/output" "$out/evidence/raw" 2>/dev/null; }; exit $status' EXIT
    unshare --net --mount --propagation private -- sh -euc '
      mount -t sysfs sysfs /sys
      exec runuser -u tilo-build -- sudo -n /usr/bin/python3 /tilo/wine-mock-producer.py "$@"
    ' producer build --target "$target" --runtime-image "$(runtime)" --pin /tilo/wine-source.json \
      --bundle "$work/inputs/bundle" --output "$work/output/output" --jobs "$jobs"
    python3 /tilo/wine_rpm_contract.py import-mock "$target" /tilo/wine-source.json \
      "$work/output/output" "$work/inputs/bundle" "$work/wine-output" "$snapshot"
    import_tree "$work" wine-output localhost/wine-rpms:build
    rpms=$(archive localhost/wine-rpms:build "$out/rpms" "$registry/wine-rpms")
    printf '%s\n' "$rpms" > "$out/evidence/wine-rpms-image.txt"
    cp -a "$work/wine-output/provenance" "$out/evidence/provenance"
    ;;
  pair)
    out=${1:?usage: wine.sh pair OUT}
    rm -rf "$out"
    mkdir -p "$out/evidence"
    refs=()
    for arch in x86_64 i686; do
      ref=$(load "$inputs/wine-build-$arch/rpms" "$registry/wine-rpms")
      refs+=("$ref")
      container=$(p create "$ref" /unused)
      mkdir -p "$work/payloads/$arch"
      p cp "$container:/wine-output/." "$work/payloads/$arch/"
      p rm "$container" >/dev/null
    done
    python3 /tilo/merge-wine-rpms.py --x64 "$work/payloads/x86_64" --x86 "$work/payloads/i686" \
      --x64-ref "${refs[0]}" --x86-ref "${refs[1]}" --pin /tilo/wine-source.json --output "$work/wine-paired"
    import_tree "$work" wine-paired localhost/wine-paired:build
    archive localhost/wine-paired:build "$out" "$registry/wine-paired" > "$out/evidence/image.txt"
    cp "$work/wine-paired/paired-contract.json" "$out/evidence/"
    ;;
  guest)
    # os/dev/run-development-windows-guest.py's packaging, with podman instead of a daemon:
    # sign the pair with a key made for this run, then resolve the guest's RPMs and assemble it.
    out=${1:?usage: wine.sh guest OUT}
    rm -rf "$out"
    mkdir -p "$out/evidence"
    buildroot=$(runtime)
    load "$tools_out" "$registry/wine-tools" >/dev/null
    pair=$(load "$inputs/wine-paired" "$registry/wine-paired")
    WINDOWS_GUEST_BUILDROOT_REF=$buildroot WINE_PAIR_REF=$pair \
      python3 "$repo_root/os/dev/run-development-windows-guest.py" --validate-only
    build() {
      local tag=$1 recipe=$2 context=$3
      shift 3
      p build --pull=never --network=host --format oci --timestamp 0 --platform linux/amd64 \
        -t "$tag" -f "$recipe" "$@" "$context" >&2
    }
    # The signer: private GnuPG state exists only in this container, which has no network.
    build localhost/windows-guest-signing:build "$repo_root/os/Containerfile.windows-guest-tools" "$repo_root" \
      --build-arg "WINDOWS_GUEST_BUILDROOT_REF=$buildroot"
    container=$(p create "$pair" /unused)
    mkdir -p "$work/pair" "$work/export"
    p cp "$container:/wine-paired/." "$work/pair/"
    p rm "$container" >/dev/null
    receipt=$(p run --rm --network=none -v "$repo_root/os/dev:/tilo-dev:ro" -v "$work/pair:/wine-paired:ro" \
      -v "$work/export:/export" --entrypoint sh localhost/windows-guest-signing:build -euc '
        cp -a /tilo-dev/. /tilo/
        python3 /tilo/development-wine-repository.py sign --pair /wine-paired --pair-reference "$1" \
          --pin /tilo/wine-source.json --output /export/signed-wine
      ' signer "$pair")
    [[ -n $receipt ]] || die 'the development Wine signer failed'
    key=$(printf '%s' "$receipt" | python3 -c 'import json, sys; print(json.load(sys.stdin)["public_key_sha256"])')
    [[ $key =~ ^[0-9a-f]{64}$ ]] || die 'the signer returned no public key digest'
    printf '%s\n' "$receipt" > "$out/evidence/signing-receipt.json"
    printf 'FROM scratch\nCOPY signed-wine /signed-wine\n' > "$work/Containerfile.signed-wine"
    build localhost/development-wine:build "$work/Containerfile.signed-wine" "$work/export"
    wine=$(archive localhost/development-wine:build "$out/development-wine" "$registry/tilo-development-wine")
    args=(--build-arg "WINDOWS_GUEST_BUILDROOT_REF=$buildroot" --build-arg "DEVELOPMENT_WINE_REF=$wine"
          --build-arg "DEVELOPMENT_WINE_KEY_SHA256=$key")
    build localhost/windows-guest-rpms:build "$repo_root/os/Containerfile.windows-guest-rpms" "$repo_root" "${args[@]}"
    rpms=$(archive localhost/windows-guest-rpms:build "$out/rpms" "$registry/tilo-development-windows-guest-rpms")
    python3 "$repo_root/os/dev/fetch-windows-guest-assets.py" "$work/assets"
    build localhost/windows-guest:build "$repo_root/os/Containerfile.windows-guest" "$repo_root" "${args[@]}" \
      --build-arg "WINDOWS_GUEST_RPMS_REF=$rpms" --build-context "github-assets=$work/assets"
    guest=$(archive localhost/windows-guest:build "$out/guest" "$registry/tilo-development-windows-guest")
    printf '%s\n' "$guest" > "$out/evidence/guest-reference.txt"
    python3 - "$out/evidence" "$buildroot" "$pair" "$wine" "$key" "$rpms" "$guest" <<'EOF'
import json, sys
from pathlib import Path
evidence, buildroot, pair, wine, key, rpms, guest = sys.argv[1:]
inputs = {"WINDOWS_GUEST_BUILDROOT_REF": buildroot, "WINE_PAIR_REF": pair, "DEVELOPMENT_WINE_REF": wine,
          "DEVELOPMENT_WINE_KEY_SHA256": key, "WINDOWS_GUEST_RPMS_REF": rpms}
# This receipt proves assembly only, never Windows application acceptance.
Path(evidence, "result.json").write_text(json.dumps({
    "schema": 1, "development_only": True, "reference": guest,
    "runtime_acceptance_verified": False, "inputs": inputs}, indent=2) + "\n")
EOF
    ;;
  *) die "unknown command $cmd" ;;
esac
