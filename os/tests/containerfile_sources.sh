#!/usr/bin/env bash
# Every host-side COPY source in the image Containerfiles exists.
#
#   ./os/tests/containerfile_sources.sh
#
# `COPY docs/secure-boot.md …` outlived its source when that file was folded into
# docs/security/, and nothing noticed: the failure is
#
#   ERROR: failed to calculate checksum of ref …: "/docs/secure-boot.md": not found
#
# which arrives ~20 minutes into a build, names a path with a leading slash that
# exists nowhere, and blocked every image build on main until someone tried to build
# one. A missing COPY source is decidable in a second from the file itself, so it
# should not cost a build to find out.
#
# Runs as a pre-flight in os/dev/build-image.sh. Exit 0 all sources resolve, 1 if any
# does not.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
# shellcheck source=lib/containerfile.sh
. "$HERE/lib/containerfile.sh"

fails=0
bad() { printf '[FAIL]   %s\n' "$*"; fails=$((fails+1)); }
ok()  { printf '[ OK ]   %s\n' "$*"; }
note() { printf '[info]   %s\n' "$*"; }

# Some sources are binaries a build script stages into the tree just before calling
# podman, so they are legitimately absent from a checkout. They are exactly the paths
# .containerignore names for that reason — os/tilo-installer and friends — so that
# file is the declaration rather than a second list here that could disagree with it.
staged_at_build_time() {
  grep -qxF "$1" "$REPO/.containerignore" 2>/dev/null
}

# Containerfile:context — the source paths are relative to the build context, which
# is not the same for all of them.
for spec in "os/Containerfile:$REPO" \
            "os/Containerfile.wpe:$REPO" \
            "os/Containerfile.fex-payload:$REPO" \
            "os/Containerfile.wpe-buildroot:$REPO" \
            "os/Containerfile.kernel-buildroot:$REPO" \
            "os/Containerfile.installer:$REPO/os" \
            "os/Containerfile.test:$REPO/os" \
            "os/Containerfile.host:$REPO" \
            "os/Containerfile.dev:$REPO" \
            "os/Containerfile.dev-installer-base:$REPO"; do
  file="${spec%%:*}"; ctx="${spec#*:}"
  [ -f "$REPO/$file" ] || { bad "$file does not exist"; continue; }

  missing=0 checked=0 skipped=0
  ctx_rel="${ctx#"$REPO"}"; ctx_rel="${ctx_rel#/}"
  while IFS=$'\t' read -r stage src destination; do
    [ "$stage" = context ] || continue
    # Build-arg-interpolated or stage-relative paths cannot be resolved from here.
    case "$src" in *'$'*|'') continue ;; esac
    [ -e "$ctx/$src" ] && { checked=$((checked+1)); continue; }
    if staged_at_build_time "${ctx_rel:+$ctx_rel/}$src"; then
      skipped=$((skipped+1)); continue
    fi
    bad "$file: COPY $src — not in the build context ($ctx), and not declared in
         .containerignore as staged at build time"
    missing=1
  done < <(containerfile_copy_contract "$REPO/$file")

  [ "$missing" = 0 ] && ok "$file: all $checked host-side COPY sources resolve$(
    [ "$skipped" -gt 0 ] && printf ' (%s staged at build time)' "$skipped")"
done

# Parsing multiline COPY correctly is also what lets the payload/layer-budget
# contract prove that grouping did not add, remove, or retarget an installed
# rootfs input. Keep the preflight and that proof inseparable.
if ! bash "$HERE/containerfile_copy_invariants.sh"; then
  bad "os/Containerfile COPY payload or rootfs layer budget drifted"
fi

# Suspend must freeze sessions before the kernel freezer reaches processes
# blocked on the userspace collaborative filesystem. Prove both halves of the
# shipped contract here: every sleep service's drop-in sets the variable
# systemd-sleep reads, and the production image installs that exact file,
# directly or through its directory.
for sleep_unit in systemd-suspend systemd-hibernate systemd-hybrid-sleep \
                  systemd-suspend-then-hibernate; do
  sleep_policy=os/etc/systemd/system/$sleep_unit.service.d/20-tilo-freeze-user-sessions.conf
  if grep -Fqx 'Environment=SYSTEMD_SLEEP_FREEZE_USER_SESSIONS=1' "$REPO/$sleep_policy" \
     && containerfile_copy_contract "$REPO/os/Containerfile" rootfs |
          awk -F '\t' -v source="$sleep_policy" \
            -v target="/etc/systemd/system/$sleep_unit.service.d/20-tilo-freeze-user-sessions.conf" \
            '$1 == "context" && (($2 == source && $3 == target) ||
               (substr(source, 1, length($2)) == $2 && substr($2, length($2)) == "/" &&
                $3 == substr(target, 1, length($3)) &&
                substr(source, length($2) + 1) == substr(target, length($3) + 1))) { found=1 }
             END { exit !found }'; then
    ok "$sleep_unit freezes user sessions before suspending"
  else
    bad "$sleep_unit is not installed with SYSTEMD_SLEEP_FREEZE_USER_SESSIONS=1"
  fi
done
if [ -e "$REPO/os/etc/systemd/sleep.conf.d/20-tilo-freeze-user-sessions.conf" ]; then
  bad "sleep.conf has no FreezeUserSessions key; systemd-sleep only reads its environment"
fi

if [ "$fails" -eq 0 ]; then
  printf '\nall Containerfile COPY sources resolve\n'
  exit 0
fi
printf '\n%s missing COPY source(s)\n' "$fails"
exit 1
