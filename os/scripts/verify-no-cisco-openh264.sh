#!/usr/bin/env bash
# Tilo never distributes Cisco's OpenH264 binary: Cisco's patent licence covers
# only binaries the user downloads from Cisco. The image carries Fedora's
# `noopenh264` stub for libopenh264.so; Cisco's build reaches a device only
# through the user-downloaded media-codecs extension, whose overlay shadows the
# stub at the same path (os/usr/share/doc/tilo/codec-pack.md).
#
#   verify-no-cisco-openh264.sh            check the rootfs this runs in
#   verify-no-cisco-openh264.sh --repo DIR check the RPMs in a package repository
#
# Run inside the finished rootfs after the last package change, and over the
# frozen RPM snapshot every shipped image installs from.
set -euo pipefail
rpm_bin="${TILO_RPM_BIN:-rpm}"
root="${TILO_ROOT:-}"

# Packages built from Cisco's OpenH264 or loading it: Fedora's
# fedora-cisco-openh264 repository (openh264, openh264-devel,
# mozilla-openh264) and the GStreamer plugin, which is only useful with it.
forbidden='^(openh264(-.+)?|mozilla-openh264|gstreamer1-plugin-openh264)$'

fail() {
  echo "Cisco OpenH264 must not ship in a Tilo image: $*" >&2
  exit 1
}

if [ "${1:-}" = --repo ]; then
  [ $# -eq 2 ] && [ -d "$2" ] || { echo "usage: $0 [--repo DIR]" >&2; exit 2; }
  shopt -s nullglob
  rpms=("$2"/*.rpm)
  [ ${#rpms[@]} -gt 0 ] || fail "no RPMs in $2"
  names="$("$rpm_bin" -qp --nosignature --nodigest --qf '%{NAME}\n' "${rpms[@]}")"
  found="$(grep -E "$forbidden" <<<"$names" | sort -u || true)"
  [ -z "$found" ] || fail "the package repository $2 carries $(tr '\n' ' ' <<<"$found")"
  echo "no Cisco OpenH264 package in $2"
  exit 0
fi
[ $# -eq 0 ] || { echo "usage: $0 [--repo DIR]" >&2; exit 2; }

installed="$("$rpm_bin" -qa --qf '%{NAME}\n')"
found="$(grep -E "$forbidden" <<<"$installed" | sort -u || true)"
[ -z "$found" ] || fail "installed: $(tr '\n' ' ' <<<"$found")"

# Whatever provides libopenh264 must be the stub, not a copy from elsewhere.
shopt -s nullglob
for library in "$root"/usr/lib64/libopenh264.so* "$root"/usr/lib/libopenh264.so*; do
  owner="$("$rpm_bin" -qf --qf '%{NAME}\n' "${library#"$root"}" 2>/dev/null)" || owner=''
  [ "$owner" = noopenh264 ] || fail "${library#"$root"} is owned by '${owner:-no package}', not noopenh264"
done
for plugin in "$root"/usr/lib64/gstreamer-1.0/libgstopenh264.so "$root"/usr/lib/gstreamer-1.0/libgstopenh264.so; do
  [ ! -e "$plugin" ] || fail "${plugin#"$root"} is present"
done
echo 'no Cisco OpenH264 in the finished rootfs'
