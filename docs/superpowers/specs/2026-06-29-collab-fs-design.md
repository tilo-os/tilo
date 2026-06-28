# Collaborative read-write filesystem — design

- **Date:** 2026-06-29
- **Status:** approved (brainstorm), implementation in progress
- **Component:** `fs/` submodule (FUSE3 + iroh)
- **Supersedes:** the skeleton in `fs/` (in-memory `MetadataStore`, stubbed `fetch_range`, unused `--ticket`)

## Goal

Turn `fs/` from a local read-only mock into a real **bidirectional, multi-peer collaborative
filesystem**: peers mount a shared workspace, see each other's structural changes propagate live,
hydrate file content on first read, write back to shared files, and resolve concurrent edits
without silent data loss.

### Success criteria (the demo)

Two `fs` instances on localhost sharing one workspace:

1. **Projection** — host shares a directory; viewer mounts it and the tree appears.
2. **Hydration** — viewer `read()`s a file → the *real bytes* are fetched over the wire, cached,
   and blake3-verified.
3. **Live structural sync** — host creates/renames/deletes a file → it appears on the viewer
   within gossip latency (sub-second on localhost).
4. **Write-back** — viewer creates/edits a file → it appears on the host.
5. **Conflict survival** — both peers edit the *same* file concurrently → one version wins in
   place, the other is preserved as a `*.sync-conflict-*` copy. No bytes are lost.

All five are covered by automated tests (in-process two-node iroh harness + CRDT merge unit
tests) and a scripted live two-instance run.

## Non-goals (this milestone)

- Three-way *content* merge (line-level merge of conflicting file bodies). We do
  last-writer-wins-in-place + conflict-copy, which is what Syncthing/Dropbox do.
- Permissions / per-cluster sandbox integration with the compositor (the compositor already owns
  path-grant policy; the FS just exposes the mount).
- Partial-block / random-write streaming dedup. Writes re-ingest the whole file blob on flush.
  (Fine for documents; large-file chunking is a follow-up.)
- Encryption at rest / authz of peers beyond possession of the ticket.

## Architecture: two planes

The filesystem is split into two planes, each using the right primitive:

### Plane 1 — Namespace (Automerge CRDT)

The directory tree + per-file metadata live in a single Automerge document, replicated to every
peer. Automerge merges concurrent structural edits deterministically (no lost creates/deletes).
`automerge` is already a workspace dependency (used by `ui`); the OS is CRDT-native by design.

**Schema — path-keyed flat map (deliberate):**

```text
root: { entries: Map<String /*path*/, Entry> }
Entry {
    kind: "file" | "dir",
    content_hash: Option<String>,   // blake3 hex; None for dirs and empty files
    size: u64,
    mtime: u64,                     // unix millis (Lamport-tiebroken; see conflict handling)
    author: String,                // peer id that last wrote
    deleted: bool,                  // tombstone (enables delete to merge, not resurrect)
}
```

**Why path-keyed, not id+parent:** an id/parent tree CRDT suffers the classic *concurrent-move
cycle* (peer A moves `/x` into `/y` while peer B moves `/y` into `/x`). A flat path-keyed map
makes that representationally impossible. Rename is modelled as delete-old + insert-new (exactly
the two operations the FUSE `rename` op already gives us). Directories are explicit `Dir` entries
(so empty dirs survive); a path's children are found by prefix scan. Tombstones (`deleted: true`)
rather than key removal so a concurrent delete+edit merges to "deleted" instead of resurrecting.

### Plane 2 — Content (iroh-blobs, content-addressed)

File bytes are blobs addressed by **blake3** hash (iroh-blobs uses blake3 natively). An `Entry`'s
`content_hash` points into the blob store. This makes "projection vs hydration" literal:

- **Projection** = the CRDT (gossiped eagerly, always present).
- **Hydration** = the blob (fetched lazily on first `read()`).

The blob store is an iroh-blobs **`FsStore` rooted at `--cache-dir`** (= `/var/cache/qos-fs` in the
OS), so content survives restarts and the root stays read-only under bootc. A `moka` cache sits in
front of the store for hot in-memory range reads.

## Transport — three layers on one iroh endpoint

1. **iroh-gossip** — a topic derived from the workspace id (carried in the ticket). Local Automerge
   changes are broadcast as they happen → push propagation of the namespace plane. *(New dep; must
   version-match iroh 0.96 / iroh-blobs 0.98 — see Risks.)*
