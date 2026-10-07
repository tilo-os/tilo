# Delivery of a streaming speech model

Status: proposed production integration, with existing artifact verification hardened.
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

The current 256 MiB boundary is `speech::MAX_MODEL_BYTES`, enforced by both
`speech_model_bytes` and `SpeechEngine::load`. That consumer accepts signed
`WhisperGgml` releases and calls a Whisper buffer loader. Nemotron is a distinct
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

The remaining blocking decisions are an approved production backend/format and
artifact-size policy, an immutable descriptor-loading design, and physical/full
OS acceptance. They are not solved by increasing the existing Whisper cap.


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
