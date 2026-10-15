# Dropbox and Google Drive import/sync test results

## Google Drive 0.0.5: Changes, resumable upload and real filesystem roundtrip

Latest Drive-only re-run on 2026-10-14 first executed
`rystic update -no-self google-drive-twin`, upgrading **0.0.3 → 0.0.5**
(commit `28864b4`, published `2026-10-14T18:05:59Z`). CLI remained **0.0.28**
(`3e381c4`). The running version handshake reported model
`sha256:2521a3325edc5162aa9da197dc7df19cec9f62049014fbbc784bcba6b830423a`.

```bash
python3 scripts/test-cloud-rystic.py gdrive --contracts --volume
```

| Target / contract | Passed | Failed | Ignored |
| --- | ---: | ---: | ---: |
| Production adapter: import/read/metadata | 1 | 0 | 0 |
| Production adapter: complete initial snapshot and stable cursor | 1 | 0 | 0 |
| Production adapter: post-cursor rename/move/delete, paginated delta, restart/resume | 1 | 0 | 0 |
| Production adapter: resumable upload, exact provider bytes | 1 | 0 | 0 |
| Production adapter: genuine native DOCX package | 0 | 1 | 0 |
| Production CloudVolume: two-way bytes/rename/delete, read-only native Docs, cursor/cache restart | 1 | 0 | 0 |
| Production CloudService + tilo-cloud-files independent binary import engine | 1 | 0 | 0 |
| **Total** | **6** | **1** | **0** |

The separate `python3 scripts/test-cloud-rystic.py gdrive --changes-only`
run passed **2 tests, 0 failed, 0 ignored, 3 filtered out**, exit **0**,
independently of the DOCX contract. Its owned instance was also stopped and
removed. These are re-runs of two rows above, not additional distinct tests.

The full-contract runner exits **1** honestly for the DOCX failure:
`DOCX export is not a ZIP Office Open XML package (stand-in bytes)`.
Changes and real resumable uploads now work; the older 404/501 observations
below apply to 0.0.3, **not** the updated twin. No fake HTTP fallback or seed
mutation was used to make these contracts pass. The new Changes test mutates
1,001 IDs after capturing a cursor and requires at least two delta pages,
exact expected metadata/tombstones, no duplicate IDs, no reset and stable idle
terminal cursor after a fresh adapter resumes. The filesystem test calls the
production inode APIs directly, without a privileged FUSE mount. A restart
reads durable cached bytes with invalid provider credentials before resuming
sync with the saved terminal cursor.

The independent-import test configures only temporary accounts/state, uses
production CloudService requests and its Files socket, then runs
`tilo-cloud-files::execute_at` with an exact-payload host-issued permit. The
published local copy has exact binary bytes; a local edit leaves the provider
unchanged, with no sync mount created. This is actual service/import-engine
coverage, **not** the Files app's complete action/UI admission journey and
**not** genuine native Office conversion.

All selected binaries were compiled with normal Cargo routing before the
uniquely owned metered instance started. Test execution took **4.05 seconds**
(adapter target) and **0.62 seconds** (filesystem target); these are test-body
measurements, not billed instance lifetime. The instance was stopped and
removed in `finally`, including on failed contracts. An earlier old-prefix
dummy-token run failed authentication, and an intermediate import test used
a symlinked temporary-root alias rejected by the engine's no-follow policy;
these were corrected in fixture setup, without changing production behavior.

Credential-free provider tests: **106 passed, 0 skipped**. Strict DOCX validator
self-tests: **4 passed**. Both opt-in targets pass Clippy with `-D warnings`.
See [current Drive feedback](rystic-gdrive-feedback.md) for scope and bounds.
Dropbox was not rerun or updated in this Drive-only verification; its earlier
results below remain historical.

## Reverification with a strict native Office export contract

Re-run on 2026-10-14 using installed Rystic CLI **0.0.28** (`3e381c4`,
built `2026-10-12T14:34:05Z`, Go 1.24.0 linux/amd64):

