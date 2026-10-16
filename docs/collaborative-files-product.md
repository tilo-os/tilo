# Collaborative Files — product completion contract

This is the definition of **full product**, not a description of the current
implementation. A green unit test for one layer does not satisfy a row whose
evidence is an installed, multi-device workflow.

## Implementation checkpoint (2026-08-31)

The repository is now substantially beyond a prototype, but it does **not**
yet meet this completion contract. Implemented product slices include stable
record identity, certified device authorship, exact-record authenticated
transport, named and authority-backed bearer sharing, one-use redemption,
revocation, availability projections, exact-version replica receipts, durable
pinning, safe history-aware physical cache GC, predictive hydration controls,
content-keyed thumbnails, signed history/conflict/restore surfaces, atomic
replacement-rename staging/recovery, atomic binary Keep Both conflict
transactions, Files collaboration panels/actions,
keyboard context menus, row accessibility metadata, and a Fluent localization
foundation with complete English and German catalogs, pseudolocale, and
active-locale plural rules. Generic Files chrome, navigation, ordinary
operations, properties and collaboration copy all resolve through that catalog.
Keyboard-operable Places,
breadcrumbs, modal collaboration sheets and live status regions have unit
coverage, and the real Linux AT-SPI smoke has observed the exported Files,
Breadcrumbs and Places nodes. A live two-endpoint Iroh
record-mesh test now proves transaction invisibility, both participant orders,
receiver restart, durable namespace publication and empty-journal completion.
Eviction intents are bounded, survive restart, are freshly reauthorized, and
repair availability after an interrupted completion update. A live
three-endpoint Iroh test now uses distinct certified identity roots and stores
to prove that an obsolete replica cannot acknowledge the current revision, a
valid wrong-version or aged receipt cannot authorize safe eviction, and a
fresh exact-version durable receipt can authorize owned-tag release. It waits
for exact backend absence after GC, then rehydrates and durably verifies the
same revision from the ACL-authorized replica.
Exact-share recipients now request reciprocal host durability receipts over
the mutually authenticated, bounded record protocol. The host signs only the
share's exact current revision after verifying its local store; the recipient
verifies the host identity, nonce and complete revision tuple, then persists
that observation across restart for ordinary safe-eviction policy. Read-only
authority remains read-only, and wrong-version or sibling-record requests fail
closed.
The mandatory real-FUSE exact-share harness now launches the shipped daemon as
separate host and recipient OS processes with distinct certified identity
roots, stores and mounts. It obtains a View link through the owner socket,
proves kernel write denial while device-local Retry/Free Local Space controls
remain usable, waits for the exact backend-GC completion record, restarts the
recipient with its persisted host receipt, and rehydrates and durably verifies
the same revision from the still-running host.
The production OS image build now fails closed on a shipped-image contract:
the assembled rootfs must contain executable `fs`, Files, launcher and FUSE
tools; enabled and mutually consistent host, recipient and identity units; the
ACL and tmpfiles seeds; and a Files registry row, payload, manifest and signed
runtime. It executes the shipped `fs` command surface and a display-independent
Files frame during the image build. Version-tag and manual image builds also
boot the installed bootc disk, log into a real user session, and require a real
FUSE workspace plus active identity and recipient services and their live Unix
socket. The VM uses its graphical/virgl configuration, requires the Tilo shell's
published Wayland socket, and launches the integrity-verified Files package
payload that the installed registry resolves. It must observe that payload's
Files activity, Breadcrumbs and Places nodes over the live AT-SPI bus. Missing
VM/GPU/FUSE/session/accessibility facilities fail the job rather than skip.

