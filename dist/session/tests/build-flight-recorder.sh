# Sourced by the session tests: the flight recorder and the session report
# built from the tree under test, so the tests run the programs the packages
# ship. Call it before tilo_test_isolate: the isolated environment has a fresh
# HOME, and the exported paths carry over its re-execution.
#
#   tilo_flight_recorder_build "$repo_root"
#   # sets and exports TILO_FLIGHT_RECORDER and TILO_SESSION_REPORT
tilo_flight_recorder_build() {
    local repo=$1 bin
    if [ -n "${TILO_FLIGHT_RECORDER:-}" ] && [ -x "$TILO_FLIGHT_RECORDER" ] \
        && [ -n "${TILO_SESSION_REPORT:-}" ] && [ -x "$TILO_SESSION_REPORT" ]; then
        return 0
    fi
    cargo build --quiet --locked --manifest-path "$repo/Cargo.toml" -p tilo-session-record \
        --bin tilo-flight-recorder --bin tilo-session-report >&2 || {
        echo 'cannot build the flight recorder (tilo-session-record)' >&2
        return 1
    }
    bin="${CARGO_TARGET_DIR:-$repo/target}/debug"
    TILO_FLIGHT_RECORDER="$bin/tilo-flight-recorder"
    TILO_SESSION_REPORT="$bin/tilo-session-report"
    export TILO_FLIGHT_RECORDER TILO_SESSION_REPORT
}
