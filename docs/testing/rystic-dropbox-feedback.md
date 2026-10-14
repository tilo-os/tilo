# Dropbox adapter checks against Rystic

## Versions and isolation

Measured on 2026-10-14 with authenticated installed Rystic CLI
`0.0.28` (`3e381c4`, Go 1.24.0 linux/amd64), Dropbox twin `0.0.2`,
commit `41c9c6fa969f7c1e8ffb171344f427ff1c0a738d`, model
`sha256:f8be3e88fddfe7699c9c55484dbcbf0946f9e294a9828c7eba70472687d53ebf`.
The CLI uses its existing license/token; do not put that credential in test
configuration. The twin accepts the non-secret bearer `sl.tilo-test`.

```bash
name="tilo-dropbox-$(date +%s)-$$"
rystic run dropbox-twin --name "$name" --port 0 -d -json
# Set the URL to data.url from the output, not a hard-coded port.
export TILO_RYSTIC_DROPBOX_URL=http://localhost:PORT
trap 'rystic stop "$name"; rystic rm "$name"' EXIT
cargo test --locked -p cloud-source-dropbox --features rystic-tests --test rystic
```

Only loopback HTTP URLs are accepted and the test checks the version endpoint's
product before using the production `DropboxSource`. A missing environment
variable, wrong product, dead twin or failed assertion fails the test, never
skips it. Tests use UUID-named folders and delete them on success. Failed tests
can leave folders, so use a dedicated disposable instance. No reset is performed
against someone else's twin. `rystic-tests` is explicit opt-in via a required
Cargo feature; ordinary fake/conformance tests need no credentials or twin.

If Cargo routes test execution to a remote builder, its loopback is not the
host twin and custom environment variables may not be forwarded. Build with
`cargo test --locked -p cloud-source-dropbox --features rystic-tests --test
rystic --no-run`, then execute the returned `target/debug/deps/rystic-<hash>`
test binary on the machine running the twin with `TILO_RYSTIC_DROPBOX_URL` set.
This compiles remotely but runs locally; it does not bypass build routing.

## Production-adapter results

The supported integration suite passed both tests: paginated folder listing and
recursive import (including parent identity), Unicode/supplementary-plane names,
100,003-byte upload with 32 KiB session chunks and persisted-session resume,
verified whole downloads, revision-preconditioned ranged reads, revision listing
and historical reads, restore, share-link reuse, quota, move and conditional
delete. The resume is intentionally smaller than the twin's 1 MiB cap: this
proves session persistence but not a 4 MiB hash-block-boundary resume.

A verified adapter defect allowed foreign Dropbox IDs to list another folder,
read its bytes or revision metadata, start replacement uploads or create share
links outside a configured folder mount, although `stat` already rejected them.
Saved sessions from a wider mount could also be resumed against foreign targets,
and a target moved outside the mount could still be committed. The adapter now
uses scoped folder metadata for listing, checks download response paths before
returning bytes, and checks scope before replacement/share operations and upload resume/commit.
Revision enumeration validates returned paths without dropping support for
revision history of deleted files. A
credential-free fake regression checks full/ranged/historical reads and
conditional/unconditional replacement, as well as permitted in-mount writes.
These are mount-policy checks, not simulator compatibility changes.

Validation completed:

```bash
cargo test --locked -p cloud-source-dropbox --test root_scope --test dropbox --test conformance --lib
cargo check --locked -p cloud-source-dropbox --tests --features rystic-contract-tests
cargo clippy --locked -p cloud-source-dropbox --tests --features rystic-contract-tests -- -D warnings
```

All passed. The supported twin suite passed 2/2 locally after remote compilation;
the separately opted-in replacement contract failed on ID preservation as
recorded below. An initial remote execution correctly failed on the missing
twin URL environment variable; it was rerun locally, not skipped. The dedicated
twin was stopped and its instance record removed after testing.

## Reproducible twin findings

