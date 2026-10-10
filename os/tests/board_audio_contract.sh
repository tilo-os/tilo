#!/usr/bin/env bash
# What the Lenovo Yoga Slim 7x (lenovo-83ed) needs for its speakers and
# microphones. Its sound card (snd-soc-x1e80100) runs on the audio DSP and
# shows up without any of the configuration PipeWire needs: without the ALSA
# UCM profile for the machine, WirePlumber lists "Built-in Audio" with no sink
# and no source.
#
#   board_audio_contract.sh              the image recipes (hermetic)
#   board_audio_contract.sh --root DIR   a built lenovo-83ed rootfs, as the
#                                        board job runs it inside the image
#
# Speaker safety: the four WSA8845 amplifiers run without DSP speaker
# protection upstream (VISENSE off, no protection module in the topology).
# alsa-ucm-conf ships them enabled with a fixed amplifier gain, which is what
# the image installs unchanged. The rootfs check pins that sequence, so an
# alsa-ucm update that changes how the speakers are driven fails here and is
# reviewed before it reaches the laptop.
set -uo pipefail

fail=0
ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

# board_vendor-product_family-board_name as UCM composes it, read on an 83ED.
dmi='LENOVO-Yoga Slim 7 14Q8X9-LNVNB161216'
# The kernel loads the topology named after the device tree's sound card model.
topology='qcom/x1e80100/X1E80100-LENOVO-Yoga-Slim7x-tplg.bin'

check_sources() {
  local repo containerfile board profile
  repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
  containerfile="$repo/os/Containerfile"
  board="$repo/os/Containerfile.board"
  profile="$repo/os/hardware/profiles/lenovo-83ed.json"

  # The audio stack step: pipewire, wireplumber and the UCM profiles they read.
  # The step is a group of the one package RUN (TIL-733): its comment is
  # indented and the group ends with its own `dnf clean all`.
  if awk '/^[[:space:]]*# Audio\/Video$/ { s = 1 } s && (/^$/ || /dnf clean all/) { exit } s' "$containerfile" \
    | grep -q '^    alsa-ucm \\$'; then
    ok 'the audio stack installs the ALSA UCM profiles'
  else
    bad 'the audio stack installs no alsa-ucm: X1E sound cards get no sink or source'
  fi

  # Fedora's qcom-firmware carries the AudioReach topology for the 83ED.
  if grep -q 'TILO_HARDWARE_PROFILE" = lenovo-83ed' "$containerfile" \
    && grep -q 'dnf install -y --setopt=reposdir=/etc/tilo-swap.d qcom-firmware' "$containerfile"; then
    ok 'the release image installs qcom-firmware for lenovo-83ed'
  else
    bad 'the release image does not install qcom-firmware for lenovo-83ed'
  fi
  if grep -q 'TILO_HARDWARE_PROFILE" != lenovo-83ed ] || dnf install -y --setopt=reposdir=/repo qcom-firmware' "$board"; then
    ok 'the board dev image installs qcom-firmware for lenovo-83ed'
  else
    bad 'the board dev image does not install qcom-firmware for lenovo-83ed'
  fi

  # The audio DSP firmware is the board's own, from its pinned artifact.
  for name in qcadsp8380.mbn adsp_dtbs.elf; do
    if grep -q "\"$name\": \"qcom/x1e80100/LENOVO/83ED/$name\"" "$profile"; then
      ok "the profile stages the audio DSP's $name"
    else
      bad "the profile does not stage the audio DSP's $name"
    fi
  done
  if grep -q '"devicetree": "qcom/x1e80100-lenovo-yoga-slim7x.dtb"' "$profile"; then
    ok 'the profile boots the Yoga Slim 7x device tree, whose card model names the topology'
  else
    bad 'the profile boots another device tree than the one the topology is named for'
  fi
}

firmware_present() {
  local base="$1/usr/lib/firmware/$2" ext
  for ext in '' .xz .zst; do [[ -e $base$ext ]] && return 0; done
  return 1
}

