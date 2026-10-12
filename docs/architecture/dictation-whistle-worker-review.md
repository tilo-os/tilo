# Compiled Whistle worker boundary

The default-off `whistle-v8-review` example connects the compiled public decoder
ABI to the **existing** `speech::worker::{configure, serve_decoder}` dispatcher.
It does not select a new production backend, install a component, add a catalogue
format or activate a model. The normal worker still requires `whisper`.

The actual production policy rejects this runtime before readiness. On the
pinned public French clip, the child reached one thread, VmPeak 188396 KiB and
RSS 71656 KiB; no memory-pressure, OOM or external resource guard fired. It sent
**zero replies**. A second bounded attempt using the already-installed tracer
observed:

```text
pkey_mprotect(..., 8192, PROT_READ|PROT_WRITE|PROT_EXEC, 1) = -1 EPERM
Fatal process out of memory: Commit wasm code space
```

The diagnostic's “out of memory” wording does not establish memory exhaustion.
The mapping fails under the existing parser filter's W^X rule. The pinned
[Node 22.23.1 V8 code manager](https://github.com/nodejs/node/blob/v22.23.1/deps/v8/src/wasm/wasm-code-manager.cc#L1887)
requests RWX code pages with or without protection keys. Disabling protection
keys is therefore not a write-then-execute remedy. This candidate remains
**unqualified**; no sandbox rule, limit or V8 build security setting was relaxed.

## Source and transport boundary

The opt-in example prepares the fresh owned child with the production descriptor,
core-dump, tracing, address-space and parent-death safeguards. It opens only
explicit owned regular review inputs, checks exact pinned module/model hashes,
and loads the review bridge from a hash-checked, kernel-sealed memfd. The bridge
lifetime outlives its exclusively owned decoder. Its runtime library closure is
verified by the outer public-fixture harness, not claimed as catalogue authority.
Elevated identities cannot use these review inputs.

Configuration, S16LE audio, sequence checks, bounded replies and EOF behavior use
the production Rust protocol. Language and one-word dictionary hints reach the
public ABI as data. Drafts accumulate incremental audio; the endpoint uses the
exact full audio to replace provisional text. No text rewrite follows decoding.
Configuration, sample buffers and temporary replies remain private; the example
prints only an opaque failure status. Only the public fixture observer prints
recognition replies.

This review has an explicit 30-second input bound. It does **not** change the
production 120-second limit or establish a segmentation policy for longer speech.
The decoder's pipe readiness, acoustic result, EOF cancellation and native field
insertion were not reached. Existing production transport/editing regressions
remain useful evidence for those separate boundaries, not proof of this engine.

The source is independently reviewable from live activation. It adds no engine
SDK dependency, lockfile change, default feature, microphone permission, network
access, installed binary, model catalogue entry or trust key. The public SDK and
runtime are explicit external qualification inputs from the earlier pinned V8
review. The emitted `.inc` is generated from the checked-out review-owned JS, not
publisher JS:

```python
from pathlib import Path
source = Path('tools/release/fixtures/whistle_v8_worker.js').read_text()
assert ')TILO"' not in source
Path('/review-build/whistle_v8_worker.inc').write_text(
    'const char worker_source[] = R"TILO(' + source + ')TILO";\n')
```

Copy the matching C++ fixture into that private build directory, then compile a
shared library with C++20, `-O2 -Wall -Wextra -Werror -fPIC -shared`, the authenticated
public headers as a system include, and the previously verified `libnode.so.127`.
The exact compiler command, source/binary digests, runtime origin, resource
receipts and stopped-service observations are in the adjacent JSON and the
referenced prior V8 ledger. No generated or downloaded runtime is committed.

Build the protocol adapter through the supported remote build tool:

```sh
${CARGO_DROP_IN} --no-fallback --no-fetch build --locked -p local-model-service \
  --no-default-features --features whistle-v8-review --example whistle_v8_worker_review
```

The scope harness requires an explicit builder receipt tying the bridge, worker
binary and checked-out Rust/C++/JS sources to their measured bytes. It verifies
all earlier pinned runtime and licence objects, exact public corpus PCM, the
existing cgroup limits, memory-pressure admission and observed native process
identity. Run `tools/release/qualify_whistle_worker_scope.py` inside the same
admitted transient scope documented in `dictation-whistle-v8-endpoint-review.md`.
A missing observation or absent successful endpoint **fails** qualification; the
known code-page refusal is never converted into a skip or a passing speech test.

## Packaging and supported development flow

Three independent shipping objects remain:

1. A compatible immutable decoder/runtime closure, architecture-specific and
   packaged with its licence notices. Existing CI Node/V8 presence proves neither
   OS inclusion nor compatibility with the production worker policy. The current
   binary needs a runtime implementation compatible with that policy; larger
   limits, moving this V8 initialization ahead of confinement or enabling an unrestricted JIT
   profile do not qualify it.
2. The exact speech module and model provenance: pinned publisher revision,
   format, size, digest, languages, capability and Apache notices. A model needs
   the existing signed catalogue and participant licence/selection flow before
   `speech_model_file` can issue an immutable model witness. That path currently
   accepts `WhisperGgml`; this review does not make Whistle an admitted format.
   Review SHA receipts and a caller-supplied bridge hash are not signing authority.
3. The native editing components already developed on this held branch: matching
   shell, toolkit/apps and root-owned ush editor module. They use the existing
   resource/actions and original-focus gates, not a new input injection channel.

`os/components.toml` already declares `model-manager` as a session component
containing `tilo-model-manager` and `tilo-speech-worker`, and `ush` contains its
root-owned Bash editor module. Once source/runtime packaging is qualified,
[the supported component flow](../../crates/dev/tilo-vm/guide/swapping.md) can
build and swap those session components in an existing dedicated development
slot. Matching unpublished app/toolkit work uses `apps+runtime`; shell swaps
include their matching session wrapper. A source swap uses the standard layer
provenance checks and rollback. It does not require a full OS image or kernel
reboot. No swap, registration, session restart, production activation or device
reboot was performed here.

The historical full-image unsigned RPM snapshot prevents that image publication;
it is not the reason this source-level worker attempt failed. Signed production
runtime/catalogue publication remains a separate release boundary. Current-main
integration and the source-preserving UI split remain with their existing owners.

## Validation

The exact confined attempt and syscall diagnostic are **failed qualification**,
not successful recognition. The adapter built remotely and passed strict model-service all-target
Clippy with the review feature. Focused custody tests exercise actual no-follow artifact admission and
kernel memfd immutability (two passed, zero skipped). The required shell `--tests`
compile check passed in 1m13s. Rust formatting, strict C++ compilation, Python
Ruff/syntax, JS syntax and staged repository checks passed. The initial scope attempt lacked an existing scorer
import and stopped before starting the decoder; its receipt is retained.

Not exercised by this candidate: successful compiled worker readiness, worker
endpoint/EOF/silence/repeat journeys, native insertion with this decoder, real
microphone access, assembled apps, long speech, older-device or ARM performance,
model activation and component swaps. These cannot be inferred from the earlier
isolated public-fixture recognition results.
