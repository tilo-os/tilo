#!/usr/bin/env bash
set -euo pipefail

repo="$(cd "$(dirname "$0")/../.." && pwd)"
work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT
mkdir -p "$work/zram0"
mkdir -p "$work/block"
printf 'lzo lzo-rle [lz4] zstd\n' >"$work/zram0/comp_algorithm"
export TILO_ZRAM_SYSFS="$work/zram0"
export TILO_BLOCK_SYSFS="$work/block"
export TILO_NTSYNC_DEVICE=/dev/null
export TILO_GREETD_SOCKET="$work/greetd.sock"
python3 - "$TILO_GREETD_SOCKET" <<'PY'
import socket
import sys
sock = socket.socket(socket.AF_UNIX)
sock.bind(sys.argv[1])
sock.close()
PY

cat >"$work/systemctl" <<'EOF'
#!/usr/bin/env bash
if [ "${1:-}" = is-active ]; then
  case "${3:-}" in
    graphical.target) [ "${TILO_TEST_GRAPHICAL_ACTIVE:-yes}" = yes ] ;;
    greetd.service) [ "${TILO_TEST_GREETD_ACTIVE:-yes}" = yes ] ;;
    *) exit 1 ;;
  esac
  exit
fi
case "${2:-}" in
  --property=InitRDTimestampMonotonic) printf '%s\n' "${TILO_TEST_INITRD_US:-}" ;;
  --property=UserspaceTimestampMonotonic) printf '%s\n' "${TILO_TEST_USERSPACE_US:-}" ;;
  graphical.target) printf '%s\n' "${TILO_TEST_GRAPHICAL_US:-${TILO_TEST_ACTIVE_US:?}}" ;;
  greetd.service) printf '%s\n' "${TILO_TEST_GREETD_US:-${TILO_TEST_ACTIVE_US:?}}" ;;
  *) exit 1 ;;
esac
EOF
cat >"$work/systemd-analyze" <<'EOF'
#!/usr/bin/env bash
printf 'mock critical chain\n' >&2
EOF
cat >"$work/swapon" <<'EOF'
#!/usr/bin/env bash
if [ "${TILO_TEST_ZRAM_SWAP:-yes}" = yes ]; then
  printf '/dev/zram0\n'
fi
[ -z "${TILO_TEST_EXTRA_SWAP:-}" ] || printf '%s\n' "$TILO_TEST_EXTRA_SWAP"
EOF
chmod +x "$work/systemctl" "$work/systemd-analyze" "$work/swapon"

PATH="$work:$PATH" TILO_TEST_ACTIVE_US=19999000 \
  "$repo/os/tests/boot_performance_gate.sh" >"$work/pass"
grep -Fq 'kernel+userspace reached usable greeter in 19.999s (budget: 20s)' "$work/pass"

if PATH="$work:$PATH" TILO_TEST_ACTIVE_US=20000001 \
  "$repo/os/tests/boot_performance_gate.sh" >"$work/fail.out" 2>"$work/fail.err"; then
  echo 'boot performance gate accepted an over-budget boot' >&2
  exit 1
fi
grep -Fq '20.000s (budget: 20s)' "$work/fail.err"
grep -Fq 'mock critical chain' "$work/fail.err"

PATH="$work:$PATH" TILO_TEST_ACTIVE_US=19000000 TILO_TEST_INITRD_US=4000000 \
  TILO_TEST_USERSPACE_US=14000000 "$repo/os/tests/boot_performance_gate.sh" >"$work/phases"
grep -Fq 'phases: kernel 4.000s, initrd 10.000s, userspace to greeter 5.000s' "$work/phases"

if PATH="$work:$PATH" TILO_TEST_ACTIVE_US=10000000 TILO_TEST_GREETD_US=20000001 \
  "$repo/os/tests/boot_performance_gate.sh" >"$work/late-greeter.out" 2>"$work/late-greeter.err"; then
  echo 'boot performance gate accepted a greeter that became usable over budget' >&2
  exit 1
fi
grep -Fq 'usable greeter in 20.000s (budget: 20s)' "$work/late-greeter.err"

