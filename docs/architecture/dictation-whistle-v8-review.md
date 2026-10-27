# Compiled Whistle with the existing V8 runtime

Status: the source-only compiled resident backend is viable under the existing
resource controls. Model accuracy and early committed hallucinations still prevent
shipping it as the dictation default. No dependency, runtime package, microphone
permission, catalogue activation or deployment was added.

The [exact ledger](https://code.tilo.so/tilo/tilo/src/commit/0ebed9cae919c6f3e2a57565bde34beac0325ec5/docs/architecture/dictation-whistle-v8-review.json) contains public artifact/source
hashes, raw stream responses, resource observations and scoring. It extends the
[interpreter qualification](dictation-whistle-wasm-review.md).

## Existing runtime and public source

The repository already uses Node in its CI/browser tooling. The cached, digest-pinned
CI image contains Node 22.23.1 / V8 12.4.254.21-node.56. Its public library exports
[`NewSingleThreadedDefaultPlatform`](https://raw.githubusercontent.com/nodejs/node/v22.23.1/deps/v8/include/libplatform/libplatform.h),
which explicitly disables the worker pool and requires `--single-threaded`.
The adapter calls that API without initializing the Node launcher. Matching public
headers were fetched from the publisher's exact release and checked against its
SHA256 manifest. Existing combined Node and libuv notices were retained; their
packages were neither installed nor redistributed.

`tools/release/fixtures/whistle_v8_review.cpp` provides an isolated V8 context with
only explicit public input buffers, bounded output, monotonic clock and pacing.
`whistle_v8_review.js` is our bounded host glue for the publisher's public module
exports, not execution of its JavaScript loader. It retains one engine across
chunks, authenticates materialization through the external harness, denies native
filesystem imports, and bounds linear memory at 512 MiB.

The existing distro's pointer-compression/sandbox build configuration is unchanged.
No WebAssembly trap handler is activated: inline bounds checks remain. The public
heap/code-range constraints narrow reservations to 64/32 MiB; external native
address-space, thread, CPU, cgroup and deadline limits are unchanged. Lowering only
old-space heap had not reduced the initial virtual peak; that control is retained.
The final constraints reduced French virtual peak from 980,232 to 742,536 KiB.

## Same model and French audio

The interpreted and compiled tests use identical authenticated module, model and
little-endian float PCM bytes. Both supply two seconds initially, then follow the
one-second source clock with catch-up after decoder backpressure. Faster decoding
therefore changes call boundaries; this is an end-to-end resident comparison, not
a kernel microbenchmark. All cases are previously exposed development fixtures.

| French measurement | Wasmi interpreter | Existing V8 compiled |
| --- | --- | --- |
| First lexical draft | 6.469 s, incorrect Merci. | 2.184 s, incorrect Merci. |
| First reference opening in draft | 22.028 s | 3.187 s |
| First committed words | 46.055 s | 4.320 s |
| Stream completion for 10.2 s audio | 46.055 s | 11.897 s |
| Native CPU | 43.93 s | 7.24 s |
| Native threads | 1 | 1 |
| Final raw word errors | 0 / 20 | 0 / 20 |
| Peak native RSS | 90,448 KiB | 199,216 KiB |

V8 module compilation took 1.116 s and subsequent initialization/model load
20.5 ms, separately from the warm speech clock. Native adapter compilation took
0.852 s on the remote compiler; neither compilation time is counted as first
returned words. No model load, language token or vendor TTFT is substituted for
actual lexical text.

## Existing multilingual and name controls

| Public case | First lexical draft | First reference opening | Completion | Raw final errors |
| --- | --- | --- | --- | --- |
| English | 2.117 s, However, | 2.117 s | 11.527 s | 4 / 19 |
| German | 2.167 s, Vielen Dank. | 5.275 s | 20.187 s | 7 / 18 |
| Spanish | 2.164 s, Gracias. | 3.210 s | 15.161 s | 2 / 29 |
| German, existing two-name cue | 2.165 s, Vielen Dank. | 5.273 s | 20.260 s | 6 / 18 |

All five final-profile scopes completed with one native thread and no memory
high/max/OOM-event increments. Final-profile native RSS high-water marks ranged
from 199,216 to 207,172 KiB; virtual peaks remained below 743,472 KiB. The native
observer and admission controls are the existing production review functions.

German actually commits the initial “Vielen Dank.” hallucination. The newline
cue contains only the two existing public names Hongkong and Kowloon: it repairs
Hongkong, but Kowloon is still transcribed as Korlum. No reference-aware text cleanup,
numeric conversion, accent folding or fixture-conditioned runtime filter is used.
The reference-opening comparison is post-hoc scoring only. This evidence rejects
model/default qualification even though compiled execution is substantially faster.
Arabic and Mandarin are outside this model's supported language set.

## Reproduce and remaining validation

This ledger records the original streaming adapter at commit
`1f102809554b28a3bea932e368b99437ca8f2165`. Check out that commit to
reproduce its exact bundle. The current harness and source qualify the separate
[endpoint and lifecycle review](dictation-whistle-v8-endpoint-review.md).

`tools/release/qualify_whistle_v8_scope.py` verifies the exact owned artifact bundle
against the ledger and checked-out source, materializes only verified public PCM,
runs bwrap with no home/network/hardware access, and calls the existing resource
observer. Missing resource observation or an incomplete stream fails the check.
Run it inside the same documented transient user scope used by the CLI review,
with `--runtime-dir`, `--manifest`, `--case` and optionally `--name-cues` for the
existing German case. The ledger records the compiler command and source inputs.

The compiled source uses only the existing runtime and matching public headers;
there was no approval boundary for this private source-only experiment. Production
worker integration, silence/cancellation/repeated sessions, physical laptop and
older-device/ARM journeys remain unqualified. This result does not fix the earlier
physical laptop measurement or authorize microphone/model activation. Keep the
engine experiment separate from the already implemented editable-resource path.

The repository-owned harness completed an additional French control and rejected
both size and same-size hash corruption before decoder startup. One initial
private observer-closure import failure is retained; it did not run the decoder.
All eleven owned review services were verified inactive. Remote C++ compilation
used strict warnings, and Python Ruff/py_compile passed. No Rust source changed
in this increment; the preceding required shell integration check remains valid.
