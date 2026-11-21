#!/usr/bin/env bash
# A staged update becomes the plan the next boot follows, without waiting for a
# clean shutdown.
#
#   ./os/tests/deployment_finalize.sh
#
# bootc turns a staged composefs deployment into the boot plan only at shutdown
# (`bootc-finalize-staged.service`'s ExecStop). A machine that is switched off
# instead of shut down never gets there, so the update it staged is never booted;
# that is how a headless board stays on its old image for weeks. So Tilo
# finalizes right after staging, and at shutdown again, where finalizing merges
# the staged deployment's /etc once more: whatever /etc became after staging is
# what the next boot has. The production `tilo-deployment finalize` is called
# here against a stand-in ESP and bootc, and each case asserts what it asks bootc to
# do, what it leaves alone, and what the update pass does around it.
set -uo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
deployment="$repo/os/usr/libexec/tilo-deployment"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
fails=0
ok()  { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*"; fails=$((fails + 1)); }

running=$(printf 'a%.0s' {1..128})
staged=$(printf 'c%.0s' {1..128})

entry() {  # FILE DIGEST SORT-KEY
  printf 'title Tilo 44\nversion 44\nlinux /EFI/Linux/bootc_composefs-%s/vmlinuz\ninitrd /EFI/Linux/bootc_composefs-%s/initrd\noptions root=tilo-auto rw composefs=%s\nsort-key %s\n' \
    "$2" "$2" "$2" "$3" >"$1"
}

install -d "$work/bin"
# bootc: a status that reports the staged deployment when the fixture has one,
# and a finalize that does what Tilo's bootc does: merge the staged deployment's
# /etc (counted in merges), then exchange the entry directories while the staged
# one holds an entry.
cat >"$work/bin/bootc" <<'STUB'
#!/usr/bin/env bash
echo "bootc $*" >>"$FIXTURE/log"
case "$*" in
  "status --format json")
    staged=null
    if [ -n "$FIXTURE_STAGED" ]; then
      staged="{\"image\": {\"image\": {\"image\": \"registry.tilo.so/tilo/tilo-vm:next\"}, \"version\": \"44.20261107.0\"}, \"composefs\": {\"verity\": \"sha256:$FIXTURE_STAGED\"}}"
    fi
    printf '{"status": {"booted": {"image": {"image": {"image": "registry.tilo.so/tilo/tilo-vm:main-1a2b3c4"}, "version": "44.20261021.0"}, "composefs": {"verity": "sha256:%s"}}, "staged": %s}}\n' \
      "$RUNNING" "$staged"
    ;;
  composefs-finalize-staged)
    [ -z "${FIXTURE_NOOP:-}" ] || exit 0
    [ -z "${FIXTURE_FINALIZE_FAILS:-}" ] || exit 1
    echo etc >>"$FIXTURE/merges"
    compgen -G "$FIXTURE/esp/loader/entries.staged/*.conf" >/dev/null || exit 0
    mv "$FIXTURE/esp/loader/entries" "$FIXTURE/esp/loader/entries.prev"
    mv "$FIXTURE/esp/loader/entries.staged" "$FIXTURE/esp/loader/entries"
    rm -rf "$FIXTURE/esp/loader/entries.prev" ;;
  *) exit 2 ;;
esac
STUB
cat >"$work/bin/findmnt" <<'STUB'
#!/usr/bin/env bash
[ "$*" = "-n -o SOURCE /" ] && { echo "composefs:$RUNNING"; exit 0; }
exit 1
STUB
cat >"$work/bin/id" <<'STUB'
#!/usr/bin/env bash
[ "$1" = -u ] && { echo 0; exit 0; }
exec /usr/bin/id "$@"
STUB
# The identity tool reads fixed fields out of a bootc status snapshot.
cat >"$work/bin/tilo-image-trust" <<'STUB'
#!/usr/bin/env python3
import json, sys
selector = sys.argv[2]
if selector == "rollback-timestamp":
    value = (json.load(sys.stdin)["status"].get("rollback") or {}).get("timestamp")
else:
    deployment, _, field = selector.rpartition("-")
    value = json.load(sys.stdin)["status"].get(deployment) or {}
    if field == "image":
        value = (value.get("image") or {}).get("image", {}).get("image")
    elif field == "version":
        value = (value.get("image") or {}).get("version")
if value is None:
    sys.exit(1)
