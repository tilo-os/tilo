#!/usr/bin/env bash
# Prevent the configured kernel/runtime hardening baseline from drifting away
# from the live boot audit. A setting that is only written to a drop-in is an
# intention, not evidence: an /etc override or unsupported kernel can defeat it.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
sysctl_file="$repo/os/usr/lib/sysctl.d/60-tilo-hardening.conf"
audit="$repo/os/usr/libexec/tilo-security-audit"
kernel_config="$repo/os/kernel/config-fragments/tilo.config"
arm64_kernel_config="$repo/os/kernel/config-fragments/aarch64.config"
x86_kernel_config="$repo/os/kernel/config-fragments/x86_64.config"
performance_sysctl="$repo/os/usr/lib/sysctl.d/70-tilo-performance.conf"
image_recipe="$repo/os/Containerfile"
initramfs_recipe="$repo/os/scripts/build-initramfs.sh"
kargs="$repo/os/usr/lib/bootc/kargs.d/01-hardening.toml"
fail=0

bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }
ok() { printf 'ok    %s\n' "$*"; }

fragment_has() {
  case "$1" in
    CONFIG_X86_*|CONFIG_DEVPORT=*|CONFIG_IA32_EMULATION=*|CONFIG_USELIB=*|CONFIG_LEGACY_VSYSCALL_*|CONFIG_RANDOMIZE_MEMORY=*|CONFIG_SCHED_MC_PRIO=*|CONFIG_SND_SOC_INTEL_AVS_MACH_I2S_TEST=*|CONFIG_USB_GADGET=*|CONFIG_ACPI_EXTLOG=*)
      grep -Fqx "$1" "$x86_kernel_config" ;;
    *) grep -Fqx "$1" "$kernel_config" ;;
  esac
}

# merge_config.sh treats a `CONFIG_NAME ` token anywhere on a line as a
# setting. Permit canonical unset assignments, but reject symbol-shaped prose.
ambiguous_comments="$(grep -E '^#.*CONFIG_[A-Z0-9_]+[[:space:]]' "$kernel_config" \
  | grep -Ev '^# CONFIG_[A-Z0-9_]+ is not set$' || true)"
if [ -n "$ambiguous_comments" ]; then
  printf '%s\n' "$ambiguous_comments" >&2
  bad 'kernel fragment has prose that merge_config can misparse as a setting'
fi

while read -r key expected; do
  if grep -Eq "^[[:space:]]*check_sysctl $key $expected$" "$audit"; then
    ok "$key is checked at boot"
  else
    bad "$key = $expected is configured but not checked at boot"
  fi
done < <(awk '$1 !~ /^#/ && NF == 3 && $2 == "=" { sub(/^-/, "", $1); print $1, $3 }' "$sysctl_file")

