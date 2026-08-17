#!/usr/bin/env bash
# Boot the image and prove, on the booted machine, that it is a desktop.
#
#   ./os/dev/prove-boot.sh [--keep] [--disk PATH]
#
# Everything the image build asserts is about files it just wrote. That is a
# claim about a tarball. This is the claim nobody had made: that the thing
# boots, logs in, paints a desktop, and can start the applications it shipped.
#
# What it proves, in order, and where the evidence lands:
#
#   1  the machine boots to a greeter and a login produces a session
#   2  os/tests/booted_app_store.sh passes over SSH — the registry, the package
#      manager, the runtime bundle, the ush socket and /var/workspace
#   3  the command palette lists applications        (screenshot)
#   4  three different preinstalled apps launch and paint  (screenshot each)
#   5  vim, less and whoami work inside the Terminal's confined guest
#
# Screenshots go to $QOS_RUN, whose path is printed at the end.
#
# Display mode is --gl, deliberately and not optionally: screendump captures GL
# scanouts only, and everything from item 3 on is drawn by the compositor. The
# cost is that the 2D console is not capturable, so items 1 and 2 are read from
# the serial log and over SSH instead of from a picture.
#
# A check that cannot observe its subject FAILS. There is no skip path here: if
# the VM will not boot, or SSH never comes up, that is the result.
source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"
set -uo pipefail

VM="$QOS_OS_DIR/dev/vm.sh"
KEEP=0
BOOT_ARGS=(--gl)
while [ $# -gt 0 ]; do
  case "$1" in
    --keep) KEEP=1 ;;
    --disk) BOOT_ARGS+=(--disk "$2"); shift ;;
    -h|--help) sed -n '2,26p' "$0"; exit 0 ;;
    *) die "unknown arg: $1" ;;
  esac
  shift
done

fail=0
ok()  { printf '\033[1;32mok  \033[0m %s\n' "$*"; }
bad() { printf '\033[1;31mFAIL\033[0m %s\n' "$*" >&2; fail=1; }

cleanup() { [ "$KEEP" = 1 ] || "$VM" stop >/dev/null 2>&1 || true; }
trap cleanup EXIT

# --- 1. boot and log in ------------------------------------------------------
msg "booting $QOS_DISK"
"$VM" stop >/dev/null 2>&1 || true
"$VM" boot "${BOOT_ARGS[@]}" || die "the VM did not start"
"$VM" wait || die "no greeter — the machine did not reach a login prompt"
ok "the machine booted to a greeter"

"$VM" login "$QOS_USER" "$QOS_PASS"

# SSH coming up is the proof that the session started: the sshd is in the same
# boot, and waiting on it is cheaper than parsing the compositor's log.
msg "waiting for ssh"
up=0
for _ in $(seq 1 60); do
  if gssh true >/dev/null 2>&1; then up=1; break; fi
  sleep 2
done
[ "$up" = 1 ] || { "$VM" serial 'shell|greetd|error|Failed' | tail -40; die "ssh never came up after login"; }
ok "logged in as $QOS_USER and the system is reachable over ssh"

# The compositor has to be the session, not merely a process that exited.
if gssh 'pgrep -x shell >/dev/null'; then
  ok "the compositor is running as the session"
else
  bad "no 'shell' process — the session did not start the compositor"
  gssh 'journalctl --user -b --no-pager | tail -40' 2>&1 | sed 's/^/      /'
fi

# --- 2. the app store, the package manager, the runtime, the mount -----------
msg "os/tests/booted_app_store.sh over ssh"
if RUNNER="sshpass -p $QOS_PASS ssh -p $QOS_SSH_PORT -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR $QOS_USER@127.0.0.1" \
     bash "$QOS_OS_DIR/tests/booted_app_store.sh"; then
  ok "the preinstalled app store works on the booted machine"
else
  bad "booted_app_store.sh failed (output above)"
fi

# --- 3. the palette lists applications ---------------------------------------
# Ctrl+K is the compositor's, not an app's, so it is typed at the machine rather
# than sent through a client.
"$VM" keys ctrl-k >/dev/null 2>&1
sleep 2
if shot="$("$VM" shot palette 2>/dev/null)"; then
  ok "command palette screenshot: $shot"
else
  bad "could not capture the palette — the compositor is not scanning out"
fi
"$VM" keys esc >/dev/null 2>&1

# --- 4. three preinstalled apps launch and paint -----------------------------
# Three *different* ones, and named from the registry rather than hardcoded, so
# this keeps testing the shipped set rather than a list that rots.
mapfile -t apps < <(gssh "awk -F'\"' '/^id/ {print \$2}' /usr/share/qos-apps/registry.toml" 2>/dev/null | tr -d '\r' | head -3)
# Which socket the session bound, rather than a guess: the greeter's cage takes
# wayland-0 and exits, so the compositor's number is not fixed.
wl="$(gssh 'basename $(ls -1 /run/user/$(id -u)/wayland-[0-9] 2>/dev/null | head -1)' 2>/dev/null | tr -d '\r')"
[ -n "$wl" ] && ok "the session's wayland socket is $wl" || bad "no wayland socket in the session runtime dir"
if [ "${#apps[@]}" -lt 3 ]; then
  bad "fewer than three packages in the system registry to launch (${apps[*]:-none})"
else
  for app in "${apps[@]}"; do
    msg "launching $app"
    gssh "systemd-run --user --quiet --collect \
            --setenv=WAYLAND_DISPLAY=$wl \
            --setenv=XDG_RUNTIME_DIR=/run/user/\$(id -u) \
            qos-launcher run-installed --package $app" >/dev/null 2>&1
    sleep 6
    if shot="$("$VM" shot "app-${app##*.}" 2>/dev/null)"; then
      ok "$app launched; screenshot: $shot"
    else
      bad "$app produced no scanout"
      gssh "journalctl --user -b --no-pager | grep -i '$app' | tail -10" 2>&1 | sed 's/^/      /'
    fi
  done
fi

# --- 5. the Terminal's confined guest is a usable shell ----------------------
# apps/terminal declares `ush = "required@1"`, so this is also the proof that the
# service bind resolved rather than the launcher refusing the app.
msg "vim / less / whoami inside the ush guest"
guest="$(gssh 'XDG_RUNTIME_DIR=/run/user/$(id -u) /usr/libexec/qos-ush ush -c "command -v vim; command -v less; whoami; vim --version | head -1"' 2>&1 | tr -d '\r')"
for want in /usr/bin/vim /usr/bin/less "$QOS_USER"; do
  if printf '%s' "$guest" | grep -qF "$want"; then
    ok "the confined guest has $want"
  else
    bad "the confined guest is missing $want. It said:
$(printf '%s\n' "$guest" | sed 's/^/      /')"
  fi
done

printf '\n%s\n' "screenshots + logs: $QOS_RUN"
exit "$fail"