Use a fresh dedicated instance. These probes require only curl and the
non-secret bearer above; paths may be UUID-suffixed to avoid collisions.

```bash
u=$TILO_RYSTIC_DROPBOX_URL
rpc() { curl -sS "$u/2/$1" -H 'Authorization: Bearer sl.tilo-test' \
  -H 'Content-Type: application/json' -d "$2"; }
put() { curl -sS "$u/2/files/upload" -H 'Authorization: Bearer sl.tilo-test' \
  -H 'Content-Type: application/octet-stream' -H "Dropbox-API-Arg: $1" \
  --data-binary "$2"; }
rpc files/create_folder_v2 '{"path":"/feedback"}'
put '{"path":"/feedback/z.txt","mode":"add"}' original
```

### ID-addressed replacement creates a bogus new path

Take `id` and `rev` from the upload above and run:

```bash
put '{"path":"id:RETURNED_ID","mode":{".tag":"update","update":"RETURNED_REV"},"strict_conflict":true}' changed
```

Replace `id:RETURNED_ID` with the entire returned ID (including its `id:` prefix),
without adding a second prefix. Expected: replace the original file, keep its ID and update its
revision. Observed: HTTP 200 with a **new ID**, `path_lower` equal to the literal
old ID, and the original file unchanged. For example, original
`id:0000000000000000000011` became a new entry
`id:0000000000000000000012` at path `id:0000000000000000000011`.
The strict production-adapter regression fails its ID equality assertion
(new ID versus original ID). A nested-folder replacement can instead fail
while resolving the bogus parent metadata (`Status(400)`). This is not a reason to rewrite
ID-based upload commits as paths: Dropbox's public
[`CommitInfo.path` specification](https://github.com/dropbox/dropbox-api-spec/blob/main/files.stone)
is `WritePathOrId`, explicitly permitting `id:.*`.

The strict regression is available separately and is expected to **fail on
this twin version**, without an ignored test or weakened assertion:

```bash
cargo test --locked -p cloud-source-dropbox --features rystic-contract-tests \
  --test rystic id_addressed_replacement_preserves_identity_and_checks_revision
```

### Cursor continuation is not a change feed

```bash
rpc files/list_folder '{"path":"/feedback"}'
# Save the returned cursor, then insert an entry sorting before z.txt.
put '{"path":"/feedback/a.txt","mode":"add"}' new
rpc files/list_folder/continue '{"cursor":"RETURNED_CURSOR"}'
```

Observed: the continuation resends `z.txt`, omits newly inserted `a.txt`, and
reports `has_more:false`. This reproduces the published positional-window gap.
The production-adapter contract
`sync_delivers_new_items_after_initial_enumeration` also fails on this defect.
Run `python3 scripts/test-cloud-rystic.py dropbox --contracts` for both strict
replacement and sync contracts; both fail on the measured twin release.
Do not infer deletion, move, overwrite or post-enumeration sync correctness from
the passing import test. Those change-feed contracts remain tested against the
credential-free fake, not repaired around the simulator.

### File-size cap and notification endpoint

An upload body of 1,048,577 bytes returns HTTP 413
`{"message":"request body exceeds blob_cap_bytes 1048576"}`. The version
endpoint advertises `blob_cap_bytes:1048576`. Dropbox permits much larger
requests and sessions; the adapter's chunk defaults must not be reduced to fit
the twin. Session aggregate caps are also documented by Rystic.

```bash
rpc files/list_folder/longpoll '{"cursor":"RETURNED_CURSOR","timeout":30}'
```

Observed: HTTP 404 with `No route in twin for POST
/2/files/list_folder/longpoll`. There is no twin-backed long-poll proof.
`Dropbox-API-Path-Root`, namespace isolation, strict identical-byte conflicts
and shared-folder collaboration also have published twin gaps and are not
claimed covered by this suite. Refer to `rystic docs dropbox-twin` for the
installed release's limitations.
