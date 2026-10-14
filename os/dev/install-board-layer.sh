#!/usr/bin/env bash
# Install the board dev image's system configuration into / (the board-root
# stage of os/Containerfile.board). Runs after prepare-hardware-profile.py,
# whose installed definition says whether this board is an always-on host.
#
#   install-board-layer.sh SOURCE
#
# SOURCE is the board-inputs tree (the repository paths below, under it).
#
# Every board: sshd keeps restarting, since SSH is how a board dev image is
# reached before it has a display, brcmfmac keeps its permanent MAC, and
# Energy Efficient Ethernet is turned off on the onboard end0 (its PHY drops
# the cable after idle, which looks like SSH dying minutes after boot).
#
# An always-on board (`"always_on": true`) also gets the board session:
# tilo-board-headless-session@<account> runs the person's session headless,
# or on a display once one is connected. greetd is masked rather than
# disabled, because another unit can still start a disabled greeter, and a
# greeter that cannot open a display restart-loops. Sleep is masked: the
# board is the host the person's other devices reach. Chrony accepts one
# NTS handshake before the certificate's validity can be checked, since the
# board has no battery-backed clock.
set -euo pipefail

src=${1:?usage: install-board-layer.sh SOURCE}

install -D -m 0644 "$src/os/usr/lib/systemd/system/sshd.service.d/20-tilo-board.conf" \
  /usr/lib/systemd/system/sshd.service.d/20-tilo-board.conf
install -D -m 0644 "$src/os/etc/NetworkManager/conf.d/10-tilo-brcmfmac.conf" \
  /etc/NetworkManager/conf.d/10-tilo-brcmfmac.conf
install -D -m 0755 "$src/os/usr/lib/NetworkManager/dispatcher.d/10-tilo-disable-eee" \
  /usr/lib/NetworkManager/dispatcher.d/10-tilo-disable-eee
test -x /usr/sbin/ethtool || { echo 'the EEE dispatcher needs /usr/sbin/ethtool' >&2; exit 1; }

# Per-board firmware trim: remove the vendor firmware RPMs the board's reviewed
# definition lists in `firmware_exclude` (os/dev/hardware_profile.py allows only
# separable packages and refuses any whose tree the definition itself names).
# --no-autoremove keeps dnf from widening the transaction; a package the base
# does not carry is skipped; the board's own system_firmware must survive.
definition=/usr/lib/tilo/hardware-profile-definition.json
json_list() {
  python3 -B -c 'import json, sys; print("\n".join(json.load(open(sys.argv[1])).get(sys.argv[2], [])))' "$definition" "$1"
}
installed=()
while IFS= read -r package; do
  [ -n "$package" ] || continue
  if rpm -q "$package" >/dev/null 2>&1; then installed+=("$package"); fi
done < <(json_list firmware_exclude)
if [ "${#installed[@]}" -gt 0 ]; then
  dnf remove -y --no-autoremove "${installed[@]}"
  for package in "${installed[@]}"; do
    ! rpm -q "$package" >/dev/null 2>&1 || { echo "$package is still installed" >&2; exit 1; }
  done
  while IFS= read -r path; do
    [ -n "$path" ] || continue
    if [ ! -e "/usr/lib/firmware/$path" ] && [ ! -e "/usr/lib/firmware/$path.xz" ] \
        && [ ! -e "/usr/lib/firmware/$path.zst" ]; then
      echo "firmware trim removed required $path" >&2
      exit 1
    fi
  done < <(json_list system_firmware)
fi

always_on=$(python3 -B -c 'import json, sys; print(json.load(open(sys.argv[1])).get("always_on", False))' \
  /usr/lib/tilo/hardware-profile-definition.json)
[ "$always_on" = True ] || exit 0

install -D -m 0644 "$src/os/usr/lib/systemd/system/tilo-board-headless-session@.service" \
  /usr/lib/systemd/system/tilo-board-headless-session@.service
for helper in tilo-board-session-prepare tilo-seat-session tilo-display-present; do
  install -D -m 0755 "$src/os/usr/libexec/$helper" "/usr/libexec/$helper"
done
install -d -m 0755 /usr/share/tilo
: > /usr/share/tilo/board-headless-session
systemctl mask greetd.service sleep.target suspend.target hibernate.target hybrid-sleep.target
systemctl set-default multi-user.target
grep -q '^nocerttimecheck ' /etc/chrony.conf \
  || printf '\n# No battery-backed clock: trust the first NTS certificate before the\n# clock can be checked against it. Later handshakes are checked again.\nnocerttimecheck 1\n' \
    >> /etc/chrony.conf
