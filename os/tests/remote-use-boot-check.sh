#!/usr/bin/env bash
# What another of the person's devices needs of this one to use its camera, microphone, scanner or
# game controller here, on the booted image as the signed-in person: the virtual camera the image
# creates at boot, the virtual game pad the session can plug in, and the programs that capture and
# feed them (crates/desktop/session/src/remote_use/edge.rs).
set -euo pipefail

fail=0
ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

# The signed loopback module, loaded at boot by modules-load.d with the options modprobe.d names.
if [[ -d /sys/module/v4l2loopback ]]; then
  ok 'v4l2loopback is loaded'
else
  bad 'v4l2loopback is not loaded after boot'
fi
signer=$(modinfo -F signer v4l2loopback 2>/dev/null || true)
[[ -n $signer ]] && ok "v4l2loopback is signed ($signer)" || bad 'v4l2loopback carries no signature'

camera=
for node in /sys/class/video4linux/video*; do
  [[ -r $node/name ]] || continue
  if [[ $(<"$node/name") == "Tilo Remote Camera" ]]; then
    camera=/dev/${node##*/}
  fi
done
if [[ $camera == /dev/video42 ]]; then
  ok 'the virtual camera "Tilo Remote Camera" is /dev/video42'
else
  bad "no \"Tilo Remote Camera\" at /dev/video42 (found: ${camera:-none})"
fi
# The session writes another device's frames into it as the person.
if [[ -n $camera && -w $camera ]]; then
  ok "$(id -un) may write frames into $camera"
else
  bad "$(id -un) may not write into the virtual camera"
fi
if [[ -n $camera ]] && v4l2-ctl --device "$camera" --info | grep -q 'Tilo Remote Camera'; then
  ok 'v4l2-ctl answers for the virtual camera'
else
  bad 'v4l2-ctl cannot read the virtual camera'
fi

# A controller another device sends here plays through a uinput pad the session creates.
if [[ -c /dev/uinput && -w /dev/uinput ]]; then
  ok "$(id -un) may create a virtual game pad through /dev/uinput"
else
  bad "$(id -un) may not open /dev/uinput: a sent controller could not play here"
fi

for program in gst-launch-1.0 pw-record pw-loopback pw-cat scanimage v4l2-ctl; do
  command -v "$program" >/dev/null && ok "$program is installed" || bad "$program is missing"
done
for element in v4l2src videoconvert videoscale videorate jpegenc fdsink; do
  gst-inspect-1.0 "$element" >/dev/null 2>&1 && ok "GStreamer has $element" || bad "GStreamer lacks $element"
done

exit "$fail"
