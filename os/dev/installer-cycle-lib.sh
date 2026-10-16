#!/usr/bin/env bash
# Framebuffer/OCR helpers for graphical VM cycles, sourced by
# os/dev/test-internal-install-cycle.sh.
#
# The functions follow os/dev/test-installer-cycle.sh exactly (same OCR
# oracle, same private recovery-key handling, same keyboard routes through
# the installed Tilo first login). That release gate keeps its own inline
# copies: dozens of source-level invariants in os/tests pin its text, so it is
# not rewritten to source this file.
#
# Callers set: vm (path to os/dev/vm.sh), out (artifact directory), run
# ($TILO_RUN), and define die().

shot_ocr() {
  local name="$1"
  local image="$run/$name.png" text_file="$out/$name.txt"
  "$vm" shot "$name" >/dev/null || return 1
  tesseract "$image" stdout 2>/dev/null > "$text_file" || return 1
  printf '%s\n' "$image"
}

# OCR text with line breaks folded, so a pattern may span wrapped lines.
ocr_flat() { tr '\n' ' ' < "$1" | tr -s '[:space:]' ' '; }

wait_screen() {
  local saved="$1" pattern="$2" attempts="${3:-60}" delay="${4:-5}" image i
  for i in $(seq 1 "$attempts"); do
    if image="$(shot_ocr observe 2>/dev/null)" \
      && ocr_flat "$out/observe.txt" | grep -Eqi "$pattern"; then
      cp -f "$image" "$out/$saved.png" || return 1
      cp -f "$out/observe.txt" "$out/$saved.txt" || return 1
      return 0
    fi
    "$vm" status | grep -q '^running' || return 1
    sleep "$delay"
  done
  [ ! -f "$run/observe.png" ] || cp -f "$run/observe.png" "$out/$saved-timeout.png"
  [ ! -f "$out/observe.txt" ] || cp -f "$out/observe.txt" "$out/$saved-timeout.txt"
  return 1
}

# Observe a screen which contains the recovery key without ever retaining its
# framebuffer or OCR output (see test-installer-cycle.sh).
wait_private_screen() {
  local pattern="$1" attempts="${2:-720}" delay="${3:-5}" image text i
  for i in $(seq 1 "$attempts"); do
    image="$run/private-observe.png"
    if "$vm" shot private-observe >/dev/null 2>&1; then
      text="$(tesseract "$image" stdout 2>/dev/null || true)"
      rm -f -- "$image" "$run/private-observe.ppm"
      if printf '%s\n' "$text" | grep -Eqi "$pattern"; then
        text=""
        printf 'finish_screen_wait_seconds=%s\n' "$((i * delay))" > "$out/install-timing.txt"
        return 0
      fi
      text=""
    else
      rm -f -- "$image" "$run/private-observe.ppm"
    fi
    "$vm" status | grep -q '^running' || return 1
    sleep "$delay"
  done
  return 1
}

scrub_artifact_secrets() {
  local file leaked=0
  while IFS= read -r -d '' file; do
    if LC_ALL=C grep -aEqi \
      '::tilo-recovery-key::|RECOVERY KEY.*(write|save|off-device)|[bcdfghjkmnpqrstvwxyz]{8}(-[bcdfghjkmnpqrstvwxyz]{8}){7}' \
      "$file"; then
      : > "$file"
      leaked=1
    fi
  done < <(find "$out" -type f ! -name artifact-secret-redaction.txt -print0)
  if [ "$leaked" -ne 0 ]; then
    printf 'A diagnostic containing recovery material was withheld.\n' \
      > "$out/artifact-secret-redaction.txt"
    return 1
  fi
}

# Prove the upload boundary rejects the backend marker and the recovery-key
# shape before any VM work begins.
prove_secret_scrubber() {
  printf '%s\n%s\n' '::tilo-recovery-key::artifact-canary' \
    'bbbbbbbb-cccccccc-dddddddd-ffffffff-gggggggg-hhhhhhhh-jjjjjjjj-kkkkkkkk' \
    > "$out/.artifact-secret-canary.txt"
  if scrub_artifact_secrets; then
    die "artifact secret scrubber accepted its canary"
  fi
  test ! -s "$out/.artifact-secret-canary.txt" || die "artifact secret scrubber kept its canary"
  rm -f -- "$out/.artifact-secret-canary.txt" "$out/artifact-secret-redaction.txt"
}