print(value)
STUB
# The update pass's two delegated steps: the ESP plan, and the finalize unit.
cat >"$work/bin/record-esp-plan" <<'STUB'
#!/usr/bin/env bash
echo esp-plan >>"$FIXTURE/order"
cat "$FIXTURE/plan/plan.json" >>"$FIXTURE/plans"
STUB
cat >"$work/bin/record-finalize" <<'STUB'
#!/usr/bin/env bash
echo finalize >>"$FIXTURE/order"
STUB
chmod +x "$work/bin"/*
echo dev >"$work/flavour"

reset() {  # STAGED-DIGEST-OR-EMPTY  PLAN-DIGEST-OR-EMPTY  (the running entry is always there)
  rm -rf "$work/esp" "$work/plans" "$work/order" "$work/plan" "$work/merges"
  install -d "$work/esp/loader/entries" "$work/esp/EFI/tilo" "$work/esp/EFI/Linux"
  # The boot menu's editor is off already, so the pass stages nothing for it.
  echo 'editor no' >"$work/esp/loader/loader.conf"
  entry "$work/esp/loader/entries/bootc_bootc-44-0.conf" "$running" bootc-fedora-1
  [ -z "$1" ] || entry "$work/esp/loader/entries/bootc_bootc-44-1+3.conf" "$1" bootc-fedora-0
  if [ -n "$2" ]; then
    install -d "$work/esp/loader/entries.staged"
    entry "$work/esp/loader/entries.staged/bootc_bootc-44-0.conf" "$running" bootc-fedora-1
    entry "$work/esp/loader/entries.staged/bootc_bootc-44-1.conf" "$2" bootc-fedora-0
  fi
  : >"$work/log"
}

run() {  # ARGS...
  out="$(env -i PATH="$work/bin:/usr/bin:/bin" FIXTURE="$work" RUNNING="$running" \
    FIXTURE_STAGED="$fixture_staged" FIXTURE_FINALIZE_FAILS="$fixture_fails" \
    FIXTURE_NOOP="$fixture_noop" \
    TILO_ESP="$work/esp" TILO_ESP_CANDIDATES="$work/nowhere" \
    TILO_IMAGE_FLAVOUR_FILE="$work/flavour" TILO_ESP_PLAN_DIR="$work/plan" \
    TILO_ESP_WRITE_CMD=record-esp-plan TILO_FINALIZE_CMD=record-finalize \
    bash "$deployment" "$@" 2>&1)"
  status=$?
}

fixture_staged=""; fixture_fails=""; fixture_noop=""

# --- a staged plan becomes the boot plan --------------------------------------
reset "$staged" "$staged"
fixture_staged="$staged"
run finalize
[ "$status" = 0 ] && ok "a staged deployment is finalized" || bad "finalize failed ($status): $out"
grep -qx 'bootc composefs-finalize-staged' "$work/log" \
  && ok "it ran bootc's own finalize step" || bad "bootc's finalize was not run: $(cat "$work/log")"
[ ! -e "$work/esp/loader/entries.staged" ] \
  && ok "the staged plan is gone, so the next boot reads the new entries" || bad "entries.staged is still there"
grep -q "composefs=$staged" "$work/esp/loader/entries/bootc_bootc-44-1.conf" 2>/dev/null \
  && ok "the staged plan's entry is the boot plan now" || bad "the boot plan does not hold the staged entry"
grep -q 'tilo-vm:next 44.20261107.0 boots next' <<<"$out" \
  && ok "it says which image boots next" || bad "the report names no deployment: $out"

# A second finalize, which is what the shutdown hook makes: the computer kept changing
# /etc after the update was staged, and the deployment that boots next must have
# those changes. So bootc's finalize is called again and merges /etc again, and the
# boot plan it already made stays.
: >"$work/log"
run finalize
[ "$status" = 0 ] && ok "a deployment already finalized is finalized again" \
  || bad "the second finalize failed ($status): $out"
grep -qx 'bootc composefs-finalize-staged' "$work/log" \
  && ok "the finalize at shutdown asks bootc to merge /etc again" \
  || bad "the finalize at shutdown did not call bootc's, so /etc changed since staging is lost: $(cat "$work/log")"
[ "$(wc -l <"$work/merges" 2>/dev/null)" = 2 ] \
  && ok "so /etc was merged at staging and at shutdown" || bad "merges of /etc: $(cat "$work/merges" 2>&1)"
grep -q "composefs=$staged" "$work/esp/loader/entries/bootc_bootc-44-1.conf" 2>/dev/null \
  && [ ! -e "$work/esp/loader/entries.staged" ] \
  && ok "and the boot plan stays the staged deployment" || bad "the second finalize changed the boot plan"

# --- nothing staged -----------------------------------------------------------
reset "" ""
fixture_staged=""
run finalize
[ "$status" = 0 ] && grep -q 'no staged deployment to finalize' <<<"$out" \
  && ok "a machine with nothing staged says so" || bad "empty run: $out"
grep -q 'composefs-finalize-staged' "$work/log" \
  && bad "bootc was asked to finalize nothing: $(cat "$work/log")" \
  || ok "and does not ask bootc to finalize anything"

# --- a plan an earlier finalize already consumed ------------------------------
reset "$staged" ""
fixture_staged="$staged"
run finalize
[ "$status" = 0 ] && grep -q 'boots next, with /etc as it is now' <<<"$out" \
  && ok "a staged deployment with no staged entries is finalized" || bad "consumed plan: $out"
[ "$(cat "$work/merges" 2>/dev/null)" = etc ] \
  && ok "its /etc is merged again" || bad "its /etc was not merged: $(cat "$work/log")"
grep -q "composefs=$staged" "$work/esp/loader/entries/bootc_bootc-44-1+3.conf" 2>/dev/null \
  && ok "and its boot plan stays" || bad "the boot plan changed: $(ls "$work/esp/loader/entries")"

# --- a finalize that fails ----------------------------------------------------
reset "$staged" "$staged"
fixture_fails=1
run finalize
[ "$status" = 1 ] && grep -q 'WARNING: bootc could not finalize the staged deployment' <<<"$out" \
  && ok "a failed finalize is reported as a failure, not as success" || bad "failed finalize ($status): $out"
[ -f "$work/esp/loader/entries.staged/bootc_bootc-44-1.conf" ] \
  && ok "the staged plan is left in place for the next attempt" || bad "the failed attempt removed the plan"
fixture_fails=""
run finalize
[ "$status" = 0 ] && [ ! -e "$work/esp/loader/entries.staged" ] \
  && ok "the next attempt finalizes it" || bad "the retry did not finalize ($status): $out"

# --- bootc says success but the plan is still staged --------------------------
reset "$staged" "$staged"
fixture_noop=1
run finalize
[ "$status" = 1 ] && grep -q 'still there; the next boot tries again' <<<"$out" \
  && ok "success without an exchanged plan is not accepted" || bad "a no-op finalize was accepted ($status): $out"
fixture_noop=""

# --- the update pass finalizes after the try counter, never before ------------
reset "$staged" "$staged"
fixture_staged="$staged"
run entries
[ "$status" = 0 ] && ok "the boot-entry pass succeeds with a staged deployment" || bad "entries failed ($status): $out"
[ "$(tr '\n' ' ' <"$work/order" 2>/dev/null | sed 's/ $//')" = "esp-plan finalize" ] \
  && ok "the try counter is applied first, then the deployment is finalized" \
  || bad "wrong order of the update pass's steps: $(cat "$work/order" 2>&1)"
grep -q 'bootc_bootc-44-1+3.conf' "$work/plans" 2>/dev/null \
  && ok "the staged entry it finalized carries a try counter" || bad "no counter in the plan: $(cat "$work/plans" 2>&1)"

# Nothing to count (the staged entry already has its counter): still finalized.
reset "$staged" "$staged"
mv "$work/esp/loader/entries.staged/bootc_bootc-44-1.conf" "$work/esp/loader/entries.staged/bootc_bootc-44-1+3.conf"
run entries
[ "$status" = 0 ] && [ "$(cat "$work/order" 2>/dev/null)" = finalize ] \
  && ok "a staged entry that already has its counter is finalized without a second rename" \
  || bad "counted entry: $(cat "$work/order" 2>&1) $out"

# --- nothing staged: the update pass still runs the step, which does nothing ---
reset "" ""
fixture_staged=""
run entries
[ "$status" = 0 ] && [ "$(cat "$work/order" 2>/dev/null)" = finalize ] \
  && ok "a machine with nothing staged still runs the (idempotent) step" || bad "plain entries: $(cat "$work/order" 2>&1) $out"

[ "$fails" -eq 0 ] || { printf '\n%d check(s) failed\n' "$fails"; exit 1; }
printf '\nall checks passed\n'