if PATH="$work:$PATH" TILO_TEST_ACTIVE_US=invalid \
  "$repo/os/tests/boot_performance_gate.sh" >/dev/null 2>"$work/invalid"; then
  echo 'boot performance gate accepted an invalid timestamp' >&2
  exit 1
fi
grep -Fq 'no monotonic activation timestamp' "$work/invalid"

printf 'lzo [zstd] lz4\n' >"$work/zram0/comp_algorithm"
if PATH="$work:$PATH" TILO_TEST_ACTIVE_US=1000 \
  "$repo/os/tests/boot_performance_gate.sh" >/dev/null 2>"$work/wrong-zram"; then
  echo 'boot performance gate accepted a high-latency zram compressor' >&2
  exit 1
fi
grep -Fq 'zram0 active compressor is not lz4' "$work/wrong-zram"
printf 'lzo lzo-rle [lz4] zstd\n' >"$work/zram0/comp_algorithm"

if PATH="$work:$PATH" TILO_TEST_ACTIVE_US=1000 TILO_TEST_ZRAM_SWAP=no \
  "$repo/os/tests/boot_performance_gate.sh" >/dev/null 2>"$work/inactive-zram"; then
  echo 'boot performance gate accepted an inactive zram swap device' >&2
  exit 1
fi
grep -Fq 'zram0 is not active swap' "$work/inactive-zram"

if PATH="$work:$PATH" TILO_TEST_ACTIVE_US=1000 TILO_TEST_EXTRA_SWAP=/dev/nvme0n1p3 \
  "$repo/os/tests/boot_performance_gate.sh" >/dev/null 2>"$work/persistent-swap"; then
  echo 'boot performance gate accepted persistent disk swap beside zram' >&2
  exit 1
fi
grep -Fq 'persistent or unexpected swap is active' "$work/persistent-swap"

if PATH="$work:$PATH" TILO_TEST_ACTIVE_US=1000 TILO_NTSYNC_DEVICE="$work/not-a-device" \
  "$repo/os/tests/boot_performance_gate.sh" >/dev/null 2>"$work/no-ntsync"; then
  echo 'boot performance gate accepted a missing ntsync device' >&2
  exit 1
fi
grep -Fq 'Wine/Proton ntsync device is unavailable' "$work/no-ntsync"

mkdir -p "$work/block/sda/queue"
printf 'mq-deadline [bfq] none\n' >"$work/block/sda/queue/scheduler"
PATH="$work:$PATH" TILO_TEST_ACTIVE_US=1000 \
  "$repo/os/tests/boot_performance_gate.sh" >/dev/null
printf '[mq-deadline] bfq none\n' >"$work/block/sda/queue/scheduler"
if PATH="$work:$PATH" TILO_TEST_ACTIVE_US=1000 \
  "$repo/os/tests/boot_performance_gate.sh" >/dev/null 2>"$work/wrong-scheduler"; then
  echo 'boot performance gate accepted non-BFQ scheduling on a physical disk' >&2
  exit 1
fi
grep -Fq 'physical disk BFQ scheduler is not active' "$work/wrong-scheduler"
printf 'mq-deadline [bfq] none\n' >"$work/block/sda/queue/scheduler"

if PATH="$work:$PATH" TILO_TEST_ACTIVE_US=1000 TILO_TEST_GRAPHICAL_ACTIVE=no \
  "$repo/os/tests/boot_performance_gate.sh" >/dev/null 2>"$work/inactive"; then
  echo 'boot performance gate accepted an inactive graphical target' >&2
  exit 1
fi
grep -Fq 'graphical.target is not active' "$work/inactive"

if PATH="$work:$PATH" TILO_TEST_ACTIVE_US=1000 TILO_TEST_GREETD_ACTIVE=no \
  "$repo/os/tests/boot_performance_gate.sh" >/dev/null 2>"$work/no-greeter"; then
  echo 'boot performance gate accepted a failed display manager' >&2
  exit 1
fi
grep -Fq 'greetd.service is not active' "$work/no-greeter"

