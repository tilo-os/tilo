#!/usr/bin/env bash
# What a booted qOS image has to be able to do with its preinstalled apps.
#
#   ./os/tests/booted_app_store.sh            # over SSH into a running vm.sh guest
#   OS_ROOT=/ ./os/tests/booted_app_store.sh  # on the machine itself
#
# Everything the image build asserts is about files it just wrote. None of it
# survives the question "does this work once it has booted", which is the claim
# that keeps being made without evidence: an image that ships 46 packages, a
# registry listing them, a runtime bundle and a package manager, and no proof
# that any of them resolve on a running system.
#
# Run it against a guest by exporting a RUNNER that executes a command there:
#
#   RUNNER='sshpass -p pw ssh -p 2222 user@127.0.0.1' ./os/tests/booted_app_store.sh
#
# A check that cannot observe its subject FAILS. There is no skip path: if the
# registry cannot be read, that is a failure, not an absence of information.
#
# Exit codes: 0 all assertions hold; 1 at least one failed.
set -uo pipefail

RUNNER="${RUNNER:-}"
STORE="${STORE:-/usr/share/qos-apps}"
RUNTIME_VERSION="${RUNTIME_VERSION:-2026.07}"
# How many packages the store must list. The image build fails unless every app
# with a built binary installs, so this is a floor, not a guess — set it to what
# the build reported and a silently-shrinking store is caught.
MIN_PACKAGES="${MIN_PACKAGES:-40}"

fail=0
ok()  { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

# Run a command on the subject (guest or local). Never swallows failure: the
# caller decides, and the output is returned either way.
on() {
  if [ -n "$RUNNER" ]; then $RUNNER "$@"; else bash -c "$*"; fi
}

# --- 0. we can reach the subject at all --------------------------------------
if ! on 'echo reachable' | grep -q reachable; then
  bad "cannot run commands on the subject (RUNNER='$RUNNER'). Every assertion
      below would report nothing rather than something, so stop here."
  exit 1
fi
ok "the subject is reachable"

# --- 1. the system app store is populated ------------------------------------
rows="$(on "grep -c '^\[\[entry\]\]' $STORE/registry.toml 2>/dev/null" | tr -d '\r')"
case "$rows" in
  ''|*[!0-9]*) bad "$STORE/registry.toml is unreadable or has no [[entry]] rows.
      The launcher reads this file; an empty one is an empty command palette." ;;
  *) if [ "$rows" -ge "$MIN_PACKAGES" ]; then
       ok "$STORE/registry.toml lists $rows packages"
     else
       bad "$STORE/registry.toml lists only $rows packages (expected >= $MIN_PACKAGES)"
     fi ;;
esac

# Rows are worthless if the payload they point at is not there. Check every one
# rather than a sample — this is the failure mode of recording absolute paths
# from a build stage.
missing="$(on "awk -F'\"' '/^payload_root/ {print \$2}' $STORE/registry.toml \
                 | while read -r p; do [ -e \"\$p\" ] || echo \"\$p\"; done" | tr -d '\r')"
if [ -z "$missing" ]; then
  ok "every registry row's payload path exists on the booted system"
else
  bad "registry rows point at payloads that are not there:
$(printf '%s\n' "$missing" | sed 's/^/      /')"
fi

# --- 2. the registry is what a reader of the store actually sees -------------
# The file being right and a reader seeing it are different claims: the store is
# merged with a per-user one behind SYSTEM_DATA_DIR, and a wrong constant there
# shows up as an empty palette with a perfectly good registry.toml. `qos-install
# list` goes through that same read path, so it is the cheapest way to exercise
# it without a compositor. The palette itself is proved by screenshot.
listed="$(on 'qos-install list 2>&1' | grep -cE '^[a-z0-9.-]+\.[a-z0-9-]+' | tr -d '\r')"
if [ "${listed:-0}" -ge "$MIN_PACKAGES" ]; then
  ok "qos-install list reports $listed packages from the system store"
else
  bad "qos-install list reported $listed packages (expected >= $MIN_PACKAGES):
$(on 'qos-install list 2>&1' | head -8 | sed 's/^/      /')"
fi
if on 'command -v qos-launcher' >/dev/null 2>&1; then
  ok "qos-launcher is on PATH, so the palette has something to exec"
else
  bad "qos-launcher is not on PATH, so nothing can start a packaged app"
fi

# --- 3. the runtime bundle every app declares actually resolves --------------
# This is what RuntimeBundleMissing is: the manifest names qos-runtime/<v> and
# the launcher refuses to start the app when /usr/lib/qos-runtime/<v>/ has no
# bundle.toml. Assert the file, then assert the tool agrees it is valid.
if on "test -f /usr/lib/qos-runtime/$RUNTIME_VERSION/bundle.toml"; then
  ok "/usr/lib/qos-runtime/$RUNTIME_VERSION/bundle.toml is present"
else
  bad "no bundle at /usr/lib/qos-runtime/$RUNTIME_VERSION/ — every packaged app
      declaring qos-runtime/$RUNTIME_VERSION fails at launch with RuntimeBundleMissing"
fi
if on "qos-runtime-bundle verify /usr/lib/qos-runtime/$RUNTIME_VERSION" >/dev/null 2>&1; then
  ok "the baked runtime bundle verifies"
else
  bad "qos-runtime-bundle verify failed on /usr/lib/qos-runtime/$RUNTIME_VERSION:
$(on "qos-runtime-bundle verify /usr/lib/qos-runtime/$RUNTIME_VERSION 2>&1" | head -3 | sed 's/^/      /')"
fi

# --- 4. the package manager is usable on the running system ------------------
for t in qos-install qos-pkg; do
  if on "command -v $t" >/dev/null 2>&1; then
    ok "$t is on PATH"
  else
    bad "$t is not on PATH — the only apps this machine could ever have are the
        ones the image shipped"
  fi
done
# On PATH is not the same as working. `list` reads the same registry the
# launcher does and exercises the store code path.
if on 'qos-install list' >/dev/null 2>&1; then
  ok "qos-install list succeeds against the real store"
else
  bad "qos-install list failed:
$(on 'qos-install list 2>&1' | head -5 | sed 's/^/      /')"
fi

# --- 5. the confined shell runtime the Terminal requires is serving ----------
# apps/terminal declares ush = "required@1"; without this socket the launcher
# refuses to start it at all.
if on 'test -x /usr/libexec/qos-ush'; then
  ok "/usr/libexec/qos-ush is installed"
else
  bad "/usr/libexec/qos-ush is missing — the packaged Terminal cannot launch"
fi
if on 'test -S "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/qos/services/ush.sock"'; then
  ok "the ush service socket is bound for this session"
else
  bad "no ush socket at \$XDG_RUNTIME_DIR/qos/services/ush.sock; qos-ush.service says:
$(on 'systemctl --user status qos-ush.service 2>&1' | head -8 | sed 's/^/      /')"
fi

# --- 6. the collaborative filesystem is mounted ------------------------------
if on 'mountpoint -q /var/workspace'; then
  ok "/var/workspace is a mountpoint"
else
  bad "/var/workspace is not mounted; fs.service says:
$(on 'systemctl status fs.service 2>&1' | head -10 | sed 's/^/      /')"
fi

exit "$fail"
