#!/usr/bin/env bash
# Stamp the exact runtime build an app was linked against over the release its
# authored manifest declares.
#
#   stamp-runtime.sh <manifest.toml> <name>/<release>+native.<digest>
#
# A thin app links the runtime's Rust dylibs, which have no stable ABI: it
# loads on the build it was linked against and no other. The authored
# manifest says `runtime = "tilo-runtime/<release>"`; the packaged one must
# say which build, so an installer can refuse, or fetch the runtime first,
# on a device that has another. Refuses a manifest that declares another
# release, or none.
set -euo pipefail
manifest=$1
runtime=$2
[[ $runtime =~ ^[A-Za-z0-9._-]+/[0-9A-Za-z.-]+\+native\.[0-9a-f]{32}$ ]] || {
    printf 'stamp-runtime: %s is not <name>/<release>+native.<32 hex>\n' "$runtime" >&2
    exit 1
}
release=${runtime%%+native.*}
grep -Eq "^runtime[[:space:]]*=[[:space:]]*\"${release//./\\.}\"[[:space:]]*$" "$manifest" || {
    printf 'stamp-runtime: %s does not declare runtime = "%s"\n' "$manifest" "$release" >&2
    exit 1
}
sed -i -E "s|^runtime[[:space:]]*=[[:space:]]*\"${release//./\\.}\"[[:space:]]*$|runtime = \"$runtime\"|" "$manifest"
grep -Fqx "runtime = \"$runtime\"" "$manifest"
