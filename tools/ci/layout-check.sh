#!/usr/bin/env bash
# Lay every view of the tilolang apps out headlessly at one standard window size, on the build
# server: what `tilolang layout-check --strict` finds is the check's verdict, and the size's
# layout dump (dump.txt) and PNG captures (one per window, in its light and dark theme) are its
# outputs.
#
#   tools/ci/layout-check.sh 390x844
#
# The `layout-check/<size>` checks run it in the test environment (fonts, lavapipe); `t test
# layout-check` runs all three sizes and `t outputs layout-check/<size> --to DIR` fetches a run's
# dump and shots. Never run it on a workstation: it lays out every app, which takes the machine.
set -euo pipefail

size=${1:?usage: $0 WIDTHxHEIGHT, such as 390x844}
target=${CARGO_TARGET_DIR:-target}
out=$target/layout-check/$size
rm -rf -- "$out"
mkdir -p -- "$out"

cargo build --locked -p tilolang-cli --bin tilolang

# Every app in `apps/tl`: a package directory with an `app.tl`, as the library's own tests find
# them, so an app added there is laid out without being named here.
apps=()
for dir in apps/tl/*/; do
    [[ -f $dir/app.tl ]] && apps+=("$dir")
done
if [[ ${#apps[@]} -eq 0 ]]; then
    echo "layout-check: apps/tl holds no app" >&2
    exit 1
fi

status=0
"$target/debug/tilolang" layout-check "${apps[@]}" \
    --allow crates/lang/tilolang-cli/tests/layout_check/allowed \
    --strict --dump "$size" --shots "$out" >"$out/report.txt" 2>"$out/dump.txt" || status=$?
cat -- "$out/report.txt"
if [[ ! -s $out/report.txt ]]; then
    echo "layout-check: laying the apps out at $size wrote no report; it said:" >&2
    tail -n 20 -- "$out/dump.txt" >&2
    exit 1
fi
if ! compgen -G "$out/*.png" >/dev/null; then
    echo "layout-check: no window was shot at $size" >&2
    status=1
fi
exit "$status"
