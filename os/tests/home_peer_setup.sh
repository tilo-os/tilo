#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
helper="$root/crates/hosting/tilo-relay/src/home_peer_control/broker.rs"
enroll="$root/crates/hosting/tilo-relay/src/enroll.rs"
firstboot="$root/os/usr/libexec/tilo-relay-firstboot"
policy="$root/os/usr/share/polkit-1/actions/org.tilo.home-peer.policy"
rule="$root/os/etc/polkit-1/rules.d/49-tilo-home-peer.rules"
firstboot_unit="$root/os/usr/lib/systemd/system/tilo-relay-firstboot.service"
replica_unit="$root/os/usr/lib/systemd/system/tilo-fs-replica.service"
relay_unit="$root/os/usr/lib/systemd/system/tilo-relay.service"
migrate_unit="$root/os/usr/lib/systemd/system/tilo-relay-state-migrate.service"
migrate="$root/os/usr/libexec/tilo-relay-state-migrate"
settings="$root/apps/settings/src/panes/device_mesh.rs"
runner="$root/crates/system/bounded-command/src/lib.rs"
grep -q 'serde(deny_unknown_fields)' "$helper"
grep -q 'MAX_REQUEST' "$helper"
grep -q 'configuration was rolled back' "$helper"
grep -q 'PKEXEC_UID' "$helper"
grep -q 'identity root must be the requesting user' "$helper"
grep -q 'export-home-peer' "$helper"
grep -q '\.mode(0o700)' "$helper"
grep -q 'bounded_command::run_prepared' "$helper"
grep -q 'MAX_HOME_PEER_EXPORT' "$helper"
! grep -q '\.output()' "$helper"
! grep -q 'Command::new("systemctl")' "$helper"
grep -q 'tilo-enroll join' "$firstboot"
grep -q 'tilo-enroll pair' "$firstboot"
grep -q 'home-peer-output /etc/tilo/home-peer.toml' "$firstboot"
grep -q "stat -c '%u:%a:%h:%s'" "$firstboot"
grep -q 'enrollment token must be mode 0600' "$firstboot"
grep -q 'valid_device_certificate && \[\[ -s "\$home_peer" \]\]' "$firstboot"
grep -q 'refusing unsafe device-certificate inode' "$firstboot"
grep -q 'chown --no-dereference tilo-relay:tilo-relay "\$home_peer"' "$firstboot"
grep -q 'refusing unsafe home-peer configuration inode' "$firstboot"
cert_check_line="$(grep -n 'valid_device_certificate && \[\[ -s "\$home_peer" \]\]' "$firstboot" | cut -d: -f1)"
pair_line="$(grep -n 'tilo-enroll pair' "$firstboot" | cut -d: -f1)"
(( cert_check_line < pair_line ))
grep -q 'tilo_files::native::atomic::write_with_mode' "$enroll"
grep -q '0o644' "$enroll"
grep -q 'relay_service_node_id' "$enroll"
grep -q '/usr/libexec/tilo-home-peer-control' "$policy"
! grep -q 'allow_active>yes' "$policy"
grep -q 'subject.local && subject.active && subject.isInGroup("wheel")' "$rule"
grep -q 'Before=tilo-relay.service tilo-fs-replica.service' "$firstboot_unit"
! grep -q '^ConditionPathExists=/etc/tilo/enrollment-token.txt' "$firstboot_unit"
for policy in \
  'NoNewPrivileges=yes' \
  'LimitCORE=0' \
  'ProtectSystem=strict' \
  'PrivateDevices=yes' \
  'ReadWritePaths=/etc/tilo /var/lib/tilo' \
  'CapabilityBoundingSet=CAP_CHOWN' \
  'AmbientCapabilities=' \
  'RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX' \
  'SystemCallFilter=@system-service'; do
  grep -Fqx "$policy" "$firstboot_unit"
done
grep -q 'After=network-online.target tilo-relay-firstboot.service' "$replica_unit"
grep -q 'Requires=tilo-relay-firstboot.service' "$replica_unit"
grep -q 'After=network-online.target tilo-relay-firstboot.service' "$relay_unit"
grep -q 'Requires=tilo-relay-firstboot.service' "$relay_unit"
grep -q 'ConditionPathExists=!/var/lib/tilo-relay/decommissioned' "$relay_unit"
grep -q 'ConditionPathExists=!/var/lib/tilo-relay/decommissioned' "$replica_unit"
for daemon in "$relay_unit" "$replica_unit"; do
  grep -Fqx 'Requires=tilo-relay-state-migrate.service' "$daemon"
  grep -Fqx 'After=tilo-relay-state-migrate.service' "$daemon"
