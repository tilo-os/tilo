# Whistle resident WebAssembly qualification

Status: the public streaming interface works within the unchanged resource limits,
but the interpreted execution path fails interactive latency qualification.
No runtime, model, microphone permission or production catalogue was activated.

The [measurement ledger](https://code.tilo.so/tilo/tilo/src/commit/0ebed9cae919c6f3e2a57565bde34beac0325ec5/docs/architecture/dictation-whistle-wasm-review.json) records exact artifacts,
source and binary hashes, raw streaming responses, scoring and retained failures.
This extends the [native initialization review](dictation-whistle-thread-review.md).

## Supported source boundary

The pinned needle wrapper does not supply the matching native C++ engine source.
Its public native header has no pre-initialization thread setter. The CLI's
resident server advertises text `/complete` and `/reset`, not speech streaming.
The publisher's WASI component interface similarly declares no speech exports.
Neither provides the requested resident speech alternative to process-per-prefix
transcription.

The publisher's [pinned browser loader](https://huggingface.co/Cactus-Compute/needle3/blob/2ae11323dc000f5e70c49f7403efa6af12ba9e67/wasm/needle.js)
explicitly maps model load, streaming process and streaming stop in its core module.
The review adapter follows those mappings; it does not call private native symbols,
patch engine code, run the JavaScript or import the Python wrapper.

`crates/agent/local-model-service/examples/whistle_wasm_review.rs` is an opt-in,
review-only adapter using the already locked Wasmi 1.1.0 interpreter. It authenticates
both input artifacts, bounds linear memory and output, supplies only the declared
host imports, refuses filesystem access and retains one engine between chunks.
The external observer separately enforces native threads, address space and deadline.
All builds used ${CARGO_DROP_IN} without local fallback.

## Actual words and resources

One previously exposed French FLEURS development case was source-paced through the
resident module. The first two seconds of audio were supplied at two seconds; later
calls caught up to the source clock after backpressure. This is not a microphone or
unseen holdout measurement.

| Measurement | Result |
| --- | --- |
| Initialization and load | 143 ms |
| First lexical draft | 6.469 s, incorrect “Merci.” |
| First draft containing the reference opening | 22.028 s |
| First committed words | 46.055 s |
| Final output | 46.055 s for 10.2 s audio |
| Raw final word errors | 0 / 20 |
| Native CPU time | 43.93 s |
| Native thread peak | 1 |
| Native RSS high-water mark | 90,448 KiB |
| Scope memory peak | 103,501,824 bytes |
| Memory high/max/OOM events | No increments |

The good final result on this one exposed fixture does not repair the wrong early
hypothesis or unacceptable delay. No language token or model load time is counted
as first-word latency.

Two failures remain recorded: the initial 2-billion-instruction fuel ceiling trapped
before words; a scalar validation trial established that the module requires SIMD.
The completed trial explicitly used 48 billion fuel per call, subordinate to the
unchanged 180-second external deadline. Native thread, CPU, memory and scope limits
were never relaxed. Silence and repeated-session runs were not expanded after this
latency rejection.

## Concrete alternative

The working adapter separates the publisher's streaming contract from its execution
backend. A compiled WebAssembly host can use the same authenticated module and
bounded imports without the native engine's startup compute pool. For example,
[Wasmtime's public configuration](https://docs.wasmtime.dev/api/wasmtime/struct.Config.html)
exposes parallel compilation, SIMD and linear-memory reservation controls. This is
an implementation alternative, not an observed speedup: it requires a separately
pinned runtime/source/license review and measurement under the same limits before
any production integration. No such runtime was installed or added here.

The native constructor thread sizing remains an upstream-source boundary. Retain
the current editable-resource, cancellation, correction and worker contracts;
do not evade that boundary by private ABI, per-chunk process reloads or a hidden
thread-budget exception.

The final hardened source was rebuilt and the same fixture rerun. Its exact
source hash matches the primary receipt; the earlier completed run is retained
separately in the ledger. All four owned review services are inactive.

The [compiled existing-V8 follow-up](dictation-whistle-v8-review.md) implements
that alternative without adding a runtime dependency. Its final profile passes
the same resource controls and improves French timing substantially, while the
multilingual/name controls still reject model/default admission.