for required in \
  CONFIG_MODULE_SIG_FORCE=y \
  CONFIG_MODULE_FORCE_LOAD=n \
  CONFIG_MODULE_UNLOAD=n \
  CONFIG_MODULE_FORCE_UNLOAD=n \
  CONFIG_LIVEPATCH=n \
  CONFIG_DEBUG_FS=n \
  CONFIG_BSD_PROCESS_ACCT=n \
  CONFIG_TASKSTATS=n \
  CONFIG_PROC_EVENTS=n \
  CONFIG_IO_URING=n \
  CONFIG_USERFAULTFD=n \
  CONFIG_FANOTIFY=n \
  CONFIG_DNOTIFY=n \
  CONFIG_CUSE=n \
  CONFIG_FUSE_PASSTHROUGH=n \
  CONFIG_VIRTIO_FS=n \
  CONFIG_FUSE_DAX=n \
  CONFIG_FUSE_IO_URING=n \
  CONFIG_PM_TEST_SUSPEND=n \
  CONFIG_PM_TRACE=n \
  CONFIG_PM_TRACE_RTC=n \
  CONFIG_PCIEAER_INJECT=n \
  CONFIG_NET_DROP_MONITOR=n \
  CONFIG_DEBUG_SHIRQ=n \
  CONFIG_MMIOTRACE=n \
  CONFIG_X86_DECODER_SELFTEST=n \
  CONFIG_ATOMIC64_SELFTEST=n \
  CONFIG_MEMTEST=n \
  CONFIG_PM_DEBUG=n \
  CONFIG_PM_SLEEP_DEBUG=n \
  CONFIG_ACPI_DEBUG=n \
  CONFIG_FW_LOADER_DEBUG=n \
  CONFIG_OVMF_DEBUG_LOG=n \
  CONFIG_SCSI_IPR_TRACE=n \
  CONFIG_SCSI_IPR_DUMP=n \
  CONFIG_DM_DEBUG=n \
  CONFIG_MLX4_DEBUG=n \
  CONFIG_ATH5K_DEBUG=n \
  CONFIG_ATH6KL_DEBUG=n \
  CONFIG_IWLEGACY_DEBUG=n \
  CONFIG_IWLWIFI_DEBUG=n \
  CONFIG_USB_SERIAL_DEBUG=n \
  CONFIG_DLM_DEBUG=n \
  CONFIG_DYNAMIC_DEBUG=n \
  CONFIG_DYNAMIC_DEBUG_CORE=n \
  CONFIG_SLUB_DEBUG=n \
  CONFIG_DEBUG_MEMORY_INIT=n \
  CONFIG_RCU_TRACE=n \
  CONFIG_TRACEFS_AUTOMOUNT_DEPRECATED=n \
  CONFIG_CHECKPOINT_RESTORE=n \
  CONFIG_MEM_SOFT_DIRTY=n \
  CONFIG_PROC_PAGE_MONITOR=n \
  CONFIG_LEGACY_PTYS=n \
  CONFIG_LEGACY_TIOCSTI=n \
  CONFIG_STRICT_KERNEL_RWX=y \
  CONFIG_STRICT_MODULE_RWX=y \
  CONFIG_RANDOMIZE_BASE=y \
  CONFIG_RANDOMIZE_MEMORY=y \
  CONFIG_STACKPROTECTOR_STRONG=y \
  CONFIG_FORTIFY_SOURCE=y \
  CONFIG_HARDENED_USERCOPY=y \
  CONFIG_SLAB_FREELIST_RANDOM=y \
  CONFIG_SLAB_FREELIST_HARDENED=y \
  CONFIG_SECURITY_LANDLOCK=y \
  CONFIG_SECCOMP_FILTER=y \
  CONFIG_SECURITY_YAMA=y \
  CONFIG_SECURITY_LOCKDOWN_LSM_EARLY=y \
  CONFIG_SECURITY_SELINUX_BOOTPARAM=n \
  CONFIG_SECURITY_SELINUX_DEVELOP=n \
  CONFIG_SECURITY_TOMOYO=n \
  CONFIG_SECURITY_IPE=n \
  CONFIG_BPF=y \
  CONFIG_BPF_SYSCALL=y \
  CONFIG_DEBUG_INFO_BTF=y \
  CONFIG_BPF_LSM=y \
  'CONFIG_LSM="lockdown,yama,integrity,selinux,landlock,bpf"' \
  CONFIG_SECURITY_DMESG_RESTRICT=y \
  CONFIG_BPF_UNPRIV_DEFAULT_OFF=y \
  CONFIG_PROC_KCORE=n \
  CONFIG_DEVKMEM=n \
  CONFIG_DEVPORT=n \
  CONFIG_X86_MSR=n \
  CONFIG_X86_CPUID=n \
  CONFIG_X86_IOPL_IOPERM=n \
  CONFIG_X86_MCELOG_LEGACY=n \
  CONFIG_X86_MCE_INJECT=n \
  CONFIG_ACPI_EXTLOG=n \
  CONFIG_IA32_EMULATION=y \
  CONFIG_X86_X32_ABI=n \
  CONFIG_USELIB=n \
  CONFIG_COREDUMP=n \
  CONFIG_ALLOW_DEV_COREDUMP=n \
  CONFIG_USB_MON=n \
  CONFIG_MAGIC_SYSRQ=n \
  CONFIG_FW_LOADER_USER_HELPER=n \
  CONFIG_FW_LOADER_USER_HELPER_FALLBACK=n \
  CONFIG_ACPI_CUSTOM_METHOD=n \
  CONFIG_ACPI_CONFIGFS=n \
  CONFIG_EFI_CUSTOM_SSDT_OVERLAYS=n \
  CONFIG_KEXEC=n \
  CONFIG_KEXEC_FILE=n \
  CONFIG_KEXEC_HANDOVER=n \
  CONFIG_HIBERNATION=n \
  CONFIG_PSTORE=n \
  CONFIG_BINFMT_MISC=n \
  CONFIG_ATM=n \
  CONFIG_ATALK=n \
  CONFIG_CAN=n \
  CONFIG_CAIF=n \
  CONFIG_HAMRADIO=n \
  CONFIG_6LOWPAN=n \
  CONFIG_AF_RXRPC=n \
  CONFIG_IEEE802154=n \
  CONFIG_IP_DCCP=n \
  CONFIG_IP_SCTP=n \
  CONFIG_AF_KCM=n \
  CONFIG_LAPB=n \
  CONFIG_L2TP=n \
  CONFIG_MCTP=n \
  CONFIG_MISDN=n \
  CONFIG_MPLS=n \
  CONFIG_NET_NSH=n \
  CONFIG_NFC=n \
  CONFIG_PHONET=n \
  CONFIG_RDS=n \
  CONFIG_SMC=n \
  CONFIG_TLS=n \
  CONFIG_INET_PSP=n \
  CONFIG_IP_VS=n \
  CONFIG_NET_PKTGEN=n \
  CONFIG_GTP=n \
  CONFIG_PFCP=n \
  CONFIG_AMT=n \
  CONFIG_BLK_DEV_NBD=n \
  CONFIG_ISCSI_TCP=n \
  CONFIG_ISCSI_BOOT_SYSFS=n \
  CONFIG_ISCSI_IBFT=n \
  CONFIG_SCSI_QLA_ISCSI=n \
  CONFIG_SCSI_BNX2_ISCSI=n \
  CONFIG_SCSI_CXGB3_ISCSI=n \
  CONFIG_SCSI_CXGB4_ISCSI=n \
  CONFIG_INFINIBAND_ISER=n \
  CONFIG_BE2ISCSI=n \
  CONFIG_QEDI=n \
  CONFIG_SCSI_ISCSI_ATTRS=n \
  CONFIG_FCOE=n \
  CONFIG_FCOE_FNIC=n \
  CONFIG_QEDF=n \
  CONFIG_SCSI_BNX2X_FCOE=n \
  CONFIG_SCSI_CHELSIO_FCOE=n \
  CONFIG_LIBFCOE=n \
  CONFIG_SCSI_FC_ATTRS=n \
  CONFIG_NET_FC=n \
  CONFIG_NVME_RDMA=n \
  CONFIG_NVME_FC=n \
  CONFIG_NVME_TCP=n \
  CONFIG_NVME_TARGET=n \
  CONFIG_NVME_HOST_AUTH=n \
  CONFIG_NVME_AUTH=n \
  CONFIG_NVME_KEYRING=n \
  CONFIG_BLK_DEV_RNBD_CLIENT=n \
  CONFIG_BLK_DEV_RNBD_SERVER=n \
  CONFIG_TARGET_CORE=n \
  CONFIG_INFINIBAND=n \
  CONFIG_TIPC=n \
  CONFIG_X25=n \
  CONFIG_CRYPTO_USER_API=n \
  CONFIG_CRYPTO_USER_API_HASH=n \
  CONFIG_CRYPTO_USER_API_SKCIPHER=n \
  CONFIG_CRYPTO_USER_API_RNG=n \
  CONFIG_CRYPTO_USER_API_AEAD=n \
  CONFIG_CRYPTO_USER_API_ENABLE_OBSOLETE=n \
  CONFIG_IMA=n \
  CONFIG_EVM=n \
  CONFIG_PERSISTENT_KEYRINGS=n \
  CONFIG_BIG_KEYS=n \
  CONFIG_TRUSTED_KEYS=n \
  CONFIG_ENCRYPTED_KEYS=n \
  CONFIG_KEY_DH_OPERATIONS=n \
  CONFIG_KEY_NOTIFICATIONS=n \
  CONFIG_SIGNED_PE_FILE_VERIFICATION=n \
  CONFIG_SYSTEM_BLACKLIST_AUTH_UPDATE=n \
  CONFIG_SECURITY_SELINUX_AVC_STATS=n \
  CONFIG_AFFS_FS=n \
  CONFIG_ADFS_FS=n \
  CONFIG_BEFS_FS=n \
  CONFIG_BFS_FS=n \
  CONFIG_CRAMFS=n \
  CONFIG_EFS_FS=n \
  CONFIG_F2FS_FS=n \
  CONFIG_HFS_FS=n \
  CONFIG_HPFS_FS=n \
  CONFIG_JFS_FS=n \
  CONFIG_MINIX_FS=n \
  CONFIG_NILFS2_FS=n \
  CONFIG_OMFS_FS=n \
  CONFIG_GFS2_FS=n \
  CONFIG_OCFS2_FS=n \
  CONFIG_ORANGEFS_FS=n \
  CONFIG_QNX4FS_FS=n \
  CONFIG_QNX6FS_FS=n \
  CONFIG_ROMFS_FS=n \
  CONFIG_SYSV_FS=n \
  CONFIG_UFS_FS=n \
  CONFIG_VXFS_FS=n \
  CONFIG_9P_FS=n \
  CONFIG_AFS_FS=n \
  CONFIG_CEPH_FS=n \
  CONFIG_CIFS=n \
  CONFIG_CODA_FS=n \
  CONFIG_NFS_FS=n \
  CONFIG_NFSD=n \
  CONFIG_ACORN_PARTITION=n \
  CONFIG_AMIGA_PARTITION=n \
  CONFIG_ATARI_PARTITION=n \
  CONFIG_KARMA_PARTITION=n \
  CONFIG_SGI_PARTITION=n \
  CONFIG_SUN_PARTITION=n \
  CONFIG_ULTRIX_PARTITION=n; do
  fragment_has "$required" \
    || bad "required kernel setting missing: $required"
done

# Without kernel NFS the base image's NFS userspace can only fail at boot.
grep -Fq 'RUN rpm -e nfs-utils nfs-client-utils nfsv3-client-utils nfsv4-client-utils' "$image_recipe" \
  && grep -Fq '        nfs-common-utils rpcbind \' "$image_recipe" \
  || bad 'the image keeps NFS userspace the kernel cannot serve (rpc_pipefs fails at boot)'
grep -Fq '"preempt=full"' "$repo/os/usr/lib/bootc/kargs.d/03-boot.toml" \
  || bad 'signed UKI command line does not pin full dynamic preemption'
[ "$fail" -ne 0 ] || ok 'required kernel self-protection options are pinned'

if grep -Eq '^CONFIG_(X86_|DEVPORT=|IA32_EMULATION=|USELIB=|LEGACY_VSYSCALL_|RANDOMIZE_MEMORY=|SCHED_MC_PRIO=|SND_SOC_INTEL_AVS_MACH_I2S_TEST=)' "$kernel_config" "$arm64_kernel_config"; then
  bad 'x86-only settings leaked into shared or ARM64 kernel fragments'
