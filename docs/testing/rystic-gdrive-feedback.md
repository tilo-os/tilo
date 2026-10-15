# Google Drive adapter against Rystic

## Current verification

On 2026-10-14, **before starting a metered instance**, ran
`rystic update -no-self google-drive-twin`. This updated the installed twin from
**0.0.3 to 0.0.5**, published `2026-10-14T18:05:59Z`, commit `28864b4`.
The CLI remained **0.0.28**, commit `3e381c4`, built
`2026-10-12T14:34:05Z`, Go 1.24.0 linux/amd64.
The running twin's version handshake reported **0.0.5**, model
`sha256:2521a3325edc5162aa9da197dc7df19cec9f62049014fbbc784bcba6b830423a`.

The new twin requires a bearer value starting `ya29.`. Tests use the dummy
`ya29.tilo-rystic-gdrive`, not a Google credential. An initial run with the old
dummy prefix failed authentication; that was a test configuration mismatch,
not evidence of missing Changes or upload endpoints. Installation credentials
are neither read nor printed by the test harness.

```bash
python3 scripts/test-cloud-rystic.py gdrive --contracts --volume
# Independently green Changes-only contract (2 passed, 0 failed):
python3 scripts/test-cloud-rystic.py gdrive --changes-only
```

The runner updates only the Drive twin, compiles all selected targets using
normal Cargo routing **before** starting a uniquely named, dedicated instance,
then executes fetched test binaries locally. Startup is bounded to 60 seconds;
each binary to 240 seconds. Setup HTTP requests have explicit timeouts;
production adapter requests retain their production timeouts but cannot outlive
the binary's paid-run deadline. `finally` stops and removes only the owned
instance, including test failures, panics, timeouts and keyboard interruption.
Each cleanup command is bounded to 30 seconds; removal is attempted even if
stopping fails. No privileged FUSE mount or personal account is used.

The adapter target finished **4 passed, 1 failed, 0 ignored**; the real
filesystem target finished **2 passed, 0 failed, 0 ignored**. Overall:
**6 passed, 1 failed**, runner exit **1**. The only remaining failed contract
is genuine native DOCX export. Adapter and filesystem test-body times were
4.05 and 0.62 seconds respectively, not billed instance lifetime.

## Measured production paths

`crates/system/cloud-source-gdrive/tests/rystic.rs` exercises the real
`GDrive` implementation of `CloudSource`, not its fake server:

- Existing import/read/metadata contract: 1,001 seeded binary files, paginated
  listing preserving duplicate names, exact complete/ranged/revision-pinned
  bytes, revisions, rename/move/delete, quota and native-document classification.
- Initial sync: complete snapshot of all 1,002 entries, no duplicate IDs,
  bounded pagination, nonempty terminal cursor, empty subsequent delta and
  stable idle cursor.
- Post-cursor Changes contract: after saving a terminal cursor, rename 1,000
  files (including a move to an existing folder) and delete another through
  ordinary provider APIs. A fresh adapter resumes the saved cursor across at
  least two Changes pages and verifies exactly the expected 1,001 metadata
  updates/tombstones, without duplicate IDs, loss or reset. The terminal cursor
  remains stable when idle. Fixture seeding is setup only, never a mutation
  under proof.
- Real resumable upload commits exactly the submitted bytes and reads them
  through the production provider. No multipart/media fallback substitutes for
  a failed resumable request.
- Strict native DOCX export still validates ZIP integrity, required XML parts,
  Office types/relationships and the WordprocessingML body. It is not weakened
  to accept stand-in bytes.

`crates/system/fs/tests/gdrive_rystic_volume.rs` is the opt-in
`tilo-fs --features rystic-tests --test gdrive-rystic-volume` target. Depending
from fs to the provider avoids a reverse development-dependency cycle. It uses
production `CloudVolume` inode APIs, the same operations the FUSE adapter calls:

- Local create/write/release, `upload_pending`, exact provider bytes.
- External byte replacement using the real upload API, `sync_once`, exact
  mounted bytes; local and external rename/delete propagation.
- Native Docs remain ordinary-sync read-only exports (0444; write-open refused).
- A persisted terminal cursor is reused on idle sync and after reopening the
  volume. Reopened cached bytes are checked with an invalid provider credential,
  distinguishing durable content from silently downloading it again.
- Independent binary import uses a private account configuration, credential
  handoff and the production `CloudService` Files socket. The production
  `tilo-cloud-files::execute_at` engine consumes a host-issued exact-payload
  permit, streams provider bytes and publishes a descriptor-pinned local copy.
  The copy has exact initial bytes; editing it leaves provider bytes unchanged.
  No sync mount or Remote directory is created. This covers the service and
  import engine, **not** the Files app's action/UI admission journey or native
  Office conversion. Binary seed bytes are setup, not a simulated external edit.

## Remaining vendor fidelity limit

On 0.0.5, the strict native export contract still reports:

```text
DOCX export is not a ZIP Office Open XML package (stand-in bytes)
```

The native document is created and exported by the production adapter, but the
export cannot prove an independent Office import. A genuine, structurally valid
Office package is required; preserving populated content and rendering fidelity
would require additional checks. The validator remains strict.

Earlier 0.0.3 measurements returned 404 for `changes/startPageToken` and 501
for resumable initiation. **Those are historical observations, not current
endpoint claims.** Changes pagination and real resumable upload now succeed on
0.0.5. Installed descriptive metadata still contains an old “resumable upload
answers 501” sentence, while the HOWTO describes resumable sessions; measured
production requests, not that stale sentence, establish the result above.
Shared drives and rate-limit modeling were not newly verified in this run.

## Credential-free checks

```bash
cargo test --locked -p cloud-source-gdrive --lib --test drive --test conformance
python3 crates/system/cloud-source-gdrive/tests/validate_docx.py --self-test
cargo clippy --locked -p cloud-source-gdrive --test gdrive-rystic \
  --features rystic-contract-tests -- -D warnings
cargo clippy --locked -p tilo-fs --test gdrive-rystic-volume \
  --features rystic-tests -- -D warnings
cargo check --locked -p cloud-source-gdrive --tests --features rystic-contract-tests
cargo check --locked -p tilo-fs --tests --features rystic-tests
```

Ordinary provider tests: **106 passed, 0 skipped**. Validator self-tests:
**4 passed**. Both opt-in target Clippy checks passed with `-D warnings`;
both packages' all-test compile checks passed. These are separate from
simulator test counts.
