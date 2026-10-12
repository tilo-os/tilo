#!/usr/bin/env bash
# What a booted Tilo image has to be able to do with its preinstalled apps.
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
STORE="${STORE:-/usr/share/tilo-apps}"
RUNTIME_RELEASE="${RUNTIME_RELEASE:-2026.07}"
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
# shows up as an empty palette with a perfectly good registry.toml. `tilo-install
# list` goes through that same read path, so it is the cheapest way to exercise
# it without a compositor. The palette itself is proved by screenshot.
listed="$(on 'tilo-install list 2>&1' | grep -cE '^[a-z0-9.-]+\.[a-z0-9-]+' | tr -d '\r')"
if [ "${listed:-0}" -ge "$MIN_PACKAGES" ]; then
  ok "tilo-install list reports $listed packages from the system store"
else
  bad "tilo-install list reported $listed packages (expected >= $MIN_PACKAGES):
$(on 'tilo-install list 2>&1' | head -8 | sed 's/^/      /')"
fi
if on 'command -v tilo-launcher' >/dev/null 2>&1; then
  ok "tilo-launcher is on PATH, so the palette has something to exec"
else
  bad "tilo-launcher is not on PATH, so nothing can start a packaged app"
fi

# --- 3. the runtime bundle every app declares actually resolves --------------
# This is what RuntimeBundleMissing is: the manifest names the runtime build
# tilo-runtime/<release>+native.<digest> and the launcher refuses to start the
# app when /usr/lib/tilo-runtime/<that version>/ has no bundle.toml. Every
# preinstalled app names the one build the image bakes. Assert the file, then
# assert the tool agrees it is valid.
RUNTIME_VERSION="$(on "sed -n 's|^runtime *= *\"tilo-runtime/\\($RUNTIME_RELEASE+native\\.[0-9a-f]*\\)\".*|\\1|p' $STORE/pkgs/*.pkg/manifest.toml | sort -u")"
if [ -n "$RUNTIME_VERSION" ] && [ "$(printf '%s\n' "$RUNTIME_VERSION" | grep -c .)" = 1 ]; then
  ok "every preinstalled app names one runtime build: $RUNTIME_VERSION"
else
  bad "preinstalled apps do not all name one runtime build of $RUNTIME_RELEASE: ${RUNTIME_VERSION:-none}"
fi
if on "test -f /usr/lib/tilo-runtime/$RUNTIME_VERSION/bundle.toml"; then
  ok "/usr/lib/tilo-runtime/$RUNTIME_VERSION/bundle.toml is present"
else
  bad "no bundle at /usr/lib/tilo-runtime/$RUNTIME_VERSION/ — every packaged app
      declaring tilo-runtime/$RUNTIME_VERSION fails at launch with RuntimeBundleMissing"
fi
if on "tilo-runtime-bundle verify /usr/lib/tilo-runtime/$RUNTIME_VERSION" >/dev/null 2>&1; then
  ok "the baked runtime bundle verifies"
else
  bad "tilo-runtime-bundle verify failed on /usr/lib/tilo-runtime/$RUNTIME_VERSION:
$(on "tilo-runtime-bundle verify /usr/lib/tilo-runtime/$RUNTIME_VERSION 2>&1" | head -3 | sed 's/^/      /')"
fi

# --- 4. the package manager is usable on the running system ------------------
for t in tilo-install tilo-app-package; do
  if on "command -v $t" >/dev/null 2>&1; then
    ok "$t is on PATH"
  else
    bad "$t is not on PATH — the only apps this machine could ever have are the
        ones the image shipped"
  fi
done
# On PATH is not the same as working. `list` reads the same registry the
# launcher does and exercises the store code path.
if on 'tilo-install list' >/dev/null 2>&1; then
  ok "tilo-install list succeeds against the real store"
else
  bad "tilo-install list failed:
$(on 'tilo-install list 2>&1' | head -5 | sed 's/^/      /')"
fi

# The ergonomic commands are part of the image contract, not development-only
# binaries. Exercise both aliases and the super-command before asking the
# sandbox runtime to broker them.
for t in app store tilo system; do
  if on "command -v $t" >/dev/null 2>&1 && on "$t --help" >/dev/null 2>&1; then
    ok "$t is installed and its command help works"
  else
    bad "$t is missing or unusable on the booted image"
  fi
done
if on 'tilo app --help' >/dev/null 2>&1 \
   && on 'tilo store --help' >/dev/null 2>&1 \
   && on 'tilo system --help' >/dev/null 2>&1; then
  ok "the tilo super-command discovers both providers"
else
  bad "tilo cannot discover its app/store/system provider commands"
fi

# --- 4b. a build linked against another runtime is refused, not installed ---
# The field failure this guards: registry builds linked against a different
# Rust libstd than the image's runtime were installed over the system apps,
# and every one then died at exec. Repack a shipped app as if it had been
# linked against another runtime build and install it into a scratch store
# (nothing on the subject machine changes): the installer must refuse it and
# say how to get the runtime it needs.
refusal="$(on 'set -e
scratch=$(mktemp -d); trap "rm -rf \"$scratch\"" EXIT
pkg=$(ls -d '"$STORE"'/pkgs/*.pkg | head -1)
payload=${pkg%.pkg}
tilo-runtime-bundle key generate --out "$scratch/id.json" >/dev/null
key=$(tilo-runtime-bundle key pubkey --identity "$scratch/id.json")
sed -e "s|^\(identity *= *\)\"ed25519:[0-9a-fA-F]*\"|\1\"ed25519:$key\"|" \
    -e "s|^runtime *= *\"tilo-runtime/\([0-9.]*\)+native\.[0-9a-f]*\"|runtime = \"tilo-runtime/\1+native.00000000000000000000000000000000\"|" \
    "$pkg/manifest.toml" > "$scratch/manifest.toml"
grep -q "+native.00000000000000000000000000000000" "$scratch/manifest.toml"
tilo-app-package pack --manifest "$scratch/manifest.toml" --payload "$payload" \
    --identity "$scratch/id.json" --output "$scratch/other.tapp" >/dev/null
if TILO_APPS_DATA_DIR="$scratch/apps" XDG_DATA_HOME="$scratch/data" \
   XDG_CONFIG_HOME="$scratch/config" HOME="$scratch" \
   tilo-install install "$scratch/other.tapp" --allow-self-signed 2>&1; then
  echo INSTALLED
fi' 2>&1)"
if grep -q "needs runtime" <<<"$refusal" && ! grep -q INSTALLED <<<"$refusal"; then
  ok "an app linked against another runtime build is refused with the way to fetch it"
else
  bad "an app linked against another runtime build was not refused:
$(printf '%s\n' "$refusal" | tail -5 | sed 's/^/      /')"
fi

# --- 5. the confined shell runtime the Terminal requires is serving ----------
# apps/terminal declares ush = "required@1"; without this socket the launcher
# refuses to start it at all.
if on 'test -x /usr/libexec/tilo-ush'; then
  ok "/usr/libexec/tilo-ush is installed"
else
  bad "/usr/libexec/tilo-ush is missing — the packaged Terminal cannot launch"
fi
if on 'test -S "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/tilo/services/ush.sock"'; then
  ok "the ush service socket is bound for this session"
else
  bad "no ush socket at \$XDG_RUNTIME_DIR/tilo/services/ush.sock; tilo-ush.service says:
$(on 'systemctl --user status tilo-ush.service 2>&1' | head -8 | sed 's/^/      /')"
fi

# `app` and `system` inside Terminal are ush-owned shims, not the host binaries
# accidentally visible through the sandbox. This read-only discovery pass
# proves the guest created all four shims and can parse their supported surface;
# the broker request/response and signed update mutation are covered by native
# Rust integration tests so this boot check never changes the subject machine.
if on 'test "$(readlink -e /usr/bin/ush)" = /usr/libexec/tilo-ush'; then
  ok "ush is available on PATH through the confined runtime"
else
  bad "ush is missing from PATH or does not use the confined runtime"
fi

if on "/usr/bin/ush -c 'app --help >/dev/null && store --help >/dev/null && tilo --help >/dev/null && system --help >/dev/null'"; then
  ok "the sandboxed Terminal exposes discoverable app and system commands"
else
  bad "the sandboxed Terminal cannot discover its brokered management commands"
fi

# --- 6. the collaborative filesystem is mounted ------------------------------
if on 'mountpoint -q /var/workspace'; then
  ok "/var/workspace is a mountpoint"
else
  bad "/var/workspace is not mounted; fs.service says:
$(on 'systemctl status fs.service 2>&1' | head -10 | sed 's/^/      /')"
fi

exit "$fail"
