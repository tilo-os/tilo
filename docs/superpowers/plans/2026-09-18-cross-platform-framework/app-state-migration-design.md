# Task 5 — bounded app-state migration contract

2026-09-18. Approved design for the bounded Unix consumer increment; implementation is active after Task4 Unix source acceptance and verified shell integration. The Windows namespace-commit candidate below remains unresolved. No product/build/test changes or native qualification were performed. `atomic-file/src/provision.rs` was inspected while its sole implementer was active; rebase the extraction on the accepted Task 4 result. The names below are proposed implementation contracts, not APIs already approved or implemented.

## Decision and completion boundary

Implement the Unix consumer slice using the accepted provisioner's real existing-chain barrier engine, owned `FileLease`, held file publication, and an explicit existing-file commit operation. Preserve the exact canonical regression assertions. The first real chain commit must occur after `open_canonical_with` installs its configuration; actor publication must also move after initial source admission. Do not successfully provision first and then simulate failure on a redundant sweep.

Full Task 5 remains dependent on Task 4's unresolved Windows ancestor/bootstrap barrier and a related existing-file namespace reconciliation argument. The existing Windows publication strategy grounds its candidate sequence in the original write-through staging handle through rename and final flush. It does not by itself establish the same guarantee for a file reopened after the original error/handle was lost. Both migration winners and `accept_canonical` without repair need this distinction. A permanent Unsupported branch, byte equality, or passing cross-compilation cannot close that work.

No global registry, stored proof, serialized native identity, new grants, app OS branches, replacement claim format, or alternate storage engine is needed. Native code stays in atomic-file; app-state owns actor/schema/room and acknowledgement policy.

## Findings that determine the patch

* `DocStore::{gate,lock}` currently own raw `File` / `Option<File>`. `acquire_gate` embeds Unix flock and refuses canonical Windows ownership; the Windows legacy writer uses `share_mode(0)`. `same_file` always returns false off Unix. `FileLease` already consumes a single handle, distinguishes Busy/Io, and exposes full native identity without a handle clone.
* `open_canonical_with` currently creates directories, opens an ordinary directory File, acquires the gate, **writes the actor if needed**, constructs/configures the store, then checks the source and commits the chain. Its pre-actor source requirement is presently unmet.
* `sync_directory_chain` flushes every physical ancestor exactly once leaf-to-root on success. The fault callback precedes each real fsync. The retry test requires all descendants to exist at the first injected `profile` failure, and the subsequent successful open to record exactly the full physical ancestor vector. General held-object authority after a rename is insufficient for canonical source acceptance.
* `write_full` currently publishes through an ambient path and only then repeats the canonical chain barrier. `save` advances persisted heads only after success; the autosave worker updates `durable_heads` only after its actual write returns success. Preserve these frontiers.
* `load_canonical_inner` and `accept_canonical` use ambient reads; acceptance without repair opens the document read-only and calls `sync_all`. The Windows native `open_read` also requests only `FILE_GENERIC_READ`. Neither supplies the writable existing-file commit contract needed here.
* `claim_room_marker` and the hard-link collision branch return owner equality immediately. `.qos-` before `.tilo-`, permanent winners, `.recovered`, exact `room\n`, and malformed-marker refusal are compatibility requirements; equality is not a durability proof.
* Current native Unix `CommitGuard::commit` flushes the parent because its publication path already flushed the retained stage data. It cannot be reused unchanged to reconcile an arbitrary existing winner: that route must first flush the existing file itself.

## File ownership and minimum shared seams