2. **`qos-fs-sync/0` custom ALPN** — bootstrap. Gossip cannot catch up a late joiner, so on join a
   peer performs one Automerge **sync exchange** (standard `generate_sync_message` /
   `receive_sync_message` loop) with the ticket's node to converge, then rides gossip for deltas.
3. **iroh-blobs (existing ALPN)** — content hydration. When the CRDT references a hash not present
   locally, fetch the blob from a peer (the entry's `author` node, or any connected peer), verify
   blake3, cache, serve.

## Read / hydration path

```text
read(ino, off, size)
  → resolve ino → path → Entry.content_hash   (from CRDT)
  → moka.get(hash) ─hit→ slice & return
                   └miss→ FsStore.get(hash) ─present→ cache & return
                                            └absent → iroh-blobs fetch from peer
                                                       → blake3 verify → FsStore.put → cache → return
```

## Write path (bidirectional)

FUSE write-family ops mutate local state, then publish:

- **content** (`write`/`flush`/`release`): buffer to a per-handle staging file; on `release`/`flush`
  ingest the staged bytes as a blob (blake3), set `Entry.content_hash`/`size`/`mtime`/`author` in
  the CRDT, broadcast the change over gossip. Peers hydrate the new blob lazily.
- **structural** (`create`/`mkdir`/`unlink`/`rmdir`/`rename`/`setattr`): mutate the CRDT entry
  (insert / tombstone / delete-old+insert-new) and broadcast.

## Conflict handling

- **Structural conflicts** (concurrent create/delete/rename across peers): resolved by Automerge's
  merge — deterministic, no data loss. Delete+edit → tombstone wins (no resurrection).
- **Content conflicts** (two peers set a different `content_hash` on the same path concurrently):
  Automerge surfaces the conflicting register values. Resolution:
  1. Deterministic winner = highest `(mtime, author)` Lamport tuple — same on every peer.
  2. The loser's blob is **not discarded**: a new entry `…/name.sync-conflict-<peer>-<lamport>` is
     created pointing at the loser's `content_hash`. (Syncthing semantics.)
  3. Both files are now visible; the user reconciles. Tested explicitly.

## Modes, CLI, and the OS service

```text
fs serve <dir>     --cache-dir <p>            # seed CRDT+blobs from <dir>, print ticket
fs mount <point>   --ticket <T> --cache-dir <p>   # join workspace, project+hydrate+writeback
```

- `--cache-dir` gets a **real job** (FsStore + moka root). This makes the `fs.service` crash-loop
  fix meaningful rather than a no-op arg-accept: the service line
  `fs --mount-point /var/workspace --cache-dir /var/cache/qos-fs` is migrated to the `mount`
  subcommand and the arg is honored. (Service unit updated in `os/`.)
- Startup `unwrap`/`expect` on mount, lock acquisition, and endpoint init are hardened to log and
  exit cleanly (or retry) rather than panic → `Restart=always` no longer loops on a transient.

## Module structure (`fs/src/`)

| file | role | change |
|------|------|--------|
| `main.rs` | CLI (clap subcommands), endpoint+router+gossip wiring, mode dispatch | rewrite |
| `fs.rs` | FUSE3 `Filesystem` impl — read **and** write ops | extend |
| `namespace.rs` | Automerge-backed tree: project/insert/tombstone/rename, ino↔path map | **new** (replaces `metadata.rs`) |
| `sync.rs` | gossip broadcast + `qos-fs-sync/0` bootstrap ALPN, change apply loop | **new** |
| `hydration.rs` | real iroh-blobs fetch + moka + blake3 verify; blob ingest | rewrite |
| `store.rs` | FsStore + moka wiring rooted at `--cache-dir` | **new** |
| `conflict.rs` | content-conflict detection + conflict-copy creation | **new** |
| `smart.rs` | prefetch heuristics | keep stub, wire `on_open`→hydrate hint |

`metadata.rs` is absorbed into `namespace.rs` (its `FileNode`/inode-allocation logic moves there,
now backed by the CRDT instead of a `HashMap`).

## Testing strategy (verification covenant applies — assert on the real path)

- **Namespace unit** (`namespace.rs`): project a manifest → correct inode tree; insert/tombstone;
  rename = delete+insert; prefix-scan children; tombstone beats resurrect on merge.
- **CRDT merge** (`conflict.rs` + `namespace.rs`): two docs, concurrent structural edits → converge
  identically; concurrent content edits → deterministic winner + one conflict-copy entry; delete+edit
  → deleted.
- **Two-node iroh harness** (`sync.rs` + `hydration.rs`): two in-process endpoints; node A serves,
  node B bootstraps via `qos-fs-sync/0`, receives a gossiped change, and hydrates a real blob whose
  bytes + blake3 match. (No GPU/display; pure tokio.)
- **FUSE ops** (`fs.rs`): extend the existing mount integration test with create/write/read-back,
  mkdir/rmdir, unlink, rename.
- **Service regression**: clap accepts `mount … --cache-dir …` and `serve … --cache-dir …` (the exact
  crash-loop trigger) — asserted against the real `Args` parse.
- **Live demo script**: two `fs` processes on localhost; assert the five success criteria; tears down.

## Phased implementation plan

Each phase lands with its tests green before the next starts.

1. **Namespace CRDT** — `namespace.rs` replacing `metadata.rs`; FUSE read ops served from the CRDT;
   projection from a seeded dir. (No network yet.)
2. **Gossip + bootstrap sync** — `sync.rs`; iroh-gossip topic + `qos-fs-sync/0`; structural changes
   propagate between two in-process nodes.
3. **Real hydration** — `store.rs` + rewritten `hydration.rs`; iroh-blobs fetch + moka + blake3;
   `read()` pulls real remote bytes.
4. **Write ops** — `fs.rs` write-family → CRDT mutation + blob ingest + gossip; write-back works.
5. **Conflict handling** — `conflict.rs`; content-conflict winner + conflict-copy.
6. **Service robustness** — clap subcommands, `--cache-dir` honored, startup hardening, `os/` unit
   updated; crash-loop fixed.

Then: parallel subagent verification pass (independent review of merge correctness, hydration
correctness, and the conflict semantics) before declaring done.

## Risks & fallbacks

- **iroh-gossip version skew** (chosen transport): must pin to the release matching iroh 0.96 /
  iroh-blobs 0.98. *Fallback (same architecture, different pipe):* broadcast Automerge changes over a
  second custom ALPN (`qos-fs-gossip/0`) using direct connections to gossip-discovered/ticketed peers
  — no extra crate. The CRDT + blob design is unchanged either way.
- **Automerge doc growth**: long-lived docs accumulate history. Acceptable this milestone; periodic
  `save()`+compaction is a follow-up.
- **Whole-file re-ingest on write**: fine for documents; large files want chunked/range dedup later.
- **FsStore API surface** across iroh-blobs 0.98: if `FsStore` is awkward, `MemStore` + manual
  persistence is the interim fallback (loses cross-restart persistence only).

## Implementation status (2026-06-29)

All six phases landed in `fs/` (commits `49c8258` → `63ab5f0`); `iroh-gossip` pinned to `0.96.0`
(matches iroh 0.96 — no version-skew fallback needed). Verified by 29 automated tests (namespace
merge/tombstone/rename + inode-identity, content store, two-node networked hydration, two-node
bidirectional gossip propagation, end-to-end "write on A → hydrate on B", FUSE RW round-trip,
rmdir/unlink semantics, conflict-copy, service-command-line regression) **and** a scripted live
two-process run proving projection, hydration, write-back, and host-edit propagation.

An adversarial three-agent review drove a follow-up data-integrity + robustness pass (`63ab5f0`):
no error-swallowing on the write path, `rmdir`/`unlink` kind+emptiness checks, single-lock writes,
buffer-aware `stat`, rename inode-alias cleanup, a periodic **anti-entropy `reconcile_loop`** (pulls
+ merges peers' full docs to recover from dropped gossip or a mid-join window), a pump that survives
transient gossip errors, and timeouts on the bootstrap exchange.

## Known limitations (deferred, documented from review)

- **Delete-dir vs create-child race:** if one peer removes a directory while another concurrently
  creates a child inside it, the child can be orphaned (live entry under a tombstoned parent). This
  is inherent to the path-keyed flat-map model; the proper fix is a move-aware tree CRDT (Kleppmann)
  — a future schema change, out of scope for this milestone.
- **Modify/delete race:** the no-loss guarantee covers concurrent *edit/edit* (→ conflict copy). If a
  *delete* wins over a concurrent *content edit* of the same file, the editor's bytes are not yet
  surfaced as a conflict copy. Deferred.
- **Whole-file re-ingest on write** (no chunked/range dedup) — fine for documents, suboptimal for
  large files.
- **Anti-entropy is a periodic full-doc pull** (3 s) — fine at metadata scale; Automerge history
  growth / doc compaction is a later optimization.
