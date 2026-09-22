# Native edited-content proof

The opt-in `--edited-handoff` mode of `shared-runtime-proof` launches real Photos
and Writer windows through `Session::launch`. An additional SDK participant sends
`CollabRequest::InvokePublishedAction` with typed JSON arguments and observes
`PublishedInvocationStatus`. No model or external service participates.

Build and run from the repository root, outside performance collection windows:

```sh
cargo build -p shell --example shared-runtime-proof --no-default-features
cargo build -p photos -p word-processor --no-default-features
python3 scripts/edited-handoff-proof.py
```

The runner uses a fresh isolated profile. It first checks that a certified,
read-only delegated agent cannot invoke either the producer or consumer, and
that Writer rejects a nonexistent content hash. It then invokes the Document
rotation and the refreshed edited-content Effect, requires one canonical live
`tilo.content` result with a different hash, verifies the stored PNG's BLAKE3,
and compares every decoded RGBA pixel using the independent clockwise mapping
in `pixels.rs`. It passes that actual returned hash to Writer and reconstructs
the signed document stream for precisely the published Writer route, requiring
one image with the matching hash and dimensions.

Evidence includes both PNGs, live typed invocation results, the producer's
contract, expected edit snapshot, binary SHA256 values, signed Writer events,
and the reconstructed Automerge document. The live result file is **not** a
redacted durable broker receipt. The Python runner never copies identity keys.

This is native GUI runtime evidence. It does not prove headless parity, Windows
behavior, package sandbox enforcement, power-loss durability, or agent planning
and immediate-next-step handoff invalidation. Those require separate checks.
The existing default proof and `--benchmark` mode keep their original behavior.
