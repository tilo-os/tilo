## Spec compliance

❌ Issues found: Unix `from_file` does not check that the admitted directory handle can perform the required durability operation before namespace mutation (`crates/system/atomic-file/src/native/unix.rs:50`). The remainder of the scoped Unix publication contract is implemented in the reviewed snapshot.

⚠️ Native Linux and Windows execution remain unverified. The report explicitly identifies those results as compile-only; macOS ACL qualification beyond conservative rejection remains Task 2 work. No Windows publication support is inferred from the legacy bridge (`crates/system/atomic-file/src/lib.rs:47`).

## Strengths

- The compatibility wrapper retains the typed error and original source, and commit retry uses the retained native guard rather than republishing (`crates/system/atomic-file/src/publication.rs:88`, `crates/system/atomic-file/src/publication.rs:97`).
- Cleanup is disarmed before ambiguous submission and rearmed only following the scoped namespace proof; dropping a pending error cannot delete the published object (`crates/system/atomic-file/src/native/unix.rs:258`, `crates/system/atomic-file/src/native/unix.rs:281`).
- Identity retains the complete device/inode pair, held-parent replacement is tested through production publication, and umask behavior has a process-isolated regression (`crates/system/atomic-file/src/native/unix.rs:23`, `crates/system/atomic-file/tests/native_publication.rs:29`, `crates/system/atomic-file/tests/native_publication.rs:135`).
- Owner-only validation runs before secret bytes are written, macOS inherited ACL rejection is exercised, and observers run before failure outside the hook-registry borrow (`crates/system/atomic-file/src/native/unix.rs:335`, `crates/system/atomic-file/tests/native_publication.rs:164`, `crates/system/atomic-file/src/test_support.rs:100`).

## Issues

### Important — P2

1. **Reject special files without blocking first.** `crates/system/atomic-file/src/native/unix.rs:74` opens the child with blocking `RDONLY`, then checks whether it is regular. An existing FIFO without a writer blocks inside `openat`, so neither the type check nor the privacy validation can reject it. This new storage-read API can therefore hang its caller indefinitely on an invalid child. Use a nonblocking open followed by held-handle regular-file validation; retain no-follow behavior and clear the flag afterward if needed for the returned regular-file contract. Add a bounded production-path regression with a FIFO, both privacy modes, and a deadline that fails rather than leaving a stuck test process.

2. **Reject Linux O_PATH directory handles before publication.** `crates/system/atomic-file/src/native/unix.rs:50` accepts every handle whose metadata says directory. A Linux directory opened with `O_PATH` passes that check, can act as the dirfd for stage creation and rename, and remains `O_PATH` after `try_clone`; however, the required `fsync` at `crates/system/atomic-file/src/native/unix.rs:104` fails with `EBADF`, including every retry. Thus an unsupported input handle mutates the target before revealing a capability failure that was knowable at admission. Validate/reject path-only handles in `from_file` without reopening via an ambient path or widening authority. Add a Linux native test constructing `File` from a directory `O_PATH` descriptor, requiring constructor rejection and preservation of the existing target. This is a native-capability preflight gap, not a request to preclude ordinary runtime flush failures.

### Critical / Minor

None.

## Checks and limits

- Reviewed the snapshot-relative diff, brief, and implementation report. The tool truncated the first diff output, so the missing middle was read from the same diff; no changed production file or unrelated source was reread.
- No outside-source checks, product changes, staging, commits, formatting, or suite reruns were performed. The two findings follow directly from the opened-handle flags and validation order; their new bounded/native regressions remain required implementation work.
- The report records the direct facade compensation audit and leaves the separate runtime-identity writer untouched. This scoped diff does not independently prove all downstream recovery policies; their migration review remains later-task work.
- Accepted the reported macOS 16 unit + 10 lease + 13 publication results as existing evidence; neither missing native cross-host evidence nor the conservative ACL policy is represented as passing qualification.

## Assessment

**Spec verdict: Needs fixes. Quality verdict: Needs fixes.**

The publication state machine, source retention, exact-mode/default-mode separation, and cleanup ownership are sound within the explicitly trusted child-namespace contract. The newly introduced directory-read/admission boundary needs the two narrowly scoped fixes above before approval.


# Fix round 1 resolution

### Finding Verdicts

- **Reject special files without blocking first** — ADDRESSED. `open_read` now retains `O_NOFOLLOW`, adds `O_NONBLOCK` for admission, rejects a held non-regular object, and clears `O_NONBLOCK` before returning a regular file (`crates/system/atomic-file/src/native/unix.rs:79`). The bounded child-process regression exercises both privacy modes and kills/reaps on its three-second deadline (`crates/system/atomic-file/tests/native_publication.rs:138`). The report records the expected FIFO-blocking RED and focused GREEN, and the current macOS native-publication run passed all 14 tests.
- **Reject Linux `O_PATH` directory handles before publication** — ADDRESSED. Linux `from_file` checks the admitted descriptor with `F_GETFL` and rejects `O_PATH` before constructing `Directory`, without reopening an ambient path (`crates/system/atomic-file/src/native/unix.rs:50`). The target-gated regression constructs an `O_PATH | O_DIRECTORY` descriptor and verifies constructor rejection plus preservation of the existing target (`crates/system/atomic-file/tests/native_publication.rs:172`). The exact production and fixture path compiled for Linux ARM64; native Linux execution was unavailable, so required native qualification evidence remains outstanding.

### New Breakage in the Fix Diff

None. The fix preserves no-follow admission, restores blocking semantics for returned regular files, does not reopen held authority, and leaves runtime commit failures phase-typed rather than trying to preflight ordinary `fsync` outcomes.

### Out-of-Scope Observations

None.

### Verdict

**Fix round:** All findings addressed, no new Critical/Important breakage.

**Spec verdict:** Pass for the scoped fix by code and test inspection; native Linux execution remains outstanding qualification evidence. **Quality verdict:** Pass.
