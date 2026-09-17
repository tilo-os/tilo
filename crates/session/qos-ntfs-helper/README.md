# NTFS Stage 1 diagnostic helper

This standalone GPL-2.0-or-later package reads native NTFS metadata. It does not
mount, repair, unlock, hydrate, replay logs, authorize a later open, or enable
Connected folder capabilities. Full Task 2 and Windows/Linux session-cycle
support remain incomplete. No service or polkit policy is activated here.

## Build and host fixtures

Install the explicit build prerequisites in vendor/README.md, then:

```
python3 crates/session/qos-ntfs-helper/tests/build_native_fixtures.py --output /tmp/ntfs-new-fixtures
QOS_NTFS_FIXTURES=/tmp/ntfs-new-fixtures cargo +1.97.1 test -p qos-ntfs-helper --tests --offline
cargo +1.97.1 clippy -p qos-ntfs-helper --all-targets --offline -- -D warnings
```

The fixture output directory must not exist. All mutations are limited to newly
created regular image files. Generated full image hashes and expected content
are in manifest.json; the checked-in manifest records one verified generation.
Images are never committed. The uninstalled `qos-ntfs-image-test IMAGE` opens
only a readonly regular file, then calls the same C reader in a child process.
It is diagnostic only and is never included in install staging.

Windows builds/checks do not bootstrap or link this Unix reader. Both binaries
return an explicit unsupported error on Windows. macOS is only a fixture runner,
not a production endpoint implementation. Linux production uses a hard 256 MiB
RLIMIT_AS; macOS rejects both RLIMIT_AS and RLIMIT_DATA with EINVAL. Host fixture
results explicitly contain `memory_limit_enforced: false` and cannot prove the
Linux allocation boundary. Both use 10 CPU seconds and a 20-second parent wall
deadline, checked read/output bounds, and child failure invalidation.

## Administrative Linux command

After separate review/install/account provisioning by an administrator:

```
sudo /usr/libexec/qos-ntfs-probe inventory
sudo /usr/libexec/qos-ntfs-probe inspect-selected --inventory-id ID --root-mft 5
```

The dedicated `qos-ntfs` UID/GID must already exist and must both be nonzero.
No account is created by these commands. Inventory IDs select freshly discovered
native sysfs objects; paths, supplied descriptors, major/minor, claimed health
or exclusive flags are not accepted as authority. The broker supports physical
partitions with native WWID and refuses whole devices, virtual/stacked/holder
configurations or missing hardware evidence. This conservative initial scope
also excludes loop images from production identity.

The broker owns O_RDONLY|O_EXCL no-follow opens beneath its pinned /dev directory,
validates S_ISBLK/F_GETFL/size/sector/read-only/disk-sequence ioctls and sysfs
geometry, and retains all candidate claims through postflight. Native kernel
mount types classify only explicitly recognized non-NTFS mounts. Remaining
candidates are exclusively claimed and their native signatures/serials checked
independently of health; unknown, locked, unreadable, busy or duplicate serial
candidates refuse the selection. Candidate identity, boot bytes and inventory
are rechecked before publication. No current-namespace mount list substitutes
for an NTFS exclusive claim.

A least-privilege child retains only device fd 3 and output pipe fd 4. It drops
supplementary groups, UID/GID, effective/permitted/inheritable and bounding
capabilities, applies no_new_privs, resource limits and an architecture-checked
seccomp allowlist. pread/fstat/F_GETFL are limited to fd 3 and write to pipe 4.
Open, mount, network, pwrite, discard and credential changes are not allowed.
The Linux child dies if the broker exits. Parent preflight and postflight must
agree, and both reader passes share the read and wall budgets.

Persistent diagnostic hashes use length-encoded, domain-separated hardware
WWID, partition start/length, validated native serial/geometry and root MFT
record/sequence/creation metadata. No boot ID, disk sequence or device path is
used as persistent identity. A stable partition UUID is not available from this
sysfs-only inventory and is explicitly null; there is no invented UUID fallback.
Returned JSON expires as authority when the command ends and contains no
capability token. A future broker must freshly claim and compare native identity.

## Native facts and limits

The pinned library uses a custom adapter with no path-opening operations.
Writable callbacks permanently poison results. Native $Volume flags/version,
$LogFile cleanliness and restart-page checks, strict absent/all-zero 4096-byte
hiberfile header, the complete native 128 KiB $UpCase, native directory/MFT
references, all walked attributes, streams, DOS flags, reparse data and security
descriptors are returned. $SII/$SDS security resolution validates index/header,
bounds, descriptor and hash; it never uses the upstream ACL getter's synthetic
fallback. Original names and comparison keys remain UTF-16LE hex. Raw binary
metadata is framed in bounded chunks, not converted to host permission bits.

Limits: at most 256 MiB aggregate reads, 31,744 native read calls per pass
(reserving room within 65,536 for two passes and inventory reads), 4096 entries,
64 components, 255 UTF-16 units per component, 1 MiB metadata/content per entry,
64 KiB frames and 4 MiB transport output. A single attribute iterator has a
4096-step limit; allocations/loops inside the Linux native parser are additionally
bounded by OS address-space/CPU limits. Any limit, truncation, crash, unexpected
write or identity change refuses evidence. Status codes: 0 native health facts
collected; 1 Unknown; 2 Locked signature; 3 unsafe volume/log; 4 hibernation;
5 malformed/I/O; 6 limit; 7 policy violation. Status 0 is not a support boolean.

