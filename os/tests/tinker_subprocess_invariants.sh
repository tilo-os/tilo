#!/usr/bin/env bash
# OCI work-layer setup must not interpolate image data into a shell pipeline.
set -uo pipefail

TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$TEST_DIR/../.." && pwd)"
SOURCE="$ROOT/crates/system/tinker/src/main.rs"

if [ ! -r "$SOURCE" ]; then
    echo "FAIL: cannot inspect $SOURCE" >&2
    exit 1
fi

fails=0
checks=0
must_contain() {
    if grep -Eq "$1" "$SOURCE"; then checks=$((checks + 1)); else
        echo "FAIL: $2" >&2; fails=$((fails + 1))
    fi
}
must_not_contain() {
    if grep -Eq "$1" "$SOURCE"; then
        echo "FAIL: $2" >&2; fails=$((fails + 1))
    else checks=$((checks + 1)); fi
}

must_contain 'bounded_command::run\(' \
    "Podman metadata/control operations are not bounded"
must_contain 'Stdio::from\(exported\)' \
    "OCI export is not streamed directly to the extractor"
must_not_contain 'Command::new\("sh"\)|\.arg\("-c"\)' \
    "OCI extraction still crosses an unnecessary shell boundary"
must_not_contain 'shell_quote' \
    "shell quoting remains in the OCI extraction path"

if [ "$fails" -gt 0 ]; then
    echo "tinker subprocess invariants: $fails failed, $checks passed" >&2
    exit 1
fi
echo "OK: tinker subprocess invariants ($checks checks)"
