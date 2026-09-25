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
    rm -f "$log" "$log.memory-before" "/tmp/ci-run-distilled.$$"
}
trap cleanup EXIT

echo "==> ${label}: $*"
start=$(date +%s)
progress_script="$(dirname "${BASH_SOURCE[0]}")/../tools/ci/process_progress.py"
memory_script="$(dirname "${BASH_SOURCE[0]}")/../tools/ci/cgroup_memory.py"
# Capture before running the command; only print memory evidence on failure.
python3 "$memory_script" > "$log.memory-before" 2>/dev/null || true
# Forgejo's runner terminates a step after 15 minutes without output. Cargo can
# legitimately be silent that long while linking a large workspace target, so
# emit one compact progress line per minute until the command exits.
(
    # A foreground sleep survives killing this subshell and keeps the runner's
    # output pipe open for the rest of the minute. Wait on an owned child so a
    # signal interrupts the wait, then terminate and reap that child on exit.
    heartbeat_child=""
    # Query this subshell's own jobs: TERM can arrive after launching a child
    # but before assigning its PID below. Never signal unrelated descendants.
    trap 'for child in $(jobs -pr); do
        kill "$child" 2>/dev/null || true
    done
    wait 2>/dev/null || true' EXIT
    trap 'exit 0' TERM INT
    while true; do
        sleep 60 &
        heartbeat_child=$!
        wait "$heartbeat_child" || exit 0
        heartbeat_child=""
        echo "==> ${label}: still running ($(( $(date +%s) - start ))s)"
        # A wrapper heartbeat alone cannot distinguish work from a stuck child.
        # Numeric descendant CPU/RSS/I/O counters let successive receipts show
        # progress without exposing process arguments or environment secrets.
        timeout 5s python3 "$progress_script" --root-pid "$$" 2>/dev/null &
        heartbeat_child=$!
        wait "$heartbeat_child" || echo 'CI_PROCESS_PROGRESS=unavailable'
        heartbeat_child=""
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

# Best-effort evidence only: never replace the command's captured exit status.
python3 "$memory_script" "$log.memory-before" 2>/dev/null || echo 'CI_CGROUP_MEMORY=unavailable'

# ---- failed: distil the reason ------------------------------------------
# rustc diagnostics, build-script failures, panics and the test harness's own
# verdict. `error:` also catches "error: could not compile", which names the
# failed crate and is usually the most useful line in the log.
pattern='^error(\[E[0-9]+\])?:|^error: |could not compile|^thread .* panicked at|^test result: FAILED|^---- .* stdout ----|The system library .* was not found|^failures:$'
distilled="$(grep -nE "$pattern" "$log" | head -40)"
# clippy-diff deliberately prints newly introduced lints as warnings: Cargo ran
# successfully, but the diagnostic ratchet rejected the new sites. Preserve the
# warning, source location and lint name when that explicit verdict is present.
if [ -z "$distilled" ] && grep -qE '^introduced [0-9]+ Clippy diagnostic site\(s\):$' "$log"; then
    clippy_pattern='^introduced [0-9]+ Clippy diagnostic site\(s\):$|^warning: |^[[:space:]]+--> .*:[0-9]+:[0-9]+$|^[[:space:]]+= (help|note):.*(rust-clippy|clippy::)'
    distilled="$(grep -nE "$clippy_pattern" "$log" | head -40)"
fi
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
if [ -z "$annotations" ] && grep -qE '^introduced [0-9]+ Clippy diagnostic site\(s\):$' "$log"; then
    annotations="$(grep -E '^warning: ' "$log" | head -5)"
fi
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
