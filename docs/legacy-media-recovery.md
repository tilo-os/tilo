# Import selected legacy profile media

An offload input must be readable in the invoking room. A resident blob without
that room's reference is **not** an input, even when its hash is known. The
missing-content diagnostic does not distinguish a missing reference from absent
bytes; it never probes another room's inventory. Fetch content through its
record's normal route first.

For old content retained in a standalone runtime profile, the local owner can
explicitly create new room references with the registered workspace action
`workspace.legacy-media#import-legacy-media`. This is an import, not evidence that
the room historically owned the bytes. No scan, guessed ownership, automatic
retagging, or deletion of old roots takes place.

## Standalone owner command

Stop the standalone runtime daemon first. Run as the account that owns its
private profile, outside an app sandbox. Same-account unconfined administrator
scripts are trusted owner callers; no human-versus-script distinction is made.
The command takes the profile's existing exclusive lock, starts its normal
services, invokes the canonical registered action host, prints the final receipt,
and exits. It cannot run alongside a daemon and introduces no additional IPC
server or recovery-only socket.

Choose exact known hashes, the profile's own room UUID, and a total byte ceiling:

```sh
export PROFILE="$HOME/.local/state/tilo/runtime"
export ROOM='the-profile-room-uuid'
export HASH='one-exact-lowercase-blake3-hash'
request=$(python3 - <<'PY'
import json, os, uuid
room = os.environ['ROOM']
print(json.dumps({
    'invocation': str(uuid.uuid4()),
    'target': {
        'provider': 'workspace:' + room,
        'address': {
            'resource': 'workspace.legacy-media',
            'action': 'import-legacy-media',
        },
    },
    'argument': {'Json': {
        'source_store': 'runtime-profile-content',
        'source_room': 'unscoped',
        'destination_room': room,
        'hashes': [os.environ['HASH']],
        'max_bytes': 16 * 1024 * 1024,
    }},
    'confirmation': 'Confirmed',
}))
PY
)
tilo-runtime --profile "$PROFILE" --owner-action "$request"
```

The fixed source is this profile's `content/` store, not a caller-supplied path or
another profile. The unscoped selection covers resident raw blobs without
requiring newly invented historical room tags. Before room-scoped offload
binding, raw blob binding used store residency; current binding requires the
room's media reference. The importer preserves that boundary rather than
restoring residency-based access to ordinary callers.

The action accepts 1–128 unique hashes and at most 256 MiB in total. It requires
explicit confirmation, the current destination owner and current enrolled,
unrevoked device identity. Apps, scoped agents, bots, Shortcuts, remote-device
callers and on-behalf-of requests cannot import. Authority and any originating
execution lease are checked again during publication. Processing is asynchronous
and bounded to 120 seconds. A successful receipt reports `imported_bytes` only
after the destination writes and completion marker are durable.

Every publication attempt durably spends its original invocation UUID. A crash,
revocation, timeout or I/O failure can leave a partial import, never a success
receipt. Reusing the UUID after restart refuses rather than repeating publication.
Check the selected content through the ordinary scoped media API; any further
attempt requires a fresh explicit owner invocation. Source bytes and roots are
never removed.

## Limits

This increment supports the standalone runtime's own writable room and its own
profile store. It does **not** recover the desktop filesystem daemon's store,
an adopted read-only desktop workspace, another profile, remote-only content,
file-manifest hashes, or content no longer retained on disk. The desktop's
existing authenticated internal protocol would need an owner-custody-preserving
extension before that host can offer the same action. Ordinary scoped reads and
offload retain their existing isolation and local fallback behavior.