if PATH="$work:$PATH" TILO_TEST_ACTIVE_US=1000 TILO_GREETD_SOCKET="$work/missing-greetd.sock" \
  "$repo/os/tests/boot_performance_gate.sh" >/dev/null 2>"$work/no-greeter-socket"; then
  echo 'boot performance gate accepted a display manager without a login endpoint' >&2
  exit 1
fi
grep -Fq 'greetd login endpoint is unavailable' "$work/no-greeter-socket"

if PATH="$work:$PATH" TILO_TEST_ACTIVE_US=1000 TILO_BOOT_USERSPACE_BUDGET_SEC=99999999999999999999 \
  "$repo/os/tests/boot_performance_gate.sh" >/dev/null 2>"$work/huge-budget"; then
  echo 'boot performance gate accepted an overflow-prone budget' >&2
  exit 1
fi
grep -Fq 'boot budget must be between 1 and 600 seconds' "$work/huge-budget"

fs_unit="$repo/os/usr/lib/systemd/system/fs.service"
if grep -Eq '^(After|Wants)=.*network-online.target|^Before=.*(greetd|graphical)' "$fs_unit"; then
  echo 'collaborative filesystem still blocks login on network readiness' >&2
  exit 1
fi
grep -Fq 'After=local-fs.target network.target systemd-tmpfiles-setup.service' "$fs_unit"
grep -Fq "TILO_BOOT_USERSPACE_BUDGET_SEC=20 /usr/libexec/tilo/boot-performance-gate.sh" \
  "$repo/.forgejo/workflows/build-image.yml"

# Native Tilo performs personal setup in the compositor OOBE. The old system
# unit only created /root/.config, yet its oneshot delayed multi-user.target
# and therefore the greeter on every fresh installation.
if grep -Fq 'systemctl enable shell-firstboot.service' "$repo/os/Containerfile"; then
  echo 'root-only shell firstboot work still blocks the native login path' >&2
  exit 1
fi
grep -Fq 'systemd-timesyncd.service' "$repo/os/Containerfile" \
  || { echo 'redundant systemd time synchronization can re-enter the boot path' >&2; exit 1; }
grep -Fq 'systemd-network-generator.service' "$repo/os/Containerfile" \
  || { echo 'unused kernel-command-line network generation can re-enter sysinit' >&2; exit 1; }
grep -Fq 'systemd-pstore.service' "$repo/os/Containerfile" \
  || { echo 'persistent crash-log copying can re-enter the login path' >&2; exit 1; }
grep -Fq 'systemd-coredump.socket' "$repo/os/Containerfile" \
  || { echo 'inherited core-dump collector can remain socket-activated' >&2; exit 1; }
grep -Fq 'systemd-binfmt.service' "$repo/os/Containerfile" \
  || { echo 'unused binary-format registration can re-enter sysinit' >&2; exit 1; }
for pcrlock_unit in \
  systemd-pcrlock-file-system.service \
  systemd-pcrlock-firmware-code.service \
  systemd-pcrlock-firmware-config.service \
  systemd-pcrlock-machine-id.service \
  systemd-pcrlock-make-policy.service \
  systemd-pcrlock-secureboot-authority.service \
  systemd-pcrlock-secureboot-policy.service; do
  grep -Fq "$pcrlock_unit" "$repo/os/Containerfile" || {
    echo "unused TPM boot collector can re-enter the login path: $pcrlock_unit" >&2
    exit 1
  }
done
shell_firstboot_unit="$repo/os/usr/lib/systemd/system/shell-firstboot.service"
if grep -Eq '^(After|Wants)=.*network-online.target' "$shell_firstboot_unit"; then
  echo 'local-only machine firstboot setup still waits for network readiness' >&2
  exit 1
fi
if grep -Eq 'systemctl enable (fs\.service|tilo-security-audit\.service)' \
    "$repo/os/Containerfile"; then
  echo 'background filesystem or audit work still blocks multi-user.target' >&2
  exit 1
