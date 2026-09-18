# Native8166 thin-cache recovery seam — preparation only

This implementation is specific to application source
`4583a44c4056c67cfd1ebc4f1e02c7fa4aff3ffc` and producer helper
`9d7a6b259c9b75b6c5031472bfed05b6461c1893`. The importer and optional workflow/controller path require independent admission.
No approval, dispatch or completed native payload is supplied by this document.

## Recorded evidence and limits

The current capture script records a bounded, non-paused, incomplete inventory of
regular files from `/build/target/thin-app`. It excludes observed active compiler
units, writers and temporaries, and retains PAX nanosecond mtimes and file hashes.
It explicitly states `source_mtime_restoration_not_proven=true`.

The verified source metadata records workspace and actual inner tool identities.
The independently reviewed admission must bind these inputs:

1. Workspace source files used by Cargo/build scripts: canonical relative path,
   byte SHA256, type, mode and exact nanosecond mtime. Include Cargo.toml/lock,
   `.cargo/config.toml`, thin-app scripts/wrapper and all selected workspace sources.
2. Relevant registry/git dependency sources under `/build/.cargo-home`, with their
   original canonical path, hash and mtime. Record any externally generated inputs
   identified by dep-info/build-script rerun directives. Do not collect credentials.
3. Actual nested cargo/rustc versions, compiler sysroot identity, linker executable
   hash, installed RPM inventory/native-library identity and immutable tools/builder
   image identities. The capture script's outer `rustc -vV` is insufficient alone.
4. Exact Cargo argument/feature graph and public wrapper/profile inputs. Browser
   remains enabled; the original single graph still builds runtime-dylib and apps.

The source inventory must be stable and complete enough to justify each timestamp
reset. Matching Git revision alone does not prove generated/dependency inputs.
Missing tracked-source/toolchain identity is a preflight failure. Registry dependency
timestamps were not captured and remain untouched. Cargo evaluates those units
normally; no timestamp fabrication or assumed freshness is performed.

## Small implementation boundary

Extend `native_resume.derive_recipe` with an optional independently hashed cache
admission. The default path remains byte-identical. In its existing build RUN,
add a read-only named context `tilo-thin-cache` and a separately reviewed helper.
Insert restoration immediately after the existing `cargo clean --profile thin-app`
and retained-release copy. Keep both clean commands. Do not move the source root,
Cargo home or target root from their original `/build` paths.

An operator-produced approval binds immutable checkpoint reference/config digest,
cache metadata SHA, archive SHA, source-mtime manifest SHA, inner toolchain evidence
SHA, helper SHA, source/helper revisions and original/derived recipe hashes.
The image itself stays `complete=false`, `installable=false`, `resume_admitted=false`.
Admission does not convert a cache into a native payload.

The helper validates every input before changing source timestamps or target files:

- Require exact source/toolchain/command identity and bounded regular metadata.
- Verify every source/dependency byte hash before restoring its captured mtime;
  never make a fingerprint artificially newer or touch an unverified source.
- Require archive SHA, at most 30,000 unique canonical relative paths and at most
  10 GiB total uncompressed bytes. Reject absolute/traversal names, links, special
  files, unexpected entries, setuid/setgid modes, locks and known temporary files.
  The current capture includes only regular files, so no symlink support is needed.
- Extract only into an owned empty sibling temporary directory beneath
  `/build/target`, using exclusive creates and per-file hash/size/mode checks.
  Set exact inventory nanosecond mtimes explicitly; do not rely on OCI timestamps.
- Only after full verification, replace the empty cleaned `target/thin-app` with
  that directory. Reject a nonempty destination. Fail before running Cargo if
  restoration is incomplete; do not modify the release checkpoint or source bytes.

Then run the original Cargo graph and final native inventory/ELF/runtime/app-store
checks unchanged. Cargo may rebuild units; report actual freshness from its output.
No retained fingerprint or dylib ever permits skipping this graph. Provenance must
record cache admission, actual restored file/byte count and original recipe not
executed verbatim. Existing reconstructed-native receipt hashing already binds an
extended `reconstruction` object; retain its independently approved hash and final
complete-inventory check.

## Focused acceptance tests before dispatch

Use the exact frozen Containerfile blob to prove placement after clean, unchanged
thin graph/final validation tail, and no-cache byte compatibility. Test archive
traversal/duplicate/link/type/hash/size/mode refusals, exact nanosecond restoration,
source-byte mismatch causing zero timestamp updates, compiler mismatch refusal and
nonempty destination refusal. A small fixture should retain one valid Cargo unit
and force an invalid/missing unit to rebuild; this comes after metadata admission,
not during this preparation task. No full compilation is needed to test extraction.

## Workflow inputs and staged context

Optional `thin_cache_approval` is small exact JSON, independently bound by
`thin_cache_approval_sha256`. It names cache and source-metadata immutable OCI
references/config digests. Metadata carrier repository is
`tilo-development-thin-cache-metadata-aarch64`, path
`/checkpoint/source-metadata.json`. Cache paths are
`/checkpoint/thin-cache.tar` and `/checkpoint/cache-metadata.json`.

Admission purpose: `tilo-incomplete-thin-cache-admission`. Required fields:
`source_revision`, `producer_helper_revision`, `checkpoint_reference`,
`checkpoint_config_digest`, `source_metadata_reference`,
`source_metadata_config_digest`, `metadata_sha256`, `source_metadata_sha256`,
`archive_sha256`, `restore_helper_sha256`, `incomplete_cache_reviewed=true`,
and `cargo_revalidation_required=true`. Hash committed Linux helper bytes.
The staging controller copies the reviewed helper from its checkout, never from
the checkpoint image. Exact inner RPM inventory and four tool executable hashes
and versions are checked inside the new builder. Actual restored evidence is
retained and bound into the reconstructed-native receipt after the original build
succeeds. The 9000-second internal build limit remains unchanged.