| File | Bounded change |
| --- | --- |
| `crates/system/app-state/src/lib.rs` | Owned gate/writer leases; canonical StorageDirectory; source admission ordering; held actor/document paths; exact acceptance; typed error preservation; room claim integration; regressions |
| `crates/system/app-state/src/autosave.rs` | Worker-local production fault entry and frontier assertions; preserve scheduling/sticky-error behavior |
| `crates/system/app-state/Cargo.toml` | Default-off atomic-file test-support dev edge; remove Unix libc edge only after its final use is gone and parent accepts that necessary manifest refinement |
| `crates/system/atomic-file/src/provision.rs` | Extract existing-only admission using the same Chain validation/commit engine; bind expected leaf; distinguish binding changes from barrier I/O |
| `crates/system/atomic-file/src/directory.rs` | Narrow production methods for sentinel acquisition, existing-file commit, and held existing append |
| `crates/system/atomic-file/src/native/{unix,windows}.rs` | Implement native access/validation for those actual consumers; no native logic copied into app-state |
| `crates/system/atomic-file/src/lib.rs` | Only corresponding used exports |
| `crates/system/atomic-file/src/test_support.rs` and existing native test files | Actual existing-commit checkpoint/fault observations and retained identity tests, if existing checkpoints cannot express them honestly |

Do not change the current `FileLease` ownership model or expose a raw handle accessor. Three narrow shared responsibilities need review before implementation:

1. **Existing-only directory reconciliation.** Proposed Unix signature:

   ```rust
   pub fn reconcile_directory(
       path: &Path,
       expected_leaf: &StorageDirectory,
       privacy: Privacy,
   ) -> Result<(), DirectoryReconcileError>;
   ```

   Expected identity is queried from the actual held authority, never supplied as a serialized assertion. Admission canonicalizes an entirely existing path, forward-opens and retains every physical edge, compares the leaf against `expected_leaf`, then executes the exact shared `Chain::commit`. It never calls mkdir, `provision_directory`, or the missing-suffix path. Ready-state retry retains the expected identity and all edges, rechecks them, and repeats the full barrier sequence. A mismatch/disappearance has an explicit SourceChanged discriminator/path; true native open/flush failure retains its original I/O error and the owned `ProvisionError`/complete-chain obligation. Do not map every native error to SourceChanged or infer a mismatch from arbitrary ErrorKind. This may be a small wrapper enum around source-changed versus the existing ProvisionError, rather than a second commit engine. Provisioning's external error semantics remain intact.

2. **Sentinel lease acquisition.** Proposed method:

   ```rust
   pub fn try_acquire_lease(
       &self, name: &OsStr, mode: LeaseMode,
   ) -> Result<FileLease, AcquireError>;
   ```

   A single-component, held-parent, no-follow, regular-file, nontruncating create-or-open; ordinary inherited creation policy preserves the current sentinels. Immediately consume the sole opened File in `FileLease::try_acquire`. Validate kind/reparse policy before acquiring; do not replace the sentinel. No duplicate file handle survives the transfer. This operation promises coordination, not sentinel/ancestor durability; canonical reconciliation supplies namespace obligations. Source checks use `FileLease::identity()` and an independently opened current child solely for identity comparison, not a duplicate lease-bearing handle or another owner acquisition. No separate public arbitrary native open-flags API is needed.

3. **Commit an existing file and its existing name.** Proposed method:

   ```rust
   pub fn commit_existing(
       &self, name: &OsStr, expected: &FileIdentity, privacy: Privacy,
   ) -> Result<(), ExistingCommitError>;
   ```

   Open the existing child relative to the retained parent with native rights needed to read/flush; never create, truncate, rename, replace, link or unlink it. Validate kind/privacy and full identity against the handle from which the consumer read the winner/candidate. Retain the file, parent, expected identity and component through all barriers. Revalidate binding before and after commit. `ExistingCommitError` must retain source and non-deleting owned retry custody; `retry_commit(self)` repeats only this existing-object commit after revalidation. A source-change discriminator maps deliberately to app-state SourceChanged. `into_io_error()` preserves the owned error. This is not a new publication attempt, so do not manufacture `PublicationState::NotPublished` or `PublishedDurabilityUnknown` for it. Failure means existing bytes/name may still be uncommitted. A fresh call can reacquire and repeat under the same checks after an old error is dropped.

   On Unix: flush the admitted existing file, then the retained parent, checking binding; the app-state operation separately reconciles the full directory chain when required. The public method is genuinely consumed by matching claim winners and no-repair canonical acceptance. Its `Privacy` parameter also makes it reusable by Task 7's existing private key winner: that consumer validates exact 32-byte key semantics, while this method validates OwnerOnly and commits the native file/name. Do not add key parsing here or duplicate native flush code in accounts. On Windows: the exact successful namespace-commit implementation is a **candidate awaiting the refinement below**, not permission to ship a permanent failure stub or to certify the Unix-only method as portable.

