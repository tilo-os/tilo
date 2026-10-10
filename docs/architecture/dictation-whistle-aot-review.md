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
two jobs, through `${CARGO_DROP_IN} --no-fallback --no-fetch +1.97.1`. WASI, asynchronous
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

## Native-call cancellation and synthetic insertion follow-up

The separate [cancellation ledger](dictation-whistle-aot-cancellation.json)
qualifies both an active streaming call and an active final call on the same AOT
engine and authenticated public French recording. The review child has opt-in
fixed begin/end markers containing only the operation number. After observing
the begin marker, the controller requires two additional CPU ticks (20 ms on
the measured host), with neither an end marker nor an output reply readable,
before sending SIGKILL through a pidfd for its exact owned child.

Streaming kill-to-reaped latency was **7.220 ms**; final-call latency was
**7.214 ms**. Both children exited by SIGKILL, produced zero trailing reply
bytes and no Done, and stayed at one native thread. The unchanged memory/thread/
deadline guards admitted both. A fresh French session afterward returned the
same literal final, with Ready at 0.724 s and final at 12.486 s after Ready.
All three owned scopes are inactive. This is actual native termination and
reaping; the compositor's actor cancellation and dictionary revocation retain
their separate production-owner regression evidence.

Run the existing controller with `--phase worker --mode cancel-draft` or
`--mode cancel-finish`, using a fresh source/binary receipt and the same admitted
scope. Cancellation requires the direct worker; trace wrapping is rejected.
The first copied review directory lacked receipt-listed C++ inputs and failed
before child launch. Its diagnostic is preserved; no missing observation was
converted into a passing or skipped case.

Two new headless insertion tests replay **all recorded drafts and literal finals**
from the original four-language ledger through production native chrome/Notes
and a real foreign `zwp_text_input_v3` client. Each draft, including the invented
opening phrases, must leave the field untouched. The final reaches each field
as one exact commit, with no synthetic key events; native Notes replaces its
selection and supports undo and redo through owned history. These are replayed
engine-output insertion journeys, not a concurrent microphone-to-application
or browser-product qualification. Existing focus-change, cancel/repeat and
Terminal editing tests retain their independent boundaries.

The visible and accessible preview explicitly says **“May change”**. It remains
bounded, private and separate from the field; this qualification adds no decoding
or presentation delay. It does not claim hallucination suppression. Signal
inspection found quiet French/German prefixes and Spanish background sound;
high-confidence invented words and quiet real French speech make a fixed
confidence/volume threshold insufficient evidence for an inclusive speech gate.
No phrase, reference-aware or language-specific output filter was introduced.

`tools/release/prepare_whistle_aot_package.py` now prepares a **new, private image
layout candidate** from these exact qualified native objects. It verifies fixed
source pins, owned regular files, sizes, notices and the bridge/configuration
source before writing output. Caller-selected native bytes, symlinks and writable
inputs fail before the destination exists. The actual candidate includes the
runtime library, bridge and AOT under `/usr/lib64/tilo-speech/whistle/x86-64-v3`,
both complete upstream notices under `/usr/share/licenses/tilo-speech-whistle`,
and sources/provenance under `/usr/share/tilo-speech/whistle`. Model bytes are
excluded. The [package ledger](dictation-whistle-aot-package.json) records the
actual nine files and forged-code rejection.

The package remains a private candidate: no root-owned host package was installed,
no production model catalogue was activated, and no signing or publication occurred.
The subsequent default-off production source integration is described below.

Follow-up validation: both four-language insertion tests pass (1.135 s combined,
zero ignored); the accessible preview test passes (0.112 s); required shell
`--tests` compile passes (47.03 s); strict service all-target Clippy passes
(0.72 s); all three custody tests pass (zero skipped); both package-custody tests
pass, including symlink/group-write subcases; Python Ruff/syntax pass. The actual
native-call cancellation, subsequent fresh recognition and package preparation
also pass without changing admission controls.

## Reproduction and remaining boundary

### Default-off production source admission

The `whistle-aot` service feature now admits signed `whistle_cact` weights through
the existing object verification, licence acceptance and sealed-model path. Weights
cannot nominate an executable runtime. Without that feature the worker rejects
this format before configuring or parsing it as Whisper. With it, the worker
requires x86-64-v3 CPU support and exact model bytes, and authenticates three
fixed image runtime objects by root ownership, non-writable parents, no-follow
opens, sizes and compiled-in digests. A caller receipt cannot grant code authority.
Native deserialization and model preparation still happen after the existing
Landlock/seccomp confinement. No JIT or additional filesystem/network permission
is enabled.

The decoder uses the production full-context cadence and shared bounded
segmentation, preserving the 120-second session cap while keeping each native
call below 30 seconds. This differs from the earlier incremental review profile;
its timings are recorded separately. The feature remains off by default.

The [production-handler ledger](dictation-whistle-aot-image.json) records actual
`worker::serve` journeys in an isolated synthetic image whose root maps to an
unprivileged host account. The signed catalogue, trust key, licence selection and
model store are ephemeral fixtures in the namespace. There is no host root,
microphone, home directory, network, installed package or persistent trust change.
Ready times include fixture setup, and replayed application insertion is a
separate test rather than a concurrent microphone-to-app journey.

