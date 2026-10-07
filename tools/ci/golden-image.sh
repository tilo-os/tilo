#!/usr/bin/env bash
# The builder image the visual goldens were rendered on.
#
#   golden-image.sh check IMAGE    succeed when IMAGE is that image; otherwise
#                                  say so and exit 3
#   golden-image.sh record IMAGE   make IMAGE the goldens' image
#
# The visual-baselines workflow records the image it rendered on next to the
# candidates, so a refresh commits both together.
set -euo pipefail
record="$(cd "$(dirname "$0")/../.." && pwd)/.config/visual-goldens-image"
[[ $# == 2 ]] || { echo "usage: $0 check|record IMAGE" >&2; exit 2; }
case "$1" in
check)
    recorded=$(<"$record")
    [[ $2 == "$recorded" ]] && exit 0
    printf '%s\n' \
        "The visual goldens were rendered on $recorded," \
        "but this job runs on $2." \
        "Refresh them: run Dev / visual baselines on a branch and commit its candidates." >&2
    exit 3
    ;;
record) printf '%s\n' "$2" > "$record" ;;
*) echo "usage: $0 check|record IMAGE" >&2; exit 2 ;;
esac