For canonical append, add only a narrow `StorageDirectory::open_append_existing(name, privacy) -> io::Result<File>` if migrating this path in the same slice. It opens a writable, no-follow regular file relative to the held parent and never creates. This is consumed by the existing delta writer; `File::sync_all` then has native write access. Legacy append's deliberate create policy can remain its current shared std path in the bounded patch. Do not introduce a generic configurable file service just to express one operation. Leaving canonical append ambient would leave a check/open race into a replaced directory and must be explicitly excluded from any held-canonical-write completion claim.

## Canonical ordering: exact first real barrier

Select the approved **keep initial construction, reconcile afterward** route for the Unix slice:

1. Validate package and absolute path exactly as today. Initial construction may retain `create_dir_all` with Inherit policy; it is setup, not a durability acknowledgement. Admit the resulting `StorageDirectory` and acquire its exclusive canonical sentinel through the shared method. The Windows construction path cannot be marked complete until Task 4's native creation/anchor protocol is selected.
2. Install the existing `open_canonical_with` configuration before the first real chain commit and before actor I/O effects. Preserve its exact test call signature. Construction-only state may hold an in-memory actor placeholder, but must not escape, create a worker, accept a document, or become a usable owner before initialization succeeds. Prefer this invariant over a mandatory placeholder implementation; a private preparation/configuration split is equally valid if the exact tests remain unchanged.
3. Check current directory identity against the retained directory; check current held-directory gate child against `FileLease::identity`; any missing/replaced canonical directory or gate yields SourceChanged. Check before actor/document effects. A successful independently reopened ambient directory is only comparison evidence; never replace held authority with it.
4. Call existing-only reconciliation once. It opens the full existing chain and validates expected leaf before any barrier; its shared native loop emits BeforeSync immediately before each actual fsync, leaf-to-root through `/`. Recheck canonical source/gate after success. A changed ancestor, alias or gate never triggers recreation.
5. Only now load or create the actor through the retained directory, then recheck source/gate. Return the fully initialized store. Actor publication's native parent commit is a real file-publication obligation, not a second synthetic directory-chain pass and not an extra entry in `sync_visits`.

For `sync_directory_chain`, wrap the shared operation with existing before/after canonical checks. In tests install a scoped provisioning observer/failure bridge for that actual operation. Populate the existing `faults.sync_visits` only on real BeforeSync events; use the configured `sync_failure` path with existing `with_provision_failure(BeforeSync, path, ...)` around that same operation. `with_provision_observer` runs before failure at the same native checkpoint. The current test fault path is configured before each operation, so no new fallible observer is required. Do not call callbacks manually outside the engine. Hooks must restore previous scope on unwind and run on the thread doing the fsync. No release hook or extra successful first pass.

Retain `Arc<()>` for CanonicalLoad's owner-generation check; native FileIdentity complements it and does not replace that logical owner token. Reconciliation's retained edges detect source changes beyond simple leaf equality. Real mounts/firmlinks preserve Task 4's forward-edge/full-chain contract; don't add a same-volume ancestor rule.

## Gate and writer policy

`DocStore::gate: FileLease`, `lock: Option<FileLease>`, and `CanonicalState::directory: StorageDirectory`. Acquire canonical exclusive / legacy shared gates before actor effects. Both use the same sentinel name and native FileLease semantics. Canonical Busy maps to existing WriterBusy; native open/lock failures remain StoreError::Io. The legacy writer's existing policy deliberately collapses contention or inability to acquire into `None` and uses a fresh in-memory actor; preserve that policy explicitly rather than accidentally treating every gate failure as a legacy writer fallback.