| Installed twin | Version | Commit | Contract |
| --- | --- | --- | --- |
| Dropbox | 0.0.2 | `41c9c6fa969f7c1e8ffb171344f427ff1c0a738d` | 24 |
| Google Drive | 0.0.3 | `5b4a15bd9931181da6c77f2a668c16c41b004475` | 24 |

Dropbox model: `sha256:f8be3e88fddfe7699c9c55484dbcbf0946f9e294a9828c7eba70472687d53ebf`.
Drive model: `sha256:d8f46d5b8036d37ac3fe481297e7204843fb765814a1ef1d9a0e18070e6f863f`.
The installed `rystic describe`, `features` and `docs google-drive-twin`
advertise native document creation and Office exports. The HOWTO's Exports
section explicitly says the bytes are name/description-based stand-ins, not
renderings. That is sufficient API surface to exercise the production export
path, but not to claim successful Office conversion.

| Command | Passed | Failed | Ignored | Exit |
| --- | ---: | ---: | ---: | ---: |
| `python3 scripts/test-cloud-rystic.py all` | 3 | 0 | 0 | 0 |
| `python3 scripts/test-cloud-rystic.py all --contracts` | 3 | 5 | 0 | 1 |

The strict run reproduced the original **four** failing contracts below and
added **one** distinct failure: native DOCX export is not a ZIP Office Open XML
package. Dropbox remains 2 passed/2 failed; Drive is now 1 passed/3 failed.
No new production-adapter defect was established. These counts are separate
from the original vendor feedback and its two additional HTTP-probe limits.

The new `native_document_exports_a_genuine_docx_package` contract creates a
Google Docs entry through the production adapter, checks its advertised DOCX
MIME/extension pair, exports through the adapter, and validates ZIP integrity,
required XML parts, the Word main-part content type, internal Office relationship,
and WordprocessingML document/body. The diagnostic on this exact twin is:
`DOCX export is not a ZIP Office Open XML package (stand-in bytes)`.
It does not accept non-JSON bytes or a ZIP signature as proof, and it makes no
claim about layout fidelity or populated document content.

The Python standard-library validator's four credential-free self-tests pass:

```bash
python3 crates/system/cloud-source-gdrive/tests/validate_docx.py --self-test
```

It also accepts the genuine Office package in `tests/fixtures/docx.rs`, stored
as an escaped Rust byte constant to keep the repository text-only. The self-tests
reject text/JSON stand-ins, a forged ZIP prefix, ZIPs without Office parts,
malformed XML and incorrect Office declarations. These are validator checks,
not additional provider passes.

Compilation and builder-lock waits happened before metering. Test bodies took
0.28s/0.34s for the supported Dropbox/Drive run and 0.20s/2.05s for the strict
run. Every uniquely owned instance was stopped and removed in the runner's
`finally`; a subsequent `rystic ps` reported zero instances. No personal account,
provider credential, production workaround or supported-suite widening was used.
The native contract is enabled only by the existing `rystic-contract-tests`
feature and requires Python 3 on the machine executing the test binary.
Targeted strict-test Clippy (`-D warnings`), Rust formatting and
`cargo check --locked -p shell --tests` also passed on this checkout.

## Original failure counts

Measured against Dropbox twin 0.0.2 and Google Drive twin 0.0.3 on 2026-10-14.
Counts are distinct causes, not individual assertion failures or retries.

| Category | Dropbox | Google Drive | Total |
| --- | ---: | ---: | ---: |
| Confirmed Tilo defects fixed | 1 | 0 | **1** |
| Strict production-adapter contracts failing because of simulator gaps | 2 | 2 | **4** |
| Additional simulator limitations reproduced with HTTP probes | 2 | 0 | **2** |

The one Tilo defect affected **five operation families**: listing, reading
(including ranged/historical reads), revision enumeration, uploads (including
saved-session resumption and targets moved before commit), and sharing. A
folder-scoped Dropbox mount rejected foreign IDs in `stat`, but these other
operations could access items outside the configured root. Scope checks now
reject those operations. This is one missing-boundary defect, not five counted
bugs. Credential-free regressions and a twin-backed regression cover the fix.

