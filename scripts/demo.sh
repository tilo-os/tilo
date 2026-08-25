#!/usr/bin/env bash
# Run the demo. Rebuild only what changed, and leave nothing behind.
#
#   scripts/demo.sh              # rebuild what changed, then the drivable rig
#   scripts/demo.sh --scripted   # the unattended rig instead
#   scripts/demo.sh --no-build   # skip the rebuild entirely
#   scripts/demo.sh --no-bumper  # do not join the boot bumper afterwards
#   scripts/demo.sh -- --foo     # everything after `--` goes to the rig
#
# ---------------------------------------------------------------------
# The bumper is joined here, because a manual step is a step that is
# skipped
# ---------------------------------------------------------------------
#
# Neither rig records the boot: it comes off a QEMU capture, is cut by
# hand once per OS build, and lives at `films/qos-boot-bumper.mp4`.
# Joining it was a thing somebody was supposed to remember to run
# afterwards, which is how it went missing unnoticed once already. So
# this does it, and says so when it cannot.
#
# It is not fatal either way. The take is the expensive artefact and it
# is already on disk by the time this runs; a missing bumper costs the
# opening three seconds and nothing else.
#
# ---------------------------------------------------------------------
# Orphans are impossible here, and that is the point
# ---------------------------------------------------------------------
#
# One run starts three compositors and up to four applications each. Every
# one holds a GPU context on the same device driving the desktop you are
# sitting at. The kernel's account of what that costs when they are not
# reclaimed:
#
#   amdgpu: VM memory stats for proc shell(2523673) ... non-zero when fini
#   amdgpu: VM memory stats for proc file-explorer(2528974) ... non-zero when fini
#   amdgpu: VM memory stats for proc word-processor(2532222) ... non-zero when fini
#   NVRM: Check failed: Out of memory [NV_ERR_NO_MEMORY]
#
# Five reboots in one day came out of that.
#
# The rigs already set `die_with_parent`, and that is not enough. PDEATHSIG
# is a courtesy the kernel extends to a *direct child* when its parent
# exits; it says nothing about a grandchild, and an application started by
# the compositor is a grandchild. Nor does it help when the rig itself is
# SIGKILLed between spawning something and recording that it did.
#
# So the run happens inside a transient systemd scope. Every process it
# starts, at any depth, lands in that scope's cgroup, and when the scope
# ends systemd kills what is left in it. That is a property of the cgroup,
# not an agreement between processes: a SIGKILLed rig, a crash mid-spawn
# and a Ctrl-C all end the same way. `--collect` disposes of the unit even
# when the run fails, so a failed run leaves no residue either.
#
# The pre-flight reap in `demo-guard.sh` stays, because it cleans up what
# escaped *before* this existed. Once every run goes through here there
# should be nothing for it to find, and finding something means a path
# around this script — which is worth knowing about.

set -uo pipefail
cd "$(dirname "$0")/.." || exit 1

RIG=live
BUILD=1
BUMPER=1
RIG_ARGS=()
while [ $# -gt 0 ]; do
  case "$1" in
    --scripted) RIG=scripted; shift ;;
    --no-build) BUILD=0; shift ;;
    --no-bumper) BUMPER=0; shift ;;
    --) shift; RIG_ARGS=("$@"); break ;;
    *) RIG_ARGS+=("$1"); shift ;;
  esac
done

say() { printf '\033[1m[demo]\033[0m %s\n' "$*"; }

# 1. Nothing of ours may already hold the GPU.
bash scripts/demo-guard.sh --reap || exit 1

# 2. Rebuild only what changed. cargo decides that; both commands are
#    no-ops when nothing has. The payload script re-packs the apps, which
#    at the demo profile is tens of megabytes rather than the ~450MB a
#    debug build produces -- that difference is most of the wait.
if [ "$BUILD" -eq 1 ]; then
  say "rebuilding what changed (demo profile)"
  cargo build --profile demo -p shell -p agent -p launcher -p app-catalogue -p install \
    || { echo "[demo] compositor/tooling build failed"; exit 1; }
  bash scripts/demo-apps-live.sh \
    || { echo "[demo] app payload failed"; exit 1; }
else
  say "skipping build (--no-build)"
fi

# 3. The run itself, inside a scope that cannot leak.
case "$RIG" in
  live)     SCRIPT=scripts/yc-demo-live.py ;;
  scripted) SCRIPT=scripts/yc-demo.py ;;
esac

# Both rigs write their finished take into `films/`, under a timestamped
# name. Listing it first is how the one this run produced is identified
# afterwards without capturing the rig's transcript -- which is on screen
# while somebody is filming, and is not something to put behind a pipe.
FILMS="$PWD/films"
mkdir -p "$FILMS"
films_before="$(mktemp)"
trap 'rm -f "$films_before"' EXIT
ls -1 "$FILMS" 2>/dev/null | sort > "$films_before"

say "starting $SCRIPT in a transient scope"
systemd-run --user --scope --collect \
  --unit "qos-demo-$$" \
  --description "qOS demo run (everything it spawns dies with it)" \
  -- python3 "$SCRIPT" "${RIG_ARGS[@]}"
status=$?

# 3b. The bumper, on the front of whatever this run produced.
took="$(ls -1 "$FILMS" 2>/dev/null | sort | comm -13 "$films_before" - \
        | grep -E '\.mp4$' | grep -v -- '-with-bumper\.mp4$' | tail -1)"
if [ "$BUMPER" -eq 1 ] && [ -n "$took" ]; then
  if [ -f "$FILMS/qos-boot-bumper.mp4" ]; then
    say "joining the boot bumper on the front of $took"
    python3 scripts/demo-assemble.py \
      --out "$FILMS/${took%.mp4}-with-bumper.mp4" \
      "$FILMS/qos-boot-bumper.mp4" "$FILMS/$took" \
      || say "the join failed; the take itself is at $FILMS/$took"
  else
    say "no films/qos-boot-bumper.mp4, so $took has no boot on the front"
    say "  cut one from a QEMU capture — see docs/boot-bumper-cut.md"
  fi
elif [ "$BUMPER" -eq 1 ]; then
  say "no new file in films/, so there was nothing to put a bumper on"
fi

# 4. Say what the scope left behind. It should always be nothing; if it is
#    not, something started outside the cgroup and that is a real defect
#    rather than a stray to shrug at.
say "run exited $status — checking the machine is clean"
bash scripts/demo-guard.sh --check >/dev/null 2>&1
if [ $? -eq 0 ]; then
  say "clean: nothing of ours is still holding the GPU"
else
  echo "[demo] WARNING: processes survived the scope — this should be"
  echo "[demo] impossible. Report it; do not just reap and move on."
  bash scripts/demo-guard.sh --check
fi
exit $status