fi
grep -Fq 'CONFIG_RANDOMIZE_BASE=y CONFIG_RANDOMIZE_MEMORY=y' \
  "$repo/os/kernel/build-support.sh" \
  || bad 'x86 kernel build does not verify both address-randomization layers'
grep -Fq "'# CONFIG_DEVPORT is not set' '# CONFIG_X86_MSR is not set'" \
  "$repo/os/kernel/build-support.sh" \
  && grep -Fq "'# CONFIG_X86_CPUID is not set'" "$repo/os/kernel/build-support.sh" \
  || bad 'x86 kernel build does not verify removal of raw CPU and I/O devices'
grep -Fq "CONFIG_IA32_EMULATION=y '# CONFIG_X86_X32_ABI is not set'" \
  "$repo/os/kernel/build-support.sh" \
  && grep -Fq "'# CONFIG_USELIB is not set'" "$repo/os/kernel/build-support.sh" \
  || bad 'x86 kernel does not preserve i686 while rejecting obsolete ABIs'

for live_absence in DEVKMEM PROC_KCORE COREDUMP FW_LOADER_USER_HELPER \
  FW_LOADER_USER_HELPER_FALLBACK TLS CRYPTO_USER_API; do
  grep -Fq "$live_absence" "$audit" \
    || bad "installed audit does not inspect removed kernel interface: $live_absence"
done
grep -Fq 'unused privileged kernel interface $interface' "$audit" \
  || bad 'installed audit does not fail when a removed privileged kernel interface returns'

for required in \
  "device_key_dir=/var/lib/tilo/device-private" \
  "device_key_meta=\"\$(stat -c '%u:%G:%a:%h:%s' \"\$device_key\" 2>/dev/null || echo invalid)\"" \
  "[ \"\$device_key_meta\" = '0:tilo-identity:640:1:32' ]"; do
  grep -Fq "$required" "$audit" \
    || bad "installed audit does not prove device-key custody: $required"
done

arm64_missing=0
for required in \
  CONFIG_ARM64_PTR_AUTH=y \
  CONFIG_ARM64_PTR_AUTH_KERNEL=y \
  CONFIG_ARM64_BTI=y \
  CONFIG_RANDOMIZE_BASE=y \
  CONFIG_RANDOMIZE_MODULE_REGION_FULL=y; do
  grep -Fqx "$required" "$arm64_kernel_config" || arm64_missing=1
done
if [ "$arm64_missing" -eq 0 ] \
   && grep -Fq '"/work/kernel-config/$arch.config" > /work/tilo-effective.config' \
        "$repo/os/kernel/build-support.sh"; then
  ok 'ARM64 kernel enables pointer authentication and KASLR hardening'
else
  bad 'ARM64 kernel control-flow or address-randomization hardening is incomplete'
fi

grep -Fqx '# CONFIG_MODULE_SIG_ALL is not set' "$kernel_config" \
  || bad 'upstream libcrypto module signing is enabled'
grep -Fq 'tilo-crypto verify-module' "$repo/os/kernel/build-support.sh" \
  || bad 'kernel build does not verify pure-Rust module signatures'

for required in \
  CONFIG_PREEMPT=y \
  CONFIG_PREEMPT_DYNAMIC=y \
  CONFIG_HZ_1000=y \
  CONFIG_HZ=1000 \
  CONFIG_DEBUG_INFO_NONE=n \
  CONFIG_DEBUG_INFO_DWARF5=y \
  CONFIG_UBSAN=n \
  CONFIG_FUNCTION_TRACER=n \
  CONFIG_STACK_TRACER=n \
  CONFIG_PAGE_OWNER=n \
  CONFIG_KALLSYMS=y \
  CONFIG_KALLSYMS_ALL=n \
  CONFIG_KUNIT=n \
  CONFIG_RCU_TORTURE_TEST=n \
  CONFIG_LOCK_TORTURE_TEST=n \
  CONFIG_TEST_BPF=n \
  CONFIG_TEST_KMOD=n \
  CONFIG_TEST_LKM=n \
  CONFIG_CRYPTO_BENCHMARK=n \
  CONFIG_RING_BUFFER_BENCHMARK=n \
  CONFIG_X86_AMD_PSTATE_UT=n \
  CONFIG_MAC80211_HWSIM=n \
  CONFIG_NETDEVSIM=n \
  CONFIG_USB_DUMMY_HCD=n \
  CONFIG_VDPA_SIM=n \
  CONFIG_USB_GADGET=n \
  CONFIG_USB_CONFIGFS=n \
  CONFIG_USB_GADGETFS=n \
  CONFIG_USB_FUNCTIONFS=n \
  CONFIG_USB_MASS_STORAGE=n \
  CONFIG_USB_G_SERIAL=n \
  CONFIG_USB_G_PRINTER=n \
  CONFIG_USB_G_WEBCAM=n \
  CONFIG_USB_RAW_GADGET=n \
  CONFIG_USBIP_CORE=n \
  CONFIG_NET_KEY=n \
  CONFIG_XFRM_USER=n \
  CONFIG_XFRM_INTERFACE=n \
  CONFIG_INET_AH=n \
  CONFIG_INET_ESP=n \
  CONFIG_INET_IPCOMP=n \
  CONFIG_INET6_AH=n \
  CONFIG_INET6_ESP=n \
  CONFIG_INET6_IPCOMP=n \
  CONFIG_MPTCP=n \
  CONFIG_INET_MPTCP_DIAG=n \
  CONFIG_BCACHE=n \
  CONFIG_BCACHEFS_FS=n \
  CONFIG_BLK_DEV_RBD=n \
  CONFIG_BLK_DEV_DRBD=n \
  CONFIG_ATA_OVER_ETH=n \
  CONFIG_BLK_DEV_UBLK=n \
  CONFIG_BLK_DEV_NULL_BLK=n \
  CONFIG_SCSI_DEBUG=n \
  CONFIG_DM_DELAY=n \
  CONFIG_DM_FLAKEY=n \
  CONFIG_DM_DUST=n \
  CONFIG_FAULT_INJECTION=n \
  CONFIG_LKDTM=n \
  CONFIG_IP_MROUTE=n \
  CONFIG_IPV6_MROUTE=n \
  CONFIG_IPV6_SEG6_LWTUNNEL=n \
  CONFIG_IPV6_SEG6_HMAC=n \
  CONFIG_IPV6_SEG6_BPF=n \
  CONFIG_IPV6_IOAM6_LWTUNNEL=n \
  CONFIG_RV=n \
  CONFIG_UAPI_HEADER_TEST=n \
  CONFIG_SCSI_VIRTIO=y \
  CONFIG_BTRFS_FS=y \
  CONFIG_XFS_FS=m \
  CONFIG_EXT4_FS=m \
  CONFIG_DRM_VIRTIO_GPU=y \
  CONFIG_HW_RANDOM_VIRTIO=y \
  CONFIG_VIRTIO_NET=m \
  CONFIG_E1000E=m \
  CONFIG_IGB=m \
  CONFIG_R8169=m \
  CONFIG_IWLWIFI=m \
  CONFIG_SCHED_AUTOGROUP=y \
  CONFIG_SCHED_CLASS_EXT=n \
  CONFIG_SCHED_CORE=n \
  CONFIG_SCHED_DEBUG=n \
  CONFIG_X86_INTEL_PSTATE=y \
  CONFIG_X86_AMD_PSTATE=y \
  CONFIG_X86_AMD_PSTATE_DEFAULT_MODE=3 \
  CONFIG_CPU_FREQ=y \
  CONFIG_CPU_FREQ_GOV_SCHEDUTIL=y \
  CONFIG_CPU_FREQ_DEFAULT_GOV_SCHEDUTIL=y \
  CONFIG_CPU_FREQ_STAT=n \
  CONFIG_CPU_FREQ_GOV_USERSPACE=n \
  CONFIG_CPU_FREQ_GOV_ONDEMAND=n \
  CONFIG_CPU_FREQ_GOV_CONSERVATIVE=n \
  CONFIG_IRQ_TIME_ACCOUNTING=y \
  CONFIG_HIGH_RES_TIMERS=y \
  CONFIG_NO_HZ_IDLE=y \
  CONFIG_HZ_PERIODIC=n \
  CONFIG_NO_HZ_FULL=n \
  CONFIG_UCLAMP_TASK=y \
  CONFIG_UCLAMP_TASK_GROUP=y \
  CONFIG_UCLAMP_BUCKETS_COUNT=5 \
  CONFIG_ENERGY_MODEL=y \
  CONFIG_NTSYNC=y \
  CONFIG_IOSCHED_BFQ=y \
  CONFIG_BFQ_GROUP_IOSCHED=y \
  CONFIG_MQ_IOSCHED_DEADLINE=y \
  CONFIG_TRANSPARENT_HUGEPAGE_ALWAYS=n \
  CONFIG_TRANSPARENT_HUGEPAGE_MADVISE=y \
  CONFIG_MODULE_COMPRESS_ZSTD=y \
  CONFIG_MODULE_COMPRESS_ALL=y \
  CONFIG_RD_ZSTD=y \
  CONFIG_CRYPTO_LZ4=y \
  CONFIG_LZ4_COMPRESS=y \
  CONFIG_ZRAM=y \
  CONFIG_ZRAM_BACKEND_LZ4=y \
  CONFIG_ZRAM_DEF_COMP_LZ4=y; do
  fragment_has "$required" \
    || bad "required interactive kernel setting missing: $required"
