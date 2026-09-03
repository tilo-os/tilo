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

fails=0
bad() { printf '[FAIL]   %s\n' "$*"; fails=$((fails+1)); }
ok()  { printf '[ OK ]   %s\n' "$*"; }
note() { printf '[info]   %s\n' "$*"; }

# Some sources are binaries a build script stages into the tree just before calling
# podman, so they are legitimately absent from a checkout. They are exactly the paths
# .containerignore names for that reason — os/qos-installer and friends — so that
# file is the declaration rather than a second list here that could disagree with it.
staged_at_build_time() {
  grep -qxF "$1" "$REPO/.containerignore" 2>/dev/null
}

# Containerfile:context — the source paths are relative to the build context, which
# is not the same for all of them.
for spec in "os/Containerfile:$REPO" \
            "os/Containerfile.wpe-buildroot:$REPO" \
            "os/Containerfile.installer:$REPO/os" \
            "os/Containerfile.test:$REPO/os" \
            "os/Containerfile.host:$REPO"; do
  file="${spec%%:*}"; ctx="${spec#*:}"
  [ -f "$REPO/$file" ] || { bad "$file does not exist"; continue; }

  missing=0 checked=0 skipped=0
  ctx_rel="${ctx#"$REPO"}"; ctx_rel="${ctx_rel#/}"
  while read -r src; do
    # Build-arg-interpolated or stage-relative paths cannot be resolved from here.
    case "$src" in *'$'*|'') continue ;; esac
    [ -e "$ctx/$src" ] && { checked=$((checked+1)); continue; }
    if staged_at_build_time "${ctx_rel:+$ctx_rel/}$src"; then
      skipped=$((skipped+1)); continue
    fi
    bad "$file: COPY $src — not in the build context ($ctx), and not declared in
         .containerignore as staged at build time"
    missing=1
  done < <(awk '
    /^COPY / && !/--from=/ {
      # COPY [--flags] SRC... DEST — every field but the first and last is a source.
      for (i = 2; i < NF; i++) if (substr($i, 1, 2) != "--") print $i
    }' "$REPO/$file")

  [ "$missing" = 0 ] && ok "$file: all $checked host-side COPY sources resolve$(
    [ "$skipped" -gt 0 ] && printf ' (%s staged at build time)' "$skipped")"
done

if [ "$fails" -eq 0 ]; then
  printf '\nall Containerfile COPY sources resolve\n'
  exit 0
fi
printf '\n%s missing COPY source(s)\n' "$fails"
exit 1
