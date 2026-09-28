#!/usr/bin/env bash
# Tilo draws every surface with its own toolkit, so the finished rootfs ships
# no third-party UI toolkit: no GTK, libadwaita or Qt package, and no package
# linking one. Run inside the finished rootfs after the last package change.
set -euo pipefail
rpm_bin="${TILO_RPM_BIN:-rpm}"

# One line per package that still needs a toolkit: the name, then why. An entry
# is a replacement still owed, not a permanent allowance.
exceptions='gtk3 GTK 3 itself, while the packages below still require it
gstreamer1-plugins-bad-free its gtkwaylandsink plugin; the bundle carries the AV1/VP9 encoders and RTP payloaders Tilo uses
libdecor its GTK decoration plugin; Xwayland and SDL3 (FreeRDP) require libdecor
gcr3 its prompter and viewer; gnome-keyring, the Secret Service, requires gcr3
virt-viewer the Windows appliance recovery console and whole-desktop SPICE view; no toolkit-free SPICE client exists
spice-gtk3 virt-viewer
gtk-vnc2 virt-viewer
vte291 virt-viewer'

requirements="$(mktemp)"
trap 'rm -f "$requirements"' EXIT
# `=NAME` repeats the scalar package name inside REQUIRENAME's array iterator.
"$rpm_bin" -qa --qf '[%{=NAME}\t%{REQUIRENAME}\n]' >"$requirements"
installed="$("$rpm_bin" -qa --qf '%{NAME}\n' | sort -u)"

found="$( {
  # RPM macro packages (qt5-srpm-macros, ...) are build metadata, not Qt.
  grep -E '^((gtk[234]|libadwaita)(-.+)?|(.+-)?qt[56](-.+)?)$' <<<"$installed" \
    | grep -Ev -- '-srpm-macros$' || true
  awk -F '\t' '$2 ~ /^lib(gtk-(x11-2\.0|3|4)|gdk-(x11-2\.0|3)|adwaita-1|Qt[56][[:alnum:]]*)\.so/ { print $1 }' \
    "$requirements"
} | sort -u)"
allowed="$(awk 'NF { print $1 }' <<<"$exceptions")"
if [ -n "$allowed" ]; then
  found="$(grep -Fvx -f <(printf '%s\n' "$allowed") <<<"$found" || true)"
fi
if [ -n "$found" ]; then
  echo 'third-party UI toolkit packages or consumers remain in the finished rootfs:' >&2
  printf '%s\n' "$found" >&2
  exit 1
fi
echo 'no third-party UI toolkit in the finished rootfs'
