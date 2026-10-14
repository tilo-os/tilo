# Dropbox and Google Drive import/sync test results

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

This does **not** prove end-to-end FUSE import or live-account OAuth. It does not
prove Dropbox incremental sync, Drive initial/incremental sync, Drive upload
resume, shared drives, or real Google-native export conversion against Rystic.
Existing credential-free provider tests still cover those production paths.
No production sync fallback or simulator-specific upload workaround was added.

## Running the suites

From the repository root, with an installed/authenticated Rystic CLI:

```bash
# Supported scenarios; successful on the measured twin releases.
python3 scripts/test-cloud-rystic.py all

# Strict vendor contracts too; currently exits nonzero with five failures.
python3 scripts/test-cloud-rystic.py all --contracts
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
