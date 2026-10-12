# Source-project history qualification

`source-history-qualification` is an explicit real-directory probe of Code
Editor's native `ResourceApp` history. It is not a Git history viewer, a backup
format, or a whole-repository qualification. It launches `TestApp<CodeEditorShell>`
on the actual supplied directory; no seeded fixture replaces that directory.

## Run

Whole-source qualification is currently unsupported and **exits unsuccessfully**:

```sh
cargo run --locked -p code-editor --example source-history-qualification -- \
  --root "$PWD" --exclude .git --exclude target
```

A deliberately narrower text workload requires explicit opt-in:

```sh
cargo run --locked -p code-editor --example source-history-qualification -- \
  --root "$PWD/apps/code-editor/src" --focused-text
```

Repeat `--exclude RELATIVE/PATH` for exact paths/subtrees, not glob patterns.
Exclusions must exist, be relative to `--root`, and contain neither `.` nor `..`.
There are no implicit harness exclusions, no current-directory default and no
Git fallback. Choose a real directory, not a symlink. The importer itself skips
certain names (including VCS/build internals) and symlinks; that behavior does
**not** count as a harness exclusion. An included entry silently skipped by the
importer makes completeness fail. Avoid changing the source tree while running.

`--focused-text` qualifies only included UTF-8, NUL-free files of at most
200 KiB and their recorded file/folder paths. Included binaries, oversized files,
symlinks and special files are reported and refused, never converted to empty
text or silently omitted. Narrow the root or explicitly exclude those paths.
Inventory is read in bounded `inventory:N` pages of 128 entries, pinned to
the same native checkpoint. A page exceeding the typed-read response bound
fails; the probe does not switch to an unbounded dump. A narrow successful run must not be
reported as qualification of the repository outside its root/exclusions.
The independent audit refuses more than 64 MiB of retained text; narrow the
root or exclusions rather than increasing memory without a bound. The probe
prints elapsed hydration and total milliseconds for repeatable text measurements.

## What the probe proves

1. Independently walk the included disk tree using fallible filesystem reads.
   Record kinds, bytes, modes and text hashes. No Git index or raw CRDT dump is
   used to manufacture coverage.
2. Launch the editor on `--root`, make a native launch checkpoint, and read its
   typed inventory. Audit each recorded path, kind, id and item resource address
   against disk; report missing paths before hydration. `complete: true` with
   `scope: "recorded-project"` describes the recorded inventory, **not** disk
   importer completeness.
3. In focused mode, open every included text file through the published
   `open-file` action. This exercises the actual disk effect and lazy app tree.
   Make another checkpoint and require coverage of every included file **and
   folder**. Nested empty directories that the lazy importer never discovers
   cause failure rather than being dropped from the expected tree.
4. Read every included file at the baseline checkpoint through the native
   `read` action, selecting the inventory's item resource address. Text must
   exactly equal disk. Inventory reads contain metadata; per-item text is
   loaded on demand and remains bounded by the native adapter.
5. Change one deterministic included file through its published `replace-text`
   action (empty it, or insert a short line if already empty). Checkpoint the
   edit, read both versions, and require the native diff to name its buffer.
   Request `review-restore` for that exact buffer path and pass the returned
   review unchanged to `restore-reviewed`. No broad legacy restore or invented
   authorization is substituted. Checkpoint the result and verify all included
   texts equal their baseline.
6. Rewalk the included disk tree and require it to be unchanged, including modes.
   Require the host's native app custody audit to remain certified.

The probe edits only the ephemeral test host's recorded state. It never invokes
`save`, writes source files, or persists a history artifact. Each invocation is
independent and repeatable; checkpoint names are reused only in its fresh test
host. Excluded paths are not qualified or checked for disk changes. Exclusions
filter the audit/workload, **not** the editor importer: excluded metadata may
still appear in the native recorded inventory, and inventory-size refusal is
still a refusal.

## Typed read contract

Whole `read` returns only inventory metadata. `read({item: "inventory:0"})`
returns the first 128 entries with `total_entries` and a `next` item string or
null. Historical reads wrap this in `content` and return immutable `version` and
`heads`; use that version on subsequent pages. A source item accepts its UUID
or published `resource_id`. Its relative path and text belong to the requested
frontier, so renames do not change identity and earlier versions of deleted
entries remain addressable. Folder items carry metadata, not text. A file
listed with `text_available: false` returns null text: it has not been imported
into native history. This is not a request to hydrate its current disk bytes.

The adapter caps serialized content at 240 KiB, including JSON escaping.
Large inventories require pages; an oversized single text or page fails. The
focused workload's 200 KiB disk bound leaves some envelope headroom, but heavily
escaped text can still fail the serialization bound. Typed reads omit terminal
commands, output, private document keys and local filesystem locations. They
are not a secret detector for credentials inside explicitly requested source.

## What remains unsupported

The Project record stores text and file/folder metadata, not full binary file
contents, POSIX permission/executable modes, symlink targets, or the entirety of
an arbitrary source checkout. Comparing disk modes before/after only establishes
that this text-history exercise did not change them; it does not establish
historical mode restoration. Lazy directory import and bounded native reads
must not be marketed as a whole-source snapshot. Full binary/mode/symlink and
whole-source qualification remains unsuccessful even when a selected text
workload passes. The harness does not weaken custody, sandboxing, read limits or
restore-review freshness to make a run green.