Atomic binary **Keep Both** is complete: one V2 manifest signs the exact open
conflict/current revision CAS, stable parent binding, source resolution and new
sibling record. Both participant arrival orders remain invisible until complete;
failure injection between history append/resolution and after durable namespace
delta staging recovers across repeated reopen without a half namespace, duplicate
history or changed sibling id/name/bytes. Files exposes the exact-member action.
Codec-aware Merge is exposed for `.txt` and canonical `.qdoc`. Its bounded
deterministic helper accepts a conservative plain UTF-8 three-way subset and
merges qdoc Automerge replicas independent of arrival order. Signed
single-record Merge transactions bind exact conflict/current/base CAS and pass
local crash/replay, remote repository restart, and remote RecordMaterializer
namespace/restart/replay coverage. Malformed and non-qdoc bytes fail closed.
The Files identity directory is now a real governed projection rather than an
empty placeholder. A room governor assigns a display name to an authenticated
Iroh endpoint; that device announces its human-root-signed certificate, and
only the governor may promote the exact endpoint/certificate binding into the
replicated room governance document. Schema V2 groups each governed human root
with the full certified device details (key, root-signed label, capabilities,
issue time and expiry). Files accepts V1 as an explicit names-only migration
format, reads V2 device details, and shows certified device labels in Sharing &
Access while every share command continues to carry only the human root key.
Duplicate display names therefore remain presentation-only. Invalid roots,
signatures, expired certificates, device/endpoint mismatches, duplicate keys,
oversized projections and guest governance edits all fail closed; Contacts and
unauthenticated presence labels never enter this path. Identity announcement V2
also carries bounded root-signed device revocations while retaining the V1 wire
shape for older peers. Shell re-reads the signed revocation source for every
announcement, room governance merges it additively, and un-name/re-name cannot
resurrect a revoked device. Accepted changes atomically replace the projection
inside a read-only directory already bound into a running Files sandbox; Files
periodically reloads it and filters certificate expiry at read time. Direct
tests cover announcement-to-governor-to-launcher promotion, cross-endpoint
refusal, revocation/removal/rename/new-device/expiry after launch, and malformed
or cross-root revocation refusal.

The storage, transport and Files product workflows are green in this checkout.
Named grants now use authenticated, durable recipient delivery; Files exposes
the complete permission/expiry/usage composer; and collaborative search is
bounded and keyed by stable record identity. The shipped daemon now has one
process-wide clock source for live ACL, share, session, delivery, subscription,
device-certification and exact-receipt decisions. Production defaults to
`SystemTime`; the hidden release-test option accepts only an absolute,
owner-only regular file, pins its device/inode, and revalidates it on every
read. Missing, replaced, permission-weakened or malformed runtime state maps to
the fail-closed maximum timestamp. Exact-share source-loss, process and semantic
forward/backward clock boundaries, concurrent remote
structural conflicts and forced-partition multi-writer coverage are enforced
as mandatory release tests.