done

# Intel Turbo Boost Max / AMD preferred-core scheduling is an x86-only Kconfig
# facility. It remains mandatory in the x86 fragment only.
grep -Fqx 'CONFIG_SCHED_MC_PRIO=y' "$x86_kernel_config" \
  || bad 'x86 preferred-core scheduler support is not pinned'
grep -Fqx 'CONFIG_INTEL_HFI_THERMAL=n' "$x86_kernel_config" \
  || bad 'unused Intel HFI userspace telemetry remains enabled'
for removed in CONFIG_PARPORT=n CONFIG_BLK_DEV_FD=n CONFIG_PCCARD=n; do
  grep -Fqx "$removed" "$x86_kernel_config" \
    || bad "obsolete x86 hardware surface is not disabled: $removed"
done
grep -Fq '"/work/kernel-config/$arch.config" > /work/tilo-effective.config' \
  "$repo/os/kernel/build-support.sh" \
  || bad 'kernel merge does not select the native architecture fragment'

io_scheduler_rule="$repo/os/usr/lib/udev/rules.d/60-tilo-io-scheduler.rules"
for physical_disk in 'KERNEL=="sd*"' 'KERNEL=="nvme[0-9]*n[0-9]*"'; do
  grep -Fq "$physical_disk" "$io_scheduler_rule" \
    || bad "BFQ policy does not cover physical disk selector $physical_disk"
done
grep -Fq 'ATTR{queue/scheduler}=="*bfq*", ATTR{queue/scheduler}="bfq"' "$io_scheduler_rule" \
  || bad 'physical storage policy does not select an available BFQ scheduler'
if grep -Eq 'KERNEL=="(vd|loop|dm-)' "$io_scheduler_rule"; then
  bad 'BFQ policy changes virtual, loop, or device-mapper release paths'
fi

grep -Fq 'python3 /work/check-config.py .config --fragment /work/tilo-effective.config' "$repo/os/kernel/build-support.sh" \
  || bad 'kernel build does not reject silently dropped fragment settings'

grep -Fqx 'compression-algorithm = lz4' "$repo/os/etc/systemd/zram-generator.conf" \
  || bad 'zram is not using the low-latency LZ4 compressor'
boot_performance_gate="$repo/os/tests/boot_performance_gate.sh"
grep -Fq "*' [lz4] '*)" "$boot_performance_gate" \
  || bad 'release boot gate does not verify the live zram compressor'
grep -Fq 'grep -Fqx /dev/zram0' "$boot_performance_gate" \
  || bad 'release boot gate does not verify zram is active swap'
grep -Fq 'if [ "$active_swaps" != /dev/zram0 ]; then' "$boot_performance_gate" \
  || bad 'release boot gate permits persistent swap beside zram'
grep -Fq 'if [ "$active_swaps" = /dev/zram0 ]; then' "$audit" \
  || bad 'installed security audit does not detect persistent swap'
grep -Fq "*' [bfq] '*)" "$boot_performance_gate" \
  || bad 'release boot gate does not verify BFQ on physical disks'

for required in \
  'kernel.sched_autogroup_enabled = 1' \
  'vm.swappiness = 100' \
  'vm.page-cluster = 0' \
  'vm.compaction_proactiveness = 0'; do
  grep -Fqx "$required" "$performance_sysctl" \
    || bad "required interactive sysctl missing: $required"
done
[ "$fail" -ne 0 ] || ok 'interactive scheduler and VM policy is pinned'

# Background OS maintenance must not compete equally with games, the
# compositor, or application cold starts. The shutdown signer is deliberately
# excluded: it must finish promptly when no interactive workload remains.
for unit in \
  tilo-boot-entries.service \
  tilo-growfs-sysroot.service \
  tilo-secureboot-sign.service \
  tilo-system-extension-boot-pack-stage.service \
  tilo-system-extension-boot-pack-status.service; do
  unit_path="$repo/os/usr/lib/systemd/system/$unit"
  for required in 'Nice=10' 'CPUWeight=20' 'IOWeight=20' \
      'IOSchedulingClass=best-effort' 'IOSchedulingPriority=7'; do
    grep -Fqx "$required" "$unit_path" \
      || bad "$unit does not yield background resources with $required"
  done
done
for required in 'Nice=19' 'CPUSchedulingPolicy=idle' 'CPUWeight=20' 'IOWeight=20' \
    'IOSchedulingClass=idle'; do
  grep -Fqx "$required" "$repo/os/usr/lib/systemd/system/tilo-security-audit.service" \
    || bad "tilo-security-audit.service does not run idle with $required"
done
update_resources="$repo/os/usr/lib/systemd/system/bootc-fetch-apply-updates.service.d/30-tilo-background-resources.conf"
for required in 'Nice=10' 'CPUWeight=20' 'IOWeight=20' \
    'IOSchedulingClass=best-effort' 'IOSchedulingPriority=7'; do
  grep -Fqx "$required" "$update_resources" \
    || bad "automatic updates do not yield background resources with $required"
done
grep -Fq 'os/usr/lib/systemd/system/bootc-fetch-apply-updates.service.d/30-tilo-background-resources.conf' \
  "$image_recipe" || bad 'automatic-update resource policy is not copied into the OS image'
[ "$fail" -ne 0 ] || ok 'background maintenance yields to interactive workloads'

