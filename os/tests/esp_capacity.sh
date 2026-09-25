#!/usr/bin/env bash
# Assert that the disk layout can hold the rollback retention window.
#
#   ./os/tests/esp_capacity.sh
#
# Two settings that had no knowledge of each other:
#
#   * tilo-rollback keeps a sealed LUKS keyslot for the last `retained_generations`
#     generations (default 3), plus up to RETAINED_NEWER_GENERATIONS (2) staged
#     ahead of the running one. Every one of those has to be *bootable*, so every
#     one of them needs its kernel on disk.
#   * os/dev/disk-image.sh sizes the ESP. On the sealed layout the ESP is /boot and
#     each generation's kernel there is a ~134 MB UKI, so the ESP is what bounds
#     how many generations can exist at once — 2 GiB holds five comfortably, and
#     raising the retention window past what fits would overflow it silently, at
#     update time, on a machine in the field.
#
# So this asserts the arithmetic against the *actual defaults on both sides*,
# reading each from its own source rather than restating it. Raising
# retained_generations without raising --esp fails here.
#
# Exit codes: 0 all assertions hold; 1 at least one failed.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
DISK_IMAGE="$REPO/os/dev/disk-image.sh"
ROLLBACK_LIB="$REPO/crates/system/tilo-rollback/src/lib.rs"
RETENTION="$REPO/crates/system/tilo-rollback/src/retention.rs"

fail=0
ok()  { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

for f in "$DISK_IMAGE" "$ROLLBACK_LIB" "$RETENTION"; do
  [ -r "$f" ] || { bad "missing $f"; exit 1; }
done

# --- the two sides, each read from where it is defined ------------------------
retain="$(sed -n 's/^ *retained_generations: \([0-9]\+\),$/\1/p' "$ROLLBACK_LIB" | head -1)"
newer="$(sed -n 's/^pub const RETAINED_NEWER_GENERATIONS: usize = \([0-9]\+\);$/\1/p' "$RETENTION" | head -1)"
esp="$(sed -n 's/^COMPOSEFS=auto;.*; ESP_SIZE=\([0-9]\+\)G$/\1/p' "$DISK_IMAGE" | head -1)"
budget="$(sed -n 's/^RETAIN=.*UKI_BUDGET=\([0-9]\+\)M$/\1/p' "$DISK_IMAGE" | head -1)"
script_retain="$(sed -n 's/^RETAIN="\${TILO_RETAINED_GENERATIONS:-\([0-9]\+\)}".*$/\1/p' "$DISK_IMAGE" | head -1)"
slack="$(sed -n 's/^KERNEL_FS_SLACK_MIB=\([0-9]\+\)$/\1/p' "$DISK_IMAGE" | head -1)"
script_newer="$(sed -n 's/^RETAINED_NEWER=\([0-9]\+\)$/\1/p' "$DISK_IMAGE" | head -1)"
repair="$(sed -n 's/^REPAIR_UKI=\([0-9]\+\)$/\1/p' "$DISK_IMAGE" | head -1)"

for pair in "retained_generations:$retain" "RETAINED_NEWER_GENERATIONS:$newer" \
            "ESP_SIZE:$esp" "UKI_BUDGET:$budget" "RETAIN:$script_retain" \
            "KERNEL_FS_SLACK_MIB:$slack" "RETAINED_NEWER:$script_newer" \
            "REPAIR_UKI:$repair"; do
  [ -n "${pair#*:}" ] || bad "could not read ${pair%%:*} — the declaration it is parsed out of moved"
done
[ "$fail" = 0 ] || exit 1

# --- the defaults have to agree ----------------------------------------------
if [ "$script_retain" = "$retain" ]; then
  ok "disk-image.sh --retain default ($script_retain) matches tilo-rollback retained_generations"
else
  bad "disk-image.sh --retain defaults to $script_retain but tilo-rollback retains $retain generations"
fi
if [ "$script_newer" = "$newer" ]; then
  ok "disk-image.sh RETAINED_NEWER ($script_newer) matches RETAINED_NEWER_GENERATIONS"
else
  bad "disk-image.sh RETAINED_NEWER is $script_newer but retention.rs says $newer"
fi

# --- the repair image is on the same partition and has to be counted ---------
# It is derived on the machine from the running deployment's UKI rather than
# shipped, so there is exactly one at a time — but it is the same ~134 MB, it
# lands on the ESP, and it is created at UPDATE time on a machine in the field.
# An ESP sized without it overflows there, which is the worst possible moment.
if [ "$repair" -ge 1 ]; then
  ok "disk-image.sh budgets $repair repair image on the ESP"
else
  bad "disk-image.sh REPAIR_UKI is $repair — the repair-mode entry
      (docs/security/repair-mode.md) puts a UKI-sized file on the ESP, so a budget
      of zero would overflow it the first time a repair entry is made."
fi

# --- and the ESP has to hold that many kernels -------------------------------
esp_mib=$(( esp * 1024 ))
need=$(( (retain + newer + repair) * budget + slack ))
if [ "$esp_mib" -ge "$need" ]; then
  ok "ESP ${esp}G (${esp_mib} MiB) holds $(( retain + newer + repair )) x ${budget} MiB + ${slack} MiB = ${need} MiB"
else
  bad "ESP ${esp}G (${esp_mib} MiB) cannot hold $(( retain + newer + repair )) x ${budget} MiB + ${slack} MiB = ${need} MiB
      Raise ESP_SIZE in os/dev/disk-image.sh, or lower retained_generations in
      crates/system/tilo-rollback/src/lib.rs. Highest window this ESP supports:
      $(( (esp_mib - slack) / budget - newer - repair ))."
fi

# --- and the check must actually be wired into the script --------------------
if grep -q 'kernels_have" -lt "\$kernels_need' "$DISK_IMAGE"; then
  ok "disk-image.sh enforces the arithmetic at build time, not only here"
else
  bad "disk-image.sh no longer checks kernels_have against kernels_need — a raised
      retention window would reach a build without being caught"
fi

exit "$fail"