Windows sentinel access must support the locking implementation: read/write synchronous File, explicit compatible sharing for independent shared/exclusive lease attempts, no `share_mode(0)` substitute for a lease. Use native open-if semantics without truncation and reject reparse/nonregular objects. A legacy binary still holding its old deny-sharing writer blocks a new writer's open; test this interoperability. A new lease cannot make old pre-gate binaries cooperate, and documentation already excludes those from canonical concurrency. Verify Unix std FileLease/flock interoperability through the actual child-process matrix in this pinned toolchain rather than assuming source-level similarity proves exclusion.

Never reopen/replace a missing gate on save, acceptance, append, or reconciliation. Only initial setup may create it. Identity comparisons query live owned objects and fail closed on observation failure; no zero/fabricated identities and no path-based dev/inode implementation in app-state.

## Held writes, errors, and acknowledgement

Actor reads use `StorageDirectory::open_read` and `Read::read_to_string`; preserve trimming/hex and exact no-newline serialization. Missing or parse-malformed text mints/replaces as before; UTF-8/read errors that previously returned I/O still return I/O. Gate ownership and source checks precede publication. Publish the hex bytes with Replace and Inherit: this slice does not silently impose OwnerOnly on preexisting app-state directories/files.

Canonical `write_full` does: source/gate check → `held_directory.publish(DOCUMENT_FILE, bytes, Replace, Inherit)` → existing-only full-chain reconciliation with pre/post checks → success/completion hook. It never creates missing canonical directories. Legacy full save preserves its recovery policy and prepares/adopts its directory through the accepted provisioning path before using the resulting held directory. Preserve exact Automerge bytes, schema markers, refusal, load, incremental save, and tail-repair decisions.

Map a PublicationError with `error.into_io_error()`, then existing `io_err(context, source)`. Add `StoreError::publication_state()` by delegating only its Io source to `atomic_file::publication_state`. Do not stringify, rebuild `io::Error` from kind alone, or claim directory preparation errors are file NotPublished. A native BeforeCommit failure remains PublishedDurabilityUnknown with owned retry custody inside StoreError::Io. Normal save must return that failure, not silently retry until the required test turns green. A later chain error preserves its separate directory obligation even though file bytes are already visible.

`save` updates persisted heads only after full success. `accept_canonical` sets accepted/persisted only after its complete acceptance sequence. Autosave's real worker remains the sole authority for setting durable_heads and clearing the sticky error after an explicit successful checkpoint. No outer-thread fault injection, queued-head update, or bytes-visible observation advances the frontier.

For canonical load/acceptance: read through the held directory; retain full FileIdentity alongside exact source bytes and owner token in the private CanonicalLoad. At acceptance compare token, exact bytes and native identity from a newly held no-follow read, validate schema, and check canonical source. Repair still calls `write_full(repair_bytes)`. **No-repair acceptance calls `commit_existing` on the exact admitted identity, then full-chain reconciliation**. This makes existing externally seeded documents durable before acknowledging their heads; it does not rewrite a valid document to manufacture proof. Recheck source/gate before acknowledgement. The cooperative writer-gate/trusted-namespace premise remains necessary for same-object mutable contents; identity alone does not freeze data against hostile same-principal writes.

## Room claims: IfAbsent and existing winners

Admit one held marker parent for the entire alias operation. The public APIs currently expect a retained legacy source directory and do not create absent parents; preserve that behavior. Reconcile its existing ancestor chain as required before a positive durability acknowledgement. No claim error removes a winner, reassigns ownership or deletes the legacy source.

