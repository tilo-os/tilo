#!/usr/bin/env bash
# Lay every view of the tilolang apps out headlessly at one standard window size, on the build
# server: what `tilolang layout-check` finds is the check's verdict, and the size's report, the
# layout of each frame a finding came from (dump.txt) and PNG captures (one per window, in its
# light and dark theme) are its outputs.
#
#   tools/ci/layout-check.sh 390x844
#
# The `layout-check/<size>` checks run it in the test environment (fonts, lavapipe) and take
# inputs, which narrow a run to what is being looked at:
#   app     one package of `apps/tl` to lay out (default: every one)
#   press   a control's words, pressed before each window is read (default: none)
#   dump    `true` prints every frame laid out at the size, not only the frames a finding came
#           from (default: false)
#
#   t test layout-check/390x844 --input app=apps/tl/tasks \
#       --input press="Mark Buy oat milk and eggs done" --input dump=true
#
# `t test layout-check` runs all three sizes and `t outputs layout-check/<size> --to DIR` fetches a
# run's report, dump and shots. Never run it on a workstation: it lays out every app, which takes
# the machine.
set -euo pipefail

size=${1:?usage: $0 WIDTHxHEIGHT, such as 390x844}
target=${CARGO_TARGET_DIR:-target}
out=$target/layout-check/$size
rm -rf -- "$out"
mkdir -p -- "$out"

cargo build --locked -p tilolang-cli --bin tilolang

app=${T_INPUT_APP:-}
press=${T_INPUT_PRESS:-}
dump=${T_INPUT_DUMP:-false}
case $dump in true|false) ;; *) echo "dump must be true or false, not $dump" >&2; exit 2 ;; esac

# Every app in `apps/tl`: a package directory with an `app.tl`, as the library's own tests find
# them, so an app added there is laid out without being named here. `app` lays out one instead.
apps=()
if [[ -n $app ]]; then
    [[ -f ${app%/}/app.tl ]] || { echo "app $app: no such package, which is a directory with an app.tl" >&2; exit 2; }
    apps=("$app")
else
    for dir in apps/tl/*/; do
        [[ -f $dir/app.tl ]] && apps+=("$dir")
    done
fi
if [[ ${#apps[@]} -eq 0 ]]; then
    echo "layout-check: apps/tl holds no app" >&2
    exit 1
fi

presses=()
[[ -z $press ]] || presses=(--press "$press")
# A narrowing input reads its own layouts: `dump` prints every frame laid out, where the whole
# tree's run prints the frames its findings came from.
read_frames=(--dump-findings "$size")
[[ $dump == false ]] || read_frames=(--dump "$size")

# Strict judges the whole tree (an allow-list line whose finding no longer shows, a view no
# scenario reaches), so it is the run that names no app and no press; a narrowed run still fails
# on a finding the allow-list does not name.
verdict=(--strict)
[[ -z $app && -z $press ]] || verdict=()

status=0
"$target/debug/tilolang" layout-check "${apps[@]}" \
    --allow crates/lang/tilolang-cli/tests/layout_check/allowed \
    "${verdict[@]}" "${read_frames[@]}" "${presses[@]}" --shots "$out" >"$out/report.txt" 2>"$out/dump.txt" || status=$?
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
