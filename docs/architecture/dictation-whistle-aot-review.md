# Whistle AOT qualification inside the production speech worker

The pinned public Whistle speech module now recognizes and streams through the
real private speech protocol **after** production worker confinement. Wasmtime
publishes code as RX without requesting RWX. This resolves the V8 compatibility
blocker; it does not activate a model or finish every-field dictation.

## Source, compilation and native-code custody

Wasmtime 49.0.2 was built from official release commit
[`3c8a3e79aa3188a1a12b3aebbaa097abccceac92`](https://github.com/bytecodealliance/wasmtime/tree/3c8a3e79aa3188a1a12b3aebbaa097abccceac92)
in a separate clean checkout, using its committed lock and the existing remote
builder image. The public SDK download was inspected and authenticated but was
never executed. The actual library is source-built. No Tilo dependency or lock
version changes, installation or persistent grant were needed.

The build is `wasmtime-c-api --no-default-features --features cranelift`, release,
two jobs, through `rcargo --no-fallback --no-fetch +1.97.1`. WASI, asynchronous
execution, caches and parallel compilation are absent. The upstream licence is
Apache-2.0 WITH LLVM-exception; the public Whistle module/model notices are
Apache-2.0. Review binaries remain private, uninstalled artifacts.

An owned factory compiles the authenticated 923348-byte `needle.wasm`, SHA-256
`c43f48e11f302087250d1e406024956781cd2f02595a5e343b3d7ddd5ef707fa`.
Factory compilation took **1.165 s**, one native thread and VmPeak 219572 KiB.
Its serialized output is 3437656 bytes, SHA-256
`a74daea810fcccb4053ff8639f7bae9b7010fd9d9e1ad80a0deecc17c2cb3d26`.
Compilation is separate from recognition and is not first-word latency.

Both factory and runtime set an explicit x86-64-v3 CPU feature set, disabling
host CPU inference. Memory reservation, growth reservation and guard size are
zero; guest bounds checks and the **default signal and heap/table Spectre
protections remain enabled**. The store limits guest memory to 512 MiB and one
instance/table/memory. Only the sixteen authenticated Emscripten function imports
are supplied, with bounded memory ranges, suppressed diagnostics, clocks and
explicit filesystem-unavailable responses. There is no publisher JavaScript,
Python wrapper, WASI linker, filesystem import or network import.

The final review children pin the exact compiled digest and length in source.
A caller cannot bless arbitrary native code by supplying its own matching hash.
Model bytes retain their pinned digest. Owned regular files, immutable memfd
library snapshots and the outer runtime/compiler/configuration custody receipt
are checked before use. These review receipts are **not production catalogue
authority**. A shipping compiled artifact needs the existing immutable packaging
and admission trust path rather than a caller-supplied file.

## Actual confinement and protocol behavior

The child uses production `worker::prepare`, then production
`speech::confine_worker`, **before deserialization, initialization and model
preparation**. Landlock filesystem/network denial, the parser filter, closed
ambient descriptors, disabled dumps/core files, parent-death termination and the
1 GiB address-space limit are unchanged. A direct memory-syscall trace observed
one successful RX publication and zero RWX requests. The earlier kernel probe's
positive RX and denied ordinary/V8-shaped RWX observations remain separate
policy evidence.

Only after the first confined batch recognition succeeded was the default-off
`whistle-aot-review` worker adapter connected to `SpeechDecoder`/`serve_decoder`.
It forwards each newly received PCM range once. Stable and pending hypotheses
are provisional; Finish flushes the stream, transcribes the exact complete
audio with the same language/cues, and returns that literal final text. No
dictionary substitution, reference-aware cleanup or thank-you filtering occurs.
Parsed replies/configuration zeroize; the bridge clears copied audio/output
buffers and guest data when its owned decoder retires.

The first isolated confined French batch initialized in **0.331 s** and decoded
in **1.958 s**, with one native thread and VmPeak 147092 KiB. That is a batch
compatibility result, not a streaming microphone measurement. Its original
source/binary receipt is preserved separately from the final worker profile.

The final worker uses the same four authenticated FLEURS development clips as
the prior V8 comparison, without accuracy tuning. Public audio is paced as an
initial two-second prefix and subsequent one-second acquisitions; a slow decoder
coalesces newly available audio. Ready includes child input/library preparation
and model initialization. Draft/final times start **after Ready**; they exclude
startup and are not measured from a physical microphone.

| Public case | Audio | Ready | Final after Ready | Raw word errors | VmPeak KiB |
|---|---:|---:|---:|---:|---:|
| English | 10.56 s | 0.732 s | 12.765 s | 4/19 | 161108 |
| German | 17.94 s | 0.731 s | 22.367 s | 6/18 | 187696 |
| French repeat | 10.20 s | 0.731 s | 12.498 s | 0/20 | 160852 |
| Spanish | 12.84 s | 0.727 s | 16.979 s | 2/29 | 167252 |

Every native worker stayed at **one thread**, below the original two-thread
ceiling, with no guard termination or high/max/OOM event increase. The score
uses the existing NFKC/casefold/punctuation/whitespace scorer on the literal
endpoint. Numerical formatting and compounds are not normalized against the
reference. These four clips are not broad accuracy acceptance.

First nonempty output is misleading: French, German and Spanish initially emit
`Merci.`, `Vielen Dank.` and `Gracias.` before usable spoken fragments appear.
The ledger preserves every raw draft; those artifacts are not counted as useful
first words or removed by a stopword heuristic. In German the provisional
thank-you persists until the endpoint replaces it. Preview quality remains a
material product limitation.

Three seconds of silence produce empty drafts and an empty final. Closing an
unfinished three-second session exits with failure and **no Done**. An independent
fresh French session returns the identical literal final. This proves fresh
repeated sessions, not reuse of one native context or cancellation while inside
an active native call. All eleven owned transient units are inactive.

## Reproduction and remaining boundary

The adjacent [JSON ledger](dictation-whistle-aot-review.json) records source-built
runtime/compiler/artifact digests, exact Rust/bridge source hashes, raw receipt
hashes, measured public replies, fixture custody and cleanup. First-batch and
earlier prototype receipts are identified rather than relabeled as final-source
measurements. The held branch preserves the rejected V8 profile unchanged.

Build the official source C API in its separate checkout through rcargo; compile
`tools/release/fixtures/whistle_aot_factory.cpp` and `whistle_aot_review.cpp` on
the builder against its generated headers/library, using `-L`/`-lwasmtime` and
`$ORIGIN` rpath. Never link the library by absolute filename: its private output
has no SONAME and otherwise records an inaccessible absolute DT_NEEDED path.
Build the two opt-in `local-model-service` examples and assemble the private
`source-build-receipt.json` from their exact source and binary bytes. In the
existing admitted transient scope, run `qualify_whistle_aot_scope.py --phase
compile`, then `--phase recognize` or `--phase worker`, with the authenticated
public manifest. `--trace` traces the actual worker only. Never deserialize an
externally supplied precompiled artifact.

An initial missing observer helper and absolute-library loader failure remained
failed attempts; the corrected factory passed. A later rerun stopped before its
next child because the host-pressure gate rejected admission. Work continued
independently, then final-source qualification passed without lowering any
guard. An unobservable peer trace failed; the direct-worker trace succeeded.
No missing observation is converted to a skip.

Executed checks: strict service all-target Clippy, the three actual custody tests
(zero skips), both opt-in example builds, required shell `--tests` compile,
strict C++ builds, Python Ruff/syntax, four actual confined paced-language
journeys, silence, EOF, repeat and memory trace. Staged repository checks and the
affected-package plan are recorded at publication.

Still unqualified: live microphone/account permission, complete native/browser/
terminal journeys **with this engine**, older physical devices, ARM, busy native
inference cancellation, engine-specific language/dictionary revocation and
production immutable runtime/model admission. Existing insertion/undo and
revocation regressions are transport foundations, not new acoustic evidence.
The backend is default-off and not selectable on the production model catalogue.
No model activation, deployment, reboot, permission or trust change occurred.
