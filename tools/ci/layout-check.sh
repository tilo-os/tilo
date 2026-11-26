#!/usr/bin/env bash
# Lay every view of the tilolang apps out headlessly at the standard window sizes, on the build
# server: what `tilo-layout-check` finds is the check's verdict, and each size's report, the
# layout of each frame a finding came from (dump.txt) and, when a run asks for them, PNG captures
# (one per window, in its light and dark theme) are its outputs. It runs the command as
# `tilo-layout-check` (tools/layout-check), which stages the Mail and Tube apps over the sample
# data their outside services hold, as the published `tilolang` cannot.
#
#   tools/ci/layout-check.sh [WIDTHxHEIGHT]
#
# One pass lays every window out once, and the standard sizes are the frames it reports and the
# windows it shoots: 320x568 and 390x844, a phone's, and 1280x800, a laptop's. The check's own
# verdict reads every view at every size from 320x568 to 3440x1440, so a size given here reports
# that one's frames alone, never a second pass over the apps.
#
# The pictures cost more than the verdict does: each is a software rasterisation of a whole
# window, nine hundred of them to a size, so they are taken only when a run asks for them
# (`shots=true`) rather than on every head. The verdict, the frames its findings came from and
# the counts are always written.
#
# The `layout-check` check runs it and takes inputs, which narrow a run to what is being looked
# at. While iterating, name the one app you are looking at (`t test layout-check --input
# app=apps/tl/tasks`): a run over every app of `apps/tl` takes the machine, and the log says how
# many core-hours it spent (`used` in `t follow --json`).
#   app     one package of `apps/tl` to lay out (default: every one)
#   size    the standard size to report and shoot (default: all three)
#   shots   `true` writes the window at each reported size as each stage leaves it, light and
#           dark, to PNGs in its own directory (default: false: the verdict's frames alone)
#   press   a control's words, pressed before each window is read at the first reported size,
#           where the shots show it (default: none); name a size beside it to press elsewhere
#   dump    `true` prints every frame laid out at the reported sizes, not only the frames a
#           finding came from (default: false)
#   shot_height  takes the shots in a window this tall at the size's width, so the lower part
#           of a long page shows (default: the size's own height)
#   hover   `X,Y`: the pointer rests there once the press is made, so the shots show what
#           pointing there shows, a chart's readout (default: nowhere)
#   state   a view state's value before anyone picks, as the app writes it
#           (`picked=studio`), so a screen behind a pick is the one shot (default: the view's own)
#
#   t test layout-check --input app=apps/tl/tasks --input size=390x844 --input shots=true \
#       --input press="Mark Buy oat milk and eggs done" --input dump=true
#
# `t test layout-check` runs it over every app, and `t outputs layout-check --to DIR` fetches a
# run's report and dumps (and its shots, from a run that asked for them). Never run it on a
# workstation: it lays out every app, which takes the machine.
set -euo pipefail
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
cd -- "$repo"

# The standard sizes, unless one is named: the phones' and the laptop's.
sizes=()
if [[ -n ${1:-} ]]; then
    sizes=("$1")
elif [[ -n ${T_INPUT_SIZE:-} ]]; then
    sizes=("$T_INPUT_SIZE")
else
    sizes=(320x568 390x844 1280x800)
fi

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
out=$target/layout-check
rm -rf -- "$out"
mkdir -p -- "$out"

app=${T_INPUT_APP:-}
press=${T_INPUT_PRESS:-}
dump=${T_INPUT_DUMP:-false}
shots=${T_INPUT_SHOTS:-false}
shot_height=${T_INPUT_SHOT_HEIGHT:-}
hover=${T_INPUT_HOVER:-}
state=${T_INPUT_STATE:-}
case $dump in true|false) ;; *) echo "dump must be true or false, not $dump" >&2; exit 2 ;; esac
case $shots in true|false) ;; *) echo "shots must be true or false, not $shots" >&2; exit 2 ;; esac

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
# tree's run prints the frames its findings came from. Each reported size keeps its own frames and,
# when the run asks for them, its own shots.
frames=()
for size in "${sizes[@]}"; do
    if [[ $dump == false ]]; then
        frames+=(--dump-findings "$size")
    else
        frames+=(--dump "$size")
    fi
    if [[ $shots == true ]]; then
        mkdir -p -- "$out/$size"
        frames+=(--shots "$out/$size")
    fi
done

# Strict judges the whole tree (an allow-list line whose finding no longer shows, a view no
# scenario reaches), so it is the run that names no app and no press; a narrowed run still fails
# on a finding the allow-list does not name.
verdict=(--strict)
[[ -z $app && -z $press && -z $hover ]] || verdict=()

status=0
"$target/debug/tilo-layout-check" "${apps[@]}" \
    --allow tools/layout-check/tests/layout_check/allowed \
    "${verdict[@]}" "${frames[@]}" "${presses[@]}" "${pointing[@]}" "${tall[@]}" \
    >"$out/report.txt" 2>"$out/dump.txt" || status=$?
cat -- "$out/report.txt"
if [[ ! -s $out/report.txt ]]; then
    echo "layout-check: laying the apps out at ${sizes[*]} wrote no report; it said:" >&2
    tail -n 20 -- "$out/dump.txt" >&2
    exit 1
fi
if [[ $shots == true ]]; then
    for size in "${sizes[@]}"; do
        if ! compgen -G "$out/$size/*.png" >/dev/null; then
            echo "layout-check: no window was shot at $size" >&2
            status=1
        fi
    done
fi
exit "$status"