fi
grep -Fq 'systemctl enable tilo-fs-start.timer fs-identity.path' "$repo/os/Containerfile"
grep -Fq 'systemctl enable tilo-security-audit.timer' "$repo/os/Containerfile"
if grep -Eq 'systemctl enable (NetworkManager|firewalld|chronyd)\.service' "$repo/os/Containerfile"; then
  echo 'network, firewall, or clock readiness still blocks multi-user.target' >&2
  exit 1
fi
grep -Fq 'systemctl disable NetworkManager.service firewalld.service chronyd.service' "$repo/os/Containerfile"
grep -Fq 'systemctl enable tilo-network-start.timer' "$repo/os/Containerfile"
network_timer="$repo/os/usr/lib/systemd/system/tilo-network-start.timer"
grep -Fqx 'OnBootSec=0' "$network_timer"
grep -Fqx 'AccuracySec=1ms' "$network_timer"
grep -Fqx 'Unit=tilo-network-start.target' "$network_timer"
if grep -Eq '^(Before|After|Wants|Requires)=.*(greetd|graphical)' "$network_timer"; then
  echo 'asynchronous network start gained login-path ordering' >&2
  exit 1
fi
network_target="$repo/os/usr/lib/systemd/system/tilo-network-start.target"
grep -Fqx 'Requires=firewalld.service NetworkManager.service' "$network_target"
grep -Fqx 'Wants=chronyd.service' "$network_target"
grep -Fqx 'After=firewalld.service' "$network_target"
network_manager_firewall="$repo/os/usr/lib/systemd/system/NetworkManager.service.d/20-tilo-firewall-first.conf"
grep -Fqx 'Requires=firewalld.service' "$network_manager_firewall"
grep -Fqx 'After=firewalld.service' "$network_manager_firewall"
grep -Fq 'OnUnitInactiveSec=1h' \
  "$repo/os/usr/lib/systemd/system/tilo-security-audit.timer"
grep -Fqx 'OnBootSec=2min' "$repo/os/usr/lib/systemd/system/tilo-security-audit.timer"
grep -Fqx 'CPUSchedulingPolicy=idle' "$repo/os/usr/lib/systemd/system/tilo-security-audit.service"
grep -Fqx 'IOSchedulingClass=idle' "$repo/os/usr/lib/systemd/system/tilo-security-audit.service"
if grep -Fq 'RemainAfterExit=yes' \
    "$repo/os/usr/lib/systemd/system/tilo-security-audit.service"; then
  echo 'security audit oneshot remains active and cannot recur from its timer' >&2
  exit 1
fi
if grep -Eq '^ExecStart=.*--apply' \
    "$repo/os/usr/lib/systemd/system/bootc-fetch-apply-updates.service.d/20-tilo-update-policy.conf"; then
  echo 'automatic update service can reboot an active graphical session' >&2
  exit 1
fi
grep -Fq 'systemctl enable tilo-growfs-sysroot.timer' "$repo/os/Containerfile"
grep -Fq 'OnBootSec=5s' "$repo/os/usr/lib/systemd/system/tilo-fs-start.timer"
grep -Fq 'OnBootSec=5s' "$repo/os/usr/lib/systemd/system/tilo-growfs-sysroot.timer"
grep -Fq 'systemctl enable tilo-rollback-harden.timer' "$repo/os/Containerfile"
grep -Fq 'OnBootSec=5s' "$repo/os/usr/lib/systemd/system/tilo-rollback-harden.timer"
if grep -Fq 'systemctl enable tilo-rollback-harden.service' "$repo/os/Containerfile"; then
  echo 'one-time TPM mutation still blocks multi-user.target' >&2
  exit 1
fi
grep -Fq 'systemctl enable tilo-boot-entries.timer tilo-boot-health.service' "$repo/os/Containerfile"
if grep -Fq 'systemctl enable tilo-growfs-sysroot.service' "$repo/os/Containerfile"; then
  echo 'online root growth still blocks first login' >&2
  exit 1
