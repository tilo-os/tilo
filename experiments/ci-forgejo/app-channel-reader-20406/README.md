# Real retained collection audit: compatibility gate correctly blocks promotion

Reused the already retained artifact 34748 / successful run 20406 / source
`525aa7824c7a8476aa5ea006499ddd01a8b3c2ea`, assembled previously with the current
assembler. No new download, extraction tree, compile or publication was needed.
The actual tar SHA256 is
`4cd9ab079136b8bcc97fa9000ee92485911a2182b870eab2411586fb6f1d1e84`.

The new adapter streamed and matched every archived file against the retained
local tree, validated checksums and all 51 tapp/runtime signatures and payloads
using the externally pinned replay verifier. Its hash is recorded in the audit
and historical test policy. That policy is **fixture evidence, not production
trust configuration**; keys were explicitly chosen for these retained artifacts.

The normal receipt path failed closed: three signed manifests have an empty
`[package.isa]` table:

- `contacts-0.1.0-x86_64.tapp`
- `notes-0.1.0-x86_64.tapp`
- `store-0.1.2-x86_64.tapp`

Current `crates/packaging/tilo-app-package/src/manifest.rs` explicitly defines a missing host
architecture as "no build available". These three current source app manifests
also lack ISA declarations at base `ce0eff803`; no compatibility was invented.
The other 48 artifacts declare x86_64 v3.

`verified-compatibility-audit.json` records real archive/package/runtime identities
and all three incompatibilities. Its schema is deliberately
`tilo.app-collection-audit/v1`, not a publication receipt. Passing it to the channel
reader is rejected even with the correct manifest digest. The archive has not been
declared published or installable. The first failure and subsequent explicit audit
exercise the real adapter, not a synthetic archive.

The existing verifier is
`../tilo-artifact-replay/tools/replay_app_artifact.py` in the neighboring retained
worktree. The archive/tree live under that worktree's
`dist/replay-20406/output/`; crypto runs with
an isolated replay virtualenv under WSL Fedora Linux 42. Its reviewed helper
was not copied into the production reader or silently made a trust authority.

Nine schema/reader tests independently cover successful architecture-specific
selection with an older preserved source, no architecture fallback, digest and
duplicate-key rejection, publisher/verifier/ISA trust, missing ISA/audit rejection,
unpublished refusal, uploader identity mismatch, URL pinning and verifier import
pinning. Independent review passed. Native execution, current-source compatibility,
remote upload availability and production consumer activation remain unqualified.
