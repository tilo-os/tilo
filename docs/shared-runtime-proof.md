# Shared runtime acceptance proof

The shared-runtime proof launches the real Photos and Word Processor binaries
in separate native windows against one compositor-free service process. The
host uses the production collaboration and identity brokers, workspace policy,
signed document validation, and content store. Applications do not use a proof
mode or mock endpoint.

Run from an active supported desktop session:

```sh
cargo build -p shell -p photos -p word-processor --no-default-features \
  --example shared-runtime-proof --bin photos --bin word-processor
python3 scripts/shared-runtime-proof.py
```

The runner creates an isolated profile, launches both apps, retains evidence
under `target/runtime-proof/`, stops and reaps its processes, and removes private
identity material. It does not modify the normal user profile.

The proof requires:

1. both applications join one workspace as distinct production broker peers;
2. Photos publishes an image resource whose stored bytes match its content hash;
3. an ungranted actor cannot invoke the Writer insertion operation;
4. the authorized host invokes Writer's real `place-image` action with that hash;
5. signed broker events reconstruct a document containing the expected image;
6. both application processes remain alive through verification.

Retained artifacts include broker events, runtime and application logs, the
source image, the reconstructed document, and signed document events. A run is
valid only after successful cleanup and complete evidence checks.

The shared implementation is composed in `crates/session/shell/src/runtime_host.rs`, with Unix
profile and descriptor handling in `crates/session/shell/src/runtime_host/unix.rs`. Connection
ownership lives in `crates/runtime/runtime-identity`; the filesystem content
store is selected through `QOS_BLOB_STORE`. The proof driver is
`crates/session/shell/examples/shared-runtime-proof.rs`.

This is a trusted same-user integration check. It does not claim packaged
installation, sandbox-equivalent isolation, remote federation, every GUI app,
Windows behavior, or performance. The same runtime composition is available
through `qos-runtime-mcp`; see [`runtime-mcp.md`](runtime-mcp.md) for its
protocol, authority, and lifecycle contract.