# Click the centre of the bottom-most OCR word equal to WORD (case-insensitive)
# on the current framebuffer. Coordinates are scaled from the screendump to the
# guest mode vm.sh's absolute tablet uses. Fails when the word is not visible.
click_text() {
  local word="$1" saved="$2" image coords
  image="$(shot_ocr "$saved")" || return 1
  tesseract "$image" stdout tsv 2>/dev/null > "$out/$saved.tsv" || return 1
  coords="$(python3 - "$out/$saved.tsv" "$word" "$image" "${TILO_XRES:-1280}" "${TILO_YRES:-800}" <<'PY'
import struct, sys
tsv, word, png, xres, yres = sys.argv[1], sys.argv[2].lower(), sys.argv[3], int(sys.argv[4]), int(sys.argv[5])
with open(png, 'rb') as handle:
    header = handle.read(24)
width, height = struct.unpack('>II', header[16:24])
best = None
for line in open(tsv, encoding='utf-8', errors='replace').read().splitlines()[1:]:
    cells = line.split('\t')
    if len(cells) < 12 or cells[11].strip().lower() != word:
        continue
    left, top, w, h = (int(cells[i]) for i in (6, 7, 8, 9))
    if best is None or top > best[1]:
        best = (left + w // 2, top + h // 2)
if best is None:
    raise SystemExit(1)
print(min(xres - 1, best[0] * xres // width), min(yres - 1, best[1] * yres // height))
PY
)" || return 1
  # shellcheck disable=SC2086
  "$vm" click $coords
}

# Installed target: type the disk passphrase until the greeter appears.
unlock_target_to_greeter() {
  local passphrase="$1" saved="$2" attempt image
  for attempt in $(seq 1 60); do
    if image="$(shot_ocr target-observe 2>/dev/null)" \
      && grep -Eqi 'Welcome[[:space:]]+back|Sign[[:space:]]+in' "$out/target-observe.txt"; then
      cp -f "$image" "$out/$saved.png" || return 1
      cp -f "$out/target-observe.txt" "$out/$saved.txt" || return 1
      return 0
    fi
    "$vm" status | grep -q '^running' || return 1
    if [ "$attempt" -ge 2 ]; then
      "$vm" type "$passphrase"
      "$vm" keys ret
    fi
    sleep 10
  done
  return 1
}

# First graphical login of a fresh installed account, through trusted OOBE and
# the desktop tour, exactly the keyboard route of test-installer-cycle.sh.
complete_first_login() {
  local password="$1" prefix="$2"
  "$vm" login --passcode-only "$password"
  wait_screen "$prefix-oobe-intro" 'Create[[:space:]]+your[[:space:]]+local' 120 5 \
    || die "first graphical login did not enter Tilo identity setup"
  "$vm" keys tab ret
  sleep 2
  "$vm" keys spc tab tab ret
  wait_screen "$prefix-oobe-backup" 'Back[[:space:]]+up[[:space:]]+your[[:space:]]+Tilo[[:space:]]+identity' 120 2 \
    || die "OOBE did not create the local/device identity chain"
  "$vm" keys tab tab tab spc tab ret
  wait_screen "$prefix-oobe-ready" 'Your[[:space:]]+identity[[:space:]]+is[[:space:]]+ready' 60 2 \
    || die "OOBE did not require and accept the backup-loss acknowledgement"
  "$vm" keys ret
  wait_screen "$prefix-tour-move" 'Everything[[:space:]]+is[[:space:]]+close[[:space:]]+at[[:space:]]+hand' 60 2 \
    || die "desktop tutorial did not open its navigation page"
  "$vm" keys tab tab ret
  wait_screen "$prefix-tour-arrange" 'A[[:space:]]+place[[:space:]]+for[[:space:]]+each[[:space:]]+kind[[:space:]]+of[[:space:]]+work' 60 2 \
    || die "desktop tutorial did not advance to its organisation page"
  "$vm" keys tab tab ret
  wait_screen "$prefix-tour-share" 'Sharing[[:space:]]+stays[[:space:]]+deliberate' 60 2 \
    || die "desktop tutorial did not advance to its collaboration page"
  "$vm" keys tab tab ret
  wait_screen "$prefix-tour-start" 'Your[[:space:]]+first[[:space:]]+five[[:space:]]+minutes' 60 2 \
    || die "desktop tutorial did not advance to its first-steps page"
  "$vm" keys tab tab ret
  sleep 8
}