The mandatory CI contract runs two partition layers. The state-machine matrix
uses no timing sleeps: two writers edit independently, atomically persist and
reopen their offline namespace images, then cross an explicit reconnect
barrier. The shipped-daemon matrix uses distinct identity roots, stores,
processes and real FUSE mounts; an owner and a second certified device partition
through acknowledged owner-only Unix-socket gates, write concurrently, restart
while partitioned, reconnect and expose the same deterministic live head plus
the same first-class signed open conflict containing both exact DAG heads. A
person-only ACL proves certified device-to-human resolution, a third identity
is denied writes, and an owner `--seed` file is signed before the ticket becomes
shareable and hydrates remotely. Missing FUSE is a failure, not a skip. The
same required job runs explicit future/expired replica-receipt checks,
backward/forward eviction-evidence checks, absolute grant-expiry boundaries,
authenticated read-only write denial, and the Keep
Both all-phase restart test proving journal drain and no half-publication. This
is deterministic protocol/state-machine release evidence. The exact-share
process gate additionally evicts recipient bytes through the public control,
proves restart rehydration, then uses a second store/mount to reach a persisted
`NotResident`/`projected` barrier, stops the only source process, observes `EIO`
without losing metadata, and leaves recovery actionable. It does not claim
process-wide wall-clock injection: the already-running host is moved backward,
to the exact expiry boundary and forward without sleeping, malformed live
clock content denies share creation, and the signed host receipt's issue and
observation times equal the injected process instant.
The German catalog has exact key, variable and selector parity with English;
other unsupported languages deliberately fall back to English. Dates and exact
decimal-SI byte units use ICU4X/CLDR patterns, digits and separators for the
active locale. Unix filename identity is byte-preserving end to end: signed
bindings and structural transactions use a canonical byte name, Automerge
loads legacy UTF-8 string scalars and writes byte scalars, and FUSE preserves
the exact bytes through lookup/create/list/rename/delete, replication and
restart. UI/policy paths use reversible percent escaping and never feed a
lossy label back into identity. Non-Unix filename portability remains open. The
AT-SPI runtime smoke is green on a real graphical session and is now part of
the installed-image release VM gate. A mandatory deterministic local performance gate now covers a
1,000-entry persisted metadata open/list, repeated warm listings, ranged reads
across a 64 MiB chunked file, and cold/warm thumbnail paths. It does not yet
cover network hydration, FUSE syscall overhead, memory/RSS, sustained ingest,
true cold block-device/page-cache behavior, or larger-directory construction.
The mandatory corruption/migration gate now covers truncated and tampered
namespace state, malformed repository/history/transaction/availability/eviction
state, stable-device-key truncation, and legacy Automerge string names moving
to byte scalars. Present damaged state refuses startup without being replaced
by an empty store or partially projected namespace. Namespace, signed-record
repository and derived history writes now retain three atomic, integrity-named
known-good generations. Startup automatically chooses only the newest backup
that passes the store's complete parser and invariant validation; signed record
recovery additionally re-verifies every envelope signature, sequence and DAG.
The exact damaged primary is retained beside a durable JSON recovery receipt,
and startup emits the evidence and receipt paths. If no valid generation exists,
startup still fails closed and leaves the primary untouched. The backup reader
has a 256 MiB pre-allocation bound, backup/evidence/receipt retention is bounded,
and periodic namespace persistence refuses to overwrite a primary corrupted
while the daemon is live. Repository reconciliation replays signed operations
after an older namespace/history generation is recovered. Identity keys, ACLs,
share/revocation authority, availability receipts and eviction authorization
are deliberately excluded: rolling those security-sensitive stores backward
could revive an identity, grant, stale durability claim or deletion decision.
Device-key repair therefore remains an explicit administrative operation.
Process-wide wall-clock injection is covered by the shipped-daemon exact-share
gate as well as explicit grant/receipt decision-layer inputs. The
installed-OS contract is now a release-tag gate, but no run of that new gate is
recorded in this checkout, so this document does not claim installed boot
evidence yet. The shipped greetd entries now run `tilo-session`, which hosts the
existing no-recovery identity ceremony in a temporary single-client Cage
session when needed, verifies the personal root, persisted root/device secrets
and signed certificate without mutation, and only then activates
`graphical-session-pre.target` and `graphical-session.target` through a
Tilo-owned user target. Failed or cancelled enrollment never reaches the
desktop, while exit and termination release the target cleanly. The release VM
drives that real keyboard-accessible ceremony rather than installing a test
identity behind the product's back. The real-FUSE CI
workflow exists and fails when `/dev/fuse` is absent, but it still needs to be
proven in the actual Forgejo runner environment.

### Verified evidence in this checkout

- `cargo test --locked -p tilo-fs --tests -- --test-threads=1`: 403 tests passed
  with no failures: 346 library, 20 daemon binary, 4 release-matrix, 5
  corruption-matrix, 1 exact-share process/FUSE, 26 real-FUSE integration and
  1 performance-gate test. Byte-name migration, replication and
  restart are included. V2 Keep Both coverage includes both
  participant arrival orders with no half-projection, exact conflict/current
  revision CAS, two dedicated crash/reopen cutpoints, deterministic staged
  namespace replay, idempotent retry, journal drain, and a later conflict
  authored against the committed transaction-projected revision.
