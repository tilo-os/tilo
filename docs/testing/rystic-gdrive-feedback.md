# Google Drive adapter against Rystic

## Versions and scope

Observed on 2026-10-14, Linux amd64:

- Rystic CLI `0.0.28`, commit `3e381c4`, built `2026-10-12T14:34:05Z`, Go `1.24.0`.
- `google-drive-twin 0.0.3`, commit `5b4a15bd9931181da6c77f2a668c16c41b004475`, contract `24`, metered edition.
- Model `sha256:d8f46d5b8036d37ac3fe481297e7204843fb765814a1ef1d9a0e18070e6f863f`.

The installed CLI uses `rystic run google-drive-twin`, not `rystic google-drive-twin`.
The CLI authenticates using its existing installation credentials; no license or
API key is copied into the repository. Requests to the simulator use the dummy
bearer token `tilo-rystic-gdrive`, not a Google credential.

## Reproduction

Use a dedicated instance: fixture seeding changes the twin's quota configuration.
The test refuses non-loopback HTTP origins and checks `/_rystic/version` before
sending fixture requests. It neither starts a twin nor contacts Google.

```bash
name="tilo-gdrive-$(date +%s)-$$"
rystic run google-drive-twin --name "$name" --port 0 -d -json
# Set this to data.url from the startup JSON (no trailing slash):
export TILO_RYSTIC_GDRIVE_URL=http://localhost:<allocated-port>
trap 'rystic stop "$name"; rystic rm "$name"' EXIT
cargo test --locked -p cloud-source-gdrive --features rystic-tests \
  --test gdrive-rystic -- --nocapture
```

When `cargo` routes tests to a remote builder, that builder's loopback is **not**
the host's twin. Compile remotely and execute the fetched binary locally instead:

```bash
cargo test --locked -p cloud-source-gdrive --features rystic-tests \
  --test gdrive-rystic --no-run
find target/debug/deps -maxdepth 1 -type f -name 'gdrive_rystic-*' -executable \
  -exec env TILO_RYSTIC_GDRIVE_URL="$TILO_RYSTIC_GDRIVE_URL" {} --nocapture \;
```

Ensure this finds exactly one current binary; zero binaries is not a test run.
On this checkout the fetched executable was
`target/debug/deps/gdrive_rystic-326d2b6999924b23`; execution against port `44819`
passed: **1 test, 0 ignored**. The uniquely named instance was stopped and removed
following verification. An invocation without `TILO_RYSTIC_GDRIVE_URL` fails with
an explicit configuration error, never skips. Ordinary tests do not enable the
`rystic-tests` feature and remain credential-free.

## What the production adapter proved

`crates/system/cloud-source-gdrive/tests/rystic.rs` calls `GDrive`/`CloudSource`,
not the in-memory fake. The only raw HTTP requests identify the twin and seed
fixture data. A unique folder and 1,001 binary files exercise:

- Root alias normalization in returned parents; folder creation.
- Multi-page listings preserving same-named items and metadata.
- Complete, ranged, EOF-clipped and revision-pinned reads with exact bytes.
- Revision enumeration, moving/renaming into another folder, and permanent deletion.
- Native document creation/classification and refusal of ordinary media reads.
- Quota parsing with explicitly seeded string-valued limit and usage.

No production adapter defect was verified in these operations; no adapter behavior
was changed merely to make the simulator pass. Initial quota probing returned
`{"storageQuota":{"usage":"0","usageInDrive":"0","usageInDriveTrash":"0"}}`:
a missing limit is valid unlimited storage, not an adapter bug. The test therefore
seeds a finite quota and checks the exact parsed values.

## Simulator gaps, not adapter workarounds

These can be reproduced with `$TILO_RYSTIC_GDRIVE_URL` from above:

```bash
curl -i -H 'Authorization: Bearer tilo-rystic-gdrive' \
  "$TILO_RYSTIC_GDRIVE_URL/drive/v3/changes/startPageToken"
curl -i -H 'Authorization: Bearer tilo-rystic-gdrive' \
  -H 'Content-Type: application/json' -d '{"name":"probe"}' \
  "$TILO_RYSTIC_GDRIVE_URL/upload/drive/v3/files?uploadType=resumable"
```

- **Changes API absent:** startPageToken returns HTTP 404 with a Google-style
  `notFound` error about a nonexistent file. Production `source.changes(None)`
  consequently returns `Error::NotFound`; it cannot obtain the pre-enumeration
  token needed for a race-safe initial sync. The separately enabled strict sync contract fails at this error. There is no fallback to listing-only sync, no empty successful
  snapshot, and no fabricated cursor.
- **Resumable uploads absent:** initialization returns HTTP 501 with
  `__rystic_unimplemented__: true` and an interaction-vector diagnostic.
  Production `begin_upload` returns `Error::Status(501)`; the separately
  enabled strict upload contract fails at this error. Fixture content is seeded, not uploaded by an alternative
  implementation. Resume/commit/concurrent-write behavior remains covered by
  the fake-backed tests, not claimed as Rystic-verified.
- **Shared drives absent:** the twin's declared surface excludes `drives.*` and
  shared-drive state. Shared-drive behavior remains covered by fake tests.
- **Exports are placeholders:** Rystic documents that these are generated from
  name/description rather than rendered content. This test does not assert
  Office/PDF validity or count an export placeholder as successful importing.
- **No rate-limit modeling:** backoff correctness remains a fake-backed test.

Strict vendor contracts assert successful sync and upload, not successful
reproduction of errors. Run `python3 scripts/test-cloud-rystic.py gdrive
--contracts` to reproduce both failing contracts. The supported import test
passes separately. These tests should become green when the missing endpoints
are implemented, without weakening assertions or changing production behavior.

## Credential-free verification

```bash
cargo test --locked -p cloud-source-gdrive --lib --test drive --test conformance
cargo check --locked -p cloud-source-gdrive --tests --features rystic-tests
cargo clippy --locked -p cloud-source-gdrive --test gdrive-rystic \
  --features rystic-tests -- -D warnings
```

Results: **102 tests passed, 0 skipped**; all-test compile check and opt-in test lint check passed.
The existing tests continue to exercise the genuine changes-feed implementation,
shared drives, resumable uploads, native exports, conflicts and credential rejection.