fi
device_key_unit="$repo/os/usr/lib/systemd/system/tilo-device-key-init.service"
grep -Fqx 'ConditionPathExists=!/var/lib/tilo/device-private/keypair.bin' "$device_key_unit"
if grep -Fq 'tilo-identity.service' "$repo/os/Containerfile"; then
  echo 'a pre-session identity unit is back in the login path' >&2
  exit 1
fi
if grep -Fq 'systemctl enable tilo-boot-entries.service' "$repo/os/Containerfile"; then
  echo 'boot-entry ESP maintenance still blocks first login' >&2
  exit 1
fi
if grep -Eq 'systemctl --global enable (tilo-models|tilo-model-gateway)\.service' \
    "$repo/os/Containerfile"; then
  echo 'optional model daemons still compete with first-session startup' >&2
  exit 1
fi
grep -Fq 'systemctl --global enable tilo-models.socket' "$repo/os/Containerfile"
grep -Fq 'systemctl --global enable tilo-model-gateway.socket' "$repo/os/Containerfile"
session_target="$repo/os/usr/lib/systemd/user/tilo-graphical-session.target"
if grep -Eq '^Wants=.*tilo-(models|model-gateway|app-host)\.service' "$session_target"; then
  echo 'socket-activated app/model daemons still start eagerly with the graphical session' >&2
  exit 1
fi
grep -Fqx 'Wants=tilo-ush.service' "$session_target"
for model_unit in tilo-models.service tilo-model-gateway.service; do
  if grep -Eq '^(After|Wants)=.*network-online\.target' \
      "$repo/os/usr/lib/systemd/user/$model_unit"; then
    echo "$model_unit waits for network readiness before socket activation" >&2
    exit 1
  fi
done
for model_socket in tilo-models.socket tilo-model-gateway.socket; do
  grep -Fqx 'PartOf=tilo-graphical-session.target' "$repo/os/usr/lib/systemd/user/$model_socket"
  grep -Fqx 'WantedBy=tilo-graphical-session.target' "$repo/os/usr/lib/systemd/user/$model_socket"
done
if grep -Fq 'systemctl --global enable tilo-app-host.service' "$repo/os/Containerfile"; then
  echo 'app-host broker still starts eagerly at every graphical login' >&2
  exit 1
fi
grep -Fq 'systemctl --global enable tilo-app-host.socket' "$repo/os/Containerfile"
if grep -Eq '^[[:space:]]+greetd-tuigreet([[:space:]\\]|$)' "$repo/os/Containerfile"; then
  echo 'unused text greeter remains installed beside the graphical greeter' >&2
  exit 1
fi
app_host_socket="$repo/os/usr/lib/systemd/user/tilo-app-host.socket"
app_host_service="$repo/os/usr/lib/systemd/user/tilo-app-host.service"
cmp -s "$app_host_service" "$repo/crates/packaging/app-host/systemd/tilo-app-host.service" || {
  fail 'source-packaged and OS app-host service sandboxes have drifted'
}
cmp -s "$app_host_socket" "$repo/crates/packaging/app-host/systemd/tilo-app-host.socket" || {
  fail 'source-packaged and OS app-host activation sockets have drifted'
}
app_host_source="$repo/crates/packaging/app-host/src/main.rs"
app_host_rpc="$repo/crates/packaging/app-host/src/service.rs"
grep -Fqx 'ListenStream=%t/tilo/services/app-host.sock' "$app_host_socket"
grep -Fqx 'SocketMode=0600' "$app_host_socket"
grep -Fqx 'Service=tilo-app-host.service' "$app_host_socket"
grep -Fqx 'Backlog=16' "$app_host_socket" \
  || fail 'tilo-app-host activation queue is unbounded'
for protection in ProtectKernelTunables ProtectKernelModules ProtectKernelLogs \
                  ProtectControlGroups ProtectClock ProtectHostname; do
  grep -Fqx "$protection=yes" "$app_host_service" || {
    fail "tilo-app-host sandbox is missing $protection=yes"
  }
done
grep -Fqx 'KeyringMode=private' "$app_host_service" \
  || fail 'tilo-app-host shares the session keyring'
grep -Fqx 'CapabilityBoundingSet=' "$app_host_service" \
  || fail 'tilo-app-host retains an ambient capability set'