- `cargo test -p fs --test corruption_matrix -- --nocapture --test-threads=1`:
  deterministic corruption cases cover namespace truncation, automatic
  bounded known-good recovery with retained damage and a receipt, repository
  and transaction-journal startup refusal with no
  namespace publication, history/availability/eviction refusal without silent
  reset, and stable device-key refusal without actor rotation. The paired
  `legacy_string_name_loads_exactly_and_next_rename_persists_bytes` unit gate
  proves legacy UTF-8 scalar loading, byte-scalar migration and restart. The
  mandatory workflow also pins signed-repository tamper refusal, corrupt
  history ancestry, partial transaction-batch refusal, coordinator recovery
  without duplicate history, and persisted receipt tamper refusal.
- `cargo test --locked -p fs durable_backup --lib -- --nocapture`: 6/6 passed.
  The gate covers three-generation retention, exact damage evidence and
  completed receipts, repeated recovery without artifact overwrite, fallback
  past a hash-mismatched newest backup, refusal to overwrite a live damaged
  primary, no-backup fail-closed behavior, and pre-allocation oversized-file
  refusal. A separate signed-repository recovery test revalidates envelope
  signatures and restores only the latest valid generation. Both commands are
  mandatory in the real-FUSE workflow rather than relying on an ad hoc local
  run.
- `cargo test -p fs --test integration_test -- --nocapture --test-threads=1`:
  26 real-FUSE tests passed, including concurrent writable handles and
  unlink/recreate while the old inode remains open. Every fixture now joins its
  FUSE session, awaits Iroh endpoint close, and shuts down its blob store before
  the per-test Tokio runtime exits. Three consecutive serialized full-suite
  runs passed (78/78 total) without the previously observed teardown panic or a
  leaked QuickFS mount/process; the mandatory workflow uses this deterministic
  serialized form.
  The invalid-UTF8 create/list/rename/restart/remount path is included.
- `cargo test -p fs --test exact_share_process_fuse -- --nocapture
  --test-threads=1`: 1 mandatory separate-process exact-share lifecycle passed
  on real `/dev/fuse` in 172.61 seconds, including physical safe GC, recipient
  restart and network rehydration. A distinct recipient then hydrated and
  evicted through public controls, reached durable `NotResident`/`projected`
  state, lost its only serving process, and received `EIO` without losing its
  metadata or misstating residency. The same process test changes the live
  daemon clock without wall sleeps, proves before/exact-boundary behavior and
  malformed-input denial, and binds persisted receipt timestamps to the
  injected instant.
- `cargo test -p fs --test collab_release_matrix -- --nocapture
  --test-threads=1`: 4 mandatory release-matrix tests passed. The shipped-daemon
  case used distinct processes, roots, stores and real FUSE mounts; seeded and
  remotely hydrated content was signed before ticket exposure; explicit
  partition/reconnect acknowledgements surrounded concurrent offline writes
  and a gated restart; both writers converged to one live head and the same
  signed open conflict containing both exact DAG heads; the third identity
  remained read-only. The structural matrix covers concurrent rename/move,
  delete/rename and POSIX replacement/victim-move races in both merge orders,
  then proves stable-record source preservation, exact projections, durable
  restart and idempotent repair replay.
- `TILO_FS_PERF_GATE=1 cargo test -p fs --test performance_gate -- --nocapture
  --test-threads=1`: 1 enabled performance gate passed in the debug test
  profile. Measured production paths were 94.084 ms for cold namespace
  deserialization plus a 1,000-entry listing (2 s SLO), 344.790 ms for eight
  warm listings (4 s SLO), 631.981 ms for 64 local 256 KiB ranges across a 64
  MiB chunked file (4 s SLO), 248.898 ms for cold 1024x768 image
  decode/scale/encode/cache (3 s SLO), and 4.420 ms for 1,000 warm thumbnail
  cache lookups (500 ms SLO). Fixed generated fixtures, local stores, no peers,
  no sleeps and no remote requests make the enabled gate deterministic; fixture
  construction and compilation are intentionally outside measured windows.
- `cargo test --locked -p file-explorer --all-targets -- --test-threads=1`:
  175 library, 2 binary, 42 interaction integration and 1 real-FUSE
  collaborative-mount test passed (220 total). Coverage includes localized
  exact-member Keep Both, conservative
  `.txt` Merge, canonical `.qdoc` Merge, explicit one-redemption link requests,
  and multi-selection pin/unpin through live mount projections; binary and
  unsupported Merge remain hidden.
