# buffa-build provenance

- Package: `buffa-build` 0.9.2
- Registry checksum: `eee6b6b4a8d23f1f98a043f188f4d9d2ef8bee43191f42e93ac577f3c3b0f334`
- Repository: https://github.com/anthropics/buffa
- Upstream revision: `1f5d80e2c8d31c147742d8ec826a3219c4fcd4a7`
- Upstream package path: `buffa-build`
- License: Apache-2.0

The baseline is the published crate archive, verified against Cargo.lock. All
files retain their original contents except the patch below and this record.
The archive's standalone Cargo.lock is omitted as required by the repository
pre-commit policy: the root workspace lock is authoritative.

Local patch: the precompiled descriptor mode omits its file rerun directive
only when the descriptor's supplied path and canonical path are both inside
this caller's actual Cargo OUT_DIR. Missing environment, failed canonicalization,
and external paths remain tracked. Both symlink directions across that boundary
remain tracked. Caller-owned original source directives and the codegen memory
limit environment directive are unaffected. All upstream assertions are unchanged.


The workspace selects this copy with a reviewed path override; package versions
and all transitive dependencies remain unchanged. Remove the override after an
upstream release contains the repair. Export source changes relative to the
recorded crate archive when proposing the patch upstream.
