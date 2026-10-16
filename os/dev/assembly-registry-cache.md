# Development raw assembly cache

The controller enables registry intermediate-layer caching only after successful
complete reconstructed-native import in full-build mode. It uses
`registry.tilo.so/tilo/tilo-development-assembly-cache-ARCH-v1`, with layers enabled,
cache-from/cache-to and skip-unused-stages. Existing registry authentication is
used; credentials are not placed in image layers. This is a performance cache,
not release artifact/signature acceptance. Final layer audit, retention, registry
identity verification and cleanup remain mandatory.

Native builder contexts are verified imported image IDs. Compiler secret
arguments are omitted for this path; the overridden native stage is not executed.
Only public trust is an image context. Private signing keys remain outside the
source tree and image contexts; raw mode exits before signing. Intermediate
uploads happen during assembly, including before a possible later stage failure.
The existing cache-configuration.json records the fixed namespace and flags.

Observed Podman5.8.1 cache semantics: unused build arguments and CLI revision
labels preserve earlier steps; changed late consumed ARGs invalidate their RUN
and successors. A changed source-date-epoch invalidates earlier COPY steps too.
This patch preserves the truthful existing source epoch: it enables same-source
retries and generic/board prefix sharing, but does not claim cross-revision hits.
Fresh per-run public trust also invalidates trust consumption and later layers.
Any stable build timestamp or provisional-public-trust policy requires separate
explicit provenance treatment; this cache does not silently alter either.
