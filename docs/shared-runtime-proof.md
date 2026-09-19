# Shared runtime proof on macOS

The real Photos and Word Processor executables run concurrently in separate
native windows, connected to one compositor-free service process. No app code
is forked or given a special proof mode.

The host calls the same `collab_broker::spawn_native` and `identity_broker::spawn`
implementations used by qOS. Device certificates, enrollment, workspace
membership, action policy, RPC and signed document validation use their existing
production paths. It does not use `spawn_native_harness` or mock app endpoints.

## Reproduce

From the repository root, with an active macOS desktop session:

```sh
cargo build -p shell -p photos -p word-processor --no-default-features --example shared-runtime-proof --bin photos --bin word-processor
python3 scripts/shared-runtime-proof.py
```

The runner creates an isolated temporary profile, launches both apps, and
retains evidence under `target/runtime-proof/<timestamp>`. It stops and reaps the
apps after verification and removes the temporary profile, including private
identity material. It never changes the user's normal qOS profile.

## What is verified

1. One runtime starts on macOS, then starts two distinct app PIDs.
2. Both apps join the same workspace through separate production broker peers.
3. Photos publishes an image resource containing its content hash.
4. The host verifies the published bytes match that hash in the shared content store.
5. An ungranted actor cannot invoke Word Processor's insertion operation.
6. The authorized host invokes the real `place-image` action with Photos' hash.
7. Word Processor returns a successful typed invocation result.
8. The proof independently verifies the signatures of document events received
   through the broker, reconstructs the actual document, and finds an image block
   referring to that exact hash. Both app processes must still be alive.

Evidence files include `events.jsonl`, `runtime.log`, individual app logs,
`source.png`, `received.automerge`, and `signed-document-events.json`.

Verified on macOS on 2026-09-16: `target/runtime-proof/20260916-082747/`.
Runtime PID **28630**, Photos PID **28661**, Word Processor PID **28662**.
Broker peers **0** and **1** shared workspace
`820b5f85-e015-4c9b-b7d6-8e1a6821c951`. Invocation **1** succeeded, and two
signature-verified document events reconstructed an image block with hash
`157a3eac553dd43c643d8c6c56555c6414cc18da3c727295d6d57414161da7c5`.
The permission-negative check also passed.

Targeted validation passed: 37 identity SDK tests, 11 content-store tests,
the host lifecycle test, and three existing production identity-broker tests.
Clippy passed for both changed SDK libraries.

## Shared code and platform boundary

- `shell/src/runtime_host.rs`: lifecycle and composition of existing services.
- `shell/src/runtime_host/unix.rs`: private runtime directory and Unix descriptor
  inheritance, usable by both macOS and Linux.
- `crates/runtime/runtime-identity`: shared connection ownership and framing
  serialization, fixing repeated SDK metadata lookups closing a live signer.
- `crates/doc/qdoc/src/blobstore.rs`: an explicit `QOS_BLOB_STORE` selects the
  existing filesystem store, allowing trusted local apps to share content.
  With no override the existing in-memory default remains.
- `shell/examples/shared-runtime-proof.rs`: verification driver, not a second
  action broker or workflow engine.

The host still links the shell library because its broker depends on the
workspace policy model. Extracting those modules into a smaller crate is future
dependency cleanup; their implementations are shared now.

The same service composition is now available through the native
`qos-runtime-mcp` stdio entry point. Its user-supplied manifest launches apps
through `Session::launch`; ordinary `ui::App` startup publishes each live,
peer-qualified lazy action directory automatically. The host validates the
complete manifest and acquires the profile lock before launching any child,
bounds both request lines and queued input, keeps application output away from
protocol stdout, detects child exits while stdin is idle, and reaps its owned
children on shutdown. See [runtime-mcp.md](runtime-mcp.md) for configuration and
the protocol/authority contract.

The separate 2026-09-17 source audit inventories 47 GUI entry points: all 47
reach the production `ui::App` publication path, with 41 default launch paths
and six custom live handlers that still construct the shared app host. This
establishes generic source integration, not native behavioral qualification.
The Task 2 lifecycle test proves two synthetic, normally connected instances
with the same app id publish as distinct broker peers. Real Photos, Writer and
Calculator MCP execution remains a separate native acceptance proof; no
all-47-app native qualification is claimed here.

## Scope

This is a trusted, same-user native-app proof. Bytes use the existing filesystem
content store; discovery and invocation cross the actual broker connection.
This does not claim sandbox-equivalent isolation, a packaged/background installer,
Windows support, peer-network/MLS synchronization, or model reasoning. A model
was not required to test the typed operations that cross-app AI would use.

Photos' existing live rotate action does not return edited content. This proof
uses its published original image resource, not an invented result. Continuation
metadata warnings are expected because these standalone apps have no durable
shell tab; this proof does not exercise shell-session restoration.