boot_entries_unit="$repo/os/usr/lib/systemd/system/tilo-boot-entries.service"
for required in \
  'Nice=10' \
  'CPUWeight=20' \
  'IOWeight=20' \
  'IOSchedulingClass=best-effort' \
  'IOSchedulingPriority=7' \
  'NoNewPrivileges=yes' \
  'ProtectSystem=strict' \
  'ProtectHome=yes' \
  'PrivateDevices=yes' \
  'PrivateNetwork=yes' \
  'ProtectProc=invisible' \
  'ProcSubset=pid' \
  'CapabilityBoundingSet=' \
  'AmbientCapabilities=' \
  'StateDirectory=tilo/esp-plan/boot-entries' \
  'StateDirectoryMode=0700' \
  'ReadOnlyPaths=/var/lib/tilo/secureboot' \
  'SystemCallFilter=@system-service' \
  'SystemCallFilter=~@privileged @resources'; do
  grep -Fqx "$required" "$boot_entries_unit" \
    || bad "boot-entry maintenance sandbox is missing: $required"
done
# /boot stays read-only: the unit stages plans, and only the ESP writer, with
# CAP_SYS_ADMIN for its private remount alone, applies them.
grep -q '^ReadWritePaths=.*/boot' "$boot_entries_unit" \
  && bad 'boot-entry maintenance writes the ESP itself'
esp_writer_unit="$repo/os/usr/lib/systemd/system/tilo-esp-write@.service"
for required in \
  'ExecStart=/usr/libexec/tilo-esp-write %i' \
  'PrivateMounts=yes' \
  'NoNewPrivileges=yes' \
  'ProtectSystem=strict' \
  'ReadWritePaths=/boot /var/lib/tilo/esp-plan/%i' \
  'PrivateNetwork=yes' \
  'CapabilityBoundingSet=CAP_SYS_ADMIN' \
  'AmbientCapabilities=' \
  'RestrictAddressFamilies=AF_UNIX' \
  'SystemCallFilter=@system-service @mount'; do
  grep -Fqx "$required" "$esp_writer_unit" \
    || bad "ESP writer sandbox is missing: $required"
done
[ "$fail" -ne 0 ] || ok 'boot-entry maintenance is confined to staged, validated ESP plans'

game_planner="$repo/crates/windows/compat-service/src/plan.rs"
game_executor="$repo/crates/windows/compat-service/src/execute.rs"
game_performance="$repo/crates/packaging/launcher/src/performance.rs"
native_launcher="$repo/crates/packaging/launcher/src/run.rs"
grep -Fq 'libc::SYS_sched_setattr' "$game_performance" \
  || bad 'trusted Proton launches do not consume the kernel uclamp support'
grep -Fq 'libc::PR_SET_TIMERSLACK' "$game_performance" \
  || bad 'trusted game launches retain default wake-up coalescing jitter'
grep -Fq 'libc::SYS_ioprio_set' "$game_performance" \
  || bad 'trusted game launches do not receive BFQ-visible I/O priority'
grep -Fq 'crate::performance::apply_game_performance_hints()?' "$native_launcher" \
  || bad 'game scheduler policy failures are silently ignored'
grep -Fq 'latency_sensitive: route.runtime.layer == Layer::Proton' "$game_planner" \
  || bad 'game scheduling hint is not bound to the trusted Proton route'
grep -Fq 'apply_game_performance_hints()?' "$game_executor" \
  || bad 'Proton scheduler policy failures are silently ignored'
grep -Fq 'pub const GAME_UTIL_MIN: u32 = 256;' "$game_performance" \
  || bad 'game utilization floor drifted from its reviewed 25% bound'
grep -Fq 'pub const GAME_TIMER_SLACK_NS: libc::c_ulong = 1_000;' "$game_performance" \
  || bad 'game timer slack drifted from its reviewed scoped value'
grep -Fq 'pub const GAME_IO_PRIORITY: libc::c_int = 2;' "$game_performance" \
  || bad 'game I/O priority drifted from its reviewed non-realtime bound'
grep -Fq 'is_native_game_launch(manifest, opts)' "$native_launcher" \
  || bad 'native game packages do not inherit the bounded scheduler hint'

if grep -Fq '/usr/bin/tilo-power' "$image_recipe"; then
  bad 'obsolete competing cpufreq daemon still ships in the OS image'
else
  ok 'power-profiles-daemon has no competing shipped cpufreq writer'
fi

grep -Fq 'COPY os/usr/lib/sysctl.d/70-tilo-performance.conf /usr/lib/sysctl.d/70-tilo-performance.conf' \
  "$image_recipe" || bad 'interactive sysctl policy is not copied into the OS image'

grep -Fq 'install -D -m 0755 "$scudo" /usr/lib/tilo/libclang_rt.scudo_standalone.so' \
  "$image_recipe" \
  && grep -Fq 'rpm -e --nodeps compiler-rt clang-resource-filesystem' "$image_recipe" \
  || bad 'runtime retains the full compiler-rt toolchain instead of only Scudo'

grep -Fq 'dracut --force --reproducible --compress zstd' "$initramfs_recipe" \
  && ok 'signed initramfs uses low-latency Zstandard decompression'
grep -Fq 'dracut --force --reproducible --compress zstd' "$initramfs_recipe" \
  || bad 'initramfs compression can regress to a slow distribution default'
grep -Fq -- '--omit "network network-legacy network-manager nfs iscsi nvmf nbd fcoe fcoe-uefi nbde clevis clevis-pin-null clevis-pin-sss clevis-pin-tang clevis-pin-tpm2 crypt-ssh"' \
  "$initramfs_recipe" \
  || bad 'local-root initramfs still carries unused network/NBDE attack surface'
grep -Fq -- '--add-drivers ext4' "$initramfs_recipe" \
  && ok 'initramfs supports Ext4 roots'
grep -Fq -- '--add-drivers ext4' "$initramfs_recipe" \
  || bad 'initramfs cannot mount a supported Ext4 root'
grep -Fq -- '--omit-drivers "amdgpu nouveau xe xfs"' "$initramfs_recipe" \
  && ok 'initramfs excludes non-root XFS filesystem parser'
grep -Fq -- '--omit-drivers "amdgpu nouveau xe xfs"' "$initramfs_recipe" \
  || bad 'initramfs carries non-root XFS filesystem attack surface'
grep -Fq "printf '%s\\n' amdgpu nouveau xe xfs > /usr/lib/tilo/initramfs-omitted-drivers.txt" \
  "$initramfs_recipe" \
  || bad 'built image does not record its initramfs kernel-driver exclusions'
grep -Fq -- "-path '*/fs/ext4/ext4.ko*'" "$initramfs_recipe" \
  && grep -Fq -- "-path '*/fs/xfs/xfs.ko*'" "$initramfs_recipe" \
  || bad 'image build does not preserve signed Ext4/XFS modules for post-root rescue use'

for required in \
  '"lockdown=integrity"' \
  '"init_on_alloc=1"' \
  '"init_on_free=1"' \
  '"slab_nomerge"' \
  '"page_alloc.shuffle=1"' \
  '"randomize_kstack_offset=on"' \
  '"vsyscall=none"'; do
  grep -Fq "$required" "$kargs" || bad "required kernel argument missing: $required"
done
# Firmware OEM strings, fw_cfg and boot-loader drop-ins must not become
# systemd credentials of a sealed boot; the unsealed dev image removes the file.
grep -Fxq 'kargs = ["systemd.import_credentials=no"]' "$repo/os/usr/lib/bootc/kargs.d/05-credentials.toml" \
  && grep -Fq 'os/usr/lib/bootc/kargs.d/05-credentials.toml' "$image_recipe" \
  && grep -Fq 'rm /usr/lib/bootc/kargs.d/05-credentials.toml' "$repo/os/Containerfile.dev" \
  || bad 'a sealed boot can import systemd credentials from firmware or the boot loader'