module_present() {
  local modules="$1" name="$2" alt="${2//-/_}"
  grep -Eq "/($name|$alt)\.ko\$" "$modules/modules.builtin" 2>/dev/null && return 0
  [[ -n $(find "$modules" \( -name "$name.ko*" -o -name "$alt.ko*" \) -print -quit) ]]
}

check_root() {
  local root="$1" ucm="$1/usr/share/alsa/ucm2" modules name file matches
  set -- "$root"/usr/lib/modules/*/
  if [[ $# -eq 1 && -d $1 ]]; then
    modules="${1%/}"
    for name in snd-soc-x1e80100 snd-q6apm snd-soc-wsa884x snd-soc-lpass-wsa-macro \
      snd-soc-lpass-va-macro; do
      if module_present "$modules" "$name"; then ok "the kernel has $name"
      else bad "the kernel has no $name"; fi
    done
  else
    bad "expected one kernel under $root/usr/lib/modules, found $#"
  fi

  for file in "$topology" qcom/x1e80100/LENOVO/83ED/qcadsp8380.mbn \
    qcom/x1e80100/LENOVO/83ED/adsp_dtbs.elf; do
    if firmware_present "$root" "$file"; then ok "firmware $file is installed"
    else bad "firmware $file is missing"; fi
  done

  # alsa-lib finds the card's profile by its driver name, x1e80100.
  file="$ucm/conf.d/x1e80100/x1e80100.conf"
  if [[ -e $file ]]; then
    ok 'UCM has a profile for the x1e80100 card'
  else
    bad 'UCM has no profile for the x1e80100 card'
    return
  fi
  # Every machine block whose DMI regex matches is included; the 83ED must
  # match the Slim 7x block and no other.
  matches="$(awk '
    match($0, /Regex "[^"]*"/) { re = substr($0, RSTART + 7, RLENGTH - 8) }
    match($0, /File "[^"]*"/) && re != "" { print substr($0, RSTART + 6, RLENGTH - 7) "\t" re; re = "" }
  ' "$file" | while IFS=$'\t' read -r include re; do
      if printf '%s\n' "$dmi" | grep -Eq -- "$re"; then printf '%s\n' "$include"; fi
    done)"
  if [[ $matches == /Qualcomm/x1e80100/LENOVO-Slim-7x.conf ]]; then
    ok 'the 83ED matches the Yoga Slim 7x machine profile alone'
  else
    bad "the 83ED matches [${matches//$'\n'/, }], not the Yoga Slim 7x profile alone"
  fi
  for file in Qualcomm/x1e80100/LENOVO-Slim-7x.conf Qualcomm/x1e80100/Slim7x-HiFi.conf \
    codecs/wsa884x/four-speakers/SpeakerSeq.conf; do
    if [[ -e $ucm/$file ]]; then ok "UCM has $file"; else bad "UCM has no $file"; fi
  done
  if grep -q 'SectionDevice."Speaker"' "$ucm/Qualcomm/x1e80100/Slim7x-HiFi.conf" 2>/dev/null \
    && grep -q 'SectionDevice."Mic"' "$ucm/Qualcomm/x1e80100/Slim7x-HiFi.conf"; then
    ok 'the HiFi verb has the speakers and the microphones'
  else
    bad 'the HiFi verb lacks the speakers or the microphones'
  fi

  # The speaker sequence this image was reviewed with: each amplifier enabled
  # at gain step 12, without current/voltage sense feedback.
  file="$ucm/codecs/wsa884x/four-speakers/SpeakerSeq.conf"
  local amp gains
  for amp in WooferLeft TweeterLeft WooferRight TweeterRight; do
    gains="$(sed -n "s/.*cset \"name='$amp PA Volume' \([0-9]*\)\".*/\1/p" "$file" 2>/dev/null)"
    if [[ $gains == 12 ]]; then ok "$amp is enabled at gain step 12"
    else bad "$amp's gain changed to [${gains//$'\n'/, }]; review speaker safety before shipping it"; fi
  done
}

case "${1:-}" in
  '') check_sources ;;
  --root) [[ -d ${2:-} ]] || { echo "usage: $0 [--root DIR]" >&2; exit 2; }; check_root "$2" ;;
  *) echo "usage: $0 [--root DIR]" >&2; exit 2 ;;
esac
exit "$fail"
