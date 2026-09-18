# Native8166 thin-cache recovery seam — preparation only

This design is specific to application source
`4583a44c4056c67cfd1ebc4f1e02c7fa4aff3ffc` and producer helper
`9d7a6b259c9b75b6c5031472bfed05b6461c1893`. No executable importer, approval,
workflow dispatch or completed native payload is supplied by this document.

## Evidence still required

The current capture script records a bounded, non-paused, incomplete inventory of
regular files from `/build/target/thin-app`. It excludes observed active compiler
units, writers and temporaries, and retains PAX nanosecond mtimes and file hashes.
It explicitly states `source_mtime_restoration_not_proven=true`.

Before implementing timestamp restoration, obtain independently pinned manifests
from the actual nested build root, not the outer tools process:

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
Missing metadata is an explicit preflight failure, not a silent clean rebuild.

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