done
grep -q 'BindsTo=tilo-relay.service' "$replica_unit"
grep -Fqx 'RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX AF_NETLINK' "$relay_unit"
for sandboxed_daemon in "$relay_unit" "$replica_unit"; do
 for identity in 'User=tilo-relay' 'Group=tilo-relay' 'SupplementaryGroups=tilo-identity'; do
   grep -Fqx "$identity" "$sandboxed_daemon"
 done
 for policy in \
  'LimitCORE=0' \
  'NoNewPrivileges=yes' \
  'ProtectSystem=strict' \
  'PrivateDevices=yes' \
  'PrivateIPC=yes' \
  'ProtectKernelTunables=yes' \
  'ProtectKernelModules=yes' \
  'ProtectControlGroups=yes' \
  'CapabilityBoundingSet=' \
  'AmbientCapabilities=' \
  'KeyringMode=private' \
  'SystemCallFilter=@system-service' \
  'SystemCallFilter=~@clock @debug @keyring @module @mount @obsolete @raw-io @reboot @resources @swap'; do
   grep -Fqx "$policy" "$sandboxed_daemon"
 done
done
grep -Fqx 'u tilo-relay - "Tilo host relay" /var/lib/tilo-relay /usr/sbin/nologin' \
  "$root/os/usr/lib/sysusers.d/tilo-relay.conf"
grep -Fqx 'm tilo-relay tilo-identity' "$root/os/usr/lib/sysusers.d/tilo-relay.conf"
grep -Fq 'COPY os/usr/lib/sysusers.d/tilo-relay.conf /usr/lib/sysusers.d/tilo-relay.conf' \
  "$root/os/Containerfile.host"
grep -Fq 'systemd-sysusers /usr/lib/sysusers.d/tilo-identity.conf /usr/lib/sysusers.d/tilo-relay.conf' \
  "$root/os/Containerfile.host"
grep -Eq '^d /var/lib/tilo-relay +0700 tilo-relay tilo-relay ' \
  "$root/os/usr/lib/tmpfiles.d/tilo-relay.conf"
grep -Eq '^d /var/lib/tilo/full-replica +0700 tilo-relay tilo-relay ' \
  "$root/os/usr/lib/tmpfiles.d/tilo-relay.conf"
! grep -hEq '^d +/var/lib/(tilo-relay|tilo/full-replica) ' "$root/os/usr/lib/tmpfiles.d/tilo.conf" \
  "$root/os/usr/lib/tmpfiles.d/tilo-host.conf"
grep -Fq 'chown -R --no-dereference --one-file-system tilo-relay:tilo-relay "$state"' "$migrate"
! grep -q '^ConditionPathExists=' "$migrate_unit"
grep -Fq 'tilo-relay-state-migrate: unsafe completion marker' "$migrate"
grep -Fq "stat -c '%u:%a:%h:%s' -- \"\$marker\"" "$migrate"
grep -Fqx 'Before=tilo-relay.service tilo-fs-replica.service' "$migrate_unit"
# shellcheck source=lib/containerfile.sh
. "$root/os/tests/lib/containerfile.sh"
containerfile_has_copy "$root/os/Containerfile" \
  os/usr/lib/systemd/system/tilo-relay-firstboot.service \
  /usr/lib/systemd/system/tilo-relay-firstboot.service
# The desktop image runs tilo-relay.service too once it serves as a home peer.
for image in Containerfile Containerfile.host; do
  containerfile_has_copy "$root/os/$image" \
    os/usr/lib/systemd/system/tilo-relay-state-migrate.service \
    /usr/lib/systemd/system/tilo-relay-state-migrate.service
  containerfile_has_copy "$root/os/$image" \
    os/usr/libexec/tilo-relay-state-migrate /usr/libexec/tilo-relay-state-migrate
  containerfile_has_copy "$root/os/$image" \
    os/usr/lib/tmpfiles.d/tilo-relay.conf /usr/lib/tmpfiles.d/tilo-relay.conf
done
grep -q 'bounded_command::spawn_prepared' "$settings"
grep -q 'const APPLY_TIMEOUT' "$settings"
grep -q 'const MANAGEMENT_TIMEOUT' "$settings"
! grep -q '\.spawn()' "$settings"
# Nearby pairing runs on the host behind actions: Settings starts neither
# discovery nor the privileged approval itself.
! grep -q 'tilo-enroll' "$settings"
! grep -Eq 'approve-enrollment|approve-device' "$settings"
pairing="$root/crates/session/shell/src/shell/device_pairing.rs"
grep -q 'bounded_command::spawn_prepared' "$pairing"
grep -q 'const DISCOVERY_TIMEOUT' "$pairing"
grep -q 'const APPROVAL_TIMEOUT' "$pairing"
grep -q '/usr/libexec/tilo-home-peer-control' "$pairing"
! grep -q '\.spawn()' "$pairing"
grep -q 'impl Drop for AsyncCommand' "$runner"
grep -q 'self.cancel.store(true' "$runner"
echo "home-peer setup boundary: ok"
