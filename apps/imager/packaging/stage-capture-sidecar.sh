#!/usr/bin/env bash
# Stage a migration-enabled package only after its actual candidate is qualified.
set -euo pipefail
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
host=${1:?missing candidate host}
prefix=${2:?missing package prefix}
if [ -z "${TILO_CAPTURE_PINS:-}" ] && [ -z "${TILO_CAPTURE_SIDECAR:-}" ]; then
    exit 0
fi
[ -n "${TILO_CAPTURE_PINS:-}" ] && [ -n "${TILO_CAPTURE_SIDECAR:-}" ] \
    || { echo 'capture pins and sidecar must both be selected' >&2; exit 1; }
# The fixed production qualifier must exercise the actual candidate cohort.
# A caller-supplied ready flag, detached hash list or signed protocol declaration
# is not a transfer qualification. Candidate builds remain possible via Cargo;
# packaging requires this actual qualification producer to succeed.
qualifier="$repo/apps/imager/packaging/qualify-capture-release.py"
[ -f "$qualifier" ] && [ ! -L "$qualifier" ] \
    || { echo 'migration-enabled packaging requires the actual native release qualifier; it is not yet available' >&2; exit 1; }
python3 "$repo/apps/imager/packaging/admit-capture-sidecar.py" \
    --pins "$TILO_CAPTURE_PINS" --sidecar "$TILO_CAPTURE_SIDECAR" --arch "$(uname -m)" >/dev/null
python3 "$qualifier" --host "$host" --pins "$TILO_CAPTURE_PINS" \
    --sidecar "$TILO_CAPTURE_SIDECAR" --arch "$(uname -m)"
[ ! -e "$prefix/share/tilo-imager/capture" ] && [ ! -L "$prefix/share/tilo-imager/capture" ] \
    || { echo 'capture package destination must be fresh' >&2; exit 1; }
mkdir -p "$prefix/share/tilo-imager"
cp -a "$TILO_CAPTURE_SIDECAR" "$prefix/share/tilo-imager/capture"
# Check the bytes actually staged, not only the producer's original location.
python3 "$repo/apps/imager/packaging/admit-capture-sidecar.py" \
    --pins "$TILO_CAPTURE_PINS" --sidecar "$prefix/share/tilo-imager/capture" --arch "$(uname -m)" >/dev/null