`metadata_complete` stays false: system/extent data and unsupported reparse,
EFS or hard-link scopes are explicit incomplete records. Raw NTFS security and
metadata are diagnostic facts, not an ACL policy or round-trip fidelity promise.
Later metadata capsule, write/session and durability stages remain separate.

## Native boundary tests awaiting a coordinator

The VMs were not accessed. The Linux tests are compiled but deliberately ignored
by default. A coordinator must supply disposable fixtures and set
QOS_NTFS_NATIVE_AUTHORIZATION=DISPOSABLE_FIXTURE_ONLY. For the busy-alias test,
provide QOS_NTFS_NATIVE_INVENTORY_ID of an already mounted disposable NTFS
partition. For the sandbox test, provide QOS_NTFS_NATIVE_DEVICE for an unmounted
disposable block partition and provision the dedicated account. Run the named
fd_boundary test with --ignored only in that environment. Tests do not create,
format, mount, detach or reconfigure block devices. They exercise actual kernel
readonly/seccomp/allocation limits; image-only runs are not substitutes.

The runnable `production_physical_partition_inventory_and_inspection` test
requires `QOS_NTFS_NATIVE_DEVICE` to be a direct, unmounted physical partition
with native WWID, partition geometry and disk sequence; its image must be clean
and have a root directory. The coordinator must remove clone ambiguity and
provision the dedicated account. It runs actual production inventory and
inspection and requires native identity, status 0, zero writes and enforced Linux
memory limits. It never substitutes the image-only reader for production.

For both production tests, set `QOS_NTFS_NATIVE_COORDINATOR_SCRIPT` to the deployed
`tests/native_coordinator.py`. `QOS_NTFS_NATIVE_PROBE` optionally identifies the
deployed production executable when running a cross-built test executable whose
compiled Cargo path is not present. The native runner needs Python 3 and root
ptrace permission. This does not relax the production sandbox.

Run `production_detach_replacement_invalidates_evidence` twice, on separately
prepared disposable fixtures: once with `QOS_NTFS_NATIVE_ACTION_KIND=detach`, once
with `QOS_NTFS_NATIVE_ACTION_KIND=replace`. Set `QOS_NTFS_NATIVE_ACTION` to an
absolute executable path for the **external fixture coordinator**. The harness
first requires the same positive inventory/inspection as above. It then traces a
fresh actual production process to its first native reader fork, verifies the
selected readonly block descriptor in both processes, and keeps them stopped
while the external action executes. No production hooks, fake claims or device
mutations are built into the test harness.

The action receives one argument (`detach` or `replace`) and a JSON stdin request
with schema 1, action, phase `production-claimed-reader-fork`, device, native
inventory, broker/reader PIDs and claim FDs. Within five seconds it must actually
detach or replace that explicitly authorized disposable fixture and return only
`{"schema":1,"completed":"detach"}` (or `"replace"`) on stdout. Replacement
must change the kernel object/disk sequence; a no-op action cannot pass. The
coordinator independently checks kernel identity invalidation, removes ptrace
before the production privilege/seccomp setup, and requires nonzero termination,
zero evidence output, exited reader and no remaining claim descriptors. Its
cleanup kills only its own recorded processes. The source supports these tests;
**neither positive, detach nor replacement has been executed on Linux yet**.
The external action remains environment-specific and must be supplied by the
fixture controller; there is deliberately no default device mutation command.

Cross-namespace automounter races, watchdog death, actual Windows Fast Startup/
hibernation and allocation-bomb Linux scenarios also need native evidence.

## Strict logfile admission

The pinned recovery parser can ignore one malformed restart page. Stage 1 instead
compiles its internal structural validators from the checksum-verified source
and calls them on **both** restart pages without recovery. Public recovery/write
entry points in that translation unit are renamed and unused. Both pages must
be version 1.1 with consistent geometry, intact MST sectors, valid restart areas
and client lists, and clean state. CHKD, version 2.0 and pages larger than 64 KiB
are conservatively refused. An erased logfile is accepted only after every byte
is verified as 0xff through the same bounded readonly adapter; cached empty flags
and sparse header sampling do not establish emptiness.

## Staging only

`stage_install.py --binary LINUX_ELF --output NEW.tar` makes a deterministic tar
with root-owned executable/license/source paths, no setuid bit and no service,
policy, account or mount activation. It never writes /usr or OS configuration.
Cross compilation requires explicit QOS_NTFS_CC, QOS_NTFS_AR,
QOS_NTFS_RANLIB, QOS_NTFS_NM and QOS_NTFS_HOST plus Cargo's target linker. Use
fresh output directories when configuration changes. In particular, a host BSD
ranlib must never rewrite a Linux archive; positive parser-symbol verification
catches empty or incompatible archives before linking.