[ "$fail" -ne 0 ] || ok 'required boot-time hardening arguments are pinned'

grep -Fq 'failures=$((failures + 1))' "$audit" \
  || bad 'boot audit warnings do not contribute to a failing health result'
grep -Fq 'exit 1' "$audit" \
  || bad 'boot audit cannot return failure when a required invariant is inactive'
for required in \
  "CONFIG_BPF_LSM=y" \
  "CONFIG_DEBUG_INFO_BTF=y" \
  "*,bpf,*)"; do
  grep -Fq "$required" "$audit" \
    || bad "runtime audit does not verify the systemd BPF-LSM prerequisite: $required"
done
if grep -Fq 'attrs" != 06000000' "$audit" \
   && grep -Fq '1:0) ok "Secure Boot enabled with firmware outside Setup Mode"' "$audit" \
   && grep -Fq 'SecureBoot=$sb SetupMode=$setup' "$audit"; then
  ok 'runtime Secure Boot evidence requires authentic attributes and exited Setup Mode'
else
  bad 'runtime Secure Boot audit accepts malformed or setup-mode firmware state'
fi
if grep -Fq "secureboot_enrollment_boot_marker_safe" "$audit" \
   && grep -Fq "Secure Boot enrollment occurred this boot; reboot before retiring PK/KEK" "$audit" \
   && grep -Fq "Secure Boot enrollment crossed a verified reboot boundary" "$audit"; then
  ok 'runtime audit exposes Secure Boot enrollment reboot proof'
else
  bad 'runtime audit ignores malformed or same-boot enrollment evidence'
fi
if grep -Fq 'SuccessExitStatus=0 1' "$repo/os/usr/lib/systemd/system/tilo-security-audit.service"; then
  bad 'systemd still converts the security audit failure into success'
else
  ok 'security audit failure remains visible to system health and agents'
fi

audit_unit="$repo/os/usr/lib/systemd/system/tilo-security-audit.service"
audit_timer="$repo/os/usr/lib/systemd/system/tilo-security-audit.timer"
grep -Fqx 'OnUnitInactiveSec=1h' "$audit_timer" \
  || bad 'security audit does not periodically revalidate runtime trust state'
if grep -Fqx 'RemainAfterExit=yes' "$audit_unit"; then
  bad 'security audit remains active after success, preventing timer reactivation'
fi
for required in \
  'NoNewPrivileges=yes' \
  'ProtectSystem=strict' \
  'ProtectHome=yes' \
  'RestrictAddressFamilies=AF_UNIX' \
  'PrivateTmp=yes' \
  'TimeoutStartSec=2min' \
  'DevicePolicy=closed' \
  'DeviceAllow=/dev/tpmrm0 rw' \
  'DeviceAllow=/dev/tpm0 rw' \
  'CapabilityBoundingSet=' \
  'AmbientCapabilities=' \
  'SystemCallFilter=@system-service' \
  'SystemCallFilter=~@clock @cpu-emulation @debug @module @mount @obsolete @raw-io @reboot @resources @swap'; do
  grep -Fqx "$required" "$audit_unit" \
    || bad "security audit sandbox is missing: $required"
done
# The audit inspects the host: a private network namespace has its own sysctl
# defaults, and ProtectKernelModules= hides the running kernel's config.
if grep -Eqx 'PrivateNetwork=yes|ProtectKernelModules=yes' "$audit_unit"; then
  bad 'security audit sandbox hides the host state it verifies'
fi
if grep -Eq 'CAP_SYS_PTRACE|CAP_SYSLOG|/proc/\$fspid/maps' "$audit_unit" "$audit"; then
  bad 'read-only security audit retains process-inspection or kernel-log capabilities'
fi
grep -Fq 'journalctl --quiet --boot --dmesg' "$audit" \
  || bad 'security audit does not read kernel diagnostics through the journal'
[ "$fail" -ne 0 ] || ok 'read-only security audit has a bounded runtime sandbox'

# /proc stays a plain mount: hidepid hides PID 1 from every unprivileged
# process, so libsystemd unit lookups fail (sd_pid_get_unit -> ESRCH) and
# at-spi-bus-launcher cannot start the AT-SPI bus. Services under their own
# users get a private hidepid view instead, and processes holding identity
# keys turn non-dumpable, which keeps their environ and memory root-only.
if grep -rIl 'hidepid=' "$repo/os/usr" "$repo/os/Containerfile" 2>/dev/null | grep -q .; then
  bad "a global hidepid /proc remount is back: $(grep -rIl 'hidepid=' "$repo/os/usr" "$repo/os/Containerfile" | tr '\n' ' ')"
fi
for unit in tilo-relay tilo-fs-replica tilo-local-model; do
  grep -Fqx 'ProtectProc=invisible' "$repo/os/usr/lib/systemd/system/$unit.service" \
    || bad "$unit.service can see other users' processes (ProtectProc=invisible missing)"
done
grep -Fq 'libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0)' \
  "$repo/crates/collab/tilo-user-identity/src/persist.rs" \
  || bad 'identity key holders no longer turn non-dumpable'
[ "$fail" -ne 0 ] || ok 'process visibility: per-service ProtectProc, non-dumpable key holders, no global hidepid'