1. Read `.qos-` and `.tilo-` (and recovered authority where base is malformed) through the same held directory. Decode exactly today. Any valid different owner denies with `Ok(false)`; a negative result does not promise another owner's durability. Keep the disagreement preflight.
2. Claim the old spelling first, then the current spelling. For absent authority call `publish(..., IfAbsent, Inherit)` with exactly `room_key + '\n'`. Never replace a malformed base; its `.recovered` sibling is the claim authority. Malformed recovered winners remain InvalidData; no repair/delete guess.
3. On known PublishedDurabilityUnknown from this call, retain and retry its own commit once/boundedly while holding the operation's authority; on retry failure return the typed owned error. Retry does not rename. After successful commit revalidate the named winning identity/content before `true` and complete any remaining ancestor obligations. In the immutable trusted namespace the committed own payload cannot be replaced by a cooperating contender. Do not loop forever under a persistent test fault.
4. A proven NotPublished collision is not success. Open/read the actual winner, validate complete file identity and exact owner. A matching owner calls `commit_existing` on that same identity; then reconcile required ancestry and revalidate bindings/aliases before returning true. This same route applies when the first read already found a matching winner, including a fresh invocation after a prior final-flush error or uncertain error was dropped. File-content and namespace barriers must really execute; no equality early return. Do not infer a target collision from ErrorKind::AlreadyExists alone: exclusive stage allocation exhaustion can use that kind too. Absent actual winner evidence must preserve the native error or enter only a bounded justified fresh attempt.
5. A PublicationUnknown result cannot call `retry_commit` as if publication were known. It may return the typed uncertainty for a later fresh invocation. If reconciling in the same call, use the complete existing-winner protocol and return success only after its independent native commit; otherwise preserve uncertainty. Never remove possible evidence or label the target absent based on the prior error.
6. An absent winner after a proven collision may restart a bounded fresh IfAbsent attempt after reacquisition; it must not recurse without bound or weaken admission. Under cooperating immutable winners this is exceptional. Existing owners are never republished/replaced merely to reuse fresh-publication proof.

The aliases remain the inter-version atomic exclusion protocol; no new lock sentinel can replace the old spelling's IfAbsent authority, since old binaries would not acquire it. Two same-owner resumptions can commit the same immutable winner; different-owner contenders can never both acquire positive ownership. Scope assumes the existing framework trusted/excluded namespace, not hostile same-principal rename immunity.

## Windows refinement and unresolved evidence

Existing sources establish the following access requirement: native regular-file flush needs a retained writable handle; current `relative(create=false)` requests read-only access. A new existing-file open must explicitly request appropriate read/write/metadata/synchronize rights, use FILE_OPEN (never OPEN_IF), hold RootDirectory, reject reparse/nonregular objects, compare full 128-bit file ID plus volume identity, and use sharing compatible with concurrent immutable readers. Sentinel acquisition and append need their own narrowly chosen dispositions/rights. Do not globally change open_read into a write-capable open or require DELETE merely for reading/committing a winner.

The accepted strategy's sources—Microsoft CreateFileW caching behavior, File Caching, FlushFileBuffers, and native NtCreateFile—support file-metadata flushing and state FlushFileBuffers access constraints. Their existing analysis explicitly grounds fresh publication in **same original write-through handle** continuity. This design did not repeat broad web research or produce new native evidence.

Candidate for finite review: open an existing regular file with write-through and required flush access, validate identity, flush it and establish its already-visible parent-entry reachability using the selected native Task 4 namespace barrier under retained parent/anchor authority. A reopened writable FlushFileBuffers success alone does not settle the name's prior uncertain publication or ancestor reachability. The precise file/name barrier composition needs targeted official-contract review plus native lost-error/restart fixtures. Task 4's directory/anchor qualification must explicitly cover reconciling already-existing uncertain descendants; an initial successful creation case does not cover this one.