- `cargo test -p collab-types`: 71 library and 112 integration tests passed.
  The broader identity suites passed 49 library plus 2 integration tests; the
  launcher passed 198 library, 2 binary and 36 integration tests; thumbnail
  passed 19 tests. Three launcher tests explicitly require a host with bwrap
  and unprivileged user namespaces and remained ignored.
- `cargo check -p fs --all-targets`, `cargo check -p file-explorer
  --all-targets`, the complete `cargo check --workspace --all-targets`, and
  `git diff --check` passed.
- The wider Shell/UI consumer check exposed and fixed two non-exhaustive
  `FsRecord` capability presentations. UI then passed 2,100 tests. Shell passed
  1,818 tests with two ignored and one Iroh timeout under the full parallel
  load; that exact network test passed on immediate isolated rerun.
- The production Linux `fs` and Files dependency roots have no remaining
  RustSec vulnerability or unsound findings after compatible dependency
  upgrades. `chacha20` is pinned to its non-yanked compatible patch release,
  and the narrow `genawaiter-proc-macro` compatibility fork replaces the
  abandoned `proc-macro-error` diagnostic stack without changing successful
  macro expansions. The roots still include upstream-unmaintained `paste`
  (`fs` and Files), upstream-unmaintained `ttf-parser` (Files), and yanked
  `spin` 0.9/0.10 transitives for which no compatible upstream release exists.
  `cargo deny --workspace check` remains **not green** because unrelated apps,
  dev-only and non-Linux closures retain vulnerable, unmaintained or yanked
  dependencies, and the workspace has broad pre-existing missing first-party
  license metadata. The broad workspace result remains a release blocker.
- `apps/file-explorer/tests/atspi_smoke.sh` passed in a graphical GNOME
  session; it is not executed by ordinary headless `cargo test`.
- `os/tests/collaborative-files-image-contract.sh` passed against an assembled
  rootfs fixture, and its negative control rejected a tmpfiles cache path that
  diverged from `fs.service`. `bash -n` and `git diff --check` passed. This is
  source-side evidence for the gate logic, not evidence that an installed boot
  has already completed; the first version-tag/manual VM run must provide that.

## Product truths

- `local` means bytes are cached on this device. It never means “synced”,
  “backed up”, or “safe to remove”. Those claims require a current acknowledgement
  from another acceptable replica.
- A share grants a stable record, not a mutable path and not a filtered slice of
  one Automerge change graph. Moving or renaming a file does not change who can
  access it.
- A person and a device are different identities. Authority is granted to the
  certified human root; history preserves the exact certified device.
- A one-use link requires an online authority to atomically redeem its nonce.
  A self-contained offline bearer token cannot honestly promise one use.
- Revocation stops future access and updates. It cannot erase bytes a recipient
  already obtained, and the UI must say so.
- Named delivery contains no bearer secret. Files supplies bounded certified
  device endpoint keys from the governed identity projection; the sender
  authenticates a device as the named human before disclosing the signed
  record-scoped envelope. Offline delivery remains visibly pending in a durable
  outbox and retries after restart. One accepting device is sufficient; the
  recipient inbox is owner-only, bounded, idempotent by grant id, and hides
  expired entries. Host revocation cancels pending delivery and the existing
  online authority check denies every later refresh.

## Completion requirements

### Secure record replication and sharing

- Each shareable file/record has its own replication unit and stable id. A peer
  receives no names, metadata, history, hashes, thumbnails, or changes for a
  record it cannot read.
- Every inbound operation is signed by a certified, unrevoked device and checked
  for record-scoped read/write authority before merge. Read-only participants
  cannot author or relay an accepted write.
- Blob and thumbnail requests are authorized by live record reachability, not by
  “may read something in this workspace”. Mixed authorized/unauthorized batches
  fail closed.
- Files exposes Share from the context menu: named recipient or bearer link,
  view/edit, expiry, optional one-use redemption, copy link, revoke, and audit.
