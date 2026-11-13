#!/usr/bin/env bash
# The frozen RPM snapshot's SLSA provenance predicate for one architecture, as
# os/dev/resolve-rpm-snapshot.sh and os/dev/build-image.sh verify it: for the image in
# DIR/<arch>, written to DIR/<arch>/provenance.json, and the package-set fingerprint its tag
# names (`set-<fingerprint>-<arch>`) to DIR/<arch>/fingerprint. The predicate binds the
# architecture, that architecture's package set, the image's digest and the base it was
# resolved against; the commit is left as `{commit}` for the signing check (oci-attest.sh),
# which alone knows it. Holds no secrets.
#
# The fingerprint is the whole package set's, both architectures' lists under the one name, so
# the same package set publishes the same tag suffix on each; the `-<arch>` suffix says which
# architecture's image it is. One architecture's set is published without the other: a machine
# of one builds and publishes its own.
#
# usage: rpms-provenance.sh ARCH DIR
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
arch=${1:?usage: rpms-provenance.sh ARCH DIR}
dir=${2:?}
case $arch in
  x86_64) platform_arch=amd64 ;;
  aarch64) platform_arch=arm64 ;;
  *) die "unknown architecture $arch" ;;
esac
cd "$repo_root"
digest=$(cat "$dir/$arch/digest")
[[ $digest =~ ^sha256:[0-9a-f]{64}$ ]] || die "not a digest: $digest"
. os/base-snapshot.env
fingerprint=$(python3 os/dev/rpm-snapshot-fingerprint.py)
[[ $fingerprint =~ ^[0-9a-f]{64}$ ]] || die "not a fingerprint: $fingerprint"
printf '%s\n' "$fingerprint" > "$dir/$arch/fingerprint"
python3 - "$digest" "$BASE_REF" "$dir/$arch/provenance.json" "$platform_arch" "$arch" <<'PY'
import hashlib, json, subprocess, sys
digest, base, out, platform_arch, arch = sys.argv[1:]
names = subprocess.check_output(
    ["python3", "os/dev/derive-rpm-package-set.py", "--arch", arch], text=True)
package_set = sorted({line for line in names.splitlines() if line})
package_set_sha256 = hashlib.sha256(
    "".join(f"{line}\n" for line in package_set).encode()).hexdigest()
predicate = {
    "buildDefinition": {
        "buildType": "https://tilo.sh/buildtypes/frozen-rpm-snapshot/v1",
        "externalParameters": {
            "architecture": platform_arch,
            "packageSetSha256": package_set_sha256,
        },
        "internalParameters": {"expectedReleaseDigest": digest},
        "resolvedDependencies": [{"uri": "git+ssh://code.tilo.so/tilo/tilo@{commit}"}, {"uri": base}],
    },
    "runDetails": {"builder": {"id": "https://code.tilo.so/t/checks/publish-rpms"}, "metadata": {}},
}
with open(out, "w") as f:
    json.dump(predicate, f, indent=2)
    f.write("\n")
PY
printf 'set-%s-%s: %s\n' "$(cat "$dir/$arch/fingerprint")" "$arch" "$digest"