Parent must adjudicate that candidate before a Windows successful `commit_existing` contract is advertised. Required fixture: publish a matching immutable marker; interrupt/fail at final commit; discard all original handles/error; fresh process admits it, performs proposed file and namespace barriers, externally records acknowledgement, then test storage interruption/recovery. Repeat for valid canonical no-repair acceptance and already-created parent chains. Process death exercises reacquisition only, not power loss. Record exact rights, native results, filesystem, OS/architecture and hardware/filter profile. Missing fixtures/anchor proof remain outstanding, not skipped passes.

Do not solve this by replacing the winner, rewriting identical bytes, adding a durable-claim sidecar format, flushing a read-only File, or treating ordinary directory `File::open().sync_all()` as the Windows answer. No Windows first-launch or full Task 5 completion claim until this refinement and Task 4 are implemented and qualified. Unix progress can be separately accepted with an explicit remaining-work boundary; it is not a final unsupported Windows route.

## Required production tests and sequence

After Task 4 source acceptance and this API review, write failing tests before implementation; keep work to the above paths. Preserve without weakened assertions:

* `canonical_directory_sync_failure_is_retried_even_when_ancestors_exist`: first failure after full descendants exist; no hidden successful chain commit; exact one full successful visit vector on reopen; later post-publication chain error leaves visible bytes.
* `canonical_missing_or_replaced_directory_and_gate_never_recreated`, schema/refusal/empty/corrupt-source and prefix repair tests.
* Existing child-process canonical/legacy gate matrix, orderly drop and process-death reopen. Add native Windows old deny-sharing writer compatibility and Busy versus actual acquisition-I/O errors.

Add real-path coverage:

1. Configure hook swaps/removes directory or gate before initial source check: no actor/document publication in either original or replacement; no repair/recreation. Replacement during shared reconciliation rejects before actor publication. Observe first actual BeforeSync and no actor StageCreated before it.
2. Full canonical save under native BeforeCommit fault: new complete bytes visible; StoreError phase is PublishedDurabilityUnknown; persisted unchanged; owned PublicationError survives Io wrapping. Native prepublication failure preserves old bytes. Directory error after successful file publication preserves distinct directory error/unchanged frontier.
3. Extend TestFaults with a consumed worker-local checkpoint request; install atomic-file `with_failure` inside `write_full`/worker around the real publication. Explicit checkpoint failure leaves previous durable_heads and sticky error; successful explicit retry updates only completed heads and clears the error. Retain worker cancellation/completion/append-prefix regressions.
4. No-repair acceptance: valid existing bytes unchanged; existing-file data and namespace commits observed; either fault leaves accepted false and persisted unchanged. Identity replacement after load rejects even if replacement bytes are equal. Repair remains a typed Replace publication.
5. Held actor format/read-error behavior, actor BeforeCommit failure and restart; ordinary successful first launch and reopen. Lease precedes all actor effects. Existing actor bytes and malformed compatibility behavior unchanged.
6. Two real processes claim different rooms: exactly one positive durable owner; same-owner resume succeeds; `.qos-`/`.tilo-` disagreement and `.recovered` policy preserved.
7. Claim known final failure: retry really performs native commit; retained retry failure propagates. Drop original error, fresh process finds matching winner: observe actual existing-file **and namespace** barriers before true. Inject either barrier failure and require error, unchanged bytes/names, no second owner. Add PublicationUnknown-after-real-rename case; do not equate observing winner with committed success.
8. Shared operation no-follow/nonregular/privacy admission and identity replacement tests. Source change in existing-only directory/file retry never calls mkdir, rename or deletion. No lock-handle duplication. Windows native run exercises requested rights/sharing; compilation alone does not cover it.

Scope verification to canonical, claim and autosave durability tests, then affected atomic-file regressions and app-state test compilation with `--locked --offline`. Parent coordinates required `cargo check -p shell --tests` and shared builds; this read-only worker ran none. Native Linux/macOS execution and Windows native/VM storage qualification remain separate evidence columns. No ignored tests, synthetic barrier visits, lockfile regeneration, workspace formatting, CARGO_TARGET_DIR changes, staging or commits belong to this design task.
