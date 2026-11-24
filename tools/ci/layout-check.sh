#!/usr/bin/env bash
# Lay every view of the tilolang apps out headlessly at one standard window size, on the build
# server: what `tilo-layout-check` finds is the check's verdict, and the size's report, the
# layout of each frame a finding came from (dump.txt) and PNG captures (one per window, in its
# light and dark theme) are its outputs. It runs the command as `tilo-layout-check`
# (tools/layout-check), which stages the Mail and Tube apps over the sample data their outside
# services hold, as the published `tilolang` cannot.
#
#   tools/ci/layout-check.sh 390x844
#
# The `layout-check/<size>` checks run it and take inputs, which narrow a run to what is being
# looked at:
#   app     one package of `apps/tl` to lay out (default: every one)
#   press   a control's words, pressed before each window is read (default: none)
#   dump    `true` prints every frame laid out at the size, not only the frames a finding came
#           from (default: false)
#   shot_height  takes the shots in a window this tall at the size's width, so the lower part
#           of a long page shows (default: the size's own height)
#   hover   `X,Y`: the pointer rests there once the press is made, so the shots show what
#           pointing there shows, a chart's readout (default: nowhere)
#   state   a view state's value before anyone picks, as the app writes it
#           (`picked=studio`), so a screen behind a pick is the one shot (default: the view's own)
#
#   t test layout-check/390x844 --input app=apps/tl/tasks \
#       --input press="Mark Buy oat milk and eggs done" --input dump=true
#
# `t test layout-check` runs all three sizes and `t outputs layout-check/<size> --to DIR` fetches a
# run's report, dump and shots. Never run it on a workstation: it lays out every app, which takes
# the machine.
set -euo pipefail
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
cd -- "$repo"

size=${1:?usage: $0 WIDTHxHEIGHT, such as 390x844}

# The build gets the caller's environment and the inputs every job's build shares
# (tools/test-env.sh --build); the layout that follows gets the test environment (the product's
# fonts, lavapipe, a fresh HOME). A variable in a build's environment keys every build script's
# and proc macro's cache entry by it, and the test environment's differ per job, so a build that
# carried one would share no cache with any other job's. TILO_BUILT_FOR_TEST_ENV marks the pass
# the build made.
if [[ -z ${TILO_BUILT_FOR_TEST_ENV:-} ]]; then
    bash "$repo/tools/test-env.sh" --build cargo build --locked -p tilo-layout-check --bin tilo-layout-check
    TILO_BUILT_FOR_TEST_ENV=1 \
        exec bash "$repo/tools/test-env.sh" bash "$repo/tools/ci/layout-check.sh" "$@"
fi

target=${CARGO_TARGET_DIR:-target}
out=$target/layout-check/$size
rm -rf -- "$out"
mkdir -p -- "$out"

app=${T_INPUT_APP:-}
press=${T_INPUT_PRESS:-}
dump=${T_INPUT_DUMP:-false}
shot_height=${T_INPUT_SHOT_HEIGHT:-}
hover=${T_INPUT_HOVER:-}
state=${T_INPUT_STATE:-}
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
tall=()
[[ -z $shot_height ]] || tall=(--shot-height "$shot_height")
pointing=()
[[ -z $hover ]] || pointing=(--hover "$hover")
states=()
[[ -z $state ]] || states=(--state "$state")
# A narrowing input reads its own layouts: `dump` prints every frame laid out, where the whole
# tree's run prints the frames its findings came from.
read_frames=(--dump-findings "$size")
[[ $dump == false ]] || read_frames=(--dump "$size")

# Strict judges the whole tree (an allow-list line whose finding no longer shows, a view no
# scenario reaches), so it is the run that names no app and no press; a narrowed run still fails
# on a finding the allow-list does not name.
verdict=(--strict)
[[ -z $app && -z $press && -z $hover ]] || verdict=()

status=0
"$target/debug/tilo-layout-check" "${apps[@]}" \
    --allow tools/layout-check/tests/layout_check/allowed \
    "${verdict[@]}" "${read_frames[@]}" "${presses[@]}" "${pointing[@]}" "${tall[@]}" --shots "$out" >"$out/report.txt" 2>"$out/dump.txt" || status=$?
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