if grep -Eq '^\[Install\]|^WantedBy=' "$app_host_service"; then
  fail 'tilo-app-host service remains directly enableable instead of socket-only'
fi
grep -Fqx 'RuntimeDirectoryPreserve=yes' "$app_host_service"
grep -Fq 'std::env::var("LISTEN_PID")' "$app_host_source"
grep -Fq 'actual.as_pathname() == Some(expected)' "$app_host_source"
grep -Fq 'bounded_command::run_prepared(' "$app_host_rpc" \
  || fail 'app-host RPC trusts unbounded package-manager subprocesses'
if grep -Fq '.output()' "$app_host_rpc"; then
  fail 'app-host RPC can capture unbounded helper output'
fi

if grep -Fq 'systemctl --global enable tilo-ush.service' "$repo/os/Containerfile"; then
  echo 'confined shell supervisor still starts eagerly at every user login' >&2
  exit 1
fi
grep -Fq 'systemctl --global enable tilo-ush.socket' "$repo/os/Containerfile"
ush_socket="$repo/os/usr/lib/systemd/user/tilo-ush.socket"
ush_service="$repo/os/usr/lib/systemd/user/tilo-ush.service"
ush_source="$repo/crates/runtime/ush/src/supervisor.rs"
grep -Fqx 'ListenStream=%t/tilo/services/ush.sock' "$ush_socket"
grep -Fqx 'Service=tilo-ush.service' "$ush_socket"
grep -Fqx 'SocketMode=0600' "$ush_socket"
grep -Fqx 'Backlog=16' "$ush_socket"
grep -Fqx 'ProtectHostname=private' "$ush_service"
grep -Fqx 'PartOf=tilo-graphical-session.target' "$ush_service"
if grep -Fqx 'ProtectHostname=yes' "$ush_service"; then
  fail 'ush supervisor denies Bubblewrap the nested guest hostname'
fi
for incompatible in ProtectKernelTunables ProtectKernelModules ProtectKernelLogs; do
  if grep -Eq "^${incompatible}=(yes|true|1)$" "$ush_service"; then
    fail "ush supervisor blocks nested guest mounts with $incompatible"
  fi
done
grep -Fqx 'PartOf=tilo-graphical-session.target' "$ush_socket"
grep -Fqx 'WantedBy=tilo-graphical-session.target' "$ush_socket"
grep -Fq 'std::env::var("LISTEN_PID")' "$ush_source"
grep -Fq 'listener.local_addr()?.as_pathname() == Some(expected)' "$ush_source"
ush_vm_probe="$repo/os/tests/ush-systemd-terminal-vm.sh"
grep -Fq 'systemctl --user start tilo-ush.socket' "$ush_vm_probe"
grep -Fq 'ush-systemd-terminal-probe.py' "$ush_vm_probe"
grep -Fq 'TILO_USH_SYSTEMD_OK-root' "$repo/os/tests/ush-systemd-terminal-probe.py"

if grep -Fq 'systemctl --global enable tilo-fs-recipient.service' "$repo/os/Containerfile"; then
  echo 'received-files daemon still starts eagerly in every graphical session' >&2
  exit 1
fi
grep -Fq 'systemctl --global enable tilo-fs-recipient.socket' "$repo/os/Containerfile"
recipient_socket="$repo/os/usr/lib/systemd/user/tilo-fs-recipient.socket"
grep -Fqx 'ListenStream=%t/tilo/services/tilo-fs.sock' "$recipient_socket"
grep -Fqx 'SocketMode=0600' "$recipient_socket"
grep -Fqx 'Backlog=16' "$recipient_socket"
# Activation may omit a pathname; an explicit path must still be this socket.
recipient_src="$repo/crates/system/fs/src/main.rs"
if ! grep -A3 -F 'if let Some(actual) = listener.local_addr()?.as_pathname()' "$recipient_src"   | grep -Fq 'actual == socket'; then
  echo 'recipient-service no longer requires the activation listener to be the expected socket' >&2
  exit 1