- “Shared with” resolves trusted human names, shows devices on demand, falls back
  to a fingerprint for unknown identities, and never uses a display name as an
  authorization subject.

### Availability, offline use, and cache lifecycle

- The daemon reports a versioned availability record: local residency, transfer
  phase/progress/error, automatic vs pinned policy, reachable replica count,
  durable replica count, and freshness.
- Files live-updates visible rows and provides Keep Offline, Remove Offline Pin,
  Free Local Space, and Retry. Directory pins include future descendants.
- Pins persist across restart and rename and rehydrate changed content. Eviction
  protects active transfers, pins, retained versions, and shared chunks. Normal
  eviction is allowed only after another acceptable replica acknowledges the
  content; a destructive override explains the risk.
- Cache accounting is byte-bounded and observable. Garbage collection has owned
  roots/tags and cannot accidentally retain every historical anonymous blob.
- Predictive hydration is lower priority than foreground work, bounded in queued
  work/concurrency/bytes, cancellable, ACL-filtered, and gated by network, power,
  and disk-pressure policy.

### Metadata, thumbnails, history, and conflicts

- A versioned, bounded metadata record projects independently of file bytes. It
  covers type/MIME, content hash, preview variants/status, useful media/document
  properties, latest actor, availability freshness, and index state.
- Metadata and thumbnail reads never perform network or decode work on the UI
  thread. Placeholders, failure and retry are explicit. Thumbnail cache keys are
  content hashes so identical bytes deduplicate across paths and peers.
- Every content and structural mutation appends a durable, replicated version
  with parent(s), actor person+device, timestamp, content/metadata hashes, and
  provenance. Restore appends a new version. Retention and GC preserve reachable
  history.
- Replacement rename is a signed multi-record transaction: source move and
  destination tombstone are staged, committed, recovered and materialized as
  one operation. The crash/replay/state-machine contract is specified in
  [replacement-rename-transactions.md](replacement-rename-transactions.md).
- Conflicts are first-class replicated records, not inferred from a filename.
  Files explains both versions and offers Keep Current, Use Other and Keep
  Both, plus conservative plain UTF-8 Merge for `.txt` and canonical Automerge
  Merge for `.qdoc`. Exposed resolution is attributed, convergent, idempotent
  and undoable.

### Files application quality

- All collaborative actions work for multi-selection where meaningful, have
  keyboard and accessible equivalents, preserve selection/scroll during async
  updates, and surface progress and actionable errors.
- Properties, sharing, availability, history and conflict surfaces work in wide
  and narrow layouts, with honest empty/offline/degraded states and localization.
- Search and the room record index use stable record identity and provenance;
  opening a second document never aliases another record's collaboration state.

### Required release evidence

- Deterministic two- and three-device harnesses use separate identity roots,
  stores and mounts, event barriers rather than sleeps, and controllable
  partition/reconnect/restart/clock/source-loss conditions.
- CI has a mandatory real-FUSE job; absence of `/dev/fuse` is not counted as
  coverage. Demo scripts fail on missing assertions rather than printing warnings.
- Full-stack tests cover named and link sharing, one-use races, expiry/revocation,
  read-only attack attempts, metadata privacy, blob-scope attacks, offline edits,
  conflicts and resolution, version restore after restart, pin/evict/source loss,
  predictive open-to-sibling hydration, and Files interactions against a live
  collaborative mount.
- Security, migration/corruption, accessibility, localization, performance and
  installed-OS smoke gates are green. Each release gate must link its claims to
  direct executable evidence.

## Delivery order

1. Close existing transport authorization leaks and make thumbnail lookup
   non-blocking.
2. Introduce stable record, actor, metadata, version and conflict schemas.
3. Move replication and blobs to record-scoped authenticated channels.
4. Add authority-backed share/link redemption and identity resolution.
5. Add transfer registry, replica acknowledgements, persistent residency and GC.
6. Build Files sharing/availability/history/conflict surfaces.
7. Make the full multi-device harness a mandatory release gate and audit every
   completion requirement above.
