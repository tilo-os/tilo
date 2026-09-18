#!/usr/bin/env bash
# Exercise both AppImage execution paths without silently turning a broken
# release runner into an extract-only test.
set -euo pipefail

appimage="${1:?usage: verify-appimage-runtime.sh APPIMAGE OUTPUT_DIR}"
output="${2:?usage: verify-appimage-runtime.sh APPIMAGE OUTPUT_DIR}"
[ -f "$appimage" ] && [ -x "$appimage" ] \
  || { printf 'AppImage is missing or not executable: %s\n' "$appimage" >&2; exit 1; }
mkdir -p "$output"

# Keep extraction as a separate compatibility gate even when FUSE works. This
# covers the documented escape hatch used by locked-down container hosts.
APPIMAGE_EXTRACT_AND_RUN=1 "$appimage" \
  --shots "$output/appimage-extracted.png" \
  --scenario done --viewport 1180x820
test -s "$output/appimage-extracted.png"

kernel_has_fuse=0
if [ -r /proc/filesystems ] \
   && awk '$2 == "fuse" { found = 1 } END { exit !found }' /proc/filesystems; then
  kernel_has_fuse=1
fi

if [ -c /dev/fuse ]; then
  [ -r /dev/fuse ] && [ -w /dev/fuse ] \
    || { printf '/dev/fuse exists but is not accessible to the release runner\n' >&2; exit 1; }
  command -v fusermount3 >/dev/null 2>&1 \
    || command -v fusermount >/dev/null 2>&1 \
    || { printf 'FUSE is available but fusermount is missing\n' >&2; exit 1; }

  # No APPIMAGE_EXTRACT_AND_RUN here: this is the end-user double-click path
  # through the embedded type-2 runtime and a real FUSE mount.
  "$appimage" \
    --shots "$output/appimage-runtime.png" \
    --scenario done --viewport 1180x820
  test -s "$output/appimage-runtime.png"
  printf 'fuse-runtime\n' > "$output/appimage-runtime-mode.txt"
elif [ "$kernel_has_fuse" = 1 ]; then
  printf 'the CI kernel supports FUSE but /dev/fuse was not exposed; refusing extract-only release verification\n' >&2
  exit 1
else
  # Some hosted kernels are compiled without FUSE. Extraction above still
  # verifies the exact AppImage payload; record the bounded reason in the
  # uploaded smoke artifact so this cannot look like a normal runtime pass.
  printf 'extract-only: kernel has no FUSE filesystem and no /dev/fuse device\n' \
    > "$output/appimage-runtime-mode.txt"
  printf '::warning::kernel has no FUSE support; AppImage runtime mount was not available\n' >&2
fi

if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  {
    printf '### AppImage runtime verification\n\n'
    printf -- '- `%s`: `%s`\n' "$(basename "$appimage")" \
      "$(cat "$output/appimage-runtime-mode.txt")"
  } >> "$GITHUB_STEP_SUMMARY"
fi