| Public case | Ready including fixture (s) | Final after Ready (s) | Word errors/reference words |
| --- | ---: | ---: | ---: |
| English, 10.56 s audio | 1.115 | 11.906 | 4/19 |
| German, 17.94 s audio | 1.138 | 21.077 | 6/18 |
| French, 10.2 s audio | 1.103 | 11.821 | 0/20 |
| Spanish, 12.84 s audio | 1.447 | 15.571 | 2/29 |
| French repeated four times, 40.8 s audio | 1.293 | 44.024 | 4/80 |
| French repeated eleven times plus 7.8 s silence, 120 s input | 1.163 | 122.895 | 16/220 |

All successful runs observed one native thread. Peak virtual memory ranged from
about 176 to 212 MiB for the paced speech recordings. Silence emitted no text; interrupted
input emitted no final. A forged root-owned AOT artifact, even accompanied by a
matching caller receipt, was rejected before Ready. All review units were inactive
after observation. The earlier incorrectly serialized fixture status remains a
failed attempt in the ledger. Raw draft/final text is preserved, including early
invented words and German recognition errors; preview labelling does not repair
accuracy. These are remote-builder results, not older physical-device acceptance.

Focused speech tests pass: 17 run, zero failures (27 unrelated tests filtered).
Strict all-target service Clippy passes both the default build and the opt-in
production/review features. Remaining checks and publication are recorded with
the candidate commit.

The subsequent [120-second ledger](dictation-whistle-aot-image-120s.json) exercises
the worker's exact input cap with eleven concatenated public French recordings
and a silent tail. Completion takes 122.895 seconds after Ready, with one native
thread, VmPeak 217092 KiB and no resource guard or cgroup event increase. The
literal endpoint contains eleven sentences and no separate invented tail phrase,
but has 16 word errors against 220 reference words. This is a worker-boundary
test: the production capture actor still caps live recording at 105 seconds within
its 180-second overall deadline. No capture limit or admission control changed.

New editing regressions replay the production-handler drafts and literal finals
through native Notes with selection replacement/undo/redo and a foreign Wayland
field. They also exercise focus away and back, explicit cancellation, late
callbacks and a successor session in each recorded language. The long transcript
has separate native and foreign insertion tests. These remain recorded-output
replays, not concurrent recognition and application journeys.

All five production-output insertion regressions pass (0.665 seconds combined;
426 unrelated tests filtered). Python Ruff/syntax and changed-file formatting
pass. The final required `${CARGO_DROP_IN} check --locked -p shell --tests` was attempted
but could not run: the remote builder reported its volume 95% full, above its
80% admission watermark. The same integration test target compiled successfully
for the five-test run (19.52 seconds), but this is not reported as a successful
execution of the required check command. No local compilation fallback or shared
build-data deletion was used.

After the assigned resource owner restored headroom, the bounded required check
was retried against the existing artifacts and passed in 1 minute 8 seconds.
The task did not run shared garbage collection or a fresh full build. A bounded
two-second disk observer recorded aggregate free space from 114700836864 bytes
at the start to a sampled minimum of 104898965504 and 108880007168 at completion.
The sampled peak aggregate growth was 9801871360 bytes; this measures the shared
volume and is not attributed to this compile. The observer stopped explicitly.

### Default automatic-language qualification

The [automatic-language ledger](dictation-whistle-aot-image-auto.json) exercises
the setting the user receives by default, using the same existing production
worker and four public recordings. There was no Rust rebuild, microphone or
activation. Ready includes ephemeral fixture setup, approximately 1.11 seconds.
Final delivery after Ready takes 11.848 seconds for English, 21.152 for German,
11.392 for French and 15.255 for Spanish. Every literal final equals its
explicit-language counterpart byte for byte; raw word errors remain 4/19,
6/18, 0/20 and 2/29 respectively. The existing production-output insertion
tests therefore cover these same final bytes; these runs do not add concurrent
recognition-to-application evidence.

All four observed one native thread, with maximum VmPeak 204164 KiB and no
guard or cgroup memory-event increase. All four scopes were inactive after
observation. The first nonempty German, French and Spanish previews were the
invented English phrase “Thank you.” around 2.6 seconds after Ready, before the
later hypothesis switched language. Raw drafts remain in the ledger. Automatic
detection reaching the same final is not evidence that its early preview is
accurate, nor qualification of languages outside these four recordings.

The adjacent [JSON ledger](dictation-whistle-aot-review.json) records source-built
runtime/compiler/artifact digests, exact Rust/bridge source hashes, raw receipt
hashes, measured public replies, fixture custody and cleanup. First-batch and
earlier prototype receipts are identified rather than relabeled as final-source
measurements. The held branch preserves the rejected V8 profile unchanged.

Build the official source C API in its separate checkout through ${CARGO_DROP_IN}; compile
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

Still unqualified: live microphone/account permission, complete concurrent native/browser/
terminal journeys **with this engine**, older physical devices, ARM,
engine-specific language/dictionary revocation,
and live immutable runtime/model installation and activation. Production source
admission and the isolated synthetic image are now exercised, while the package
candidate remains uninstalled.
Existing insertion/undo and
revocation regressions are transport foundations, not new acoustic evidence.
The backend is default-off and not selectable on the production model catalogue.
No model activation, deployment, reboot, permission or trust change occurred.
