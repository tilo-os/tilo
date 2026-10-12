# Storage, sync and records

Status: design for review. Nothing here is implemented unless it says so. Where
this document describes today, it cites `file:line` in the tree as of this
commit's parent. Where evidence is missing, it says so and lists the question
in [§22](#22-open-questions-for-the-owner) instead of guessing.

This document replaces the storage parts of several documents that grew up
separately: the collaborative file daemon (`crates/system/fs`), app state
(`crates/system/app-state`, `crates/sdk/ui-core/src/{persist,record}.rs`),
document blobs (`crates/doc/tilo-doc/src/blobstore.rs`) and the package store
(`crates/packaging/app-host`). They become one system with one content store,
one record model, one history and one permission model.

Contents

1. [Requirements](#1-requirements)
2. [Where we are](#2-where-we-are)
3. [Goals and non-goals](#3-goals-and-non-goals)
4. [Concepts](#4-concepts)
5. [Architecture at a glance](#5-architecture-at-a-glance)
6. [Local data plane](#6-local-data-plane)
7. [Content format](#7-content-format)
8. [Metadata plane](#8-metadata-plane)
9. [Transfer engine](#9-transfer-engine)
10. [Visibility across files](#10-visibility-across-files)
11. [Conflicts and leases](#11-conflicts-and-leases)
12. [Caching, residency and prefetch](#12-caching-residency-and-prefetch)
13. [The Record Store API](#13-the-record-store-api)
14. [Permissions](#14-permissions)
15. [History](#15-history)
16. [Installed apps](#16-installed-apps)
17. [The git test](#17-the-git-test)
18. [Keep, merge, delete](#18-keep-merge-delete)
19. [Migration stages](#19-migration-stages)
20. [Risks](#20-risks)
21. [Alternatives considered](#21-alternatives-considered)
22. [Open questions for the owner](#22-open-questions-for-the-owner)

---

## 1. Requirements

These are the owner's requirements, quoted. Every decision below names the
requirement it serves as `[R1]`…`[R9]`.

| | Requirement |
|---|---|
| **R1** | "The collaborative/syncing FS works with ALL sizes and kinds of files; normal POSIX reads/writes keep working while background work happens (predictive caching, prefetch, sync)." |
| **R2** | "Literally native FS read/write performance for files already on disk, for all file sizes." |
| **R3** | "Sync basically saturates most network connections, including for many small files." |
| **R4** | "The caching/sync design needs to be absolutely amazing." |
| **R5** | "The easy path for app developers: all storage/resources collaborative, all versioned with good history, great DX — and very little reason to leave that path regardless of size/kind of files/resources." |
| **R6** | "One implementation per system (one blob/content store for files, document media, app data and installed apps; one record model; one history; one permission model), done properly." |
| **R7** | "Installed apps use the same blob storage as everything else, and `.tapp` package files must stay very small." |
| **R8** | "App data must be fully deduplicated (at least per workspace, ideally beyond), isolated, collaborative with smart syncing and robust permissions." |
| **R9** | Success indicator, not to be special-cased: "git repositories sync between devices with zero thought". The general design must provide POSIX fidelity (exec bit, symlinks, ownership, `O_EXCL` lock files, atomic rename, honest fsync, mmap), native performance with very many small files, cross-file causal visibility (a ref is never visible before its objects), correct single-writer semantics across devices (leases or versioned conflicts for index, packfiles, SQLite), and throughput dominated by small files. There are no git-specific code paths. |

Five rules follow directly from these, and the rest of the document applies
them.

1. **The daemon is never on the hot path of a resident file.** Reads, writes,
   `stat`, `open` and mmap of bytes already on disk go to a native filesystem
   and never cross into userspace. [R2, R9]
2. **Real files on a native filesystem are the local source of truth.** The
   content store indexes them. It does not hold a second copy. [R2, R6]
3. **Size is never a mode switch.** One API and one format cover a 40-byte
   ref, 100k source files, a 10 GiB VM image and a million-item collection. The
   store picks chunking, sharding and residency; the app and the user do not.
   [R1, R5]
4. **Everything that crosses the network is batched, pipelined and streamed.**
   No request is per file, per chunk or per record. [R3]
5. **Possessing a hash never grants access.** Access comes from a readable
   record that references the content. [R6, R8]

---

## 2. Where we are

The pieces are strong. What is missing is one model and the performance path.

### 2.1 What is worth keeping

- **Stable record identity and signed per-record replication.**
  `FileRecordId`, signed `RecordEnvelopeV1` operations
  (`crates/system/fs/src/record_replication.rs:61-81`), certified device
  authorship, and a signed placement binding `FileBindingV1 {record_id, parent,
  name, kind, tombstoned}` (`crates/system/fs-protocol/src/files.rs:209-220`).
- **Share and authority machinery.** Named and link shares, one-use
  redemption, revocation, exact-version replica receipts, fail-closed eviction
  and multi-record transactions (`docs/collaborative-files-product.md:7-96`).
  403 tests cover it (`docs/collaborative-files-product.md:190-197`).
- **Actions and undo.** Typed `ActionSpec`s behind one door, `ActionContext`
  written into Automerge change messages (`docs/history-experience.md:101-121`),
  and author-scoped undo that survives concurrent remote edits
  (`crates/sdk/ui-core/src/state/mod.rs:161-178`).
- **The rooms concept.** Records with a home room, rooms hold references, and
  content is separate from person/device settings (`docs/architecture/rooms.md`).
- **The substrate.** iroh 1.0 with iroh-blobs 0.103 already provides BAO range
  requests, `HashSeq`, `GetManyRequest`, a `Downloader` with a connection pool
  and `SplitStrategy`, and a store interface that can have a custom backend
  (`iroh-blobs-0.103.0/src/protocol.rs:567-580`,
  `src/api/downloader.rs:36,317`, `src/store/readonly_mem.rs:1-7`). fuser 0.17
  supports FUSE passthrough (`fuser-0.17.0/src/ll/request.rs:1019-1020`).
  Tilo uses almost none of this.

### 2.2 What fails the requirements

| Area | Today | Requirement it fails |
|---|---|---|
| Data path | Every read and write goes through plain FUSE on **one** worker thread: `n_threads` is unset, and `init` sets only readahead (`crates/system/fs/src/main.rs:3045-3067`, `fs.rs:4373-4376`). `open` replies with `FopenFlags::empty()`: no keep-cache, no passthrough (`fs.rs:4530`). No real file exists; bytes are ~8 KiB chunks inside redb. | R2, R9 |
| Local read | `Hydrator::read` decodes the whole manifest and scans it from chunk 0 on every call. Measured in a release build on tmpfs: 216 MB/s at 64 MiB, **25 MB/s at 1 GiB**. | R2 |
| Local write | The first write loads the whole file into RAM. Every close, flush and fsync re-chunks and re-hashes the whole file, then rewrites the whole-workspace history JSON. Measured: 64 MB/s at 1 GiB, **10.4 s to re-commit a 1-byte edit** to a 1 GiB file. | R1, R2 |
| Silent loss | `flush` and `fsync` reply OK when the commit failed; the failure is only an `eprintln!` (`fs.rs:479`). | R9 (honest fsync) |
| Metadata | One Automerge document for the whole workspace. `lookup`, `readdir` and create scan every node. Measured: **12.3 ms per create at 1,000 files**; a single lookup then takes 17 ms. | R1, R2, R9 |
| Caps | 16,384 records per workspace, 1,024 versions and 4,096 operations per file, 8 MiB per record file (`record_repository.rs:24-31`). SQLite, which fsyncs per transaction, hits the version cap and then loses writes silently. | R1, R9 |
| POSIX | No symlinks, hardlinks, `statfs`, `fallocate` or locks. `setattr` ignores mode, owner and times. Everything is `0644`/`0755` and owned by root, so git refuses the repo as "dubious ownership". | R9 |
| Content sync | A new QUIC connection and a whole-blob GET per 8 KiB chunk, sequential; push is disabled (`sync.rs:862`). About two RTTs per chunk, regardless of bandwidth: ~4 MB/s on LAN, ~50 KB/s over an 80 ms relay. | R3 |
| Serving | Authorising one chunk scans every readable manifest while holding the namespace lock, in one serial task (`sync.rs:737-840`). | R3 |
| App data | 44 of 54 apps keep all state in one Automerge document per `(package, room)`. Snapshots and deltas are capped at 3 MiB (`crates/collab/collab-types/src/op.rs:30-35`), and an oversized delta is dropped after the frontier has advanced (`crates/sdk/ui-core/src/collaboration.rs:3653-3661`). Room discovery is capped at 64 records (`ui-core/src/record.rs:38`). | R5, R8 |
| Blobs | Two stores. qdoc's `BlobStore` is process-local, capped at 32 MiB per blob / 256 MiB / 8,192 entries (`crates/doc/tilo-doc/src/blobstore.rs:33-40`), and `BlobTransport` has no production implementation (`blobstore.rs:650-664`). The fs daemon has the other store. | R5, R6, R8 |
| Packages | A third store. `app-host` ingests every blob into its own `FsBlobStore` and eagerly unpacks a second copy (`crates/packaging/app-host/src/install.rs:1-20`, `store.rs:1-12`). `app-fs` (lazy FUSE) has no dependents. | R6, R7 |
| History | Automerge change graphs for app state, and signed versions plus a global JSON file for files. | R6 |

The daemon is correctness- and security-first but performance-naive. The fix
keeps the record and authority plane and replaces the data path, content
format, metadata persistence and transfer engine underneath it.

---

## 3. Goals and non-goals

### Goals

- Native local I/O for resident data, at every size. [R2]
- One content store per device for workspace files, document media, app data,
  CRDT history segments and installed packages. [R6, R7, R8]
- One record model (CRDT document, sharded collection, byte/file) with one
  identity, ACL, history and replication plane. [R5, R6]
- Sync that is bounded by the link, not by round trips, for both one big file
  and 100k small ones. [R3]
- Lazy hydration that never blocks unrelated I/O, and eviction that never loses
  the only copy. [R1, R4]
- Crash-consistent, causally ordered cross-file visibility on peers. [R9]
- An app API where the collaborative, versioned path is also the shortest
  path. [R5]

### Non-goals

- **Per-identity filtered views of one local tree.** A device materialises only
  the records its person may read. Different local Unix users get different
  stores. We do not filter one tree per process (see [§14](#14-permissions)).
- **Git-, SQLite- or tool-specific sync code.** Tools get general mechanisms:
  crash-consistent epochs, ordered application, leases and versioned
  conflicts. [R9]
- **Automatic semantic merging of arbitrary binary formats.** Binary conflicts
  are versioned and kept, never silently lost, and never auto-merged.
- **A kernel module.** It is out of tree, needs Secure Boot signing and adds
  attack surface ([§21](#21-alternatives-considered)).
- **Un-sharing bytes.** Revocation stops future flow. It cannot erase what a
  peer already has (`docs/architecture/rooms.md`, "Rooms hold references").
- **Replacing Automerge now.** The CRDT engine is hidden behind a typed
  transaction API so that it can change later ([§21](#21-alternatives-considered)).

---

## 4. Concepts

| Concept | Definition | Change from today |
|---|---|---|
| **Person, device** | A certified human root and its certified device keys. The device key is the iroh `EndpointId` (`docs/architecture/collaboration.md`, "Identity"). | Unchanged. |
| **Workspace = room** | A workspace's durable collaboration identity is its room, 1:1. It has members, roles, a host, a **catalog** of record references and a **file tree root**. The personal room is a real `RoomId` with one member. | Removes the `"personal"` slug / nil UUID split (`launcher/src/appdir.rs:37`, `ui-core/src/record.rs:692-703`). |
| **Record** | The unit of identity, sharing, ACL, history and sync. It has a minted `RecordId` (never a path), a kind, a home room, a body and references. | One id type. `ui::RecordId::for_file(path)` and `FileRecordId` merge. |
| **Record kinds** | **CRDT document**: Automerge body (a note, a spreadsheet, a room catalog). **Collection**: a record whose items are sharded into shard documents with an index (a photo library, chat channel, playlist set). **File**: a byte body plus POSIX metadata and a placement in a tree (files, directories, symlinks). A file may also carry a CRDT side-body when a format provider understands it ([§13.5](#135-files-and-format-aware-files)). | Collections are new. CRDT records move from app sandboxes into the record plane. |
| **Content store** | One per device. It holds content-addressed bytes (BLAKE3) with one index, one GC and one authorisation rule. Physically, bytes live either as ranges of native files and their snapshots or as loose blobs. | Replaces three stores ([§7](#7-content-format)). |
| **Content id** | For a body ≤ 64 KiB, the BLAKE3 hash of the bytes. For larger bodies, the hash of a file manifest `HashSeq` ([§7.2](#72-manifests)). | Replaces postcard manifests and the separate whole-file hash. |
| **Changeset** | A set of record operations from one device, cut from one crash-consistent capture epoch, signed once and applied atomically and in order by peers. | New. Multi-record transactions exist today only for replacement rename (`record_transaction.rs`). |
| **Version** | An immutable node in a record's history DAG: a changeset entry for files, Automerge heads for CRDT records. | One history source ([§15](#15-history)). |
| **Residency** | Per file: `Placeholder`, `Partial`, `Resident`, `Pinned`, `Dirty`, `LocalOnly`. | Replaces explicit-only eviction ([§12](#12-caching-residency-and-prefetch)). |
| **Lease** | A time-bounded, fenced single-writer grant on one record, issued by a lease authority. | New ([§11.2](#112-leases)). |
| **Settings** | Person-, device- or person-in-room-scoped typed values (`Settings<T>`). They follow the person or the device, never the room's members. | Formalises the split in `rooms.md`, "Not all app state is a record". |
| **Person room, person records** | The person's own room: its id is derived from their root key, its members are by that derivation exactly the devices the root certified, and it has no roster to invite anyone into. A person record is an app's record in it — data of the person's own (watch history, learned words) that follows them across their devices and never reaches a collaborator ([§13.9](#139-person-records)). | New. Replaces app state squeezed into personal settings sections. |

**Content versus settings.** Content records are room-scoped, empty in a new
room, shared with room members and versioned in full. Settings are
person/device-scoped, synced only between a person's own devices (or not at
all, for device scope), versioned lightly and never visible to collaborators.
Ebook font scale, weather's selected city and chat drafts are settings, not
content (the current placements are the defects listed in
`docs/architecture/rooms.md`).

---

## 5. Architecture at a glance

```
   SDK apps                    foreign apps, shell tools, git, compilers
   ctx.collection / open_file   plain POSIX (open/read/write/mmap/rename)
        │  capability-scoped Cap'n Proto          │
        │  (fds passed back for bytes)            │ native VFS, no daemon hop
        ▼                                         ▼
 ┌──────────────────────── storage daemon (evolves `fs`) ──────────────────────┐
 │  Record Store          Tree projector           Change capture              │
 │  CRDT records,         materialises file        fanotify journal (Tier 1),  │
 │  collections, query    records as real files,   FUSE callbacks (Tier 2/3),  │
 │  indexes, Settings     placeholders, residency  epochs, incremental rechunk │
 │ ─────────────────────────────────────────────────────────────────────────── │
 │  Authority: ACLs, capabilities, shares, leases, receipts (existing plane)   │
 │  History: one HistorySource, retention, GC roots                            │
 │  Content store: one hash space; ChunkStore over native files + loose blobs  │
 │  Metadata DB (redb): envelopes, indexes, journal, residency, reverse index  │
 │ ─────────────────────────────────────────────────────────────────────────── │
 │  Transfer engine: 1 authenticated QUIC connection per peer, set             │
 │  reconciliation for metadata, pipelined HashSeq/GetMany for content,        │
 │  multi-peer splitting, push to always-on peers, priority classes            │
 └──────────────────────────────────────┬──────────────────────────────────────┘
                                        │ iroh (direct, hole-punched, relayed)
                      other devices, home host / hosted server (tilo-relay)

 On disk (Tier 1): a dedicated Btrfs filesystem, the "store volume"
   /var/lib/tilo/store/
     live/<room-id>/…        real files: the POSIX tree per room (subvolume)
     live/apps/<room>/<pkg>/  foreign apps' private data dirs (subvolume)
     pkgs/<package-id>/…     materialised package payloads (read-only)
     .epochs/<n>/            read-only capture snapshots (short-lived)
     .versions/…             reflinked retained versions (history)
     .blobs/                 loose blobs: CRDT segments, remote-only chunks, thumbnails
     meta.redb               metadata DB
   /var/workspace → view of live/ (bind mounts), unchanged path for users and apps
```

**Only one daemon, and it is the existing one.** The `fs` binary and service
keep their names. The crate is split internally into modules along the lines
above ([§18](#18-keep-merge-delete)). The shell's collaboration broker keeps
presence, cursors and live surface streams. Durable app state leaves the
broker for the record plane.

---

## 6. Local data plane

### 6.1 Kernel tiers

One engine runs on three kernel tiers. Each tier is chosen at startup by
probing features, not by version string. Tilo builds its own 7.2 kernel
(`os/kernel/`, `docs/architecture/platform-support.md:72`), so Tilo OS targets
Tier 1. Tier 2
exists for Tilo Desktop on other distributions, and as the first shippable
stage of the migration.

| | **Tier 1: native + fanotify** | **Tier 2: FUSE passthrough** | **Tier 3: tuned FUSE** |
|---|---|---|---|
| Kernel | ≥ 6.14: pre-content events (`FAN_PRE_ACCESS`), `FAN_REPORT_DFID_NAME`, `FAN_RENAME` (5.17), `FAN_MARK_EVICTABLE` (5.19) | 6.9–6.13 (or Tier 1 missing prerequisites) | older |
| What `/var/workspace` is | bind of the real tree on the store volume | FUSE mount; hydrated files are backed by real files and opened with `FOPEN_PASSTHROUGH` | FUSE mount, multi-threaded, writeback cache, `KEEP_CACHE`, readdirplus |
| read/write/mmap of resident file | **native** | **native** (the kernel forwards to the backing file) | FUSE round trip, degraded |
| stat/open/readdir | **native** | FUSE round trip; long TTLs, readdirplus and explicit invalidation make repeats cheap | FUSE round trip |
| Placeholders | sparse file of correct size; the daemon fills ranges on `FAN_PRE_ACCESS` | `open` hydrates the needed ranges before handing out the backing fd | read hydrates |
| Change capture | fanotify filesystem mark → durable journal; Btrfs epoch snapshots | FUSE `create/rename/unlink/setattr/flush/fsync/release` → the same journal; epoch snapshots when the backing store is Btrfs | same as Tier 2 |
| Label | normal | normal ("metadata over FUSE") | shown as degraded in Settings |

**Decision:** Tier 1 is the design on Tilo OS, which ships kernel ≥ 6.14 and
a dedicated Btrfs store volume. Tiers 2 and 3 exist only for Tilo Desktop on
foreign distributions. The kernel checks in
[§6.8](#68-kernel-facts-to-verify-before-committing-to-tier-1) were run on 7.2
and hold (results there). The core of the Tier 1 data plane — probes, journal,
capture epochs, content format and placeholder hydration — is implemented in
`crates/system/fs-native`; `tilo-fs-native probe <store>` selects the tier on a
given machine. The storage daemon serves it (`fs serve|mount --data-plane
auto|fuse|native --store /var/lib/tilo/store`, `crates/system/fs/src/native/`):
at startup it checks the store volume, runs the probes (0.3–2.8 s on a loaded
host) and serves Tier 1 when they hold, else FUSE; what the implementation
proved or disproved is in [§6.9](#69-what-the-daemon-integration-established).

FUSE remains for Tier 2/3 only. Exact-share recipients use the same native
materialisation as everyone else, because a device only materialises records
it may read ([§14](#14-permissions)). The separate FUSE recipient mount
(`crates/system/fs/src/recipient_mount.rs`) is retired once Tier 1 ships.

### 6.2 Store volume and layout

- **Tier 1 needs a dedicated Btrfs filesystem** (its own superblock) for the
  store. Reasons:
  - fanotify filesystem marks are per superblock.
  - A pre-content mark on the root filesystem would put the daemon behind
    every file access on the machine.
  - Epoch capture uses Btrfs subvolume snapshots ([§6.4](#64-change-capture-off-the-hot-path)).
  - The supported root is already Btrfs on the composefs backend
    (`os/Containerfile:2168`, `os/usr/lib/bootc/install/40-tilo.toml`), but it
    is one filesystem, and bootc keeps `/var` on it.
- **Decided (Q1): every Tilo OS install gets a store volume**, mounted at
  `/var/lib/tilo/store` (`var-lib-tilo-store.mount`, from `/etc/fstab`,
  `nofail`). `os/usr/libexec/tilo-store-volume` owns the policy; its header
  and `os/tests/store_volume.sh` are the reference.
  - *Whole-disk installs* end the GPT with a LUKS2 + Btrfs partition of type
    `0b6c5d38-8ba9-40aa-a1f6-dcc035f5438d` ("Tilo store", invisible to root
    discovery and to `systemd-gpt-auto-generator`). It unlocks from a random
    key file inside the encrypted root
    (`/etc/cryptsetup-keys.d/tilo-store.key`, `/etc/crypttab`), so there is
    no second prompt; the install's recovery key is enrolled in it too, so
    the rooms never sit behind a factor the person was not shown. Sizing:
    after ESP and `/boot`, the root and the store get half each, the root
    never less than 32 GiB; a store under 8 GiB is not created. Half, because
    until the file-backed `ChunkStore` (§7.4) lands, synced content is also
    kept in the daemon's content store on the root. The store is the last
    partition, so it is the one that can grow.
  - *Alongside installs and installs that predate the store* get an image:
    `tilo-store-volume.service` (ordered before `fs.service`) creates
    `/var/lib/tilo/store.img` once, preallocated and `chattr +C`, sized to
    half the root's free space beyond a reserve of max(16 GiB, 10 %), at
    least 8 GiB, else it logs why and FUSE keeps serving. It is attached with
    direct I/O (`tilo-store-loop.service`) and covered by the root's own
    encryption. Nothing is copied: on its first native start the daemon
    projects every room from its local content store, small files at once and
    large ones as placeholders that hydrate from the same store.
    `kind=none` in `/etc/tilo/store-volume.conf` opts a machine out.
  - On every boot the service hands root-owned rooms to the local person (the
    installer's account, else greetd's session user, else the only regular
    account) and writes `--data-plane=auto --store=… --native-owner=UID:GID`
    to `/run/tilo-store/fs.env`; without that file `fs.service` serves FUSE.
  - The store is mounted with `context=…:fusefs_t:s0`, the label files under
    `/var/workspace` had on the FUSE mount, so every domain keeps exactly the
    access it had until `tilo_fs_t`'s enforcing policy defines a store type.
- Each room's tree is a Btrfs subvolume under `live/`. That lets one room be
  snapshotted, quota-accounted (simple quotas, not qgroups) and removed alone.
- `/var/workspace` stays the user-visible path. It shows one directory per
  room. Sandboxes bind their room's tree exactly as the launcher binds paths
  today (`docs/architecture/security.md:129-134`).
- Ownership: every materialised file belongs to the local person's uid/gid.
  Owner and group are **not** replicated. The mode bits `rwx` for user, group
  and other and the setgid bit on directories **are** replicated. Setuid is
  never materialised. [R9]

### 6.3 File states

```
                 metadata arrives            first read / prefetch / pin
  (not present) ───────────────► Placeholder ──────────────────────► Partial ──► Resident
                                  ▲   sparse file, real size,          ranges      │   │
                                  │   mode, mtime; residency xattr     bitmap      │   │ pin
                                  │                                                ▼   ▼
                                  └──────── evict (punch holes) ◄──────────── Resident/Pinned
                                                                                   │ local write
                                                                                   ▼
                                                                                 Dirty ─► captured
                                                                                         (new version)
  LocalOnly: never synced (sockets, FIFOs, devices; CACHEDIR.TAG trees; user-excluded)
```

- A **placeholder** is a real inode. `stat`, `readdir`, permissions, `mtime`
  and size are exact, so `ls -l`, `find` and `git status` never hydrate
  anything. [R1, R9]
- A placeholder carries a `FAN_PRE_ACCESS` inode mark; a resident file
  carries none, so its I/O raises no event at all. The kernel drops the
  marks with the daemon's fanotify group, and an unserved placeholder then
  reads as its holes (zeros), not as an error (pinned by a test). The store
  must therefore only be exposed at `/var/workspace` while the daemon holds
  its groups: the daemon re-adopts placeholders from their residency xattr
  before it binds the tree, and the bind's lifetime is tied to the service.
- Residency is published in a `trusted.tilo.residency` xattr, readable without
  triggering hydration. Tilo's own search, thumbnail and Files code read the
  daemon's metadata and never content. The existing thumbnail and metadata
  planes stay eager (`docs/collaborative-files-product.md:357-364`).
- Reading a non-resident range blocks **only that reader**, and only until
  that range arrives. There is no global worker to stall, unlike today's single
  FUSE thread. When no source is reachable, the read fails with `EIO` after a
  bounded wait; the wait is short when the daemon already knows it is offline.
  That keeps today's behaviour ("`NotResident` … observes `EIO` without losing
  metadata", `docs/collaborative-files-product.md:128-133`).

### 6.4 Change capture off the hot path

Writers write to native files. The daemon observes them and never
intercepts. [R1, R2]

1. **Journal.** fanotify (Tier 1) or FUSE callbacks (Tier 2/3) feed a durable
   journal in `meta.redb`. It records create, delete, rename, attribute changes,
   close-after-write and modify, keyed by record id. `FAN_MODIFY` is
   journaled once per file per epoch until the file is closed. Measured on
   7.2: Btrfs refuses the inode ignore mark that would suppress the repeats
   in the kernel (`EXDEV`) for any subvolume other than the top level, because
   its fsid differs from the filesystem mark's; room trees are subvolumes, so
   repeats are dropped in userspace and the kernel only merges queued
   events. Only `FAN_PRE_ACCESS` is synchronous, and it is delivered only for
   placeholders, which carry an inode mark until they are fully resident
   ([§6.8](#68-kernel-facts-to-verify-before-committing-to-tier-1)).
   Paths are resolved from the directory handles when events are read, so a
   directory renamed before its earlier events are read reports them under
   the new name; capture reads the snapshot, so the published state is
   unaffected.
2. **Epochs.** When the journal is dirty, the daemon cuts a capture epoch. It
   drains the queue, takes a read-only Btrfs snapshot of the room subvolume
   (atomic across all files) and records the journal position. The position
   is fixed by barrier events (a marker file created outside the tree just
   before and just after the snapshot): entries between the two barriers are
   examined in this epoch and again in the next. Without that split a path
   journaled earlier can be read from a snapshot that already holds a newer
   version whose dependencies are journaled later — the git test showed a ref
   published before its commit object in 2 of 9 runs. Epochs are cut:
   - at quiescence (no new events for 500 ms);
   - at the latest every 2 s while dirty;
   - later, up to 5 s, while a file in the dirty set was created less than 1 s
     ago and is still open for write. This keeps short-lived lock files out
     of most epochs using a generic signal, not a tool rule.

   All numbers are tunables; stage 3 measures them.
3. **Diff and hash from the snapshot, never the live file.** For each record
   changed since the previous epoch:
   - Compare `FIEMAP` extents with the previous epoch's snapshot. Shared
     physical extents are unchanged bytes.
   - Re-run CDC only from the chunk boundary before the first changed extent
     until boundaries realign after the last one.

   Editing 4 KiB inside a 10 GiB VM image re-hashes about one chunk, not
   10 GiB. On XFS (Tier 2) the same comparison runs against per-file reflinks.
   On ext4 there is no reflink: the daemon re-reads the file and checks that
   `size/mtime/ctime` were stable. [R1, R2]
4. **Publish.** The changed records' new versions become one **changeset**
   ([§8.3](#83-changesets-and-signatures)). Old content that history retains is
   reflinked from the epoch snapshot into `.versions/` before the snapshot is
   deleted. On Btrfs that shares extents, so a retained version costs only the
   blocks that later changed.
5. **Crash recovery.** After a daemon crash, or writes that happened before the
   daemon started, the daemon finds every inode whose Btrfs generation is newer
   than the last checkpointed epoch (a tree search, not a full walk) and replays
   it as dirty. On other filesystems it walks with `ctime` as the fallback. A
   fanotify queue overflow triggers the same scan. The journal is an
   optimisation; the filesystem is the truth.

What the writer experiences: nothing. No lock, no pause, no extra fsync. The
snapshot causes one Btrfs transaction commit per epoch. That is the one cost
on the write side, and stage 3 gates it ([§20](#20-risks), risk 2).

**Versions are per changeset, not per fsync.** A SQLite database that fsyncs
200 times a second produces at most one version per epoch. History then thins
old versions ([§15](#15-history)). This removes the failure mode where SQLite
hit `MAX_RECORD_VERSIONS` and then lost writes (`record_repository.rs:28`).

### 6.5 Applying remote changes locally

- New content is written into staging files on the store volume. Chunks are
  verified and then written at their offsets; nothing passes through a CAS copy.
  The staged file is renamed over the target (`renameat2`). An open reader keeps
  the old inode, as with native `git checkout` or `rsync`.
- A placeholder that becomes resident is filled in place, range by range. Its
  inode never changes.
- `mtime` is set with nanosecond precision. Mode bits and symlink targets are
  exact. Hardlinks inside a tree are captured as separate records that share
  content, and a peer materialises them as reflinks. Link identity is not
  preserved across devices (Q9).
- If the local file is `Dirty` (a local write not yet captured) when a remote
  version arrives, local bytes are never overwritten. The remote version
  becomes a conflict ([§11](#11-conflicts-and-leases)).
- Order and atomicity rules are in [§10](#10-visibility-across-files).

### 6.6 POSIX fidelity

| Behaviour | Tier 1 | Tier 2 | Replicated? |
|---|---|---|---|
| Exec bit, `chmod` | native | FUSE `setattr` honoured | yes (mode bits) |
| Symlinks | native | FUSE `symlink/readlink` implemented | yes (target bytes, verbatim) |
| Hardlinks | native | FUSE `link` within the tree | as reflinked copies (Q9) |
| Owner | local person's uid/gid | same, via `default_permissions` | no (local) |
| `O_EXCL` create | native (local atomicity) | FUSE `create` with `O_EXCL` | the file itself, as normal |
| `rename` atomic, incl. over an existing file | native | FUSE `rename` (+`RENAME_NOREPLACE/EXCHANGE`) | yes, one operation in a changeset |
| `fsync` | native: durable on local disk | passthrough to the backing file | never implies remote durability; that is a receipt ([§12](#12-caching-residency-and-prefetch)) |
| mmap shared/private | native | passthrough | n/a |
| `fallocate`, `SEEK_HOLE/DATA`, `copy_file_range`, reflink | native | forwarded | content only |
| `flock/fcntl` locks | native, local only | local only | no; cross-device single writer uses leases |
| xattrs | native | forwarded | `user.*` yes; `trusted/security` no |
| Sockets, FIFOs, device nodes | native | native | never (LocalOnly) |
| `statfs` | the store volume's | reported | n/a |
| Timestamps | native | `setattr` honoured | `mtime` (ns); `atime`/`ctime` local |

A failed capture never makes `close` or `fsync` lie. In Tier 1 they do not
involve the daemon at all. In Tier 2/3, an error from the backing file is
returned as is, and publishing is asynchronous, so it cannot fail a syscall.
This removes the silent-loss path at `fs.rs:472-481`. [R9]

### 6.7 Local-only content

These are general conventions, not tool rules:

- Special files are never synced.
- A directory containing a `CACHEDIR.TAG` with the standard signature
  (Cargo's `target/`, many caches) defaults to LocalOnly.
- A file or directory carrying the `user.tilo.local` xattr is LocalOnly,
  with everything below it (implemented). This and `CACHEDIR.TAG` are the
  marks app and tool authors use for derived state
  ([`docs/device-local-state.md`](../device-local-state.md)).
- A per-directory `.tiloignore` and a Files toggle let the user exclude more.

Whether other rebuildable trees such as `node_modules` sync by default is
Q10. The design handles 100k-file trees either way, so this is a product
choice, not a capacity limit.

### 6.8 Kernel facts to verify before committing to Tier 1

The stage 0 spike must answer these on the shipped 7.2 kernel with Btrfs.
Each has a fallback.

| # | Question | Needed for | Fallback if "no" |
|---|---|---|---|
| K1 | Can an evictable **ignore** mark (`FAN_MARK_IGNORE`+`FAN_MARK_EVICTABLE`) on a resident inode suppress `FAN_PRE_ACCESS`, so that resident files cost zero userspace round trips? | R2 | Stay on Tier 2 |
| K2 | Does a page fault on an mmap of a placeholder range raise a pre-content event before the page is read? | mmap of placeholders (git packs, SQLite, binaries) | Hydrate the **whole** file on `FAN_OPEN_PERM` when no fault hook exists. This is safe and slower for large cold files; mitigated by residency policy ([§12](#12-caching-residency-and-prefetch)) |
| K3 | Are `splice`, `sendfile`, `copy_file_range` and reflink **from** a placeholder covered by pre-content events? | correctness of `cp`, `rsync` | Same as K2 |
| K4 | Can `FAN_DENY_ERRNO` return `EIO`/`EROFS`/`EBUSY` from permission events? | offline reads, read-only shares, leases | `EPERM` only (honest but less precise) |
| K5 | Cost of one epoch snapshot with 100k files and 1 GiB dirty, p50/p99 | R1, R2 | Per-file reflink epochs (weaker cross-file consistency, [§10](#10-visibility-across-files)) |
| K6 | Can mount marks report directory-entry events on 7.2? If they can, a dedicated superblock may not be needed. | Q1 | Dedicated filesystem (the baseline) |

**Results on 7.2.1 with a loop-mounted Btrfs store** (`tilo-fs-native probe`,
`crates/system/fs-native`; the host was heavily loaded, so latencies are
upper bounds):

| # | Answer | Consequence |
|---|---|---|
| K1 | **Yes.** An evictable ignore mark on an inode suppresses `FAN_PRE_ACCESS` from a filesystem-wide mark (10 reads, 0 events). | Not needed on the hot path: placeholders carry an inode pre-content mark and resident files carry none, so resident I/O raises no event either way. |
| K2 | **No fault hook**, but `mmap()` itself raises one `FAN_PRE_ACCESS` for the whole mapped range; 1,024 page faults afterwards raise none. | The daemon hydrates the mapped range before `mmap` returns. Correct for git packs and SQLite; mapping a large cold file hydrates the whole mapping. |
| K3 | **Yes** for `read`, `splice`, `sendfile`, `copy_file_range` and `FICLONE` from a placeholder. | No whole-file fallback needed. |
| K4 | **Partly.** `FAN_DENY_ERRNO` accepts `EIO` and `EBUSY`; `EROFS` is rejected (`EINVAL`). | Offline reads get `EIO`, lease conflicts `EBUSY`; read-only shares use `EPERM` or write bits. |
| K5 | 100k files: snapshot p50 2.7 ms / p99 8.9 ms clean, 49 / 87 ms with 64 MiB dirty, 1.08 / 1.33 s with 1 GiB dirty. The snapshot first flushes the room's dirty data (~1 GB/s here), so the p99 < 50 ms gate holds only for small dirty sets. | Writers are not paused, but time-to-publish includes writeback. Cutting epochs often keeps the dirty set small; streaming writers stay out of epochs until quiescent. |
| K6 | **No.** A mount mark with create/delete/rename is refused (`EINVAL`). | The dedicated store superblock stays required. |

Found on the way: an inode ignore mark under a filesystem mark is refused
with `EXDEV` in any subvolume below the top level (its fsid differs), so
`FAN_MODIFY` repeats are dropped in userspace (§6.4). Pre-content inode marks
work across subvolumes in one group. Filesystem marks must be placed at the
top-level subvolume. Rootless: a user namespace can put a filesystem mark on
a tmpfs it mounted, never on the Btrfs store, and cannot create a
pre-content group at all, so the journal and hydration need `CAP_SYS_ADMIN`
(and `CAP_DAC_READ_SEARCH` for `open_by_handle_at`); unprivileged code gets
directory inode marks and snapshots of subvolumes it owns.

### 6.9 What the daemon integration established

The storage daemon serves Tier 1 from `crates/system/fs/src/native/`
(`plane.rs` wires it; `project.rs` materialises remote versions; `lease.rs`,
`state.rs`, `fetch.rs`). Local changes become the daemon's existing signed
record operations through path-level entry points shared with the FUSE
callbacks (`crates/system/fs/src/fs/native_ops.rs`). End-to-end tests run two
daemons on two loop-mounted Btrfs stores
(`crates/system/fs/tests/native_tier1.rs`, gated on
`TILO_FS_NATIVE_STORES` and `CAP_SYS_ADMIN`, mandatory with
`TILO_REQUIRE_FANOTIFY=1`). Numbers below are from that suite on 7.2.1 with a
release daemon on a host at load average ~70 on 16 cores, so they are upper
bounds.

**Held as designed**

- *Startup order.* Room subvolume, pre-content group, re-adoption of every
  placeholder from its xattr, journal, first epoch, and only then the bind at
  the workspace path. A restarted peer re-adopted a never-read placeholder and
  served its bytes; while the daemon is stopped nothing is reachable at the
  workspace path. The bind is also detached by `ExecStopPost` after a crash.
  Probing takes 0.26–2.8 s; the first epoch and projection of an empty room
  3–60 ms.
- *Crash recovery.* Each epoch is persisted in `meta.redb` (retained
  snapshot, journal position, node table, materialised versions) after its
  changeset is applied; the previous snapshot is deleted only then. After a
  `SIGKILL` and three changes made while stopped, the restart published
  exactly those three in 829 ms by comparing inode metadata against the
  persisted snapshot. A full metadata walk is cheap enough that the Btrfs
  generation search (§6.4 step 5) remains an optimisation, not a need.
- *Placeholders.* A 3 MiB remote file appeared with exact size and mtime and
  no bytes; a cold sequential read ran at 95 MB/s (missing chunks fetched 16
  at a time over one connection, with fs-native read-ahead; 8.5 MB/s with a
  debug daemon).
- *Git with no git code.* Three commits made on one device appear on the
  other after 45 s; there `git fsck --strict` is clean, `git status` is clean
  and does not rewrite the index. The only git-aware piece is configuration
  (§17, Q4).
- *Leases.* Detected from behaviour, carried in the namespace, enforced with
  pre-content events (§11.2). A second writer gets `EBUSY` while readers
  read; the lease moved to the other device 0.73–1.05 s after the holder
  closed the file.

**Found by the implementation**

- *Ordering on the receiving side is the projector's job.* V3 assumed a peer
  receives one author's changes in order. Namespace deltas do arrive in
  order, but a signed record's content revision arrives through the record
  mesh per record, in no particular order, and a record whose revision is not
  verified yet reads as an empty file. The projector therefore applies files
  in the order of their versions' times and stops at the first one it cannot
  verify (or, for a small file, whose bytes it cannot fetch); the next pass
  continues. With that, the ref-before-objects check held in every run.
- *Order key.* Peers order by the version's time, which the author now
  publishes as the file's own mtime (so that stat caches match, §17). A
  version written with an older mtime than the record's previous one keeps
  the previous time: signed metadata never goes backwards.
- *The residual window of §10 is real for git, and closed by applying
  changesets whole.* One epoch captured `git add` and `git commit` together.
  The index was rewritten after the ref update, so its only published version
  sorts after the ref; the peer briefly showed the new `HEAD` with no index,
  and a `git status` running then wrote an index, which the peer published.
  Ordering a path created within an epoch by its first journal position does
  not help: such a ref carries content written at the end of the epoch and
  would be shown before the objects it names. What does: each device marks
  the changeset it publishes (a namespace scalar per device, open before the
  first operation, closed after the last, naming the span of version times;
  namespace changes apply causally, so a closed mark means the whole epoch
  is here). A peer holds its projection while another device's changeset is
  open, prepares every version of a pass first (small files and links staged
  at the room's root), then installs them in one quick step, paths new here
  before changed ones, each in version order; a version not available yet
  holds back the rest of its changeset (at most 60 s). **Measured:** with
  `git status` running on the peer every 100 ms throughout the sync (153–211
  runs), the status was clean the moment the new `HEAD` appeared, the peer
  published nothing, and three commits arrived in 16–25 s.
- *Git's racy index is git's, not a sync artifact.* An index written in the
  same second as its files holds "racily clean" entries that the next
  `git status` rewrites, on whichever device runs first. The test commits a
  second after its edits, as a person does.
- *The blob guard was the bottleneck for placeholders.* It authorised each
  chunk request by walking the namespace and decoding every readable
  manifest, serially: ~330 ms per request for a 90-file repository, so a
  2 MiB file's ~200 chunk requests timed out and git failed with `mmap
  failed: I/O error`. The guard now keeps, per caller, the readable roots and
  their chunks keyed by namespace heads and ACL revision (recomputed on any
  change, so revocation is as immediate as before). It also recognises a
  caller for requests iroh-blobs reports after the connection's close. The
  reverse index of §9 replaces both.
- *Hydration must not move mtime.* Filling a placeholder writes, which moved
  its mtime; the hydrator now restores it after each fill. Without that, `ls
  -l` changed on read and git re-hashed every hydrated file.
- *Pre-content events do not tell reads from writes* (see §11.2). The
  blocked thread's `/proc/<tid>/syscall` does, per access.
- *Renames are moves.* Capture pairs a removed and an upserted path of one
  inode into a move (a moved directory's descendants are implied; removals
  inside it are rewritten to its new place), published through the signed
  rename, so the record keeps its id, history and access (a replacement
  transaction when it lands on a record). Peers rename the local file: a
  resident 3 MiB file kept its inode and bytes, a directory moved with its
  subtree, both ways.
- *POSIX attributes.* Mode bits (rwx; setgid on directories; never setuid)
  and "is a symbolic link" are one namespace scalar per record, published
  before the content they go with; a link's target is its record's content,
  so it is signed and versioned. Relative, absolute and dangling links, 0755
  scripts (executable on the peer), 0600 files, 0700 and 02775 directories,
  and mode-only changes replicate both ways. Everything materialised belongs
  to the room's owner (tested with a non-root owner). Hard links are
  separate records sharing content; a write through one link reaches every
  link of the inode (the journal names only one), and removing one of two
  links is a removal, not a move. Link identity does not cross devices (Q9).
  The attributes are namespace data, like placement and leases; covering them
  with record signatures needs a metadata schema version.
- *The record plane, not the data plane, bounds sync.* Publishing costs 30–60
  ms per file on the author (signed envelopes, versions, history). The mesh
  pull opens a connection per record stream (~170 ms each) and re-projected
  the whole repository after every pulled record, so a 51-file burst took
  quadratic time (the 51st reconciliation alone 7.2 s); it now reconciles
  only the pulled record and the whole repository once per pass. The
  transfer engine of §9 is now in place for the metadata plane and for
  content (see "What the transfer engine implements" in §9). The author's
  durable writes per published file (a record file with a backup
  generation, a synced history line, about 20 ms per change on a
  loop-mounted store) are gone too: an epoch is now one signed changeset,
  written in one log transaction (see "What the operation log implements"
  in §8.3).
- *Content is stored once.* The capture chunks what changed in the format
  of §7 (re-hashing only changed extents of a file it chunked before), and
  publishing clones the snapshot file into the content store's area on the
  store volume (`<store>/.content/<room>/`) instead of reading it. A peer
  fetches missing ranges into the area and fills placeholders by copying
  area ranges, which clones extents. See "What the content format and store
  implement" in §7.

**Not yet**

- Materialised files belong to the room directory's owner (`--native-owner
  UID:GID` overrides). On Tilo OS the store volume (§6.2) is prepared before
  the daemon starts and passes the local person as `--native-owner`; the
  daemon therefore needs `CAP_CHOWN`, `CAP_FOWNER` and `CAP_DAC_OVERRIDE` to
  create, fill and hand over files in a room it does not own.
- A rename cycle within one epoch (`a`↔`b` swapped) is published as content
  changes of both paths, not as moves.

### 6.10 Cloud accounts

An external file account set up in Settings (WebDAV, Nextcloud, Google
Drive, OneDrive, Dropbox and iCloud Drive) is an ordinary folder at `~/Remote/<account label>`. Every app and POSIX tool
uses it the way it uses any other folder. Rooms replicate signed records
between peers. A cloud account mirrors a server, and the server is the
authority for its own tree. That is why this is a separate backing, not a
room.

**Processes and credentials.** `fs cloud-service` (user unit
`tilo-fs-cloud.service`) follows Settings' account store. It keeps one
`fs mount-cloud` child per enabled file account. Disabling an account
unmounts it. Removing an account also drops its cached content and queued
state. Neither the service nor its children open the account vault, where
Settings keeps long-lived grants (refresh tokens, app passwords, iCloud
trust state). The service reads one account's short-lived credential from a
separate handoff store (`accounts::handoff`: one sealed file per account,
under its own key) and sends the child only that, on stdin: HTTP Basic, an
OAuth2 access token, or iCloud web session headers. When the credential is
missing, has expired or the server rejected it, the service runs Settings'
credential helper, `tilo-account-credential refresh <id>`, through
`bounded-command`. The helper opens the vault for that one call, refreshes
the grant, seals a rotated one back and publishes only the short-lived part.
A grant the provider no longer accepts marks the account "sign in again".
Closing the child's stdin unmounts it.

Settings resolves an OAuth account's storage once at sign-in (the OneDrive
drive and root ids, a Dropbox member's team space root; Google Drive and
iCloud Drive store their defaults) and keeps the provider crate's own serde
configuration in the account's non-secret `config` under `mount`. The
daemon's provider table (`crates/system/fs/src/cloud/providers.rs`) mounts
from it and sends credentials only to HTTPS or loopback endpoints.
Public OAuth client registrations are data in
`crates/system/account-custody/oauth-clients.toml`, overridable per value
from the environment. Mount health goes to the shared account status store, which
Settings and Files read. Files has no transport of its own. It opens the
folder and shows the account's health as a badge.

**Provider interface.** `crates/system/cloud-source` defines `CloudSource`:

- id-based items with a POSIX projection (duplicate names, several parents,
  provider-native documents as read-only `<name>.<link-extension>` entries);
- a cursor change feed with long-poll or push, or polling of listings that
  skips folders whose recursive revision did not change;
- provider hashes, revision preconditions and revision history;
- ranged streaming reads and resumable chunked upload sessions;
- per-provider capability flags, including a best-effort marker for
  unofficial APIs.

`crates/system/cloud-source-dav` implements it for WebDAV and Nextcloud. Its
Nextcloud support covers recursive ETags, checksums, chunked uploads,
versions and trash, and it uses a `sync-collection` feed where the server
offers one. `cloud-source-gdrive`, `cloud-source-onedrive`,
`cloud-source-dropbox` and `cloud-source-icloud` implement it for the other
providers, each with an in-process fake server (feature `fake-server`).

**Data path.** Each mount is a `CloudVolume`
(`crates/system/fs/src/cloud/`) behind FUSE:

- Metadata is persisted, so the folder lists instantly and offline after a
  restart.
- A read fetches only the missing, 1 MiB-aligned ranges of the requested
  window. The window widens for sequential readers. Every fetch is a ranged
  read pinned to the listed revision, so a read never mixes versions.
- Fetched ranges land in the content store's file area (§7, §12.2), keyed
  by remote revision: a sparse file per revision with persisted range
  residency, under the account's byte budget, least recently used first.
  Complete revisions with identical bytes share extents. The FUSE adapter
  stays separate from the rooms' FUSE filesystem: that one's inodes, reads
  and writes are the namespace, records and ACL of a room, and its
  passthrough needs privileges a per-user mount does not have. What the two
  share is the content store underneath.
- Local writes go to a staged copy. When the last writer closes the file,
  an upload is queued in a persisted outbox and sent in the background, with
  the revision the edit started from as an `If-Match` precondition.
  Creates, folders, renames and deletes queue the same way. An editor's save
  by rename replaces the target, which keeps its identity.
- On a conflict, the remote version keeps the name and the local edit is
  uploaded next to it under the daemon's conflict-copy name
  (`<name>.sync-conflict-<hash>`, §11).
- Offline, cached bytes stay readable, uncached reads fail with `EIO`, and
  writes wait in the outbox across restarts.

**Tests.** The provider conformance suite (`cloud_source::conformance`) runs
unchanged in three layers:

- in-process fakes on every build (`FakeDav` on the shared `test-http`
  responder, with injected 429/5xx responses, truncated bodies, expired
  credentials and cursor resets);
- real servers in containers: Nextcloud and Apache mod_dav, started by
  `crates/system/cloud-source-dav/tests/servers.sh`;
- live accounts on a schedule, when CI secrets are present.

`crates/system/fs/tests/cloud_mount.rs` covers the mount end to end against
the fake, including a kernel mount where `/dev/fuse` exists.
`apps/settings/tests/cloud_storage.rs` covers every provider from sign-in to
removal: Settings signs in through the fake authorization server (or Apple's
sign-in on the fake iCloud), custody publishes the handoff credential, the
cloud service mounts the account against the provider's fake, a file is
written and read back, and removing the account unmounts it and deletes its
cache.

---

## 7. Content format

### 7.1 Chunking

- **FastCDC** (normalised chunking, skipping the minimum region) replaces the
  byte-at-a-time gear hash with ~8 KiB average (`crates/system/fs/src/content.rs:10-13`).
  Chunk targets scale with body size. [R1, R3]

  | Body size | Representation | Chunk min / avg / max |
  |---|---|---|
  | ≤ 4 KiB | **inline** in the record operation ([§7.3](#73-small-content)) | – |
  | ≤ 64 KiB | one blob, content id = BLAKE3 of bytes | – |
  | 64 KiB – 64 MiB | manifest | 16 / 64 / 256 KiB |
  | 64 MiB – 4 GiB | manifest | 64 / 256 KiB / 1 MiB |
  | > 4 GiB | manifest | 256 KiB / 1 / 4 MiB |

- The tier is stored in the manifest header and **sticks to the record**. A
  record changes tier only when its size moves past a boundary by 4×. This
  keeps a file that hovers at a boundary from losing all dedup on every save.
- Effect: a 1 GiB file becomes about 4,000 chunks instead of about 130,000
  blobs plus 130,000 tags (`store.rs:45,819`).
- Compression is per chunk (zstd level 3), applied only in transfer and only
  when a sample compresses by at least 10%. It never changes the content id.

### 7.2 Manifests

A file manifest is an iroh-blobs **`HashSeq`**:

```
HashSeq[ meta, c0, c1, …, cN ]
meta = { format: 2, total_len, tier, chunk_lengths (varint, zstd) }
content id = BLAKE3(HashSeq bytes)
```

- One GET with a `RangeSpecSeq` fetches the header and exactly the missing
  chunks, or byte ranges within them. Everything is BAO-verified in 16 KiB
  groups and streamed on one QUIC stream. [R3]
- A `HashSeq` is itself a BAO-verifiable blob, so even a 100 GiB file's
  manifest (≈3 MB) can be fetched and verified by range. No second level of
  manifest is needed.
- Locally, a `(content id) → chunk offset table` lives in `meta.redb`. A random
  read of a partial file maps offset to chunk in O(log n) and never decodes a
  manifest. Today's `Hydrator::read` decodes it on every call
  (`hydration.rs:388-420`).
- Version-1 postcard manifests stay readable. They are converted when a record
  is next written. Nothing is converted in bulk.

### 7.3 Small content

Throughput for many small files is decided here. [R3, R9]

- **Inline ≤ 4 KiB.** The bytes travel inside the signed record operation.
  Metadata and content therefore arrive together, in one batched stream, with
  no second request. A 41-byte git ref, a `.gitignore`, most config files and
  many source files are inline. The threshold is a tunable, measured in
  stage 2.
- **Batch everything else.** No request is ever for one blob. The transfer
  engine builds an ad-hoc request (`GetManyRequest`, or a `HashSeq` of wanted
  hashes) for hundreds or thousands of blobs at a time ([§9](#9-transfer-engine)).
  Packing is therefore a *transfer* property, not a storage format. Nothing
  has to be repacked when one small file changes.

### 7.4 Where bytes live locally: one store, two backings

The content store has one API, one hash space, one GC and one authorisation
rule, and two physical backings:

| Backing | Holds | How it serves |
|---|---|---|
| **File-backed** | Current and retained versions of file records, package payload files. The chunk index points at `(inode or version file, offset, len)`. | The chunk range is read from the native file or the retained `.versions/` reflink and verified against its hash before sending (BLAKE3 runs at several GB/s per core). No second copy exists. |
| **Loose** | CRDT change segments and snapshots, thumbnails, chunks of remote content that is not materialised (a relay host's replica, prefetch into a partial file), manifests. | iroh-blobs `FsStore` (`store.rs:170-215`). |

- The backend is a Tilo `ChunkStore` that implements the iroh-blobs store
  protocol. iroh-blobs documents this pattern (`src/store/readonly_mem.rs:1-7`,
  "a good starting point for custom implementations"). The iroh-blobs provider
  (`handle_get`, `handle_get_many`, `handle_push`, `src/provider.rs:460-582`),
  the Downloader and BAO verification are reused unchanged. If 0.103's store
  protocol turns out not to allow a clean custom backend, the fallback is a
  Tilo ALPN that speaks the same BAO range format through `bao-tree` directly
  (Q7).
- The daemon writes the snapshot file that it hashes, so "the bytes on disk
  still match the hash" holds by construction. Sending re-verifies anyway: a
  corrupt disk must never propagate.

**What the content format and store implement** (`crates/system/fs-content`,
`crates/system/fs/src/store/`, `hydration/ranges.rs`):

- *Chunking* follows the table of §7.1: normalised FastCDC, tiers sticky per
  record with the 4× hysteresis, chunk hashes computed on several threads,
  and cut points of a large body found on several threads (each thread
  chunks a segment; the true sequence joins a segment's cuts at the first
  cut both found, which is exact because a cut depends only on the bytes
  from its chunk's start). Compression in transfer is not implemented.
- *The manifest is one self-contained blob* rather than `HashSeq[meta, …]`
  with a separate meta blob: a `HashSeq` whose first entry is a header
  (`"tilocf"`, format 2, tier, total length, chunk count), then the chunk
  hashes, then the chunk lengths packed eight to an entry. One blob tells a
  reader where every chunk starts; a `GET` of the content id names chunk
  `i` at position `i + 2` and never asks for the header or length entries,
  and the provider authorises exactly those positions of a readable content
  id. Small files keep the cost of the earlier format: a manifest and one
  chunk. Legacy postcard manifests are read everywhere; the format is told
  apart by the header, and nothing is converted in bulk.
- *Backings.* The content store keeps loose blobs in iroh-blobs (manifests,
  the chunks of content up to 1 MiB, legacy content) and everything larger
  in a **file area**: one sparse file per content id at the file's own
  offsets, with persisted range residency, a chunk index (chunk hash →
  entry, offset, length) and a retention class. The provider does not go
  through a custom iroh-blobs store backend: it is handed a store that
  answers the provider's requests itself, reading area chunks from their
  files, checking each against its hash and computing its BAO outboard
  from the bytes it just read, and forwarding everything else to the loose
  store. A corrupt or stale file fails the request instead of reaching a
  peer.
- *Ranged fetches.* A read of file-backed content turns its missing byte
  ranges into `GET`s of the content id whose `RangeSpecSeq` names the
  chunks it overlaps and, within each, the 16 KiB groups needed (16 MiB of
  groups per request, 8 requests in flight per peer, spread over peers,
  retried on the next peer). Verified groups are written straight into the
  area; residency is recorded after the data is synced, in batches of
  256 MiB, so a crash or restart resumes with what is missing and never
  trusts unsynced bytes. Chunks this device already holds, in another entry
  or as loose blobs, are copied locally instead of fetched, so a new version
  of a large file costs only its changed chunks on the wire. Concurrent
  readers of overlapping ranges share one fetch.
- *Streaming publish.* A Tier 1 author never holds a file in memory: the
  capture maps the snapshot, and the content store clones it into the area.
  Content without a capture (a FUSE commit is already in memory) is chunked
  from memory, or as a stream from a file.

### 7.5 Dedup scope and the access rule

- **Physical dedup is device-wide**, across rooms, apps, packages and history:
  - Identical content ids share bytes. When the same content is materialised a
    second time (another room, another package, a copied file), it is created
    as a reflink of the first.
  - Chunks shared between versions share extents through `.versions/`
    reflinks. [R8]
- **Transfer dedup:** a device asks only for chunks it lacks. It learns nothing
  about other people's content, because it only asks about content it is
  already authorised for.
- **Hosts serving several people** dedup physically, invisibly. Whether an
  upload is skipped because the host "already has it" is decided **only**
  against content already reachable within the same room, or the same person's
  rooms. Otherwise the host always takes the bytes. This closes the classic
  cross-tenant existence side channel of dedup. [R8]
- **The rule: possessing a hash never grants access.** A blob or chunk is
  served, or opened for a local app, only if it is reachable from a record the
  requester may read. For apps, "reachable" means from a record the app's
  capability covers ([§14](#14-permissions)).
  - An app that learns a hash cannot probe the device CAS.
  - `put` always consumes and hashes the whole stream before answering, so its
    timing does not reveal whether the content was already present.
- **Encryption at rest on untrusted hosts** is not decided (Q6). Convergent
  encryption keyed per room would keep dedup within a room and hide content
  from a hosting operator.

---

## 8. Metadata plane

### 8.1 Records and operations

- A record's durable state is its **operation log**: signed operations,
  verified once at ingest and then stored as verified. This keeps
  `RecordEnvelopeV1` semantics (`record_replication.rs:61-81`) and removes the
  O(m²) re-verification on every persist (`record_repository.rs:1509-1579`).
- Storage is `meta.redb` tables keyed by `(record, actor, seq)` with a hash
  chain per `(record, actor)` stream. The per-record JSON files with backup
  generations (`record_repository.rs:1558-1584`) go away.
- `durable_backup` stays for small security-sensitive stores: identity, ACLs,
  shares, receipts. Those must not roll back
  (`docs/collaborative-files-product.md:167-171`).
- Old operations are compacted into a **signed checkpoint** per record: the
  materialised state plus the retained version DAG. Compaction is what bounds
  a record's size. The hard caps (`record_repository.rs:24-31`,
  `namespace.rs:35`, `full_replica.rs:23-27`, `store.rs:56-70`) are removed.
  Defensive bounds remain only on single messages, and anything larger
  streams. [R1]

### 8.2 The tree is derived from signed placements

- The workspace-wide Automerge namespace document is **retired**. Placement is
  already signed per record as `FileBindingV1 {parent, name, kind,
  tombstoned}`. The tree is a deterministic projection of those bindings. That
  is what `record_materializer.rs` already does for accepted envelopes; today
  it projects *into* the Automerge document instead of replacing it.
- **Move semantics.** Keep the Kleppmann move-tree rules the namespace
  implements today (`namespace.rs:1-10`):
  - delete means move to trash;
  - a concurrently created child of a deleted directory ends in trash, never
    dangling;
  - a concurrent cross-move cycle is broken deterministically by reverting the
    lowest-id node.

  Placement operations carry a hybrid logical clock, which gives every device
  the same total order to apply the rules in.
- **Indexes in `meta.redb`**, maintained incrementally from applied
  operations, never rebuilt by scanning:
  - `(parent, name) → record`
  - `parent → children` (sorted)
  - `record → parent chain`
  - `record → materialised inode`

  Lookup and create are O(log n). Readdir is O(children). The Tier 1 hot path
  does not use them at all, because the kernel's own directory does that work.
- **Name collisions.** When two devices concurrently create the same name in
  one directory, the loser is renamed `<name>.sync-conflict-<short>`, as
  `conflict.rs` already does. The name is decided deterministically, so every
  device agrees.

### 8.3 Changesets and signatures

- A changeset is one device's operations from one epoch: bindings, metadata,
  content revisions and inline bytes.
- It is signed **once**: an Ed25519 signature over the Merkle root of its
  per-record operation list.
  - Each record's operations carry their inclusion proof, about
    32·log₂(n) bytes.
  - A peer that may read only some of the records still verifies exactly those
    records and learns nothing about the others: not names, not hashes, not
    counts beyond the proof shape. The product rule "a peer receives no names,
    metadata, history, hashes … for a record it cannot read"
    (`docs/collaborative-files-product.md:325-327`) holds.
- Cost: one signature and one verification per changeset, instead of three
  per edited file (`record_publisher.rs:753-789`). A 100k-file checkout is a
  handful of changesets. Ingest verifies with `verify_batch` across changesets.
  [R3]
- Changesets carry a **causal context**: a version vector of changesets seen
  from other devices. That is enough to order across devices without a global
  log.

**What the operation log implements** (`crates/system/fs/src/record_log.rs`,
`record_changeset.rs`, `record_repository.rs`):

- Every accepted envelope is appended to one redb table (`records.redb` in
  the repository directory) under a global position; records, stream
  cursors and history are rebuilt from it in log order. Envelopes are
  verified once, at ingest; reopening replays the log without verifying
  signatures again.
- The 16,384-record bound is gone. What bounds a device is the in-memory
  index (replica state and envelopes per record); the repository refuses
  more than 2^24 records, far past any tree one device holds.
- A native epoch is one changeset: every operation the author signs while
  publishing it stays in memory, visible to the local publisher but never
  served, until the whole batch is signed once as a `SignedChangesetV1`
  (per-device sequence, Merkle root over per-record leaves of
  `(record, first..last sequence, digest of the envelopes)`) and written
  with it in one redb transaction. The history lines of the batch are one
  synced write before it; the namespace that places its records is
  persisted after it. After a crash either the whole changeset is in the
  log or none of it, and the epoch is captured again.
- Peers see nothing of a changeset before it is durable: its namespace
  deltas are broadcast afterwards, and every namespace exchange (both
  directions, both protocols) waits on the repository's publication gate
  while a changeset is being published or applied.
- A peer pulls whole changesets (`PullChangesets` in the session protocol),
  checks the signature, root and every envelope against its leaf, applies
  all of them in one transaction, and projects the changed records into the
  namespace in one step under the same gate, which the native projector
  also takes. Changesets are stored with their envelopes, so peers relay
  them. A new file is published with its content as its first version, so
  no empty intermediate version exists at all.
- Envelopes stay individually signed: peers without changesets pull them
  stream by stream through `Pull` and the per-request mesh, as before.
- Measured end to end with the sync benchmark (two release daemons,
  loop-mounted Btrfs stores, loopback, load 2–8): 100 × 4 KiB files 4.0 s
  → 0.79 s (of which 0.5 s is the epoch's quiescence), 1000 files
  48.5 s → 2.5 s, 3000 files 651 s → 7.4 s, a git repository of 3 × 200
  files 51.6 s → 3.6 s; 100,000 files, which the record bound refused
  before, 815 s. At that size the Automerge namespace, the content store
  and replica receipts dominate, not the log: a 75,000-record changeset
  applies in 18 s.
- Not yet: signed checkpoints and compaction of old operations, inclusion
  proofs sent to peers that read only part of a workspace (the proof
  functions exist; partial readers still receive envelopes per record),
  batch signature verification, and the causal context.

### 8.4 CRDT record bodies

- A CRDT record's body is an Automerge document stored as **content-addressed
  segments**:
  - `save_incremental` chunks, grouped by time into segment blobs;
  - periodic compacted snapshots, which are `save()` output and signed as
    checkpoints.

  Segments are loose blobs in the one content store.
- **Live edits** travel as small signed operations with inline change bytes,
  gossiped to the room's online peers within one frame (16–50 ms batching).
  The **same envelope type** is persisted and pulled by set reconciliation. So
  the live lane and the durable lane share one format and one validator, where
  today there are two (`collab-types` `SignedOp` routed opaquely by the broker,
  plus the fs record plane).
- **A late joiner** fetches the latest checkpoint snapshot as a blob (BAO
  verified, resumable, any size) plus the later segments. There is no "one
  message = whole document" path, so the 3 MiB cliff
  (`collab-types/src/op.rs:30-35`) is gone. [R5]
- **Collections** are covered in [§13.3](#133-collections).

---

## 9. Transfer engine

One engine moves record operations, file content, CRDT segments and packages.
[R3, R4, R6]

```
             ┌────────────── per peer: ONE authenticated QUIC connection ──────────────┐
 priority    │ stream A: metadata set reconciliation (RBSR over record→head digests)   │
 queues      │ stream B: live operation gossip (room topic, via iroh-gossip)           │
 ──────────  │ streams C…: content GETs: GetMany / HashSeq + RangeSpecSeq, pipelined   │
 foreground  │            window = max(8 MiB, 2 × bandwidth × RTT) outstanding bytes   │
 lease/pin   └──────────────────────────────────────────────────────────────────────────┘
 predictive       ▲ Downloader: multi-provider splitting (SplitStrategy), hedging for tail
 backfill         │ scheduler: dedups wants, merges adjacent ranges, batches small blobs
```

- **One connection per peer, one authentication per connection.** The
  certified-device handshake happens once, and every request on that
  connection is authorised against the authenticated identity. Today there is
  one handshake per chunk (`hydration.rs:493-520`) and one per record-stream
  pull (`sync.rs:1871-1892`). Connections come from iroh-blobs'
  `ConnectionPool`.
- **Metadata reconciliation.** Range-based set reconciliation over
  `(record id → digest of the peer-visible log head)` exchanges differences in
  O(d·log n) with a few round trips, however large the workspace. It replaces
  both the 3 s full-namespace download for read-only peers (`sync.rs:335-358`)
  and the one-connection-per-stream pulls. Operations stream back in frames of
  up to 1 MiB, many records per frame.
- **Content.** The scheduler turns wants into as few requests as possible:
  - one `HashSeq`+ranges GET per large file;
  - one `GetManyRequest` per batch of up to ~1,000 small blobs;
  - many requests in flight per connection, up to the byte window.

  Throughput is then bounded by bandwidth, not by count × RTT.
- **Priority classes.** In order: foreground read (a blocked `read()` or
  `open`), lease and pinned hydration, predictive, replica backfill. A
  foreground want pre-empts at request granularity. A 10 GiB backfill never
  delays a 4 KiB foreground read by more than one in-flight request.
- **Multi-peer.** The Downloader splits large content across providers. Tail
  latency is hedged: a second provider starts after max(150 ms, 2×RTT), as the
  current hedging does (`hydration.rs:318-380`).
- **Push to always-on peers.** After an epoch is published, the author
  announces it. The home host and pinned devices pull immediately, or the
  author pushes (`push` re-enabled behind the same record ACL; today it is
  `RequestMode::Disabled`, `sync.rs:862`). A laptop that closes its lid right
  after saving has usually already handed off its content. Receipts then let
  it evict safely ([§12](#12-caching-residency-and-prefetch)).
- **Authorisation without scans.** `meta.redb` maintains two indexes:
  - `content id / chunk → {records}` (the reverse index);
  - `record → readable principals`.

  Both are updated when records and ACLs change. A request is authorised by an
  O(1) lookup per `HashSeq` root; children of an authorised root are implied.
  The guard is concurrent and takes no tree lock. It replaces the
  O(workspace)-per-chunk scan (`sync.rs:737-840`).
- **Transport tuning.** Stream and connection receive windows sized from
  measured BDP. BBR where quinn exposes it. zstd per chunk when it helps
  ([§7.1](#71-chunking)). Iroh chooses direct paths and falls back to its
  relay; `tilo-relay` stays a workspace host, not a transport relay
  (`docs/architecture/collaboration.md`, "Workspace hosts").
**What the transfer engine implements** (`crates/system/fs/src/sync/session.rs`,
`hydration/batch.rs`, `blob_access.rs`, `record_reconcile.rs`):

- `tilo-fs-session/2`: one connection per peer, authenticated once, a
  QUIC stream per request, every request authorised against the live
  policy and certification. It carries the namespace (Automerge sync
  between writers; changes since the reader's heads for read-only peers),
  each payload bound to the requesting workspace by the sender's device
  signature over the workspace, the payload kind and the digest of its
  bytes, and applied only from a device the receiver's policy lets read
  (or, for sync, write) the whole workspace
  (`crates/system/fs/src/namespace_binding.rs`; `tilo-fs-sync/1` does the
  same),
  range-based reconciliation of record stream heads, pulls of up to 1024
  streams per request with files up to 4 KiB inline, batched replica
  receipts (only for revisions not acknowledged recently), the authority
  exchange, and announcements that make peers pull at once. Envelopes that
  belong to a signed changeset travel as that whole changeset
  (`PullChangesets`, §8.3). Peers without it are served and reached
  through the per-request protocols, which stay registered.
- Content goes over one pooled iroh-blobs connection per peer as `GetMany`
  batches (512 blobs or 8 MiB, 8 in flight per peer, spread over every
  healthy peer). Each batch names the manifests of its chunks with empty
  ranges; the serving side authorises chunks through them from an index
  rebuilt only when the namespace heads change, concurrently and without
  holding the namespace lock.
- Large content goes as ranged `GET`s of the content id (see "What the
  content format and store implement" in §7). Priority classes order
  them: a blocked read and the read-ahead of an active sequential reader
  are interactive and sent at once; pinned, predictive and backfill work
  is admitted only while no interactive request is on the wire, and claims
  its ranges only once admitted, so it never holds a range a reader waits
  for. A background transfer therefore delays a foreground read by at most
  the requests already in flight.
- Not yet: hedged multi-peer splitting of one large file (requests are
  spread over peers, but a slow peer is not raced), and push of content
  (announcements make peers pull).

- **Resumability.** Partial content survives restarts as persisted range
  residency: the file area's per-entry range set (recorded only after its
  bytes are synced) and the placeholder's residency xattr for materialised
  files. Reconciliation resumes from digests, not cursors.

---

## 10. Visibility across files

This section is what makes git, SQLite and anything else crash-safe work
across devices without knowing about them. [R9]

**Guarantee V1: no torn files.** A file becomes visible on a peer only
through an atomic rename of a fully verified staged file, or by filling a
placeholder whose size and content id are already fixed. A reader sees the old
version or the new one, never a mix.

**Guarantee V2: every published state is a crash image.** A changeset is cut
from one atomic Btrfs snapshot. It therefore equals the state the writer's
disk would hold if the machine lost power at that instant *after* all
completed writes. Programs that are crash-safe on one machine are built to
recover from exactly that. That includes git (objects before refs, lockfile
plus rename), SQLite (journal or WAL), and any program that uses the
write-temp-fsync-rename pattern (`tilo_files::native::atomic`).

**Guarantee V3: ordered application.** A peer applies one device's changesets
in epoch order, and each changeset only after its causal context (other
devices' changesets it depends on). Within a changeset:

1. **Metadata first.** All bindings, placeholders and inline bytes are
   staged. Placeholders are safe to expose before their bytes, because reading
   them fetches exactly the bytes the changeset fixed.
2. **Then publication in the writer's journal order.** Renames, replacements
   and deletes happen in the order the writer's journal recorded their
   **last** modification in the epoch.

So a git ref update is applied after the objects it names, which are
already visible as resident files or fetchable placeholders. Nothing git-aware
is involved. The writer created the objects first, so the journal, and
therefore the peer, orders them first.

**The residual window, stated honestly.** V3 orders by *last* modification.
If one epoch contains a file that the writer modified twice, with another
file's change between the two, a reader *on the receiving device* can observe
a state the writer never had during the few milliseconds the changeset takes
to apply. Two things bound this:

- Epochs are cut at quiescence, so interleaved rewrites inside one epoch are
  rare.
- Files that programs rewrite in place (the kind for which interleaving
  matters, such as a database and its WAL) are lease-class
  ([§11.2](#112-leases)). They have one writer, and readers on other devices
  see them through V1/V2.

Strict multi-file atomicity for application data is available through Record
Store transactions ([§13](#13-the-record-store-api)). Q8 asks whether the
POSIX projection should also offer it.

**Two frontiers.** Metadata visibility follows V3 immediately; bytes hydrate
on demand. For **pinned** subtrees ("Keep offline"), the visible frontier
advances only when every byte of the changeset is local. Offline, a pinned
repository therefore always shows a complete, openable older state, never a
newer ref whose objects are still in flight. [R1, R9]

**Tier differences.** On XFS or ext4 (Tier 2 outside Tilo OS) there is no
subvolume snapshot. Capture falls back to per-file reflink or a stable
re-read. V1 and V3 hold. V2 holds per file, and cross-file only in journal
order. The daemon labels such a store accordingly.

---

## 11. Conflicts and leases

### 11.1 Policy by body type

| Body | Concurrent-edit policy | Where it exists today |
|---|---|---|
| CRDT document / collection shard | Merge (Automerge). No conflict record. | ui-core `State`, qdoc merge (`conflict_merge.rs`) |
| Format-aware file (qdoc, and files a format provider imports) | Merge through the CRDT side-body ([§13.5](#135-files-and-format-aware-files)), then re-project the bytes | canonical `.qdoc` merge |
| Plain UTF-8 text | Three-way merge when edits do not overlap. Otherwise an open conflict with both heads. | conservative `.txt` merge (`docs/collaborative-files-product.md:70-75`) |
| Opaque binary, replaced whole (write-temp + rename, or truncate + write) | **Versioned conflict.** Both versions are kept in the DAG and the live head is chosen deterministically. The other head is materialised as a sibling `<name>.sync-conflict-<short>` so POSIX tools see it. Resolving removes the sibling. | signed DAG heads → open conflict (`record_publisher.rs:473`), Keep Both transaction |
| Mutated in place (written at offsets inside the file, held open across epochs) | **Lease** (§11.2). Without an authority, a versioned conflict as above. | new |
| Directory structure | Move-tree rules ([§8.2](#82-the-tree-is-derived-from-signed-placements)) | namespace move-tree |

Nothing is ever lost. The worst outcome of any race is two retained versions
and a visible conflict.

### 11.2 Leases

Leases give single-writer semantics across devices to files that cannot be
merged or safely forked mid-stream (SQLite, LMDB/redb, VM images, anything
mmap-written). [R9]

- **Which records.** A file becomes *lease-class* automatically the first time
  it is modified in place: a write at an offset inside the existing size,
  without truncation, on a file that is not replaced by rename. The rule is
  behavioural and generic. A per-directory policy or an app declaration can
  also mark records lease-class.
- **Acquiring.** Before the first in-place write of an open-for-write session,
  the writer needs the lease:
  - Tier 1: the `FAN_PRE_ACCESS` write event on a lease-class file waits.
  - Tier 2: FUSE `open` for write waits.

  The wait is one round trip to the authority, once per session, and never
  per write. The lease is renewed while the file stays open, every 20 s with a
  60 s TTL (tunables).
- **Authority.** The room's always-on host (`can_act_as_host`,
  `docs/device-mesh.md`, "Device roles"). When a room has no host, the
  authority is the record's most recent lease holder when it is reachable. When
  nothing is reachable, the writer proceeds **optimistically** and marks its
  versions "unleased".
- **Fencing.** Every version written under a lease carries the lease epoch. A
  version from a stale epoch, or an unleased version that races a leased one,
  never becomes the head. It becomes a conflict version. So an offline write
  forks honestly instead of corrupting the database on another device.
- **Other devices** open a leased file read-only and see V2 snapshots. A write
  open is denied with `EBUSY` if K4 allows it, else `EPERM`, or waits, per
  policy (Q5). Files shows who holds the lease.
- **Checkpoints.** Versions of lease-class files are taken at epochs while the
  file is quiescent, and always at the last close. The history of a busy
  database is therefore a set of crash-consistent checkpoints, not every
  transaction.

**Implemented on Tier 1** (`crates/system/fs/src/native/lease.rs`):

- Lease-class detection: a file whose journal shows a write without a create
  or rename, whose inode survived the epoch, and whose new version still
  shares extents with the previous snapshot (FIEMAP). A truncate-and-rewrite
  allocates new extents and is not lease-class. A small file can have every
  block rewritten in place (an update of a two-page SQLite database rewrites
  the header page and the one table page); when no extent is shared, the
  rewritten ranges are compared in 512-byte sectors, and a file that kept
  non-zero sectors at their offsets while others changed is lease-class too.
- The lease is a `WriteLease {holder, epoch, expires_ms}` carried in the
  namespace (one root-level register per record), so every device sees it in
  causal order with the file's changes. A change of holder raises the epoch;
  a renewal does not; a release keeps holder and epoch with a past expiry.
- A lease-class file nobody holds is gated with `WriteGate::Ask`: the first
  write of a session reaches the daemon through `FAN_PRE_ACCESS`, which takes
  the lease before answering, and the gate is then lifted so the holder's I/O
  raises no events. A file another device holds is gated with
  `Deny(EBUSY)`; a new version of it is gated before it is renamed into place.
  The lease is renewed every 20 s while the journal shows writes, released at
  `CLOSE_WRITE` or after 5 s without writes, and lapses after 60 s offline.
- **Reads and writes, per access.** Pre-content events do not say whether
  an access reads or writes, but the accessing thread (`FAN_REPORT_TID`) is
  blocked inside the call until the daemon answers, and its
  `/proc/<tid>/syscall` names the call and its arguments: `read`/`pread`/a
  read-only or private `mmap` is a read; `write`/`pwrite`/`fallocate`/
  `truncate`/a shared writable `mmap` a write; for `splice`, `sendfile`,
  `copy_file_range` and `FICLONE` it depends on which descriptor is the
  gated file. The descriptor table decides only when the call cannot be
  read. A file another device holds also refuses *opening* it for writing
  (`FAN_OPEN_PERM`, `EBUSY`), so a program that falls back to a read-only
  open reads and reports a read-only file before it writes anything.
  **Measured:** `sqlite3` (read-write by default) on the non-holding device
  reads, and an `INSERT` fails with "attempt to write a readonly database"
  leaving no rollback journal to publish; 0.4–1.5 s after the holder lets
  go, the same command writes. The daemon needs `CAP_SYS_PTRACE` for other
  users' `/proc/<tid>/syscall`.
- **Fencing.** Every version of a lease-class file carries its tenancy
  (epoch and holder), in the same namespace change as its content, and every
  tenancy is kept. A peer applies such a version only if its author held
  that epoch and it is not older than what it shows; an unleased version
  only while nobody holds the lease. A refused version is never applied and
  never dropped: it stays in history, the holder republishes its content as
  the head, and its author keeps it beside the holder's as
  `<name>.sync-conflict-<device>-e<epoch>`. **Measured:** two partitioned
  devices writing one database, the host's granted tenancy and the peer's
  optimistic one of the same epoch: the host never showed the peer's bytes,
  both converged on the host's version 3.3–3.9 s after reconnecting, and the
  peer's version exists as a conflict copy on both.
- **Authority.** The room's always-on host runs with `--lease-authority`: a
  device asks for a free lease through the namespace and the authority
  grants it to one requester at a time with a new epoch (a peer's first
  write waited ~0.5 s for its grant); unanswered within 1.5 s, the device
  takes it optimistically. Concurrent lease values resolve alike everywhere:
  higher epoch, then granted over optimistic, then the greater holder key.
  **Measured:** three rounds of the host and the peer writing a free file at
  the same instant: each time exactly one wrote and the other got `EBUSY`.
- A lease is not released while versions written under it are
  unpublished, and the file is gated before the release is visible: the
  kernel decides at `open` whether a descriptor raises pre-content events,
  so a descriptor opened while a file is ungated stays unwatched. For the
  same reason a new version of a gated file is gated before it is renamed
  into place. Not yet: a descriptor the holder opened during its tenancy
  stays unwatched after the release, so its later writes are unleased
  versions (fenced if another device holds the lease by then).

---

## 12. Caching, residency and prefetch

[R1, R4]

### 12.1 Residency policy (defaults, per device, all tunable)

| Content | Default |
|---|---|
| Metadata of every readable record | always local (it is small, and it is what makes `stat`/`ls`/`git status` native) |
| Inline bodies (≤ 4 KiB) | always local (they arrive with metadata) |
| Files ≤ 1 MiB in rooms the device participates in | eagerly hydrated in the background, predictive priority |
| Anything written by the **same person** on another device in the last 7 days | eagerly hydrated, pinned priority: you continue where you left off |
| Larger files | placeholder until read, pinned or predicted |
| Always-on host with `FullWorkspace` intent | everything (`docs/device-mesh.md`, "One replication substrate") |

### 12.2 Eviction

- **Quota-driven and automatic.** There is a user quota for the store volume
  plus low and high free-space watermarks, 15% and 25% by default. Today
  eviction is manual only (`docs/collaborative-files-product.md:340-355`).
- **Candidates** must be clean, unpinned and not open. They must also be
  durable elsewhere, proven by an **exact-version replica receipt**; this is the
  existing fail-closed machinery (`eviction.rs`, `replica_ack.rs`), now used as
  the policy engine.
- **Scoring:** an ARC-style mix of recency and frequency, weighted by size.
  Large, cold, well-replicated files go first.
- **The operation:** `fallocate(PUNCH_HOLE)` over the whole file. The file
  returns to `Placeholder`, and inode and metadata stay. Retained versions are
  evicted before current versions, and only if they are replicated.
- **Two eviction models merge into one.** The Settings namespace path
  (`docs/sync-storage-hydration.md:22-44`) and the record-plane path become one
  (`docs/personal-and-workspace-settings.md:94-104` names this debt).

**Implemented:** the content store's file area is the one bounded cache.
Its entries carry a retention class: fetched content is *cached* and is
evicted least recently used first once cached resident bytes exceed the
budget (a quarter of the area's filesystem by default); content this device
may be the only holder of (what it authored) is *local-only*, and "Keep
offline" hydration makes an entry *pinned*; neither is ever evicted by the
budget. Explicit eviction through the queue above removes area entries by
any of their hashes. Watermarks, ARC scoring and punching holes into
partially cold files are not implemented; eviction is per entry.

### 12.3 Pinning and offline

- "Keep offline" on a record or directory includes future descendants. It
  survives rename and restart, as today.
- Pinned subtrees use the content frontier from
  [§10](#10-visibility-across-files), so they are always complete offline.
- `local` never means synced. "Synced" and "safe to free" come only from
  receipts (`docs/collaborative-files-product.md:300-302`).

### 12.4 Read-ahead and prediction

- **In-file:** sequential reads of a partial file trigger read-ahead. The
  window doubles up to 64 MiB (at least 2×BDP) and resets on a seek.
- **Working sets:** the daemon records which files are opened together
  (a co-access graph per room, bounded, pruned by age). Opening any member of a
  known working set predicts the rest. Working sets are shared **privately
  between the same person's devices**, never with collaborators. They are what
  makes "continue on the desktop what you did on the laptop" instant. The
  "recently used on another device" hint is the `ReplicationIntent` seam
  (`docs/device-mesh.md`).
- **Directory locality:** opening a file hydrates the directory's small
  siblings. This is today's rule with the limits raised (`smart.rs:19-23` caps
  it at 32 MiB files, 2 concurrent, 128 MiB queued).
- **Gates:** predictive and backfill work respects the existing network,
  battery and disk gates (`smart_monitor.rs`). Foreground and pinned work does
  not.

### 12.5 Time to first byte

The first read of a placeholder range costs:

- one pre-content event (Tier 1) or open (Tier 2);
- one round trip to the best provider, over an already-open connection;
- the needed 16 KiB BAO groups, not the whole chunk.

The target is in [§19](#19-migration-stages).

---

## 13. The Record Store API

The point is that the collaborative, versioned path is the *shortest* path.
[R5, R8]

### 13.1 Shape

```rust
use tilo_app::prelude::*;

#[derive(Record, Clone)]
#[record(kind = "weather.place", version = 1)]
pub struct Place {
    #[key]  pub id: Id,            // minted by the SDK at authoring time
    #[text] pub name: String,      // collaborative text: diff-and-splice, never replace
    pub lat: f64,
    pub lon: f64,
}

#[derive(Record)]
#[record(kind = "photos.photo", version = 1)]
pub struct Photo {
    #[key]   pub id: Id,
    pub image: BlobRef,            // bytes live in the content store
    #[index] pub captured_at: Option<Timestamp>,
    pub tags: Set<String>,         // add-wins or remove-wins declared, not hand-written
}

fn create(ctx: HostContext, target: LaunchTarget) -> Result<AppComponent, LaunchError> {
    let places:  Collection<Place> = ctx.collection("places")?;     // room-scoped, shared, versioned
    let library: Collection<Photo> = ctx.collection("library")?;    // 10^6 items: sharded, lazy
    let prefs:   Settings<Prefs>   = ctx.settings(Scope::Person)?;  // follows the person
    let doc: RecordHandle<RichText> = match target {
        LaunchTarget::Record { checkout, .. } => ctx.open(&checkout)?,
        LaunchTarget::New => ctx.create::<RichText>()?,
    };
    // …
}

places.dispatch(PlaceAction::Rename { id, name: "Ghent".into() });            // the action door
let page = library.query().order_by(Photo::captured_at).desc().page(cursor, 200);
let img  = ctx.blobs().put_stream(reader).await?;                              // chunked, deduped
let head = ctx.blobs().open(&photo.image)?.range(0..64 * 1024).await?;         // lazy hydration
let fd   = ctx.blobs().open(&photo.image)?.fd()?;                              // native fd, mmap-able

let file: RecordHandle<FileBody> = ctx.open_file(&record_id)?;                 // stable id, not a path
let f    = file.fd(Access::ReadWrite)?;                                         // native I/O
file.history().versions();                                                      // same HistorySource
```

- **`#[derive(Record)]`** generates:
  - the projection, validation and migration hooks;
  - remove-wins deletion (29 apps hand-write tombstone maps today, e.g.
    `apps/weather/src/doc.rs:9-11`);
  - conflict surfacing;
  - the `HistoryRestoreSchema` and a default preview.

  Apps override the preview only where a visual diff matters. Today an app
  hand-writes 300–3,400 lines of `doc.rs` and up to 480 lines of history
  preview per app.
- **Actions stay the mutation door.** `Apply::apply(&self, tx: &mut Tx<T>)` is
  typed, not a raw `AutoCommit`. The executor keeps writing `ActionContext` into
  the change message. `ActionSpec`s and resource trees can be derived from
  `#[action]` attributes. Undo remains `UndoLog` over Automerge heads.
- **Transport is invisible to the app.** An app never sees routes, frontiers,
  snapshots or size limits.

### 13.2 CRDT documents

- There is one Automerge document per document record, of any size, stored as
  segments ([§8.4](#84-crdt-record-bodies)).
- There is no soft limit the app has to know about. A document that grows past
  a threshold (Q11: suggested 64 MiB saved) is flagged in developer tooling as
  a candidate for a collection. It keeps working.

### 13.3 Collections

- **Structure.** A `Collection<T>` is a record with:
  - a small **index document**: key → shard, plus declared `#[index]` fields;
  - **shard documents** holding the items. Keyed sets use hash buckets. Logs
    such as chat and activity use time segments; sealed segments become
    immutable snapshot blobs that are cheap to serve and to cache.
- **Lazy.** Shards hydrate on demand, under the same residency and
  prefetch policy as files.
- **Queries.** Sort, filter and page come from a **derived, rebuildable local
  index** maintained by the daemon. The app never walks a CRDT to paginate.
  This replaces O(n) per-frame projections such as
  `PhotosDoc::library(doc)` (`apps/photos/src/doc.rs:272-274`).
- **Promotion.** One item can become its own record when it needs its own
  sharing ("share this one photo"). The collection keeps a reference.
- **Transactions.** Operations inside one collection, or across records the
  app holds, commit atomically. They reuse the multi-record transaction journal
  (`record_transaction.rs`, `transaction_coordinator.rs`).

### 13.4 Blobs

- `ctx.blobs()` is the content store behind a capability.
- `put_stream` streams, chunks, dedups and resumes. `open` returns a ranged
  async reader, or a native read-only **fd** passed over the socket
  (`SCM_RIGHTS`). mmap then works at native speed.
- A `BlobRef {content id, len, mime}` is an ordinary CRDT scalar. Referencing
  it from a record makes it reachable, which means synced, readable by whoever
  can read the record, and a GC root for as long as history retains that
  reference.
- This replaces the process-local qdoc store and its `set_store(FsBlobStore…)`
  calls (`apps/photos/src/main.rs:90-97`, `apps/chat/src/main.rs:78-83`).

**What is built today: a record's content travels on the record's route.**
A record holds a reference to its binaries — a picture's BLAKE3 hash — never
the bytes. The bytes travel beside the record, on the same route
`(app, room, record)` its changes take, through three verbs of the
collaboration channel (`runtime-collab`):

| Verb | Meaning |
|---|---|
| `offerBlob(hash, bytes)` | An app stored this content for the record: keep it where the record's readers are served from. |
| `wantBlob(hash)` | The record references this content and the app lacks it: hand it over from this device, or ask the route's other devices. |
| `subscribeBlobs` | A stream of `(hash, bytes)` for the record's scope — answers to `wantBlob`, and what another local app offered. |

- *App side.* The framework wraps the content store the host lends a window
  (`ui_core::collaboration::record_content::RecordContent`): `put` also offers
  the bytes on the window's route, a read of a missing hash asks for it and
  reads as `ContentState::Pending` until it is here, and arriving bytes are
  verified and kept. Apps reach it as `cx.content()`. A store an app opens
  itself (`ContentStore::for_app()`) would bypass the route, so only the
  hosts that lend the store call it (`tools/policy/check_app_content_store.py`).
- *Host side.* Every host that runs the broker implements the verbs with one
  service, `tilo_action_broker::record_content::RouteContent`, over two
  things the host supplies: its *shelf* — the device content store, read and
  written through the record's scope `(room, app)`, the same partition the
  app's own capability uses — and its *peers* — the record's route on the
  network. The desktop session's shelf is the filesystem daemon's store
  (`shell/record_content.rs`); the standalone runtime's is its profile's
  store (`standalone/joined_content.rs`).
- *Network.* A device asks the room's topic (`NeighbourMsg::RecordContentWant`,
  which the room's hosts fan out with presence); a device of the room keeping
  the content answers that device alone in pieces of at most 192 KiB
  (`NeighbourMsg::RecordContentPiece`): over the direct plane when it is a
  live peer, else through the room's relay (`Envelope::RecordContent`, routed
  to the addressed device's connection only — never stored, never fanned out,
  dropped when it cannot be delivered), else on the topic.
- *The rules.* Possessing a hash never grants access: a device answers only a
  peer the room admits to read, and only from content kept under that
  record's scope. Bytes are verified, never trusted: the assembled content
  must hash to what was asked before it is kept or handed to an app. Nothing
  arrives unasked: pieces for content this device did not ask for on that
  route are dropped. Each message names its sender, bound to the
  authenticated transport before it is accepted.
- *Limits.* A request that collects no answer is asked again after a minute.
  One reference names at most 64 MiB; bigger content belongs in a file
  record. Content is kept per room and app, not per record, until the
  reverse index from content to referencing records (§14) replaces the
  per-room reachability tags. This is the only way content moves between
  devices: the filesystem daemon serves what this device keeps and fetches
  nothing itself.

### 13.5 Files and format-aware files

- `ctx.open_file(id)` gives a `RecordHandle<FileBody>` for a file record in the
  app's room, or one granted through the portal. It exposes metadata, history,
  lease state and an **fd to the real file**. A sandboxed app gets native I/O
  on exactly the files it is allowed, without binding the whole workspace.
  This replaces the RPC-per-read design of `runtime-fs`
  (`crates/runtime/runtime-fs/schema/fs.capnp`), which has no server anyway.
- **Format-aware files** are file records with a CRDT side-body registered by
  a format provider (`crates/doc/format-host`). The CRDT is the source of
  truth, and the file bytes are its projection.
  - An app edits the CRDT, and the daemon writes the bytes, debounced, as a
    normal version.
  - A foreign tool that writes the bytes is imported by structural diff into
    the CRDT, never by replacing it.
  - Word-processor's history then survives export, and "`fs` understands both
    native resources and `.docx`" (AGENTS.md) becomes true.

### 13.6 Settings

`Settings<T>` has three scopes, all stored as CRDT records in the same Record
Store:

- **Person:** a record in the personal room, synced to the person's devices.
- **Device:** a local record, never synced.
- **PersonInRoom:** a record in the personal room, keyed by room.

They are versioned lightly and never visible to collaborators. `Settings<T>`
is the typed accessor that `docs/architecture/distribution.md` §6 calls for
("An application declares its settings schema … and gets a typed accessor").
How the existing `settings-store` desktop file and the Profile layers fold in
is Q12.

**What exists today.** `ui::storage::Settings<T>` is that accessor over the
desktop settings document. A `SettingsSchema` type declares its `Scope`:

- `Personal` lives in the `app_personal` section under the app id. The
  continuity registry (`settings_store::continuity`) classifies that entry as
  personal, so the device fabric carries the whole app object to the person's
  other devices (most recent write wins, at most 64 KiB serialized).
- `DeviceLocal` lives in `app_device_local` under the app id and never
  leaves the device.
- `Shared(name)` reads and writes a per-user section another owner declares
  (`locale`, `text_assistance`); each of its keys keeps the scope the registry
  reviewed for it.

Framework views and surfaces get settings from their context
(`cx.settings::<T>()`), kept current when another window or device writes
them, and the app's files and secrets from the same place. A secret that
belongs to a document item (a profile's password, a connection's passphrase)
is `SecretStore::item(purpose, id)`: the document names the item, the secret
stays in the device's store. PersonInRoom has no implementation yet. State of
the person's own that is data rather than a preference — it grows, has
history, is undone — is not a setting but a person record
([§13.9](#139-person-records)).

**The records are the source.** Every reader — the compositor, the shell,
apps — reads one document through `settings_store::load`, which a host can
back with a `SettingsSource`. At startup the shell installs
`settings_store::SessionSettings`: the person's record (the leaves the
continuity registry classifies as personal) laid over the device's record
(every other leaf), stored beside the settings file in `records/`. A write
lands in the record its leaf's scope names. The device fabric publishes the
person's leaves and writes the winners it receives back through the same
source. `settings.json` is a copy the shell rewrites after every change, for
the processes that read the document without the source: the compositor's
boot path, apps, the Settings app and anything that runs before the session.
Those processes still merge sections into the copy; every writer holds the
copy's lock across its read, merge and replace, and the session takes in each
leaf that differs from the copy it last wrote. A copy written while no session
ran (first boot) is taken in when the session opens. Replacing the fabric's
settings channel with the person record's own sync is still to come.

**Sandboxed apps reach the records through their channel.** A sandboxed app
has no settings document: its sandbox holds neither the records nor the copy
(only Settings, which owns the document, is given the copy). The collaboration
channel the launcher hands every app carries an `AppSettings` capability
instead (`ChannelHandle.settings`), bound to the channel's certified app. At
launch the toolkit installs it as the app process's `SettingsSource`, so
`Settings<T>`, `cx.settings::<T>()` and the toolkit's own readers go through
it. The shell answers from its records with the app's view
(`settings_store::app_scope`): the app's own `app_personal` and
`app_device_local` entries, and the shared sections every app's toolkit reads
(`locale`, `text_assistance`, `accessibility`, `appearance`). The app writes
only its own two entries; a write naming another app's entry or any other
section is refused, in the app and again in the shell. A write shows in the
app at once, lands in the record its scope names (so a personal one roams),
and is copied to `settings.json`; a change to the app's view, from another
window or another of the person's devices, reaches the app within a fraction
of a second and wakes it, so a view following a `Setting<T>` repaints.

### 13.7 Foreign apps

Flatpaks, legacy binaries and Wine keep a private per-`(room, package)`
directory at `/run/tilo/app`, with the same launcher bind and the same room
confinement (`crates/packaging/launcher/src/appdir.rs:12-26`). The directory
now lives on the store volume (`live/apps/<room>/<pkg>`). That gives it:

- device-wide dedup;
- sync across the **person's own devices**, with leases for their SQLite
  files;
- history;
- quota-driven eviction.

It is not shared with room collaborators by default: foreign app internals
are single-writer. Whether an app can opt into sharing is Q13. [R8]

### 13.8 Optional verified actions

- A record kind can declare `verify = "actions"`. Peers then ship
  `ActionEnvelope`s alongside the change. Receivers replay each action under
  the pinned actor, check that the resulting change hash matches, and enforce
  the action's declared class, such as Comment versus Write.
- This uses the determinism contract that exists today but is unused in
  production (`crates/collab/actions/src/executor.rs:1-15`).
- Structural separation remains the default enforcement ([§14](#14-permissions)).
  Verification is for the rare record that needs per-action authority inside
  one document.

---

### 13.9 Person records

Some of an app's state is the person's own and is still data, not a
preference: what they watched, the pages they visited, the words they taught
the keyboard, where they stopped reading, their signature. It grows without a
useful bound, it has history, and an edit to it is undone like any other.
Settings are the wrong home for it — a personal section roams as one value of
at most 64 KiB, last writer wins, with no history — and a shared room is the
wrong home too: a collaborator must never see what someone watched. It is a
record, in the **person's own room**.

**The person's own room is a room with no roster.** Its id is derived from
the person's root public key (`collab_types::person_room`, a version-8 uuid
of a domain-separated SHA-256). Every device of the person derives the same
id without asking anyone, and the room's members are, by that derivation,
exactly the devices the root certified. Nobody can be invited into it,
because there is no list to add them to. This is the "your own room is not
special" of `rooms.md` taken literally: the same kind of room, whose one
member is the person — with membership by derivation instead of by list, so
that sharing it is not an operation that exists.

**Each point that admits room traffic enforces it.**

- *Broker.* A join naming the person room is admitted, as owner — a foreign
  device, on an operating system Tilo does not control, with the
  foreign-device cap's role, as in any room — only from a device certified by
  the root the room is derived from
  (`admit_verified_join_at`); a join naming anyone else's person room fails
  like a join to a room the session does not host.
- *Network.* A document of the local person's room is never published on a
  room topic, to a shared room's workspace host or to a collaborator. It
  travels over the personal fabric to the person's other devices on their
  authenticated same-root routes (`PersonalFabricPayload::Document`), and to
  the person's own hosts on their continuity topic (host sessions only, never
  gossip). An incoming one is accepted only from such a route, or kept by such
  a host, signed by a device of the same root. A person-room document
  arriving any other way is dropped, presence on it is never sent,
  and the room can never become a workspace: an invite, an enrollment or a
  continuation naming it is refused (`is_local_person_room`).
- *History.* An artifact's scrollback is bound to the room its ops were
  validated in, durably beside its log, and answered only on that room's own
  topic — never for the person room, also for history stored by an earlier
  session.

**A person record is an ordinary record, opened beside the one a window
shows.** One record per app and resource kind
(`RecordId::derived(kind, room_default)`), the same on every device, created
with the resource's genesis (`Resource::initialize`), so two devices that
create it before they meet converge on one record. Its storage is one
directory per app on the device, `<data home>/tilo-person/<package>`, which
the launcher binds into the app's launches in every room at
`/run/tilo/person` (`TILO_PERSON_DIR`): the same app open in two rooms reads
one record. When another window of the app already holds the stored record,
a window opens a replica that stores nothing on the very same route: it
converges with that window, and what the person's other devices or host send
there reaches it as well. A record larger than one frame travels as the
1 MiB segments its app signs, which receivers reassemble. Without a root
identity there is no person room; the records are kept on the device in a
scope no route can name.

**A device acting for several people routes each one's room to that
person.** Besides the root that certified it, a device can carry workspace
identities — other roots that certified the same device key for a workspace
of theirs. Each has its own person room: the device joins each root's
continuity topic presenting that root's certificate, sends a record of a
root's room only to that root's other devices (and hosts) on that topic,
and takes in a record only into the person room of the root that certified
its sender. A workspace identity's route carries its person records and
nothing else of the device fabric.

**The framework opens it.** A view or surface asks for the person's record of
a resource type — `cx.person::<WatchHistory>()` — and gets a
`PersonRecord<P>`: its state, kept current when another window or device
changes it, and `apply` / `invoke` of the resource's own actions through the
replayable executor, attributed to the person, written into the record's
history, an undo step of theirs for content (`undo` / `redo`), none for
housekeeping or content from outside. The runtime syncs it as it syncs every
record a window keeps: the root reports it from `Widget::record_owners`, and
the record's route — `(app, person room, record)` — is joined, published to
and merged from like any other, in whatever room the window is.

**Devices that were apart catch up.** Each device publishes the record whole
when it first opens it. A device that receives a whole record and is still
ahead of it answers with its own, so two devices that meet converge in one
round trip. When one of the person's devices reappears on an authenticated
route, its peers ask their local windows holding person records to publish
them whole, so what it missed while away reaches it. The person's always-on hosts
(their home server or hosted relay) keep every person-record change too: a
device hands it to its host sessions on the person's continuity topic — a
workspace whose only member on the host is the person — and never to gossip;
a device that comes online when none of the others is gets what the host
kept, and takes it in only from a device the same root certified.

**A person record's content follows the record.** What a person record
references — the poster of something they watched — lives under the person's
room, whichever room the window that stored it shows. The record names its
own content store (`PersonRecord::content`), kept by the host under the
scope `(person room, app)` and holding the bytes in the app only while it
runs; the window sends its requests on the person record's route, never its
own. Between devices the request and its pieces travel on the personal
fabric (`PersonalFabricPayload::RecordContentWant` / `RecordContentPiece`)
to the person's other devices on their authenticated same-root routes: never
on a room topic, never through a workspace host.

**Known limits.**

- Only the person's own hosts keep person records: a workspace host of a
  shared room never sees them, so a person without an always-on host of their
  own still needs two of their devices online at once to catch up.
- A person record larger than the collaboration transport's limit (64 MiB in
  one mutation) is refused like any record's; one that grows toward it
  should become a collection.

## 14. Permissions

One model for files, CRDT records, collections, blobs and packages. [R6, R8]

- **Principals.** Person roots, room roles (Viewer, Commenter, Editor, Owner),
  share grants and device certificates. Authority is granted to the person,
  and history records the device (`docs/collaborative-files-product.md:306-307`).
- **Units.** An ACL attaches to a record or a collection. It is inherited
  from the room catalog unless overridden. Roles map to operation classes per
  collection. A `comments` collection lets Commenters write while the document
  itself does not.
  - This is enforceable on receipt without inspecting CRDT content, because the
    unit of authorisation is the record or shard.
  - It fixes the defect that every app delta is signed as `OpClass::Splice`,
    so a Commenter can write nothing and an Editor anything
    (`crates/sdk/ui-core/src/collaboration.rs:3682-3700`).
- **Enforcement on receipt.** Every operation is checked against the record
  ACL at the time of its causal context, before merge. This is today's fs rule
  (`docs/collaborative-files-product.md:325-333`), extended to CRDT records.
- **Sandboxed apps.** The launcher mints a capability per
  `(package, room, declared record kinds)`. The Record Store answers only for
  records of those kinds in that room, plus records explicitly granted through
  the portal. fds returned to an app carry exactly the granted access mode.
  POSIX access to room trees keeps going through the launcher's binds and
  Landlock (`docs/architecture/security.md:129-141`).
- **Blob reachability.** Content is served to a peer, or opened for an app,
  only when it is reachable from a record the requester may read. The check is
  indexed ([§9](#9-transfer-engine)). No ambient "anyone with the hash".
- **Local native tree.** A device materialises only records its person may
  read. Read-only records are materialised with write bits cleared. On
  Tier 1, write opens are denied by a permission event, which keeps today's
  kernel-enforced write denial for View shares
  (`docs/collaborative-files-product.md:43-49`).
  - Within one person's session, local processes that are not sandboxed see
    that person's tree, which is the same as the shipped default policy
    ("grants the local session read+write on the whole workspace",
    `docs/architecture/security.md:141`).
  - `security.md` must state this explicitly when Tier 1 lands.
- **Wire privacy.** Record ids on the wire are minted ids, never paths. This
  fixes `RecordId::for_file` leaking absolute paths in
  `NeighbourMsg::AppDocument` (`ui-core/src/record.rs:55-73`).

---

## 15. History

One `HistorySource` over every record kind. [R5, R6]

| Kind | Versions come from | Restore |
|---|---|---|
| File | changeset entries (signed versions) | appends a new version with the old content id (existing `checked_restore.rs`) |
| CRDT document | Automerge heads plus `ActionContext` messages | appends inverse actions or a restore change (existing history mode) |
| Collection | roll-up of shard histories, keyed by item | per item or per group |
| Room | catalog and governance operations | as today |

- **Timeline.** The room timeline shows roll-ups ("Ada changed 140 files in
  `src/`", "Maren made 12 changes to Chapter 3") that expand into record
  history. That is the nesting in `rooms.md`, "History nests", and it feeds the
  existing Glance / Catch up / Investigate surfaces
  (`docs/history-experience.md`).
- **One store.** History moves from the global JSON file
  (`history.rs:615-634`, O(N) rewrite per edit) into `meta.redb`, keyed by
  `(record, version)`.
- **Retention.** Per room, with a person override:
  - keep every version for 24 h;
  - then hourly for 7 days;
  - daily for 90 days;
  - weekly after that;
  - all within a history quota.

  Named versions, restore points and versions referenced by an open conflict
  are kept until released. Thinning is itself signed as a checkpoint, so every
  peer agrees on what was dropped. `docs/history-experience.md:135-136`
  promises that attribution, counts and restore horizons survive compaction.
- **CRDT compaction.** An old change graph is compacted into a signed
  snapshot checkpoint. Changes before the retention horizon may be dropped
  from local storage once a checkpoint covers them and a host holds them (Q11).
- **GC roots.**
  - current heads;
  - retained versions;
  - pins;
  - `BlobRef`s reachable from any retained CRDT head;
  - installed and running packages;
  - in-flight transfers;
  - open files.

  GC uses owned tags and the exact live-set mechanism the fs already has
  (`store.rs:147-198, 338-412`). qdoc's "never reclaim automatically"
  (`blobstore.rs:101-105`) goes away.

---

## 16. Installed apps

[R6, R7]

- **Package payload files are content in the one store.**
  - Each file is a content id: a whole-file hash for ≤ 64 KiB, a manifest root
    above that. The payload `Collection` maps path → `(content id, mode)`. The
    format is v2; the current `Collection` maps path → whole-file BLAKE3
    (`crates/packaging/tilo-app-package/src/blobs.rs:147-259`).
  - The signed root still commits to the manifest hash, payload root and
    publisher key (`docs/architecture/distribution.md:516`).
- **`.tapp` stays small: it is thin by default.** A thin `.tapp` contains
  only:
  - the 173-byte signed root;
  - the manifest;
  - the payload collection.

  That is typically a few KiB. The payload bytes come from wherever they are
  available: a peer in the room, the LAN, a mirror, the source, since "bytes
  come from anywhere" (`distribution.md`, §3). Only the chunks missing from the
  device's store are fetched. A **fat** `.tapp`, today's tar with `blobs/`
  (`app-host/src/install.rs:6-20`), remains an export format for offline
  transfer. Both verify identically.
- **Small updates.** A new version's manifests share chunks with the old one,
  so an update fetches only changed chunks. The shared UI runtime
  (`crates/runtime/runtime-bundle`, `runtime-dylib`) keeps apps around 2 MB,
  and it is deduplicated across every app that uses it.
- **Materialisation, stage A: reflink.** The payload tree is materialised
  once under `pkgs/<package-id>/`, as reflinks of existing content wherever it
  already exists (another version, another package). It is read-only, bound
  into the sandbox as today, and runs natively. There is no second copy: this
  replaces `app-host`'s blob store plus eager `payloads/` copy
  (`app-host/src/store.rs:1-12`).
- **Materialisation, stage B (optional): composefs.**
  - An EROFS image references payload objects that have fs-verity enabled, as
    the OS root already does. That adds run-time integrity (verified on every
    page read) and O(1) mounting.
  - Lazy start for very large bundles can use placeholders, the same mechanism
    as workspace files.
  - Which stage becomes the default is Q14.
- **Records, not a separate registry.** "My added apps" is a Person-scoped
  settings record, so it syncs across devices. Residency is per device. Unused
  packages are evicted by the same quota engine ("unused copies are evicted;
  keeping one is opt-in", AGENTS.md). `app-host/src/registry.rs` becomes that
  record plus the residency table.
- `crates/packaging/app-fs`, the lazy FUSE package mount with no dependents,
  is deleted.

---

## 17. The git test

Git is the success indicator because it stresses everything at once. The
table shows what git needs, which general mechanism provides it, and how
stage exit criteria test it. There is no git code path anywhere. [R9]

| Git needs | General mechanism | Section | Verified by |
|---|---|---|---|
| Executable hooks and scripts, `core.fileMode` | Mode bits materialised and replicated | 6.6 | `git status` clean after sync; hook executes |
| Symlinks in the working tree | Native symlinks, target replicated verbatim | 6.6 | round-trip test tree with relative and absolute links |
| No "dubious ownership" | Files owned by the local person | 6.2 | `git status` without `safe.directory` |
| `index.lock`, `refs/…lock` via `O_CREAT\|O_EXCL`, then `rename` | Native create and rename locally. Cross-device, lock files are ordinary files inside crash-consistent epochs (a peer sees what a crash would leave), and epochs avoid capturing fresh open files | 6.4, 10 | concurrent `git commit` on one device under capture |
| `fsync` is real | Native fsync; publishing is asynchronous and cannot lie | 6.6 | fault-injection: capture errors never fail `fsync` |
| mmap of packfiles and the index | Native mmap. Placeholders hydrate before the fault (K2) or on open | 6.1, 6.8 | `git log -p` on a cold clone |
| Very many small files: `status` over 100k entries, `checkout` | Native `stat`/`open`/`readdir`, inline ≤ 4 KiB, eager small-file residency, one signature per changeset | 6.1, 7.3, 8.3, 12.1 | `git status` ≤ 1.1× native (stage 4 exit) |
| A ref is never visible before its objects | V2 crash-image epochs plus V3 journal-order application | 10 | 10k randomised commit/sync trials with a verifying reader on the peer: zero missing-object errors |
| Concurrent commits on two devices | Refs and index are replaced by rename, so they get versioned conflicts. The losing ref is materialised as `refs/heads/<b>.sync-conflict-<x>`, which git lists as a branch. Objects are write-once, so their union is always consistent | 11.1 | partition test: both commits reachable after reconnect |
| Packfile rewrite (`gc`, `repack`) racing a peer | New packs are new files. Old packs are deleted in the same changeset, after the new ones (V3). An open fd on the peer keeps the old inode | 6.5, 10 | `git gc` on A while B runs `git log` |
| `index` stat cache across devices | Replicated `mtime` (ns) and size, and the same uid. Inode and `ctime` cannot be replicated, so see Q4 | 6.5 | `git status` on B after A's commit, then again on A: no ping-pong rewrites |
| Throughput of clone and checkout (small objects) | Batched RBSR metadata, inline bytes, GetMany batches, one connection | 9 | 100k × 4 KiB sync target (stage 2) |
| SQLite-like single writers (not git, same tests) | Leases with fencing | 11.2 | two devices open one SQLite database: one writer, zero corruption |

**The index stat cache, stated precisely (Q4).** Git's index caches `stat`
data for each working-tree file. With git's defaults it compares `ctime`
(`core.trustctime=true`) and, where the platform provides it, the inode
number. Neither can be replicated. Unless something changes, the result is:

1. After a sync, git on the receiving device sees every file as modified. It
   re-hashes the whole tree once and writes a refreshed index.
2. That index then syncs back, and the other device does the same.

The cause is general, since many tools compare inode or ctime. The Tilo fix
must be general too. Options:

- (a) ship `core.trustctime=false` and `core.checkStat=minimal` as system git
  defaults on Tilo OS. This is configuration, not a code path;
- (b) mark files whose content is a per-device cache (by an app declaration or
  user policy) as LocalOnly;
- (c) accept one re-hash per sync and rely on the refreshed index converging.
  With (a) in place it converges immediately, because `mtime` and size match.

The recommendation is (a). It needs the owner's decision because it touches
the "no special-casing" rule.

**Decided and shipped: (a).** `/etc/gitconfig` on Tilo OS includes
`/usr/share/tilo/git/synced-worktree.gitconfig` (`core.trustctime=false`,
`core.checkStat=minimal`). For it to work, a synced file's mtime must match
its author's: the native data plane publishes the file's own mtime as the
version's signed `observed_at` (never earlier than the record's previous
one), and peers materialise it. With both, `git status` on the receiving
device is clean and does not rewrite the index (tested,
`crates/system/fs/tests/native_tier1.rs`).

---

## 18. Keep, merge, delete

[R6]

### Keep (and evolve in place)

| Where | What | Change |
|---|---|---|
| `crates/system/fs` `record_replication.rs`, `record_publisher.rs`, `record_access.rs`, `acl.rs`, `workspace_authority.rs` | signed operations, authority | changeset signatures (§8.3); storage in redb; verify once |
| `fs` `record_protocol.rs`, `share_*.rs`, `named_share_delivery.rs`, `recipient_control.rs`, `share_link.rs` | sharing | unchanged semantics; delivery rides the transfer engine |
| `fs` `identity.rs`, `peer_auth.rs`, `authenticated_transport.rs`, `transport_gate.rs` | device authentication | once per connection |
| `fs` `replica_ack.rs`, `availability.rs`, `eviction.rs` | receipts, availability, safe eviction | become the automatic quota engine (§12.2) |
| `fs` `record_transaction.rs`, `transaction_coordinator.rs` | multi-record atomicity | also carries changesets and Record Store transactions |
| `fs` `conflict.rs`, `conflict_merge.rs`, `checked_restore.rs`, `history_read.rs` | conflicts, merge, restore | policy by body type (§11) |
| `fs` `rich_metadata.rs`, `thumbs.rs`, `smart.rs`, `smart_monitor.rs`, `clock.rs`, `caps_file.rs` | metadata, thumbnails, prediction gates, clock, live ACL file | limits raised; working sets added |
| `fs` `durable_backup.rs`, `tilo_files::native::atomic` | crash-safe small files | only for security stores |
| `crates/collab/collab-types` | `SignedOp`, validator, roles, MLS | `MAX_DOCUMENT_MUTATION_BYTES` stops governing durable state |
| `crates/system/fs-protocol` | `FileBindingV1`, file records and transactions, share and recipient control | unchanged |
| `crates/collab/actions`, `channel-crypto`, `tilo-user-identity` | actions, crypto, identity | unchanged |
| `crates/sdk/ui-core` `state/` (`UndoLog`, `DocumentFrontier`), `history_source/`, `history_mode/`, `action_host.rs` | undo, history UI, action door | a `HistorySource` over all kinds |
| `ui-core` `application.rs` (`ApplicationDefinition`, `RecordOwners`) | the cleanest custody model | becomes the Record Store client and the only launch path |
| `crates/packaging/tilo-app-package` | signing, manifest, trust, `Collection` | `Collection` v2 (content ids and modes), thin `.tapp` |
| `crates/packaging/launcher` | sandbox, binds, per-room app dir | source directory moves to the store volume |
| `crates/runtime/runtime-fs/src/native.rs` | cap-std granted host directories | unchanged |
| `crates/hosting/tilo-relay` | host, device mesh policy, `ReplicationIntent` | stores content and record operations as a normal replica; push target |

### Merge

| From | Into |
|---|---|
| `fs` `store.rs` (`ContentStore`), qdoc `blobstore.rs` (`BlobStore`, `FsBlobStore`), `app-host/src/store.rs` (`FsBlobStore`, `payloads/`) | **one content store** (`ChunkStore` + loose `FsStore`) |
| `fs` `content.rs` (gear CDC) | FastCDC tiers; v1 kept as a reader |
| `fs` `hydration.rs`, `full_replica.rs`, blob parts of `sync.rs` | the transfer engine (§9) |
| `fs` `record_mesh.rs` and namespace sync in `sync.rs` (Automerge sync, gossip deltas, 3 s full fetch) | RBSR reconciliation plus live operation gossip |
| `fs` `namespace.rs` (Automerge move-tree) plus `record_materializer.rs` | tree projection from signed placements plus redb indexes |
| `fs` `history.rs` (global JSON) | per-record history in redb |
| `ui-core` `persist.rs` (package document), legacy `RecordSet` in `record.rs`, canonical `RecordOwners`, `crates/system/app-state` (`Autosave`, `DocStore`) | **one Record Store client**. `app-state`'s legacy claim markers (`app-state/src/lib.rs:55-58, 436-491`) carry old data across |
| `ui::RecordId` (path, uuid, tab, derived) and `FileRecordId` | one minted `RecordId`. Paths become locator attributes |
| per-app room index (`persist::open_room`), shell `room_records.rs` index and sandbox crawl | the room catalog record |
| Automerge change-graph history and fs signed versions | one `HistorySource` (§15) |
| the two eviction models (namespace hydration authority, record plane) | one quota engine |
| `settings-store` desktop file, ad-hoc app settings in room documents | `Settings<T>` (Q12) |
| `fs` `fs.rs`, `main.rs` FUSE daemon | Tier 2/3 projector (passthrough, POSIX-complete) |
| `app-host/src/registry.rs` | Person-scoped "added apps" record plus residency |

### Delete

| What | Why |
|---|---|
| `fs` `docstore.rs` | unused. CRDT segments replace it |
| qdoc documents stored *inside* the namespace document (`fs.rs:4643-4655`, `namespace.rs:2143`) | documents are records |
| the whole-workspace Automerge namespace document and its periodic full save and validate (`main.rs:2121-2136, 2821-2835`) | O(N) per operation. Replaced by derived indexes |
| per-chunk tags `tilo/blob/v1/<hash>` (`store.rs:45`) | owned roots are per content id |
| `fs` `recipient_mount.rs` (after Tier 1) | recipients materialise natively |
| `crates/packaging/app-fs` | no dependents. Placeholders or composefs cover lazy start |
| `crates/runtime/runtime-fs/schema/fs.capnp` `WorkspaceFs` read/write RPC | no server. Replaced by the Record Store capability with fd passing |
| `tilo_doc::blobstore::BlobTransport`, the process-global store, `set_store` calls | one content store |
| package-global `state.automerge` as a content store (44 apps), `LegacyLaunchInputs`, `RecordId::for_file`/`for_tab` as identities, the 3 MiB single-message snapshot path, 29 hand-written tombstone maps, inline assets in notes | wrong grain or duplicated |
| `tilo-relay` `snapshot.automerge` / `pending/*.delta` opaque co-edit store (`tilo-relay/src/store.rs:1-20`) for app documents | hosts store record operations and segments like any replica. Live presence stays |
| the hard caps at `record_repository.rs:24-31`, `namespace.rs:35`, `full_replica.rs:23-27`, `store.rs:56-70`, `blobstore.rs:33-40`, `record.rs:38` | replaced by compaction, streaming and quotas |

---

## 19. Migration stages

Each stage ships on its own, keeps existing data readable, and has
measurable exit criteria. Numbers compare against **raw**: the same operation
on a plain directory of the same filesystem and device, with the daemon
stopped.

### 19.1 The benchmark suite (built in stage 0, run on every later stage)

| ID | Scenario | Metric |
|---|---|---|
| B1 | sequential and random read/write, file sizes 4 KiB, 64 KiB, 1 MiB, 64 MiB, 1 GiB, 10 GiB, resident | MB/s and IOPS vs raw; p99 latency |
| B2 | mmap read and write of 1 GiB, resident | throughput vs raw |
| B3 | create 100k files (4 KiB) in 1k directories; `stat` all; `readdir` all; delete all | wall time vs raw |
| B4 | SQLite: 100k single-row transactions, WAL, `synchronous=NORMAL` | tx/s vs raw; versions produced |
| B5 | sync 1 × 10 GiB, LAN (1 GbE and 10 GbE) and WAN (100 Mbit/s, 80 ms, 0.1% loss, netem) | throughput vs `iperf3` over QUIC on the same path; CPU per GB |
| B6 | sync 100k × 4 KiB, same links | wall time; effective bytes/s vs link |
| B7 | edit 4 KiB inside a 10 GiB file; sync to peer | bytes sent; capture CPU; time to peer visibility |
| B8 | git: clone a 100k-file repository into a synced room; `git status` (warm); commit; `git status` on the peer after sync; `git gc` | wall time vs raw; correctness (`git fsck`) |
| B9 | placeholder time to first byte: 64 KiB read, LAN and WAN | ms |
| B10 | causal visibility: 10k randomised multi-file commit and sync trials with a verifying reader on the peer | violations (must be 0) |
| B11 | Record Store: 1M-item collection, page query; 200 MiB document late join | ms; success |
| B12 | daemon RSS during B5 and B6 | MiB (must not scale with file size) |

### 19.2 Stages

**Stage 0: stop the bleeding, measure, decide (weeks).**

- Return `EIO` from `flush`/`fsync`/`release` when a commit fails (`fs.rs:472-481`),
  and remove the double commit on release.
- Do not advance the collaboration frontier until encoding succeeds. Surface
  oversized deltas as errors (`ui-core/src/app/collaboration.rs:163-178`,
  `collaboration.rs:3653-3661`).
- Hash record ids on the wire.
- Build the B1–B12 suite and record baselines of today's system.
- Run the kernel spike K1–K6 (§6.8).
- Exit:
  - no path returns success after a failed commit (fault-injection tests);
  - B1–B12 run in a scheduled CI job and publish numbers;
  - a written Tier 1 go/no-go decision.

**Stage 1: metadata at scale (weeks).**

- Per-record operation logs in `meta.redb`, verified once.
- History in redb.
- Tree indexes derived from signed placements, with the Automerge namespace
  kept only as a compatibility projection.
- Caps removed.
- Multi-threaded FUSE, `KEEP_CACHE` with `notify_inval_*` on remote change,
  a cached offset index for reads.
- Exit:
  - B3 per-file cost flat from 1k to 100k files, within 20% (today it grows
    linearly);
  - 1M records open in ≤ 5 s;
  - B4 completes 100k transactions with zero failed commits.

**Stage 2: transfer engine and content v2 (weeks to months).**

- Connection pool with one authentication per connection.
- Indexed authorisation with no tree lock.
- FastCDC tiers and `HashSeq` manifests (v1 still read).
- Inline ≤ 4 KiB. GetMany batching. RBSR metadata.
- Push to the home host. Changeset signatures.
- Exit:
  - B5 LAN ≥ 90% of the QUIC `iperf3` baseline on 1 GbE and ≥ 60% on 10 GbE;
  - B5 WAN ≥ 80%;
  - B6 LAN in ≤ 3× the time the link needs for the raw bytes, and B6 WAN in
    ≤ 1.5×;
  - B7 sends ≤ 2 chunks;
  - B12 flat.

**Stage 3: native data path, Tier 2, plus POSIX and consistency (months).**

- Real backing files, `FOPEN_PASSTHROUGH`, and POSIX completeness (§6.6).
- The capture journal. Epochs with snapshot or reflink. Incremental
  rechunking.
- V1–V3 application.
- Automatic residency and quota eviction.
- Exit:
  - B1/B2 resident data ≥ 95% of raw at every size;
  - B3 ≤ 2× raw (metadata still crosses FUSE);
  - B8 correct: `git fsck` clean on both devices, conflict scenario produces
    both commits;
  - B10 zero violations;
  - capture cost (K5) p99 < 50 ms.

**Stage 4: Tier 1 on Tilo OS (months, gated on stage 0's decision).**

- Store volume created by the installer, with a migration path for existing
  installs (Q1).
- fanotify journal and pre-content placeholders. Leases enforced by
  permission events.
- Recipient mount retired.
- Exit:
  - B1–B3 ≥ 95% of raw (B3 included);
  - B8 `git status` ≤ 1.1× raw;
  - B9 ≤ 1 RTT + 10 ms on LAN and ≤ 1 RTT + 30 ms on WAN;
  - leases: two devices on one SQLite database, zero corruption across
    1,000 randomised handovers.

**Stage 5: Record Store for apps (months, can overlap stages 3–4).**

- CRDT records in the record plane with segments.
- `#[derive(Record)]`, `Collection<T>`, `Settings<T>`, `ctx.blobs()` and
  `ctx.open_file()` with fd passing.
- qdoc `BlobRef` backed by the content store.
- Port weather, totp and contacts first, then chat, photos, ebook and sheets.
- Exit:
  - B11 page query ≤ 50 ms, and a 200 MiB document late-joins;
  - ported apps lose their hand-written tombstones and restore schemas, with
    line counts reported;
  - Photos holds 100k photos;
  - a Commenter can comment and cannot edit, enforced on receipt.

**Stage 6: installed apps on the content store (weeks).**

- `Collection` v2, thin `.tapp`, reflink materialisation.
- `app-host` store and `app-fs` deleted.
- Exit:
  - a thin `.tapp` ≤ 16 KiB for every app in the tree;
  - installed payload bytes counted once (no blob-plus-unpacked copy);
  - an update transfers only changed chunks (measured on two consecutive
    releases).

**Stage 7: delete the old paths.**

- Package-document default, legacy `RecordSet`, qdoc local store, shell
  crawl, path identities, the Automerge namespace compatibility projection,
  the relay's opaque co-edit store.
- Every `LaunchSpec` launches a definition (done: the widget-root launch path is gone).
- Exit: the deleted code is gone, and the B1–B12 numbers are no worse than
  after stage 6.

---

## 20. Risks

| # | Risk | Likelihood / impact | Mitigation |
|---|---|---|---|
| 1 | Tier 1 kernel semantics (K1–K4) are missing or buggy on 7.2 | medium / high | The spike decides before any Tier 1 work. Tier 2 meets R2 for data. Every fallback is listed in §6.8 |
| 2 | Btrfs snapshot per epoch stalls writers under heavy write load (it forces writeback of the subvolume) | medium / medium | Adaptive epochs (streaming writers are not published until quiescent). K5 gate. Per-file reflink fallback |
| 3 | fanotify queue overflow or a daemon crash misses changes | medium / low | Generation-based rescan (§6.4). The filesystem is the truth |
| 4 | Security model shifts from FUSE-enforced per-identity views to materialisation plus sandbox binds | certain / medium | Stated in §14. `security.md` updated in stage 4. Read-only enforced by permission events |
| 5 | A dedicated store volume complicates install and existing machines | high / medium | Decided in §6.2: a store partition on whole-disk installs, an image-backed store everywhere else, FUSE when neither fits |
| 6 | QUIC CPU cost limits 10 GbE | medium / low | Stage 2 measures CPU per GB. Multiple connections per peer if one is CPU-bound |
| 7 | Ed25519 and Merkle proof cost at 1M records | low / medium | One signature per changeset, batch verification, verify once |
| 8 | Automerge memory for large documents | medium / medium | Collections for large data. Segments and checkpoints. The engine hidden behind `Tx<T>` |
| 9 | Lease authority unavailable (no host) | medium / medium | Optimistic unleased versions with fencing, so there is a conflict, not corruption. Files shows it |
| 10 | Residual cross-file window (§10) bites a program with an unusual update pattern | low / medium | Quiescence epochs, leases, Q8 |
| 11 | Git index ping-pong (Q4) | high without a decision / low | Q4 option (a) |
| 12 | History growth under retention | medium / medium | Thinning, quotas, reflinked versions share extents |
| 13 | Migration of existing stores and app documents | certain / medium | Pre-alpha: earlier stores and app documents are not migrated; each format has one reader. Staged, reversible cut-overs once there are users to carry |
| 14 | Scope: this is a large rebuild of the data path | certain / high | Stages ship independently. Stage 0–2 fixes land before the data path changes |

---

## 21. Alternatives considered

| Alternative | For | Against | Verdict |
|---|---|---|---|
| **Tuned FUSE only** (multithread, writeback, io_uring) | Least change | Still a round trip per operation. Fails R2 by design | Tier 3 fallback only |
| **FUSE passthrough everywhere** | Native data I/O, fuser 0.17 ready | `stat`/`open` storms (git, compilers) still cross FUSE | Tier 2; first to ship |
| **Kernel module** | Anything is possible | Out of tree, Secure Boot signing, attack surface, maintenance | Rejected |
| **overlayfs / EROFS on-demand for the mutable tree** | In-kernel lazy loading | Built for immutable images | Used for packages only (§16, stage B) |
| **Full replication, no placeholders (Syncthing model)** | Simplest, fully native | Fails R1 on small disks and for multi-GB rooms. No lazy start | The behaviour of pinned subtrees and hosts, not the default |
| **Git-aware sync (sync `.git` via the git protocol)** | Perfect git semantics | Special-casing, forbidden by R9. Does not help SQLite or anything else | Rejected |
| **LWW per file (Dropbox model)** | Simple | Corrupts `.git` and SQLite, and loses writes | Rejected. Versioned conflicts and leases instead |
| **Fixed-size chunks** | Simpler, block-aligned | An insertion shifts every later chunk, so dedup is lost | Rejected. FastCDC, with FIEMAP diff for in-place edits |
| **Keep the whole-workspace Automerge namespace, add indexes** | Keeps working code | Still one document for 10⁶ nodes. Saves and sync are O(N). Duplicates the signed placements | Rejected. Indexes over signed placements |
| **iroh-docs / Willow-style store for metadata** | Ready-made RBSR and entries | Has its own identity, ACL and namespace model. It would duplicate the signed record plane with its shares and receipts | Take RBSR, not the store |
| **Consensus (Raft) for leases and ordering** | Strong guarantees | Needs a quorum of online devices, and personal device sets are often one device | Rejected. Host authority with fencing |
| **Records as files (every CRDT a `.automerge` file in the tree)** | One plane end to end | Whole-file versioning is the wrong grain for keystroke CRDT edits | Take the plane, not the POSIX surface. Records live natively, and files are one record kind |
| **Different CRDT engine (e.g. Loro)** | Shallow snapshots, history trimming | Automerge is baked into undo, frontiers, action replay and qdoc merge | Deferred. The engine is hidden behind `Tx<T>` |
| **Server-authoritative store** | Simple permissions, easy large collections | Contradicts peer-to-peer and offline-first (`collaboration.md`) | Rejected. Hosts are replicas, not authorities, except for leases |
| **SQLite as the app data store** | Familiar | Not collaborative or versioned. Would be a second record model | Only as a derived query index (§13.3) |
| **Per-process filtered local views (keep FUSE ACL for local processes)** | Per-identity local enforcement | Costs R2 for every access. The local session is one person | Rejected. Materialise only readable records, sandbox with binds |

---

## 22. Open questions for the owner

1. **Store volume (Q1).** *Decided:* a separate LUKS2 partition on
   whole-disk installs, a loop image on the root Btrfs for alongside and
   existing installs (K6 showed mount marks do not suffice). See §6.2.
2. **Tier 1 go/no-go (Q2).** Is Tier 2 (native data, FUSE metadata) an
   acceptable end state for Tilo OS if K1 or K2 fails, or is Tier 1 a hard
   requirement that justifies carrying kernel patches?
3. **Multi-user devices (Q3).** One store per local person (recommended), or
   shared stores with per-uid materialisation?
4. **Git index stat cache (Q4).** Ship `core.trustctime=false` and
   `core.checkStat=minimal` as system defaults? This is configuration, not a
   code path, but it is tool-aware.
5. **Lease contention (Q5).** When a device opens a leased file for write:
   deny immediately, wait with a UI prompt, or fork as a conflict?
6. **Encryption on hosts (Q6).** Must content on a hosted server that other
   people operate be end-to-end encrypted? Per-room convergent keys keep dedup
   inside the room. Device-to-device and home hosts would not need it.
7. **Custom iroh-blobs backend (Q7).** Accept a Tilo-maintained `ChunkStore`
   against iroh-blobs' store protocol, possibly with an upstream contribution,
   or speak our own ALPN over `bao-tree`?
8. **Strict multi-file atomicity for POSIX (Q8).** Is the §10 residual window
   acceptable, or should subtrees be able to opt into atomic publication, for
   example by swapping a per-subtree snapshot, at extra complexity?
9. **Hardlinks (Q9).** Preserve link identity across devices (a new record
   kind), or materialise as reflinked copies (recommended)?
10. **Rebuildable trees (Q10).** Sync `node_modules`-like trees by default, or
    exclude them? `CACHEDIR.TAG` trees are excluded either way.
11. **Retention defaults (Q11).** Are 24 h / hourly 7 d / daily 90 d / weekly
    right? May a device drop CRDT history before the horizon once a host holds
    a checkpoint? What document size triggers the developer-tooling warning?
12. **Settings (Q12).** Does `Settings<T>` subsume `settings-store` and
    become the implementation of the Profile layers in `distribution.md` §6,
    or stay separate from the Profile?
13. **Foreign app data (Q13).** Is the default "synced across my devices, not
    shared with the room" right? Can a package manifest opt into room sharing?
14. **Package materialisation (Q14).** Reflink (simpler), or composefs with
    fs-verity (run-time integrity, O(1) mount) as the default?
15. **Record type ownership (Q15).** Should the system own record kinds, so
    one library view can show books, playlists and documents together, or the
    apps? This carries forward `rooms.md`, "Still open".
16. **Offline placeholder read (Q16).** `EIO` (today, honest), or block
    indefinitely with a UI prompt for interactive apps?
