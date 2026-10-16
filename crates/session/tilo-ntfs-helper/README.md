# NTFS Stage 1 diagnostic helper

This standalone GPL-2.0-or-later package reads native NTFS metadata. It does not
mount, repair, unlock, hydrate, replay logs, authorize a later open, or enable
Connected folder capabilities. Full Task 2 and Windows/Linux session-cycle
support remain incomplete. No service or polkit policy is activated here.

## Build and host fixtures

Install the explicit build prerequisites in vendor/README.md, then:

```
python3 crates/session/tilo-ntfs-helper/source_artifact.py
export TILO_NTFS_SOURCE_OFFLINE=1
python3 crates/session/tilo-ntfs-helper/tests/build_native_fixtures.py --output /tmp/ntfs-new-fixtures
TILO_NTFS_FIXTURES=/tmp/ntfs-new-fixtures cargo +1.97.1 test -p tilo-ntfs-helper --tests --offline
cargo +1.97.1 clippy -p tilo-ntfs-helper --all-targets --offline -- -D warnings
```

The fixture output directory must not exist. All mutations are limited to newly
created regular image files. Generated full image hashes and expected content
are in manifest.json; the checked-in manifest records one verified generation.
Images are never committed. The uninstalled `tilo-ntfs-image-test IMAGE` opens
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
sudo /usr/libexec/tilo-ntfs-probe inventory
sudo /usr/libexec/tilo-ntfs-probe inspect-selected --inventory-id ID --root-mft 5
```

The dedicated `tilo-ntfs` UID/GID must already exist and must both be nonzero.
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
TILO_NTFS_NATIVE_AUTHORIZATION=DISPOSABLE_FIXTURE_ONLY. For the busy-alias test,
provide TILO_NTFS_NATIVE_INVENTORY_ID of an already mounted disposable NTFS
partition. For the sandbox test, provide TILO_NTFS_NATIVE_DEVICE for an unmounted
disposable block partition and provision the dedicated account. Run the named
fd_boundary test with --ignored only in that environment. Tests do not create,
format, mount, detach or reconfigure block devices. They exercise actual kernel
readonly/seccomp/allocation limits; image-only runs are not substitutes.

The runnable `production_physical_partition_inventory_and_inspection` test
requires `TILO_NTFS_NATIVE_DEVICE` to be a direct, unmounted physical partition
with native WWID, partition geometry and disk sequence; its image must be clean
and have a root directory. The coordinator must remove clone ambiguity and
provision the dedicated account. It runs actual production inventory and
inspection and requires native identity, status 0, zero writes and enforced Linux
memory limits. It never substitutes the image-only reader for production.

For both production tests, set `TILO_NTFS_NATIVE_COORDINATOR_SCRIPT` to the deployed
`tests/native_coordinator.py`. `TILO_NTFS_NATIVE_PROBE` optionally identifies the
deployed production executable when running a cross-built test executable whose
compiled Cargo path is not present. The native runner needs Python 3 and root
ptrace permission. This does not relax the production sandbox.

Run `production_detach_replacement_invalidates_evidence` twice, on separately
prepared disposable fixtures: once with `TILO_NTFS_NATIVE_ACTION_KIND=detach`, once
with `TILO_NTFS_NATIVE_ACTION_KIND=replace`. Set `TILO_NTFS_NATIVE_ACTION` to an
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
Cross compilation requires explicit TILO_NTFS_CC, TILO_NTFS_AR,
TILO_NTFS_RANLIB, TILO_NTFS_NM and TILO_NTFS_HOST plus Cargo's target linker. Use
fresh output directories when configuration changes. In particular, a host BSD
ranlib must never rewrite a Linux archive; positive parser-symbol verification
catches empty or incompatible archives before linking.

## Stage 2 administrative retained read session (native acceptance UNRUN)

`tilo-ntfs-probe serve-read-selected --inventory-id ID --root-mft RECORD --client-uid UID`
uses fresh production inventory and two agreeing metadata-only native passes. It
retains all selected/nonselected O_EXCL claims continuously; it never mounts an
NTFS filesystem. The root command requires the existing dedicated `tilo-ntfs`
parser account. This source adds no account, service, policy or installer action.
The locator printed after admission names one root-owned `/run/tilo-ntfs-*/read.sock`
SOCK_SEQPACKET socket. Socket ownership/mode permits the selected client UID;
SO_PEERCRED is checked in both directions. There is one connection and no reconnect.
The parser-free `tilo-ntfs-protocol` crate contains portable wire types and a Linux
client, with no native reader/helper dependency or build script. Imager links only
that crate. Serializable facts and locators cannot create a device claim.

Wire schema 3 has Hello, BeginObservation, NextManifestPage, ReadChunk,
FinishObservation, Cancel and Close operations. Requests carry the nonce and
strictly increasing IDs beginning at zero. Frames are at most 64 KiB. Unknown
operations/fields, ancillary FDs, truncation and replay invalidate the session.
The client sends one request at a time and consumes its complete reply. Any
pipelined request, cancellation or disconnect observed during a native worker
invalidates the operation; the parent kills/reaps the worker before dropping its
claims. The worker never inherits an active request socket after sandbox setup.

The metadata manifest omits ordinary unnamed DATA payloads, retains native UTF-16
and actual UpCase evidence, and requires positive per-record attribute/directory
completion. Its bounded verifier keeps content readability, enumeration and
metadata fidelity separate. ACL fidelity remains pending without an audited
Windows baseline policy. ADS, reparse, hard-link/external alias closure, sparse,
encrypted, compressed and custom metadata remain explicit pending scopes.
Known over-budget metadata remains scoped while enumeration continues; malformed
structure or unknown scope refuses the observation. This does not preserve or
apply a migration metadata capsule and does not authorize silent metadata loss.
Each entry carries exact signed 100 ns access/data-change values only when one
unnamed `$STANDARD_INFORMATION` attribute has complete ordered coverage in a
pinned 48-byte or 72-byte layout. Otherwise it carries a typed unavailable
reason. The raw metadata digest remains authoritative for before/after checks;
wire schema 3 does not change the native parser report schema.

Native range requests return at most 1 MiB, including unaligned/arbitrary ranges.
The broker only accepts tokens issued for its selected manifest generation.
Before/after native metadata must match the token, and root/object/serial and
retained kernel identity are rechecked. Full strict health runs at manifest and
content transaction boundaries. Random ranges are useful administrative reads;
a jump, overlap or switch discards the interrupted provisional whole-file hash.
Only a fresh contiguous offset-zero-to-EOF assembly is a complete hash. Every
unfinished file remains explicitly incomplete at Finish. Final observation
publication requires full postflight; individual chunks are provisional.

Limits: 4096 entries, 64 components, 255 UTF-16 units/component, 4096 path units,
1 MiB metadata/entry, 4 MiB manifest payload, 4 MiB worker output, 1 MiB logical
chunk, 1 GiB logical content/observation, 16 GiB physical reads/session, 4096
content requests/session. Acquisition and each operation have a 20-second wall
watchdog; children retain the existing 10-second CPU/256 MiB Linux address-space
limits. Connected idle timeout is 30 seconds; an observation has 10 minutes and a
session 30 minutes. Fresh observations do not extend session expiry. A stalled
consumer expires. Orderly close cleans the runtime path; SIGKILL/SIGALRM release
FDs and trigger child parent-death handling but may leave a nonaccepting runtime
pathname until administrative/boot cleanup. It is never reusable session authority.

The context-owned Imager read adapter runs actual protocol reads/observations,
uses Windows paths and native canonical keys, and refuses NTFS write access before
any trial. Public safe_read is still Refused(Unverified); safe_write and durable
transactions remain Refused(ReadOnly). No caller flag or environment variable
promotes those capabilities. Copy's retained File source and journals are unchanged;
Copy UI integration and full two-way Windows/Linux fidelity remain later work.

### Running the source-complete opt-in fixture entrypoints

Generate host diagnostic images with `tests/build_native_fixtures.py --output NEW`
and set TILO_NTFS_FIXTURES for native_images/native_session. The 12 MiB file uses
chunk-distinct deterministic bytes; tests compare independent hashes and every
complete image before/after. No generated image is committed. Host execution is
not Linux claim/sandbox acceptance; macOS retains its approved diagnostic-only
memory-limit exception.

The Linux coordinator is `tests/native_session_coordinator.py` (deploy beside
`native_coordinator.py`). It requires:

- TILO_NTFS_NATIVE_AUTHORIZATION=DISPOSABLE_FIXTURE_ONLY and root invocation;
- TILO_NTFS_NATIVE_DEVICE naming an already prepared direct physical partition,
  with the generated `large.bin` fixture and no unresolved inventory candidates;
- TILO_NTFS_NATIVE_CLIENT_UID naming an existing nonroot client account;
- TILO_NTFS_NATIVE_FILE_SHA256 from the independently generated 12 MiB content;
- deployed helper and parser-free example `read_session` paths passed as
  `--probe PATH --client PATH --mode MODE`.

Modes are positive, ranges, transport, aliases, parent-before-child, parent-after-child,
parser-death, client-death, cancel, cancel-manifest, cancel-postflight, watchdog,
idle, stalled-client, detach and replace. Every mode first proves an actual
production session plus the Rust client's full native 12 MiB hash/postflight,
checks retained read-only descriptors, and checks complete device nonmutation
except where explicit detach/replacement removes the device. Fresh exclusive
acquisition proves claim release. Ranges checks arbitrary offsets, interrupted
hash incompleteness and fresh contiguous restart completion. Transport includes wrong UID/nonce/IDs,
SCM_RIGHTS, malformed/oversized/unknown packets, reconnect and a real nonroot fake
broker rejected by the Rust client's kernel credential check. Alias mode performs
explicit temporary RO NTFS3 mounts in this/another namespace and concurrent Busy
attempts for each chunk; no alias mode has been run in this task.

Detach/replace require TILO_NTFS_NATIVE_ACTION, the same external action receipt
contract documented above. The coordinator independently checks changed sysfs
inode/disk sequence and rejects no-op actions. Ptrace holds the actual range-child
fork after its transaction preflight, then detaches before credentials/seccomp.
The parent-after-child fixture also inspects exact dedicated credentials, cleared
groups/capabilities, no_new_privs, seccomp, 10-second CPU/256 MiB memory limits and
absence of inherited socket FDs. Existing native sandbox attack tests remain
required alongside these new-operation lifecycle cases.

Rust ignored entrypoint `production_retained_read_session` uses
TILO_NTFS_SESSION_COORDINATOR_SCRIPT, TILO_NTFS_SESSION_CLIENT and
TILO_NTFS_SESSION_TEST_MODE. The exact-source Imager fixture
`native_session_context_reads_windows_names_and_refuses_write_trial` requires an
already admitted locator in TILO_NTFS_SESSION_LOCATOR and the independent file hash.
It proves actual context registration/read dispatch, Windows encoding, scoped
pending entries, full native hash, candidate-name keys and refusal before a write
trial. Deploy/run it as the selected UID. All Linux session/alias/credential/death/
detach/endpoint execution, all Windows runtime evidence and public capability
promotion remain UNRUN/open. Cross-linking these entrypoints is not native proof.

Stage 2 fix-round fixtures also require the generated `cancel/last.bin` subtree.
Its selected-root manifest has exactly one readable file, ensuring cancellation
at the last chunk cannot be caught by an unrelated later entry-loop check.
`endpoint-cancel-last-chunk` and `endpoint-cancel-postflight` modes require
TILO_NTFS_NATIVE_ENDPOINT_TEST naming the cross-linked actual endpoint test binary.
The coordinator first proves the subtree's native content/hash/postflight, then
stops the target native worker at fork and signals a test-only inherited control
socket. The endpoint fixture flips its supplied AtomicBool and acknowledges it
before the worker resumes; it asserts an unavailable empty tree and an unusable
client session. These are actual endpoint/client tests, without a fake authority
constructor. They remain UNRUN until the disposable Linux runner is authorized.
The positive Rust client prerequisite additionally runs under the selected nonroot
credentials and proves O_RDONLY cannot open the exact root-owned 0711 session
parent before exercising the client's search-only O_PATH pins.

`tests/test_stage_install.py` extracts two generated staging bundles, compares
reproducibility and ownership/modes, verifies all three standalone pinned manifests,
resolves their dependencies offline and builds the extracted helper. Set
TILO_NTFS_STAGING_BINARY to an existing cross-linked Linux ELF; the extracted
crate uses its own target directory. Existing cross-build variables can select Linux.
It never installs the archive or fetches dependencies. The helper stays at
`source/` and its parser-free source dependency is shipped at sibling
`tilo-ntfs-protocol/`. Its framing dependency is shipped at sibling `wire-frame/`
with its license, matching the standalone path dependencies. The upstream archive
is included in the bundle; repository builds can prewarm the checksum-pinned
source cache as described in `vendor/README.md` before going offline.

### Administrative one-time Copy consumer (production acceptance UNRUN)

Linux Imager has an early, non-GUI administrative command using a dedicated
`Client::connect` session. End an existing observer before acquiring its fresh
Copy selection. The ordinary selected client receives the broker's descriptor
file and independently expected full volume/root/UpCase identity:

```sh
imager --native-copy-selected /private/session.json '{"volume":"...","root":"...","upcase":"..."}' /private/fresh-output /private/receipts/copy.v3
imager --native-resume-selected /private/fresh-session.json '{"volume":"...","root":"...","upcase":"..."}' /private/receipts/copy.v3
```

Neither command launches the broker or opens a device. The descriptor is not a
restart grant: resume authenticates a newly selected broker, revalidates the
whole bounded native inventory and actual output identities before repairing a
journal tail, and independently rereads receipted native and destination bytes.
V3 journals (`TILOJL3\n`) retain token-free native paths, exact timestamp facts,
per-path pending fidelity, real destination identities and verified content
receipts. Existing local Copy continues writing V2 and reading V1/V2. Passing a
V3 journal to local resume requires explicit fresh native selection.

Every new file has two real source passes, independent destination verification,
exact supported timestamps and native postflight before no-overwrite publication.
Ranges are buffered at 1 MiB; files are limited to 512 MiB by the existing 1 GiB
observation budget. The session's 4096 content requests and all native physical,
time, memory and claim limits remain unchanged. Cancellation after the final
range or postflight discards that file; a blocked operation can take its existing
20-second timeout. A budget failure retains the durably receipted prefix and
requires explicit fresh-selected resume. Root-volume system entries remain
explicit pending scopes; ordinary file success does not imply whole-volume or
Windows metadata completeness. The public native capability remains unverified.

`tests/native_copy_coordinator.py` is executable production acceptance and is
**UNRUN**. It requires the existing disposable physical fixture, deployed broker
and Imager, existing parser/client accounts, and the same explicit authorization
environment as `native_session_coordinator.py`. `--mode positive` verifies the
12 MiB real file, exact times, independent destination hash, V3 file/directory
receipts, claim teardown and whole-device nonmutation before any negative mode.
Modes cover fresh-selection resume, actual native worker holds during second-pass
final range/postflight cancellation, client/broker/parser death, externally
coordinated detach/replacement, and live-Copy mount refusal. The three crash modes
require `--native-test` pointing at the cross-linked exact-source test executable;
the ignored `native_selected_copy_command_worker` uses the real authenticated
command, with test-only publication/receipt crash boundaries. `budget` additionally
requires an explicitly prepared bounded inventory at `--budget-root`; no fixture
can raise native limits. Use `--baseline-root` if the positive fixture is under a
separately selected directory. These scripts must not run on user disks.

Host diagnostics use the uninstalled image-test binary's `--copy-manifest` to
normalize actual native metadata. Test-only Imager transport invokes actual
read-only manifest/range operations, never caller-supplied reports or a production
helper/parser dependency. `build_native_fixtures.py` additionally creates
`copy-tree.img` with nested empty directories. These diagnostics demonstrate bytes,
times and consumer persistence; they do not prove Linux claim, credentials,
limits, kernel support or Windows runtime acceptance.
