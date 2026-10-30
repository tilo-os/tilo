# Device-local state in synced workspaces

Everything in a workspace syncs to the workspace's other devices and
collaborators: every file, at every size, with its history. Some files should
not. A build tree, a thumbnail cache or a tool's per-machine index is derived
state: it is rebuilt from the synced files, it is only valid on the machine
that built it, and syncing it wastes bandwidth and produces conflicts nobody
can resolve. This page is for app and tool authors whose programs write such
state inside a workspace.

## Marking derived state

The storage daemon never syncs:

- **A directory containing a `CACHEDIR.TAG`** whose first 43 bytes are the
  standard signature `Signature: 8a477f597d28d172789f06886806bc55`
  ([bford.info/cachedir](https://bford.info/cachedir/)). The directory and
  everything below it stay on this device. Cargo's `target/` already carries
  one, as do many caches; writing one is the portable way to mark a tree.
- **A file or directory with the extended attribute `user.tilo.local`** (any
  value, including empty). Use it when a tag file would be in the way:

  ```sh
  setfattr -n user.tilo.local -v 1 .mytool-index
  ```

  ```rust
  // Mark before writing content, so no version of it is ever captured.
  xattr::set(&path, "user.tilo.local", b"1")?;
  ```

  Setting the attribute on a directory covers everything below it, including
  files created later.
- **Sockets, FIFOs and device nodes.** They are never content.

Marked paths are real files like any other: native speed, `mmap`, locks. They
are simply never published, never versioned and never appear on another
device. Removing the mark makes the path sync from the next capture on.

Mark derived state when you create it, not afterwards: a file captured before
it was marked has already been published once, and the mark only stops later
versions.

## Stat caches: compare what syncs

A synced file keeps its content, size, permission bits and modification time
(to the millisecond) on every device. Its inode number, device number, change
time (`ctime`) and access time are properties of the local filesystem and
differ on every device.

A tool that caches `stat` results to skip re-reading unchanged files should
compare size and mtime, and treat inode, device and ctime as hints only.
Otherwise it sees every synced file as changed on the device that did not
change it, re-reads everything, and, if it writes its cache inside the
workspace, syncs the rewritten cache back, where the same happens in reverse.

Git is the common case. Tilo OS ships these system defaults in
`/etc/gitconfig` (from `/usr/share/tilo/git/synced-worktree.gitconfig`):

```ini
[core]
	trustctime = false
	checkStat = minimal
```

A repository created on one device then shows `git status` clean on another
without re-hashing, and `git status` does not rewrite the index. Container
images used for development (toolbox, distrobox) have their own `/etc`; add
the same two lines to their system configuration or to `~/.gitconfig`.

## What syncs, and how it appears

- **Crash-consistent epochs.** Changes are published in epochs cut from an
  atomic snapshot of the workspace, so another device always sees a state this
  device's disk could have held after a power cut: a program that is
  crash-safe locally (write a temporary file, `fsync`, `rename`; or a
  journal/WAL) is safe across devices. Short-lived lock files are usually not
  captured at all.
- **Order.** Another device applies one author's changes in the order they
  were made: a file that names another (a git ref and its objects, an index
  and its data) never appears before what it names.
- **Placeholders.** A large file that arrives from another device appears with
  its exact size and mtime before its bytes do; the first read of a range
  fetches it. `stat`, `ls -l` and `find` never fetch anything. The residency
  of a file is in the `trusted.tilo.residency` extended attribute (present
  while any range is missing); reading it never fetches.
- **Files modified in place** (databases, VM images) take a write lease per
  writing session. While another device holds it, the file is read-only here:
  reads work, even through a descriptor opened read-write before the lease
  was taken; opening it for writing, and any write, gets `EBUSY`. A program
  that retries a failed read-write open as read-only (SQLite does) reads
  normally and reports a read-only database when it writes. The lease moves
  on when the other device closes the file or goes offline; a version
  written without the lease never replaces the holder's and is kept as a
  `.sync-conflict-` copy.
- **Permissions and links.** Mode bits and symbolic links replicate; owner
  and group are always the local person's. Hard links arrive as separate
  files with the same bytes.