The four failing simulator contracts are:

1. Dropbox ID-addressed replacement creates a different file instead of updating
   the original stable ID.
2. Dropbox continuation misses a newly inserted item that sorts before the
   previous enumeration position; it is not a faithful change feed.
3. Google Drive initial sync cannot get its changes cursor: endpoint returns 404.
4. Google Drive resumable upload initialization returns 501.

Two additional reproduced Dropbox limits: a 1,048,577-byte upload returns 413,
and the longpoll endpoint returns 404. These are not additional failing Rust
cases. In this original measurement, shared-drive omissions, Google export placeholders
and other published limitations were not counted as reproduced failures. The
strict DOCX contract above now counts the export limitation separately.

## Original passing coverage and limits

- Credential-free suites: **103 Dropbox tests and 102 Google Drive tests passed**.
- Supported twin suite: **3 tests passed** (2 Dropbox, 1 Drive).
- Combined strict twin run: **7 tests executed, 3 passed, 4 failed, 0 ignored**.
- Both provider all-test compile checks and Clippy passed with contract features enabled.
- `cargo check --locked -p shell --tests` and the staged pre-commit hook passed.

The passing integration tests exercise production provider adapters, not a second
mock implementation. Dropbox covers recursive initial enumeration, pagination,
Unicode names, exact downloaded bytes/content hashes, persisted upload-session
resume, revisions, restore, moves, deletion, sharing and mounted-root denial.
Drive covers a 1,001-file paginated folder with duplicate names, exact full and
ranged reads, revision preconditions, moves, deletion, native-document
classification and quota parsing. Drive file bytes are seeded because its twin
cannot accept the adapter's resumable upload protocol.

That original run did **not** prove end-to-end FUSE import or live-account OAuth,
Dropbox incremental sync, Drive initial/incremental sync, interrupted Drive upload
resumption, shared drives, or genuine Google-native export conversion against
Rystic. The newer 0.0.5 verification above adds real Drive initial/incremental
sync, uploads and independent binary import. It does not establish the other
listed capabilities. Credential-free fixtures cover additional production paths.
No production sync fallback or simulator-specific upload workaround was added.

## Running the suites

From the repository root, with an installed/authenticated Rystic CLI:

```bash
# Supported scenarios; successful on the measured twin releases.
python3 scripts/test-cloud-rystic.py all

# Strict vendor contracts too; unsupported behavior remains a failure.
python3 scripts/test-cloud-rystic.py all --contracts

# Updated Drive: production two-way volume sync and independent import too.
python3 scripts/test-cloud-rystic.py gdrive --contracts --volume

# Isolate the updated Changes contracts from the remaining DOCX failure.
python3 scripts/test-cloud-rystic.py gdrive --changes-only
```

Use `dropbox` or `gdrive` instead of `all` to scope a run. The runner compiles
before starting each metered twin, allocates a unique loopback instance, executes
only the current Cargo-reported test binary locally (including after remote
compilation), and stops/removes its instance even when assertions fail. No key is
stored in the repository. Ordinary Cargo tests do not enable these external-test
features; opting in without a twin fails rather than silently skipping.

For trusted CI, invoke the same runner after provisioning Rystic and its key via
secret storage. Do not expose that key to unchecked fork code. No paid workflow
is automatically enabled by this change. Provider adapter tests can be selected
for relevant changes rather than run on every repository push. Fully enabled
contract runs must remain visibly failing until the vendor behavior is served;
the supported suite is not a substitute for those sync contracts.

## Feedback to send separately

- [Dropbox reproduction notes](rystic-dropbox-feedback.md)
- [Google Drive reproduction notes](rystic-gdrive-feedback.md)

Those files include measured versions and reproducible requests. This document
is the project-facing summary, separate from the provider feedback.