# systemd 259's user manager cannot start a second sandboxed process in a
# unit with PrivateIPC=yes: the IPC namespace setup fails with EPERM and the
# next exec exits 226/NAMESPACE, so the unit crash-loops. Such user units run
# only their ExecStart; directories they need come from user-tmpfiles.d.
for unit in "$repo"/os/usr/lib/systemd/user/*.service; do
  grep -Fqx 'PrivateIPC=yes' "$unit" || continue
  if grep -Eq '^Exec(StartPre|StartPost|Condition|Reload|Stop|StopPost)=' "$unit"; then
    bad "$(basename "$unit") runs a second process beside PrivateIPC=yes (226/NAMESPACE)"
  fi
done
for dir in '%h/Remote' '%h/Shared' '%h/.cache/tilo/received-files'; do
  grep -Eq "^d ${dir//./\\.}[[:space:]]" "$repo/os/usr/share/user-tmpfiles.d/tilo-fs.conf" \
    || bad "user-tmpfiles.d/tilo-fs.conf does not create $dir for the filesystem services"
done
# systemd-tmpfiles --user reads user-tmpfiles.d from /etc and /usr/share,
# never /usr/lib: a snippet there is silently dead.
if grep -Eq '/usr/lib/user-tmpfiles\.d' "$repo/os/Containerfile" || [ -e "$repo/os/usr/lib/user-tmpfiles.d" ]; then
  bad 'a user-tmpfiles.d snippet is installed under /usr/lib, which systemd-tmpfiles --user never reads'
fi
[ "$fail" -ne 0 ] || ok 'PrivateIPC user units run one process; their roots come from tmpfiles'

rollback_harden_unit="$repo/os/usr/lib/systemd/system/tilo-rollback-harden.service"
rollback_exec="$repo/crates/system/tilo-rollback/src/exec.rs"
for required in \
  'Nice=10' \
  'CPUWeight=20' \
  'IOWeight=20' \
  'IOSchedulingClass=best-effort' \
  'IOSchedulingPriority=7' \
  'NoNewPrivileges=yes' \
  'ProtectSystem=strict' \
  'PrivateNetwork=yes' \
  'ProtectProc=invisible' \
  'ProcSubset=pid' \
  'CapabilityBoundingSet=' \
  'AmbientCapabilities=' \
  'DevicePolicy=closed' \
  'DeviceAllow=block-* r' \
  'DeviceAllow=/dev/mapper/control rw' \
  'DeviceAllow=/dev/tpmrm0 rw' \
  'DeviceAllow=/dev/tpm0 rw' \
  'ReadWritePaths=/var/lib/tilo/rollback' \
  'SystemCallFilter=@system-service'; do
  grep -Fqx "$required" "$rollback_harden_unit" \
    || bad "TPM rollback-hardening sandbox is missing: $required"
done
[ "$fail" -ne 0 ] || ok 'TPM rollback hardening has device-scoped privileges'

rollback_confirm_unit="$repo/os/usr/lib/systemd/system/tilo-rollback-confirm.service"
for required in \
  'NoNewPrivileges=yes' \
  'ProtectSystem=strict' \
  'PrivateNetwork=yes' \
  'ProtectProc=invisible' \
  'CapabilityBoundingSet=' \
  'AmbientCapabilities=' \
  'DevicePolicy=closed' \
  'DeviceAllow=block-* rw' \
  'DeviceAllow=/dev/mapper/control rw' \
  'DeviceAllow=/dev/tpmrm0 rw' \
  'DeviceAllow=/dev/tpm0 rw' \
  'ReadWritePaths=/var/lib/tilo/rollback /run/tilo-rollback' \
  'SystemCallFilter=@system-service'; do
  grep -Fqx "$required" "$rollback_confirm_unit" \
    || bad "rollback-confirmation sandbox is missing: $required"
done
if grep -Fqx 'ProcSubset=pid' "$rollback_confirm_unit"; then
  bad 'rollback confirmation cannot read the measured generation from /proc/cmdline'
fi
[ "$fail" -ne 0 ] || ok 'rollback confirmation is confined to TPM and LUKS state'

if grep -Fq 'bounded_command::run_prepared(' "$rollback_exec" \
   && ! grep -Eq '\.output\(\)|wait_with_output\(' "$rollback_exec"; then
  ok 'TPM and cryptsetup rollback helpers have bounded subprocess execution'
else
  bad 'rollback trust decisions can be pinned by an unbounded helper'
fi

boot_health_unit="$repo/os/usr/lib/systemd/system/tilo-boot-health.service"
for required in \
  'NoNewPrivileges=yes' \
  'ProtectSystem=strict' \
  'PrivateDevices=yes' \
  'PrivateNetwork=yes' \
  'ProtectProc=invisible' \
  'ProcSubset=pid' \
  'CapabilityBoundingSet=' \
  'AmbientCapabilities=' \
  'RestrictAddressFamilies=AF_UNIX' \
  'SystemCallFilter=@system-service' \
  'SystemCallFilter=~@privileged @resources'; do
  grep -Fqx "$required" "$boot_health_unit" \
    || bad "boot-health sandbox is missing: $required"
done
[ "$fail" -ne 0 ] || ok 'boot-health assessment is read-only and capability-free'

# ProcSubset=pid hides non-process kernel interfaces such as /proc/cmdline and
# /proc/sys. Audit every locally owned executable placed behind that boundary;
# this caught signer and boot-entry code which worked outside its production
# sandbox. CUPS is distribution-owned, while the VM controller is compiled from
# the Rust source tree and is checked there.
procsubset_units="$(grep -Rlx 'ProcSubset=pid' "$repo/os/usr/lib/systemd/system")"
for unit_path in $procsubset_units; do
  while IFS= read -r executable; do
    executable="${executable#/}"
    staged="$repo/os/$executable"
    [ -f "$staged" ] || continue
    if sed '/^[[:space:]]*#/d' "$staged" \
        | grep -Eq '/proc/(cmdline|sys/|cpuinfo|meminfo|modules|filesystems|crypto|version|uptime)'; then
      bad "$(basename "$unit_path") executable reads a path hidden by ProcSubset=pid: /$executable"
    fi
  done < <(sed -n 's/^Exec\(Start\|Stop\|Reload\)=[-+!:]*\([^ ]*\).*/\2/p' "$unit_path")
done
if grep -REn '/proc/(cmdline|sys/|cpuinfo|meminfo|modules|filesystems|crypto|version|uptime)' \
    "$repo/crates/windows/windows-vm/src" >/dev/null; then
  bad 'ProcSubset=pid Windows VM controller reads a hidden non-process /proc path'
fi
# tilo-rollback-harden sources this library but must not reach its cmdline helper.
rollback_harden="$repo/os/usr/libexec/tilo-rollback-harden"
if grep -Fq 'rollback_running_generation' "$rollback_harden"; then
  bad 'TPM rollback-hardening service calls a helper hidden by ProcSubset=pid'
fi
[ "$fail" -ne 0 ] || ok 'ProcSubset=pid services avoid hidden non-process /proc interfaces'

fs_unit="$repo/os/usr/lib/systemd/system/fs.service"
fs_selinux="$repo/os/usr/lib/systemd/system/fs.service.d/20-selinux.conf"
for required in \
  'LimitCORE=0' \
  'NoNewPrivileges=yes' \
  'KeyringMode=private' \
  'CapabilityBoundingSet=CAP_SYS_ADMIN CAP_DAC_READ_SEARCH CAP_CHOWN CAP_FOWNER CAP_DAC_OVERRIDE CAP_SYS_PTRACE' \
  'AmbientCapabilities=' \
  'DevicePolicy=closed' \
  'DeviceAllow=/dev/fuse rw' \
  'RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6 AF_NETLINK' \
  'MemoryDenyWriteExecute=yes' \
  'SystemCallFilter=@system-service @mount'; do
  grep -Fqx "$required" "$fs_unit" \
    || bad "collaborative filesystem sandbox is missing: $required"
done
if grep -Fq 'ExecStartPre=/usr/bin/install -d -m 2770 -o root -g tilo-identity /run/tilo' "$fs_unit"; then
  bad 'collaborative filesystem retains CAP_CHOWN-driven runtime directory repair'
fi
if grep -Eq '^ExecStartPre=.*/(mkdir|cp)[[:space:]]' "$fs_unit"; then
  bad 'filesystem daemon still forks redundant path-setup helpers after tmpfiles'
elif grep -Fqx 'After=local-fs.target network.target systemd-tmpfiles-setup.service' "$fs_unit" \
  && grep -Fq 'd  /var/workspace' "$repo/os/usr/lib/tmpfiles.d/tilo.conf" \
  && grep -Fq 'd  /var/cache/quick-fs' "$repo/os/usr/lib/tmpfiles.d/tilo.conf" \
  && grep -Fq 'C  /var/lib/tilo/fs-capabilities.json' "$repo/os/usr/lib/tmpfiles.d/tilo.conf"; then
  ok 'filesystem daemon relies on ordered tmpfiles provisioning without restart-time helper forks'
else
  bad 'filesystem daemon removed setup helpers without a complete ordered tmpfiles contract'
fi
grep -Fqx 'SELinuxContext=system_u:system_r:tilo_fs_t:s0' "$fs_selinux" \
  || bad 'collaborative filesystem SELinux transition still fails open'
[ "$fail" -ne 0 ] || ok 'root FUSE daemon has a fail-closed bounded execution context'

