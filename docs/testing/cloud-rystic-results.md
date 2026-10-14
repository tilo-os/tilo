# Dropbox and Google Drive import/sync test results

## Failure counts

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
cases. Shared-drive omissions, Google export placeholders and other published
limitations are **not counted as newly reproduced failures**.

## Passing coverage and limits

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

# Strict vendor contracts too; currently exits nonzero with four failures.
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
