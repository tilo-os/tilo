#!/usr/bin/env bash
# Run the demo. Rebuild only what changed, and leave nothing behind.
#
#   scripts/demo.sh              # rebuild what changed, then the drivable rig
#   scripts/demo.sh --scripted   # the unattended rig instead
#   scripts/demo.sh --no-build   # skip the rebuild entirely
#   scripts/demo.sh -- --foo     # everything after `--` goes to the rig
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
RIG_ARGS=()
while [ $# -gt 0 ]; do
  case "$1" in
    --scripted) RIG=scripted; shift ;;
    --no-build) BUILD=0; shift ;;
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

say "starting $SCRIPT in a transient scope"
systemd-run --user --scope --collect \
  --unit "qos-demo-$$" \
  --description "qOS demo run (everything it spawns dies with it)" \
  -- python3 "$SCRIPT" "${RIG_ARGS[@]}"
status=$?

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
