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

total=${#invariants[@]}
for index in "${!invariants[@]}"; do
    invariant=${invariants[$index]}
    printf '==> security invariant %d/%d: %s\n' "$((index + 1))" "$total" "$invariant"
    case "$invariant" in
        *.py) python3 "$invariant" ;;
        *.sh) bash "$invariant" ;;
    esac
done
printf 'security boundary invariants OK (%d checks)\n' "$total"
