#!/usr/bin/env bash
# Run one build/test command and surface failures near the top of the run.
#
# Usage: scripts/ci-run.sh <label> -- <command> [args...]
#
# Cargo failures can be buried under thousands of build-progress lines. On
# failure, this script writes a summary to `$GITHUB_STEP_SUMMARY`, emits
# `::error::` annotations, and prints a delimited diagnostic block.
#
# The command streams live through `tee` while it runs because a silent
# multi-hour step looks indistinguishable from a hung one to a human or
# watchdog.
set -uo pipefail

label="${1:?usage: ci-run.sh <label> -- <command...>}"
shift
[ "${1:-}" = "--" ] && shift
[ "$#" -gt 0 ] || { echo "ci-run.sh: no command given" >&2; exit 2; }

log="$(mktemp -t "ci-${label//[^A-Za-z0-9_-]/_}-XXXXXX.log")"
heartbeat_pid=""

stop_heartbeat() {
    if [ -n "$heartbeat_pid" ]; then
        kill "$heartbeat_pid" 2>/dev/null || true
        wait "$heartbeat_pid" 2>/dev/null || true
        heartbeat_pid=""
    fi
}

cleanup() {
    stop_heartbeat
    rm -f "$log" "/tmp/ci-run-distilled.$$"
}
trap cleanup EXIT

echo "==> ${label}: $*"
start=$(date +%s)
# Forgejo's runner terminates a step after 15 minutes without output. Cargo can
# legitimately be silent that long while linking a large workspace target, so
# emit one compact progress line per minute until the command exits.
(
    while sleep 60; do
        echo "==> ${label}: still running ($(( $(date +%s) - start ))s)"
    done
) &
heartbeat_pid=$!
# Stream and capture. PIPESTATUS[0] is the command's status, not tee's.
"$@" 2>&1 | tee "$log"
rc="${PIPESTATUS[0]}"
stop_heartbeat
elapsed=$(( $(date +%s) - start ))

if [ "$rc" -eq 0 ]; then
    echo "==> ${label}: OK in ${elapsed}s"
    [ -n "${GITHUB_STEP_SUMMARY:-}" ] && echo "- **${label}** — passed in ${elapsed}s" >> "$GITHUB_STEP_SUMMARY"
    exit 0
fi

# ---- failed: distil the reason ------------------------------------------
# rustc diagnostics, build-script failures, panics and the test harness's own
# verdict. `error:` also catches "error: could not compile", which names the
# failed crate and is usually the most useful line in the log.
pattern='^error(\[E[0-9]+\])?:|^error: |could not compile|^thread .* panicked at|^test result: FAILED|^---- .* stdout ----|The system library .* was not found|^failures:$'
distilled="$(grep -nE "$pattern" "$log" | head -40)"
# Annotations get the lines that identify the fault, not the structural markers a
# test harness prints around it ("failures:", "---- x stdout ----"). Those stay
# in the block below, where their surrounding context makes them worth reading.
headline='^error(\[E[0-9]+\])?:|^error: |could not compile|^thread .* panicked at|^test result: FAILED|The system library .* was not found'
# Test names from the harness's trailing `failures:` list, if there is one.
failed_tests="$(awk '/^failures:$/{f=1;next} /^test result:/{f=0} f && /^ /{print}' "$log" | sort -u | head -20)"

{
    echo ""
    echo "############################################################"
    echo "## FAILED: ${label} (exit ${rc}, after ${elapsed}s)"
    echo "############################################################"
    if [ -n "$distilled" ]; then
        echo "## First diagnostics (line number in this step's log):"
        echo "$distilled" | sed 's/^/##   /'
    else
        echo "## No recognised diagnostic matched; last 30 lines:"
        tail -30 "$log" | sed 's/^/##   /'
    fi
    if [ -n "$failed_tests" ]; then
        echo "## Failing tests:"
        echo "$failed_tests" | sed 's/^/##   /'
    fi
    echo "############################################################"
} | tee /tmp/ci-run-distilled.$$

# Annotations: the forge shows these against the run itself.
annotations="$(grep -E "$headline" "$log" | head -5)"
if [ -n "$annotations" ]; then
    echo "$annotations" | while IFS= read -r line; do
        echo "::error::${label}: ${line}"
    done
else
    echo "::error::${label} failed with exit ${rc} (no diagnostic matched; see step log)"
fi

# Step summary: rendered above the logs on the run page.
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    {
        echo "### ❌ ${label} failed (exit ${rc}, ${elapsed}s)"
        echo ""
        echo '```'
        if [ -n "$distilled" ]; then echo "$distilled" | cut -d: -f2- | head -25
        else tail -25 "$log"; fi
        echo '```'
        if [ -n "$failed_tests" ]; then
            echo ""
            echo "Failing tests:"
            echo '```'
            echo "$failed_tests"
            echo '```'
        fi
    } >> "$GITHUB_STEP_SUMMARY"
fi

exit "$rc"
