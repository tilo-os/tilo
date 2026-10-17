#!/usr/bin/env bash
# Source this so the package gate receives the freshly observed host cohort.
[ "${1:-}" = --features ] && [ -n "${2:-}" ] \
    || { echo 'capture host build requires --features FEATURE' >&2; exit 1; }
capture_host_feature="$2"
if [ -n "${TILO_CAPTURE_PINS:-}${TILO_CAPTURE_SIDECAR:-}" ] || [ "${TILO_REQUIRE_CAPTURE:-0}" = 1 ]; then
    for input in TILO_CAPTURE_PINS TILO_CAPTURE_SIDECAR TILO_CAPTURE_BUILD_EVIDENCE \
                 TILO_CAPTURE_QUALIFIER_IMAGE TILO_CAPTURE_SOURCE_ASSETS; do
        [ -n "${!input:-}" ] || { echo "missing selected migration input: $input" >&2; exit 1; }
    done
    capture_packaging="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    capture_recorder="$capture_packaging/record-capture-cargo.py"
    export TILO_REQUIRE_CAPTURE=1
    "$capture_recorder" build --locked --release -p imager --bin imager --features "$capture_host_feature"
    "$capture_recorder" test --locked --release -p imager -p imager-backend --lib --no-run \
        --features "$capture_host_feature,native-capture-qualification"
    capture_cohort_dir="$(mktemp -d "$TILO_CAPTURE_BUILD_EVIDENCE/package-cohort.XXXXXX")"
    export TILO_CAPTURE_QUALIFICATION_COHORT="$capture_cohort_dir/cohort.json"
    "$capture_recorder" cohort --evidence "$TILO_CAPTURE_BUILD_EVIDENCE" \
        --output "$TILO_CAPTURE_QUALIFICATION_COHORT"
else
    "${CARGO:-cargo}" build --locked --release -p imager --features "$capture_host_feature"
fi
