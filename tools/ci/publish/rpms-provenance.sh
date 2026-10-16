#!/usr/bin/env bash
# The frozen RPM snapshot's SLSA provenance predicate (build-rpms.yml's frozen-rpm-index/v1, as
# os/dev/resolve-rpm-snapshot.sh and os/dev/build-image.sh verify it) for the index in
# DIR/index, written to DIR/index/provenance.json, and the package-set fingerprint its tag
# names (`set-<fingerprint>`, and `set-<fingerprint>-<arch>` for each image) to the fingerprint
# file of DIR/index and of each architecture's DIR/<arch>. The commit is left as `{commit}` for the
# signing check (oci-attest.sh), which alone knows it. Holds no secrets.
#
# usage: rpms-provenance.sh DIR
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
dir=${1:?usage: rpms-provenance.sh DIR}
cd "$repo_root"
digest=$(cat "$dir/index/digest")
[[ $digest =~ ^sha256:[0-9a-f]{64}$ ]] || die "not a digest: $digest"
. os/base-snapshot.env
fingerprint=$(python3 os/dev/rpm-snapshot-fingerprint.py)
[[ $fingerprint =~ ^[0-9a-f]{64}$ ]] || die "not a fingerprint: $fingerprint"
for d in index x86_64 aarch64; do
  printf '%s\n' "$fingerprint" > "$dir/$d/fingerprint"
done
python3 - "$digest" "$BASE_REF" "$dir/index/provenance.json" <<'PY'
import hashlib, json, subprocess, sys
digest, base, out = sys.argv[1:]
def roots(arch):
    names = subprocess.check_output(["python3", "os/dev/derive-rpm-package-set.py", "--arch", arch], text=True)
    return sorted({line for line in names.splitlines() if line})
def sha(lines):
    return hashlib.sha256("".join(f"{line}\n" for line in lines).encode()).hexdigest()
amd64, arm64 = roots("x86_64"), roots("aarch64")
predicate = {
    "buildDefinition": {
        "buildType": "https://tilo.sh/buildtypes/frozen-rpm-index/v1",
        "externalParameters": {
            "architectures": ["amd64", "arm64"],
            "packageSetSha256": sha(sorted(set(amd64) | set(arm64))),
            "packageSetSha256ByArchitecture": {"amd64": sha(amd64), "arm64": sha(arm64)},
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
printf 'set-%s: %s\n' "$(cat "$dir/index/fingerprint")" "$digest"