fi
app_host_gc_line="$(grep -n 'let _startup = tokio::task::spawn_blocking' "$app_host_source" | cut -d: -f1)"
app_host_serve_line="$(grep -n 'transport::serve(listener, store' "$app_host_source" | cut -d: -f1)"
if [ -z "$app_host_gc_line" ] || [ -z "$app_host_serve_line" ] \
   || [ "$app_host_gc_line" -ge "$app_host_serve_line" ]; then
  echo 'socket-activated app host still blocks first use on its store migration and GC pass' >&2
  exit 1
fi
grep -Fq 'Unit=tilo-boot-entries.service' "$repo/os/usr/lib/systemd/system/tilo-boot-entries.timer"
grep -Fq 'OnBootSec=30s' "$repo/os/usr/lib/systemd/system/tilo-boot-entries.timer"
grep -Fq 'systemctl enable tilo-secureboot-sign.timer tilo-secureboot-sign.path' \
  "$repo/os/Containerfile"
if grep -Fq 'systemctl enable tilo-secureboot-sign.service' "$repo/os/Containerfile"; then
  echo 'Secure Boot ESP signing still blocks first login' >&2
  exit 1
fi
grep -Fq 'Unit=tilo-secureboot-sign.service' \
  "$repo/os/usr/lib/systemd/system/tilo-secureboot-sign.timer"
grep -Fq 'OnBootSec=30s' "$repo/os/usr/lib/systemd/system/tilo-secureboot-sign.timer"
grep -Fq 'systemctl enable tilo-system-extension-boot-pack-status.timer' \
  "$repo/os/Containerfile"
if grep -Fq 'systemctl enable tilo-system-extension-boot-pack-status.service' \
  "$repo/os/Containerfile"; then
  echo 'BootPack status projection still blocks multi-user.target on staged systems' >&2
  exit 1
fi
grep -Fq 'OnBootSec=5s' \
  "$repo/os/usr/lib/systemd/system/tilo-system-extension-boot-pack-status.timer"
boot_pack_stage="$repo/os/usr/lib/systemd/system/tilo-system-extension-boot-pack-stage.service"
for policy in 'Nice=10' 'CPUWeight=20' 'IOWeight=20' \
              'IOSchedulingClass=best-effort' 'IOSchedulingPriority=7'; do
  grep -Fqx "$policy" "$boot_pack_stage" || {
    echo "BootPack staging lost background resource policy: $policy" >&2
    exit 1
  }
done

# The composefs boot-chain VM must exercise the same activation topology as the
# production image; otherwise its rollback tests can hide a critical-path
# regression by continuing to enable the old oneshot directly.
grep -Fq 'usr/lib/systemd/system/tilo-boot-entries.timer' \
  "$repo/os/dev/composefs/build.sh"
grep -Fq 'systemctl enable tilo-boot-entries.timer tilo-boot-health.service' \
  "$repo/os/dev/composefs/Containerfile"
if grep -Fq 'systemctl enable tilo-boot-entries.service' \
    "$repo/os/dev/composefs/Containerfile"; then
  echo 'composefs VM still exercises blocking boot-entry maintenance' >&2
  exit 1
fi

grep -Fq '/usr/lib/systemd/systemd-update-done --root=/' "$repo/os/Containerfile" || {
  echo 'first boot regenerates the linker cache on the login critical chain' >&2
  exit 1
}

for preset_wants in multi-user.target.wants/ModemManager.service \
    graphical.target.wants/power-profiles-daemon.service \
    multi-user.target.wants/virtqemud.service; do
  grep -Fq "/etc/systemd/system/$preset_wants" "$repo/os/Containerfile" || {
    echo "the preset still starts $preset_wants before login" >&2
    exit 1
  }
done

for demand_activated in bluetooth ModemManager fwupd power-profiles-daemon systemd-homed; do
  if grep -Fq "systemctl enable $demand_activated.service" "$repo/os/Containerfile"; then
    echo "$demand_activated is still started before login instead of on demand" >&2
    exit 1
  fi
done

printf 'ok: boot performance gate enforces its userspace deadline and diagnostics\n'
