#!/usr/bin/env bash
# Run the declarative security-boundary invariant suite used by affected CI.
set -euo pipefail

repo="${TILO_REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
manifest="${TILO_SECURITY_INVARIANT_MANIFEST:-tools/security-boundary-invariants.txt}"
cd "$repo"

if [[ ! -f "$manifest" ]]; then
    echo "security invariant manifest not found: $manifest" >&2
    exit 2
fi

mapfile -t invariants < <(sed -e 's/[[:space:]]*$//' -e '/^[[:space:]]*#/d' -e '/^[[:space:]]*$/d' "$manifest")
if [[ ${#invariants[@]} -eq 0 ]]; then
    echo "security invariant manifest is empty: $manifest" >&2
    exit 2
fi

declare -A seen=()
for invariant in "${invariants[@]}"; do
    if [[ "$invariant" = /* || "$invariant" == *".."* || "$invariant" =~ [[:space:]] ]]; then
        echo "invalid security invariant path: $invariant" >&2
        exit 2
    fi
    if [[ -n ${seen[$invariant]:-} ]]; then
        echo "duplicate security invariant: $invariant" >&2
        exit 2
    fi
    seen[$invariant]=1
    if [[ ! -f "$invariant" ]]; then
        echo "security invariant does not exist: $invariant" >&2
        exit 2
    fi
    case "$invariant" in
        *.py|*.sh) ;;
        *)
            echo "unsupported security invariant type: $invariant" >&2
            exit 2
            ;;
    esac
done

if [[ ${1:-} == --list ]]; then
    printf '%s\n' "${invariants[@]}"
    exit 0
fi
if [[ $# -ne 0 ]]; then
    echo "usage: tools/run-security-boundary-invariants.sh [--list]" >&2
    exit 2
fi

# Every invariant runs, a failed one too, and the end reports each as libtest reports a test
# (`test <path> ... ok|FAILED`, a failed one's output under `---- <path> stdout ----`): t reads
# those lines, names the failed invariants in the run's result and leads the check's log with
# their output, and compares each with main's.
total=${#invariants[@]}
outputs=$(mktemp -d)
trap 'rm -rf "$outputs"' EXIT
failed=()
for index in "${!invariants[@]}"; do
    invariant=${invariants[$index]}
    printf '==> security invariant %d/%d: %s\n' "$((index + 1))" "$total" "$invariant"
    out="$outputs/$index"
    status=0
    case "$invariant" in
        *.py) python3 "$invariant" ;;
        *.sh) bash "$invariant" ;;
    esac > >(tee "$out") 2>&1 || status=$?
    wait $! 2>/dev/null || true
    if [[ $status -ne 0 ]]; then
        printf '%s exited with %d\n' "$invariant" "$status" | tee -a "$out"
        failed+=("$index")
    fi
done
echo
for index in "${!invariants[@]}"; do
    if [[ " ${failed[*]} " == *" $index "* ]]; then
        printf 'test %s ... FAILED\n' "${invariants[$index]}"
    else
        printf 'test %s ... ok\n' "${invariants[$index]}"
    fi
done
if [[ ${#failed[@]} -ne 0 ]]; then
    printf '\nfailures:\n\n'
    for index in "${failed[@]}"; do
        printf -- '---- %s stdout ----\n' "${invariants[$index]}"
        # t ends a test's output at a rule of dashes.
        grep -v -E '^[-=]{3,}$' "$outputs/$index" || true
        echo
    done
    printf 'test result: FAILED. %d passed; %d failed\n' "$((total - ${#failed[@]}))" "${#failed[@]}"
    exit 1
fi
printf 'test result: ok. %d passed; 0 failed\n' "$total"
printf 'security boundary invariants OK (%d checks)\n' "$total"
