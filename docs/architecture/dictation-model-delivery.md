# Delivery of a streaming speech model

Status: production Whisper descriptor loading and isolated native worker qualification.
No new model is installed, selected, signed or admitted by this change. The held
speech PR and reserved holdout remain unchanged.

## Separate delivery from decoder admission

The reviewed Nemotron Q8 object is 742090464 bytes (707.71 MiB), SHA-256
`3fc991d3badad7277c11030a7519832cddaf2057aafed6d4b25147e953a070b1`.
Its upstream release is [NVIDIA's pinned model revision](https://huggingface.co/nvidia/nemotron-3.5-asr-streaming-0.6b/tree/ea30d66debe3740a08b573244286791d423d6b3e),
under the reviewed OpenMDW-1.1 terms. Native runtime/build pins and measurements
are in `dictation-streaming-optimization.json`; vocabulary measurements are in
`dictation-vocabulary-characterization.json`. Neither ledger is release authority.

`LocalModelService::fetch_verified` already supports large pinned artifacts:
HTTPS-only transport, offset resume, exact signed size and SHA-256, a private
`partial/<digest>.part`, then content-addressed `sha256/<digest>` promotion.
`ReqwestTransport::resume` and `verify_file` use 32 KiB buffers. Delivering this
object does not require a 742 MB Rust `Vec`, a second downloader, a new cache,
a microphone grant or a network-enabled recognition worker.

The current 256 MiB boundary is `speech::MAX_MODEL_BYTES`, enforced by the
verified Whisper model-file witness and both engine loaders. That consumer
accepts signed `WhisperGgml` releases and calls the descriptor loader. Nemotron is a distinct
GGUF engine; relabeling it as Whisper, llama.cpp or vLLM would be false. The
742 MB candidate remains rejected by the current consumer. Streaming delivery
alone does not change that rejection.

## Concrete integration sequence

1. Qualify the native backend and its signed runtime inputs before adding a new
   explicit speech artifact format. Its capability must be speech recognition,
   never a chat runtime. Keep Whisper's format, loader and 256 MiB limit intact.
2. Have the existing catalogue authority publish the exact format, revision,
   digest, size, licence and resource requirements after release qualification.
   Use the existing per-model/version/digest licence acceptance record. Do not
   create an alternate local allowlist or turn a review ledger into a catalogue.
3. Serve the exact object through the existing HTTPS artifact publication path.
   The production transport refuses redirects: a mutable Hugging Face resolve
   URL is not a drop-in delivery URL. The publisher must provide an immutable
   approved endpoint that returns bytes directly, with valid range responses.
   No endpoint, signature or persistent access grant is created here.
4. Reuse `queue_install`, progress state, cancellation, resumable partials,
   verification, removal and disk admission from the existing service. A queued
   or cached object must not count as installed or ready. Keep one final object
   after promotion; interrupted transfers retain only the existing partial.
5. Introduce a reviewed descriptor-based native model loader. Verify the signed
   receipt on the same open regular file and establish an immutable snapshot
   before native parsing; do not verify a path then reopen it. Landlock and
   seccomp must continue to deny network and ambient file access. Native GGUF
   loading from a descriptor needs a supported upstream or reviewed adapter;
   `/proc/self/fd` and broad path grants are not assumed to work under confinement.
6. Propose a separate explicit Nemotron artifact-size policy only after that
   loader and the complete confined worker fit the unchanged 1 GiB address-space
   limit. This requires review/approval; this change does not raise the limit.
   Avoid an extra full-model byte buffer or unbounded anonymous snapshot. Include
   snapshot/cache page costs in physical 4–8 GiB device tests, even when they do
   not increase the worker's virtual size.
7. Run original-field ownership, dictionary revision/revocation, selected-language,
   silence/failure, cancellation/repeat and latency journeys using the production
   worker before readiness or microphone capture. Existing experimental native
   results do not establish those guarantees. Keep release held until accepted.

## Verified delivery hardening

The existing `verify_file` now opens with no-follow/nonblocking flags on Unix,
checks regular-file type and exact size on that descriptor, and hashes at most
`size + 1` bytes. The extra byte detects concurrent growth; exact observed length
and digest are both required. The buffer remains 32 KiB irrespective of model size.
A focused production `fetch_verified` regression rejects a digest-matching cached
symlink, a directory and an oversized file, while accepting the real cached object
without a transport call. This strengthens existing trust rather than adding an
exception for the candidate.

## Bounded validation needs

- The focused cached-object regression and existing resume/progress regression
  use tiny synthetic fixtures, no network, no model weights or microphone, and
  two remote compile jobs. Runtime work is sub-second; compilation is separate.
- A future delivery-size check can use one sparse 742090464-byte synthetic file
  and a generated zero-stream digest, never binary weights in Git. Hashing it
  requires a fixed 32 KiB buffer and two linear reads (digest fixture plus actual
  verification), no decoding, with a 1 GiB process limit and one CPU. Reserve at
  most two minutes; record actual time/RSS before making a performance claim.
- The pending native development stress set is six paced cases, two CPUs, one
  at a time, 1 GiB address space with a 960 MiB guard, roughly 18–40 seconds per
  case. Fresh memory pressure must remain at or below 5 and available memory at
  least 8 GiB before each case. The latest continuation stopped at 9.95 before
  its first decoder; do not cycle retries to obtain a slot.
- The default vocabulary case completed within those same native guards:
  0% WER versus 5.56% off/weak, no unspoken TiLo, and about 5.33 seconds to first
  meaningful stable draft on one exposed German clip. This is a development
  result, not holdout, noisy-name or production-service acceptance.

Remaining integration work is the explicit native format/runtime and its
qualified artifact-size policy, followed by physical/full OS acceptance. Backend
implementation is authorized; release/signing/installation remains held. The
existing Whisper cap is unchanged.


## Production descriptor loading implementation

The worker now obtains an immutable, receipt-checked sealed descriptor from the
same signed catalogue and acceptance checks. `speech_model_file` streams the
exact source through a32KiBzeroized buffer into a private sealed memfd; all
write/grow/shrink/seal mutations are locked out before native parsing. The
Whisper adapter uses C++ descriptor callbacks after full Landlock/seccomp
confinement, never reopening a model path or allocating a whole-model Rust input
buffer. The descriptor is dropped before decoder arenas are prepared. The byte
API remains only for compatibility/qualification.256MiBadmission and1GiBRLIMIT_AS
remain enforced; the742090464-byte candidate is explicitly rejected by a test.

Actual production pipe proof: the pinned public base-model English clip passes
Ready/drafts/final and rawWER5.26% under the verified1.25GiBtransient scope.
The worker peaks at870792KiBvirtual/354664KiBRSS; aggregate scope peak617578496B
and zero high/max/OOM events. This is pressured-builder development evidence,
not physical latency or native Nemotron acceptance. Original readiness and
observation failures are preserved in `dictation-sealed-loading.json`; the
pinned parser's exact-EOF probes are handled without accepting truncated fields.

No automatic review has denied native backend implementation. Choosing and
implementing that backend is within the dictation request. The actual remaining
policy constraint is the existing instruction to preserve resource limits: a
distinct742MBartifact allowance would be a change to the current256MiBconsumer
boundary. Catalogue signing, installation, release holds and deployment also
remain unchanged. Do not call an ordinary backend design choice an approval gate.

For the large candidate, a full anonymous sealed copy would add707.71MiBof
physical pages while the decoder loads roughly774MiB, which cannot be assumed
to fit the verified1.25GiBcgroup. The next implementation should establish
immutability on the existing cached object and read its descriptor directly,
then measure actual memory. [Linuxfs-verity](https://docs.kernel.org/filesystems/fsverity.html)
provides immutable verified reads for supported filesystems; its Merkle digest
is distinct from the signed rawSHA-256, so retain the raw receipt check after
immutability is established. Filesystem support must be observed, not assumed;
no fs-verity operation or file-size exception is enabled by this increment.

Pinned NeMo source already has `gguf_init_from_file_ptr` and callback APIs. Its
`GGUFLoader` currently opens the path separately for metadata and tensor reads,
and reopens it after releasing resources. A native descriptor adapter must
replace all those opens, own its duplicated descriptor, and fail if weights
are requested after its immutable input has been released. Merely passing a
`/proc/self/fd` path would still reopen through the filesystem and is insufficient.
The separate native worker must avoid linking conflicting ggml generations into
one process; no dynamic engine code is loaded from model bytes.

## Pinned native descriptor adapter

`tools/release/fixtures/nemo_descriptor.patch` now implements that loader seam
against NeMo-Speech.cpp `8642eaa5cc51efbc17ad0f3e433944ba858a873f`.
The internal `ModelConfig::descriptor` reaches the real recognizer/model factory
without becoming a CLI parameter. Metadata and tensor reads share one duplicated
read-only regular-file descriptor; the path is unused in this mode. Routine
reader-buffer release retains an immutable anchor for upstream lazy sessions.
Explicit input retirement prohibits subsequent tensor reads. Tensor range checks use subtraction
to reject overflowing offsets. Existing path callers retain their API defaults.

`tools/release/qualify_nemo_descriptor.py --out <fresh-directory>` compiles the
actual loader, checks the model and recognizer call sites, and runs a tiny GGUF
through the production reader. It checks caller close/unlink, exact tensor bytes,
writable and invalid descriptor rejection, overflowing reads, and released-input
failure. The strengthened lazy-anchor run passed in 3.093 seconds. Exact original and
patched source hashes are in `fixtures/nemo_descriptor_sources.json`; the helper
checks them before and after, copies only a separate source overlay, and leaves
upstream sources and shared build objects untouched. The initial duplicate-header
overlay failure remains in the evidence log; the corrected overlay passed.

This is a tested native loading adapter; normal production selection is held. Its caller
must establish immutable input before invoking it: read-only access alone does
not prevent another writer. The isolated worker now verifies a kernel-immutable cached-file witness and
uses the existing sandbox, protocol and resource journeys, as described below. No backend implementation
approval is pending. Actual release/signing/installation remains held, the
Whisper 256 MiB admission remains unchanged, and neither the model nor audio
was used by this adapter regression.

A read-only `FS_IOC_MEASURE_VERITY` query on the existing 742090464-byte review
object on the original review filesystem returned `ENOTTY` (25). That
filesystem could not supply this witness; no mutation was attempted in that
query. The existing btrfs filesystem subsequently resolved this limitation.
`dictation-native-descriptor.json` records the adapter and storage evidence.
The historical receipt remains unchanged; the successful btrfs qualification
is recorded separately below.

## Actual native worker qualification

The builder's existing home btrfs filesystem supports fs-verity. A disposable
fixture enabled it and rejected writes, then an owned copy of the already
reviewed model established the same kernel witness. Its exact raw SHA-256 was
verified after immutability. No privileged mount/configuration change, download,
model installation, catalogue signature or access grant was needed.

The shared production handler now accepts a decoder session while retaining its
strict Configure/Audio/Draft/Finish parser, sequence checks, bounded replies and
zeroized PCM frames. Default builds retain Whisper. The explicitly selected
`nemo-review` example builds without Whisper and refuses combined linkage; the
actual binary contains no Whisper symbols. A pinned bridge is built inside the
one native backend generation, with the complete versioned ggml dependency
closure verified before execution. No engine code comes from model bytes.

Native parsing follows full existing Landlock/seccomp confinement. The private
review witness admits only the fixed reviewed size/digest, requires fs-verity,
and hashes the same descriptor with a 32 KiB buffer. Native lazy sessions retain
the descriptor anchor rather than reopening a path. A bounded cache-aware warmup prepares resources before Ready: 100 ms of
silence through the real runner, followed by step/finalize. Its temporary state
is destroyed before the fresh user stream. The earlier qualification used the
upstream four-second server warmup; the measured optimization is below. New PCM is pushed once, endpoint finals accumulate
before the current draft, and native vocabulary consumes the existing bounded
SpeechConfig spellings with at most 64 tokenizer tokens.

Actual English and exposed German vocabulary streams completed under 1 GiB
RLIMIT_AS, the 960 MiB guard and the verified 1.25 GiB scope. German raw WER was
0%, with no unspoken TiLo or observed non-prefix revisions. English raw WER was
21.05% under the existing strict number-preserving scoring. Startup was about
6.3 seconds; first partial after capture was 2.63 seconds in English and
5.39 seconds in German. These are development results, not release acceptance.

Cancellation after Ready and a draft request terminated the owned worker. A
fresh empty-vocabulary session produced empty silence drafts/final; an unknown
selected language returned a typed failure before Ready. Model file-cache pages
were advised DONTNEED only on the owned object before each fresh scope. The
scope observed memory-high reclamation, peak about 1.126 GiB, and no max/OOM
events. Worker virtual peaks stayed below the 960 MiB guard. This does not prove
physical 4–8 GiB/ARM behavior or cold-device IO isolation.

Exact source/binary/dependency hashes, positive runs and retained failed launches
are in `dictation-native-worker.json`. The immutable anchor fixes a failure found
only by the actual worker: upstream routine buffer release precedes later lazy
weight loading. The complete native format/runtime release, startup/formatting
quality, broader natural speech and renderer-owned cancellation remain pending.

The default production Whisper worker also completed the same paced English
stream after the shared-handler change: 5.26% raw WER, 207 ms preparation,
3.10 seconds to first partial after capture, and no memory-high/max/OOM events.
Focused speech tests passed (10 tests); default all-target and native-only
Clippy passed. The native review feature remains outside normal consumer builds.

## Measured startup optimization

The worst measured clean-language case is English (21.05% raw WER). The
slowest meaningful stable text is German; the worst earlier exposed condition
was German synthetic 10 dB noise (22.22% WER). These are exposed development
cases, not holdout acceptance.

The native bridge replaces the server's repeated four-second silence warmup
with one bounded initialization chunk through the public cache-aware runner.
Weights and an inference graph still prepare before Ready; the synthetic state
is destroyed before the actual user stream. No trained context, graph capacity,
model, vocabulary, trust check or runtime cap changes.

Paired actual-worker runs reduced English preparation from 6.368 to 5.175
seconds and startup through meaningful stable text from 9.688 to 8.490 seconds.
German preparation fell from 6.263 to 5.091 seconds and total time from 12.737
to 11.559 seconds. Capture-to-meaningful-stable timing stayed approximately
3.32 seconds for English and 6.47 seconds for German. Exact final transcripts
matched privately; raw WER stayed 21.05% and 0% respectively. Thus the gain
removes preparation work rather than shifting it to the first user draft.

`dictation-native-startup.json` records exact before/after library hashes,
input hashes, strict scoring, retrospective prefix stability, scope events and
memory. No max/OOM events or guard failures occurred. File-cache reclamation
and pressured-builder timing remain recorded; these measurements do not
establish physical-device performance or solve the English accuracy gap.

The worst exposed condition was also paired on the actual worker: German at
seeded synthetic 10 dB noise retained 22.22% raw WER, with first stable text
at about 6.34 seconds after capture. Preparation improved from 6.448 to 5.235
seconds, and startup through that stable text from 12.793 to 11.578 seconds.
Exact noise/PCM hashes and final transcripts matched between variants. This
confirms the preparation gain on the current worst measured condition while
preserving its accuracy failure as a release limitation.


## Phase-separated preparation and completed-session reuse

The actual private worker now reports bounded verification and native preparation
counters only under `--phase-review`. The public probe supplies PCM every20ms while
inference is busy, matching the service's two-second initial draft and one-second
repeat cadence. [The phase ledger](dictation-native-phases.json) separates owned-file
cache advice, fresh-worker cache repeat, resident-model repeat, and audio delivery
to retrospectively stable text. Pipe-write timing does not measure microphone or
hardware latency. `DONTNEED` advice does not establish physically cold storage.

Full raw digest verification remains mandatory on the fs-verity immutable descriptor.
A private fixed-Merkle shortcut and a sequential-read/no-hash variant were rejected:
they moved work into lazy native loading without a consistent end-to-end gain.
Neither shortcut is present in the landed worker source.

The review worker accepts at most two completed sessions with `--review-sessions 2`.
It retains the verified recognizer, destroys the old stream before acquiring the
single supported decoder slot, clears accumulated transcript and sample count,
and creates a fresh language/vocabulary stream. Failure or cancellation exits the
owned worker; reuse follows only a successful Finish. Default execution remains
one session. This is a bounded review path, not a production persistent service.

In paired public English sessions, Configure-to-Ready fell from5203ms to7.5ms;
German fell from5076ms to11.3ms. Each repeated final was identical. English remained
4/19 strict raw errors, German0/18; cleanup and word order were unchanged. German
with vocabulary followed by English silence with empty vocabulary returned empty
drafts and final. All six sessions stayed under existing process/scope guards with
zero hard-limit/OOM events. Scope cache reclamation and builder timing limitations
are recorded; these results do not qualify physical older devices.

Model reuse removes preparation from repeats, while capture-to-meaningful stable
text still takes about3.3s English and6.3s German. The English errors include three
numeric representation edits and one inflection error. Do not erase them with
number normalization. Production residency, recognition quality, earlier stable
streaming, and physical/full OS acceptance remain outstanding.