for unit in tilo-secureboot-sign.service tilo-secureboot-sign-shutdown.service; do
  unit_path="$repo/os/usr/lib/systemd/system/$unit"
  grep -Fqx 'RuntimeDirectory=tilo-secureboot' "$unit_path" \
    && grep -Fqx 'RuntimeDirectoryMode=0700' "$unit_path" \
    || bad "$unit does not confine transient plaintext to a private runtime directory"
  if grep -Fq 'RuntimeDirectoryPreserve=yes' "$unit_path"; then
    bad "$unit can retain an unsealed db key after abnormal service termination"
  fi
  for required in \
    'LimitCORE=0' \
    'NoNewPrivileges=yes' \
    'ProtectSystem=strict' \
    'PrivateNetwork=yes' \
    'ProtectProc=invisible' \
    'ProcSubset=pid' \
    'MemoryDenyWriteExecute=yes' \
    'SystemCallArchitectures=native' \
    'SystemCallFilter=@system-service' \
    'SystemCallFilter=~@privileged @resources' \
    'CapabilityBoundingSet=' \
    'AmbientCapabilities=' \
    'ReadOnlyPaths=/var/lib/tilo/secureboot' \
    'ReadWritePaths=/boot' \
    'RestrictAddressFamilies=AF_UNIX'; do
    grep -Fqx "$required" "$unit_path" \
      || bad "$unit does not confine its signing key boundary with $required"
  done
done
grep -Fqx 'PrivateDevices=yes' \
  "$repo/os/usr/lib/systemd/system/tilo-secureboot-sign.service" \
  || bad 'ordinary Secure Boot signer exposes host devices'
shutdown_signer="$repo/os/usr/lib/systemd/system/tilo-secureboot-sign-shutdown.service"
for required in \
  'ExecStop=/usr/libexec/tilo-secureboot sign-at-shutdown' \
  'PrivateDevices=no' \
  'DevicePolicy=closed' \
  'DeviceAllow=/dev/tpmrm0 rw' \
  'DeviceAllow=/dev/tpm0 rw'; do
  grep -Fqx "$required" "$shutdown_signer" \
    || bad "shutdown signer lacks its narrow TPM boundary: $required"
done

secureboot="$repo/os/usr/libexec/tilo-secureboot"
grep -Fq "od -An -N16 -tx1 /dev/urandom" "$secureboot" \
  || bad 'Secure Boot temporary names do not use the service-visible kernel RNG'
if grep -Fq '/proc/sys/kernel/random/uuid' "$secureboot"; then
  bad 'Secure Boot signer reads a path hidden by its ProcSubset=pid sandbox'
fi
grep -Fq 'flock -x 9' "$secureboot" \
  || bad 'Secure Boot signing is not serialized against concurrent hooks'
grep -Fq '[ ! -L "$f" ]' "$secureboot" \
  || bad 'Secure Boot loader discovery follows symlinks'
for enrollment_lock in \
  'exec 9<"$KEYDIR"' \
  'exec 8<"$ESP"' \
  'exec 7<"$EFIVARS"' \
  'flock -x 7'; do
  grep -Fq "$enrollment_lock" "$secureboot" \
    || bad "Secure Boot enrollment transaction lock is missing: $enrollment_lock"
done
for variable in db KEK PK; do
  grep -Fq "verify_firmware_variable_contains $variable \"\$KEYDIR/$variable.crt\"" "$secureboot" \
    || bad "Secure Boot enrollment does not read back and verify firmware $variable"
done
grep -Fq '[ "$(efivar_byte SetupMode)" = 0 ]' "$secureboot" \
  || bad 'Secure Boot enrollment does not confirm that owner PK left setup mode'
grep -Fq '[ "$owner_only_db" -eq 0 ] || verify_firmware_db_owner_exclusive' "$secureboot" \
  || bad 'owner-exclusive enrollment does not reject firmware-retained boot authority'
for sealed_key_control in \
  'sb_enabled || die "Secure Boot is not enabled; boot the final owner policy before sealing PCR 7"' \
  'systemd-creds encrypt --name=db.key --with-key=tpm2' \
  '--tpm2-device="$tpm" --tpm2-pcrs=7' \
  'systemd-creds decrypt --tpm2-device="$tpm" "$candidate" "$check"' \
  'tilo-crypto key-matches-certificate "$check" "$KEYDIR/db.crt"' \
  'tilo-crypto verify-exclusive-efi-signature-list "$KEYDIR/db.crt" "$firmware_db"' \
  'LoadCredentialEncrypted=db.key:%s/db.key.cred' \
  'install_sealed_credential_dropin tilo-secureboot-sign-shutdown.service no' \
  'systemd-creds decrypt --tpm2-device="$tpm" "$sealed" "$unsealed"' \
  'tilo-crypto durable-unlink "$unsealed"' \
  'systemctl restart tilo-secureboot-sign-shutdown.service' \
  'systemctl start --wait tilo-secureboot-sign.service' \
  'tilo-crypto durable-unlink "$KEYDIR/db.key"'; do
  grep -Fq -- "$sealed_key_control" "$secureboot" \
    || bad "TPM-sealed Secure Boot db key lacks: $sealed_key_control"
done
grep -Fq '|| secureboot_artifact_present /var/lib/tilo/secureboot/db.key.cred' "$audit" \
  || bad 'security audit mistakes TPM-sealed Secure Boot authority for missing keys'
for custody_check in \
  "stat -c '%u:%a:%h:%s'" \
  '[ -e "$1" ] || [ -L "$1" ]' \
  'secureboot_dir_meta" != 0:700' \
  'secureboot_artifact_present /var/lib/tilo/secureboot/db.key.cred' \
  'secureboot_public_file_safe /var/lib/tilo/secureboot/db.crt' \
  'secureboot_private_file_safe /var/lib/tilo/secureboot/db.key.cred' \
  'secureboot_private_file_safe /var/lib/tilo/secureboot/db.key'; do
  grep -Fq "$custody_check" "$audit" \
    || bad "installed audit does not continuously verify Secure Boot key custody: $custody_check"
done
for firmware_chain_check in \
  'tilo-crypto firmware-read-esl /sys/firmware/efi/efivars' \
  'secureboot_firmware_contains_certificate db /var/lib/tilo/secureboot/db.crt' \
  'secureboot_firmware_contains_certificate KEK /var/lib/tilo/secureboot/KEK.crt' \
  'secureboot_firmware_contains_certificate PK /var/lib/tilo/secureboot/PK.crt'; do
  grep -Fq "$firmware_chain_check" "$audit" \
    || bad "installed audit does not verify live firmware owner trust: $firmware_chain_check"
done
for retirement_check in \
  'secureboot_artifact_present /var/lib/tilo/secureboot/PK.key' \
  'secureboot_artifact_present /var/lib/tilo/secureboot/KEK.key' \
  'secureboot_private_file_safe /var/lib/tilo/secureboot/PK.key' \
  'secureboot_private_file_safe /var/lib/tilo/secureboot/KEK.key' \
  'secureboot_esl_matches_certificate /var/lib/tilo/secureboot/db.crt' \
  'secureboot_esl_matches_certificate /var/lib/tilo/secureboot/PK.crt' \
  'secureboot_esl_matches_certificate /var/lib/tilo/secureboot/KEK.crt' \
  'tilo-crypto key-matches-certificate /var/lib/tilo/secureboot/PK.key' \
  'tilo-crypto key-matches-certificate /var/lib/tilo/secureboot/KEK.key' \
  'secureboot_private_file_safe /var/lib/tilo/secureboot/enrollment-keys-retired'; do
  grep -Fq "$retirement_check" "$audit" \
    || bad "installed audit does not verify Secure Boot enrollment-key retirement: $retirement_check"
done
if grep -Eq 'mokutil[[:space:]]+--sb-state' "$audit"; then
  bad 'Secure Boot health still delegates firmware state parsing to removed mokutil'
fi

for policy_fragment in \
  '"registry.tilo.so/tilo/tilo"' \
  '"type": "sigstoreSigned"' \
  '"keyPath": "/usr/share/tilo/cosign.pub"'; do
  grep -Fq "$policy_fragment" "$audit" \
    || bad "boot audit does not bind image trust to $policy_fragment"
done

exit "$fail"
