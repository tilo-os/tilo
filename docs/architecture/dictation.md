# System dictation

Status: the local shortcut, one-session microphone prompt, finish/cancel controls,
compositor worker controller, native model lifecycle and private CPU worker are
assembled. The approved image catalogue still contains no speech model, so this
branch cannot provide a live dictation journey on the shipped image yet.
The microphone capture API has not opened a real microphone during qualification.
Do not advertise “works in any field” until the approved model and representative
live journeys are validated. No microphone permission has been granted by this work.

## Existing boundaries

The compositor already arbitrates exactly one trusted input method per seat
(`compositor/input_method.rs`). Its text commit path inserts picked emoji through
an existing owner or briefly lends an idle seat to an in-process connection
(`compositor/text_commit.rs`). App connections cannot choose this credential.
The text-input request observer tracks field-declared secret purposes/hints.

Smithay discards text-input requests when no input method is running. Thus a
speech service must first establish the trusted session and wait for field
activation. A surface with keyboard focus alone does not prove that its current
control is editable, nor that it is safe to dictate into. Arbitrary key injection
or clipboard replacement would bypass this evidence and can run commands in a
terminal. Neither is a fallback.

PipeWire capture exists in `compositor/audio_capture.rs` for screen recording,
using server-assigned source identity. It drops chunks when its bounded queue
fills. The new dictation mode instead fails on queue overflow and requests 16 kHz
mono S16LE directly from PipeWire, which performs conversion and resampling.
Reader EOF or failure is an interruption even before the process reports exit.
The recorder subprocess has a distinct dictation node identity. Drop terminates
and reaps only its owned process group and joins the reader; failed reader startup
also reaps the child. Dictation executes the immutable `/usr/bin/pw-record`
with kernel parent-death termination armed before exec, so a crashed compositor
cannot leave its microphone running. Its spawning thread must remain alive for
the capture lifetime. Source probing must run off the compositor thread.

Live captions already report unavailable until the image includes a local
speech engine/model lifecycle (`system-settings-services.md`). Dictation should
share that lifecycle, while selecting only the microphone the user requested.

## Implemented insertion contract

`compositor/dictation.rs` exposes begin, update, check, finish and cancel methods
on compositor `State`, plus cancellable asynchronous field preparation. This API
is for the trusted local speech service; it is not an unprivileged socket operation or an app capability grant.

- Prepare retains an existing OSK/composer owner, or establishes a bounded
  in-process input method on an idle seat. Activation must complete before any
  capture starts. Begin requires an enabled, non-secret field.
- Each request has a never-reused session ID. Late results and cancellation from
  an old request cannot mutate its successor.
- Streaming hypotheses replace a bounded ephemeral draft. Correction happens
  before insertion; it never backspaces text the user or a collaborator wrote.
- Finish commits UTF-8 once through the original trusted session. Empty speech
  produces no edit. Nothing enters clipboard history or emoji recents.
- The target includes Wayland field and surface object identities, the field's
  committed edit revision, and the input-method owner. Enable/disable, surrounding
  text, change-cause, content-type and changed caret geometry invalidate it.
  Identical caret updates and empty commits keep the target. No surrounding-text
  strings are retained.
- Any keyboard-focus change immediately drops the draft, even away and back
  before the next recognition result. Changing input-method ownership does too.
- The native service tick checks liveness and drives the owned worker actor.
  Any invalidated target stops recording/inference; callers of the low-level
  insertion API remain responsible for their own capture lifecycle.
- A request is bounded to 120 seconds and 16 KiB of draft UTF-8. A final
  is limited to 4,000 UTF-8 bytes so one commit fits the standard Wayland
  4 KiB message buffer. Larger finals enter private recovery before sending any text; splitting
  would lose atomic selected-range replacement and undo. Native final delivery
  drains the existing IME bridge and dispatches it in the same compositor turn,
  before another edit can move the selection. An incomplete delivery disconnects
  that bridge instead of retrying after a focus or selection change. Control characters,
  including line breaks, are rejected in this first boundary. Spoken paragraph
  commands require a field-aware extension; they must never submit a terminal.
- Oversized finals remain under their original session ID in zeroizing memory
  for ten minutes. The local controls visibly offer Copy transcript and Discard
  transcript. Copy explicitly exports the complete text to the system clipboard;
  it never pastes, synthesizes keys or retargets insertion. Failed export retains
  the text and offers retry. Focus changes cannot insert it elsewhere. Cancel,
  lock and expiry discard it; start/finish shortcuts cannot silently dismiss it.
  Idle recovery schedules only its expiry, without polling or microphone work.
- No transcripts, audio or surrounding-text strings are automatically logged or
  persisted. Explicit Copy uses the normal system clipboard and its history;
  discarding the private recovery does not erase an already exported copy.
  Errors contain typed status only. Dropping a draft is not a secure memory wipe.

The bounded Phonon event decoder handles partial replacement, consecutive final
segments and a complete `done` result. Engine errors are mapped to a typed status
without exposing upstream copy. Oversized results, replayed segments and control
characters fail closed. Non-English language choices are rejected explicitly.
This decoder is not yet attached to an authenticated Unix WebSocket connection.

The wire journeys in `tests/emoji_picker_commit.rs` exercise production `State`
with actual Wayland client connections. They do not measure recognition quality,
actual microphone access, native app CRDT history, or live visual status.
The native-chrome journey in `tests/input_method_wire.rs` additionally exercises
the existing `ShellTextInput` stand-in: corrected dictation becomes one native
`WindowEvent::Ime(Commit)` after finalization. That final event also drives a
real Notes component through its shared rich-text field and owning action
dispatcher: selected-range replacement preserves the surrounding text, one
system undo restores the selection's prior text, and redo reapplies the result.
This does not prove microphone capture or universal app/accessibility routing.
The native host now supplies a layout-generation-bearing widget identity or
the palette's keyboard ownership to that bridge. An owner change cancels the
session immediately and resets its text-input enable cycle, including switches
between two fields with identical caret geometry. A wire journey proves that
late results from the old session cannot reach the successor and that a fresh
session can commit there. While a dictation target exists, the native service tick
also checks the admitted record owner, its generation and read-only CRDT frontier
and pending-operation count. A keyed, ephemeral edit fingerprint covers text and
selection, including selection changes with an unchanged caret. The key is local
to the host; neither the fingerprint nor surrounding text is exposed in status.
Secret fields and fields with another active preedit fail closed. Shared query,
name and compound input controls forward the same guard; rich document targets
include paragraph/cell identity and cross-block selection. Stamps belong
to a dictation session, so an edit between sessions cannot invalidate a fresh one.
The shared TextInput implementation borrows its text for hashing; wrappers
forward the same contract rather than building another input. Terminal declares
its editable IME purpose and invalidates deferred input on PTY input/output,
including a write before echo or output with an unchanged caret.
Record edit/inverse/retirement and selection/secret-field tests exercise these
production guards. The Notes wire journey additionally proves final IME text
through selected-range replacement and owned history, without live capture.
Separate production-reader tests cover queue overflow, source EOF and owned-process teardown
using synthetic PCM/blocked pipes; they neither capture audio nor run inference.

## Candidate research (2026-10-05)

### Wispr Flow comparison (official sources checked 2026-10-06)

[Wispr's feature catalogue](https://wisprflow.ai/features) advertises dictation
across applications, over 100 languages, spoken corrections, filler removal,
punctuation, dictionaries and context-dependent styles. Those are qualification
targets, not capabilities established by this branch. This implementation has
bounded partial recognition, final IME insertion, configurable toggle/finish/cancel
shortcuts and one-session local consent. A spoken correction interpreter,
dictionary, language/source chooser and hold-to-talk interaction remain unbuilt.

[Wispr's terminal guidance](https://docs.wisprflow.ai/articles/6478598909-using-flow-with-linux-wsl-and-terminal-applications)
describes clipboard recovery and cautions that contextual formatting can change
commands. Tilo keeps insertion on the original trusted IME connection, excludes
control characters and rejects a changed terminal epoch. Command safety and
recognition accuracy still require representative live qualification.

The user's exact bookmark URLs were not available. These are public candidates,
not confirmed bookmark identities. That initial review imported no code or
weights; the later CPU-worker qualification below records the exact vendored
runtime and the public test model used on the builder.

[0xDAEF0F/whistle](https://github.com/0xDAEF0F/whistle) describes a Tauri app with
transcription, shortcuts, clipboard integration and DeepSeek text polishing; its
README claims MIT licensing. That is useful interaction precedent but does not
establish a private, compositor-native Linux insertion boundary or an audited
offline pipeline. Verify its actual dependencies, network behavior and license
files before adopting code. The upstream instructions to remove macOS quarantine
or reset permissions are not appropriate Tilo integration steps.

[Fermion Research Phonon-2](https://huggingface.co/FermionResearch/Phonon-2) is an
English ASR model with CC-BY-4.0 weights. Its published small download is promising
for older machines; download size does not establish runtime memory or latency.
Keep attribution/NOTICE with the exact artifact. It is a plausible match for
“phonnon 2”, not a confirmed identity or a multilingual default.

[Phonon runtime](https://github.com/fermionresearch/phonon) publishes CPU paths for
x86-64/SSE4.1 and Arm64/NEON, with an Apache-2.0 CLI. Its
[server protocol](https://github.com/fermionresearch/phonon/blob/main/docs/server.md)
documents streaming partial/final results and an owner-only Unix socket. Its
one-shot `language` parameter is accepted but unimplemented: do not display a
language selector that silently ignores the chosen language. Pin the exact
runtime/model revisions, audit socket peer ownership and permissions, and cap CPU
threads independently of upstream defaults. Do not open an unauthenticated
loopback port to sandboxed apps.

[whisper.cpp](https://github.com/ggml-org/whisper.cpp) remains a candidate for
multilingual local inference. Its
[streaming example](https://github.com/ggml-org/whisper.cpp/tree/master/examples/stream)
demonstrates rolling recognition; it is not the required production capture,
cancellation or insertion service. Benchmark an actual model/runtime combination
on the older laptop and Arm board before choosing defaults.

## Release integration dependency

`dictation-upstream-pins.json` records an exact model revision,
LFS SHA-256/byte count and verified public OCI index/architecture/config digests.
Only small metadata was downloaded. The runtime's compressed layers alone are
about 419 MB on x86-64 and 381 MB on Arm64, in addition to the 164 MB model.
These receipts are review inputs, not a signed production catalogue. The image
has no source-revision labels; its source/build provenance and installed runtime
version remain unverified. No image layer or model weight was pulled or executed.
The checked-in `local-model-service` manager currently assembles `VllmCommand`
for runtime-served models, and admits `hf_snapshot_tar_zst`, `vllm_gguf` and
in-process `onnx_file` and `whisper_ggml` formats. Its signed
catalogue payload and verifier are internal to its CLI. It has no speech engine
launch verb or checked-in speech release. Extend that shared lifecycle rather
than inventing a second unsigned installer or an ambient executable path.

Current main supplies the existing release route in
`.forgejo/workflows/release-local-model-catalog.yml`, documented by
`crates/agent/local-model-service/RELEASE.md`. Its production catalogue source is
`dist/local-models/catalog-source.json`, and its public key identifier is
`tilo-local-model-release-2026-10`. Immutable Forgejo package URLs already host
the approved U²-Net model/license and signed catalogue. No new trust identity or
account is needed for speech.

The protected publication job currently enforces exactly the U²-Net release,
an empty runtime list, and byte-for-byte agreement with checked-in signed image
inputs. It cannot publish speech as-is. Review an extension of that existing
job/catalogue and the matching speech manager format/engine/capability; retain
artifact provenance, license receipts and an approved runtime base-image
identity. Signing material stays in the protected publication job, never in
chat or agent output. Do not dispatch publication while this work is on hold,
add a trusted development key, or substitute an unsigned executable path.

The lifecycle now also admits a typed in-process `whisper_ggml` object, whose
signed format declares `speech_recognition`. It goes through the same license,
hash, queue, installation, removal and capability lookup as other native
models, and never enters the chat supervisor or recommendation. No fields were
added to existing signed model records; the checked-in image catalogue is
verified with its existing public trust input in the regression test.

`dictation-whisper-review.json` pins a multilingual tiny-model candidate by
upstream revision, LFS hash and size, plus the exact binding source archives
reviewed. No weights or native dependency were installed. The exact weights and licence
were subsequently downloaded into an isolated remote review directory and both
full byte counts and SHA-256 hashes verified; this was staging, not installation. The reviewed
whisper-rs 0.16.0 `set_abort_callback_safe` erases its closure into a boxed trait
object, but its trampoline casts that pointer back to the generic closure type
and does not retain ownership. Do not use that helper. A raw C API adapter needs
an explicit callback lifetime and process-isolation audit; native CPU build
flags also needed a portable baseline before adoption. The later CPU adapter
below uses its own narrow C ABI and isolated worker, rather than that Rust
callback helper. Native keyboard-owner identity and document edit/selection
stamps now come from the shared host input contract described above.

## Remaining release and acceptance gates

1. Approve the exact speech model, licence receipt and signed catalogue release
   through the existing protected publisher. The review inputs are prepared;
   no protected signing, publication, trust change or model installation has run.
   A positive installed-worker CLI journey needs a non-root image session;
   the remote builder's root-only development override is deliberately refused.
2. Validate live microphone permission denial, device removal, cancellation and
   model failure on the image, followed by representative native text, rich
   document, Terminal and browser journeys with selected-range replacement and
   system undo. Current actor, worker-core and Wayland tests prove separate
   production boundaries, not a complete assembled microphone journey.
3. Qualify recognition on public noisy and non-English recordings, and measure
   an older CPU and Arm device. The current public JFK fixture and two-thread
   CPU measurements are narrower evidence, not multilingual accuracy claims.
4. Inspect the complete local consent/status surface in light, dark, high
   contrast and enlarged text, and verify touch/switch access and screen-reader
   announcements. Toggle/finish/cancel work through shared controls; a hold
   gesture, explicit language chooser, mic-source display and editable draft
   review remain product extensions. No opaque LLM rewrite is implemented.
5. Confirm remote collaborators cannot activate local microphone consent through
   the existing input firewall and action authority boundaries. The production
   remote-input publisher now mutes shared-seat input while dictation controls
   are visible, without withdrawing the original local field's IME owner.
   A publisher regression covers opening and closing those controls. Starting
   through the action door only requests consent; stale decisions are
   session-bound. This is narrower than a live remote-collaborator journey.

These gates do not justify adding clipboard/key injection, accepting an unsigned
model, relaxing the worker sandbox, or letting readiness imply microphone consent.

## CPU worker and qualification

`tilo-speech-worker` verifies the image catalogue using the existing release
trust, the installed speech capability, exact revision, licence acceptance,
object size and SHA-256 before loading any native bytes. It then closes model
files, confines itself with full Landlock and the shared parser seccomp policy,
and loads weights through a synchronous six-parameter C++ bridge. The worker
opens no microphone or network connection. Elevated processes refuse developer
catalogue/store overrides. Core dumps and process tracing are disabled, address
space is bounded to 1 GiB, and parent death terminates the worker. The compositor
must spawn it from a thread that remains alive for the session, own its process
group, cancel/reap it off the render thread, and validate the target before each
reply. These parent/capture responsibilities are still unassembled.

The CPU source is an unchanged pinned whisper.cpp 1.8.3 subtree, with a complete
per-file receipt and MIT notice under the model-service package. The receipt
records the exact crates.io archive it came from; it does not claim the subtree
was proven equal to a Git release. No whisper-rs callback helper or Rust binding
is used. GPU, dynamic backends, OpenMP and runner-native CPU tuning are disabled;
x86 follows the existing OS x86-64-v3 floor and Arm targets armv8-a. No local
Cargo compilation or live microphone capture was used for qualification.

The private framed protocol admits monotonically sequenced configuration,
PCM16 mono at 16 kHz, draft replacement and one terminal Finish. It rejects
replay, gaps, malformed/oversized frames and more than 120 seconds of audio.
Transcript replies are bounded to 16 KiB and native diagnostics are discarded.
The engine normalizes segment whitespace; spoken Enter is not a command. Draft
recognition currently recomputes the bounded phrase, so the parent must allow
only one pending draft and rate-limit updates before this is usable for long
sessions. Language IDs are bounded by the loaded model's own vocabulary.

Remote public-fixture qualification passed partial/final decoding, automatic
English detection, independent repeat decoding, silence, and language bounds
under production confinement. The `--worker-core` journey runs the actual production handler in a separate,
uninstalled qualification child with an ephemeral signed fixture. It verifies
Ready, PCM streaming, draft/final replies, busy cancellation, reaping and a new
session after cancellation, with the same process limits and confinement. The
installed CLI's elevated override rejection was observed separately and remains
intact; `--worker` needs an unprivileged runtime for positive fixture execution.
Neither journey proves multilingual accuracy or any live microphone flow.

`dist/local-models/catalog-source-speech-candidate.json` and
`tools/release/prepare_speech_catalog.py` prepare receipt-checked review inputs
and the complete OpenAI MIT notice without signing, publishing, changing a trust
key or activating the image catalogue. The protected publisher remains unchanged.
Publication and normal image delivery require their separate approval.

## Compositor worker controller

The shell shares only bounded worker wire types through `agent-provider-config`;
it does not link the decoder or its native build. An owned actor performs source
probing, recording, pipe I/O and child cleanup away from the render thread. It
executes only `/usr/bin/tilo-speech-worker`, uses private pipes and ordered framed
messages, bounds pending audio to 256 KiB and replaces private hypotheses at most
once per second. Model readiness alone never starts recording: an explicit
capture command follows the caller's local microphone decision and target check.
Cancellation is checked before and after the bounded source probe. The compositor
retains the busy actor until its capture and worker have actually been reaped;
a successor cannot overlap them. Recording reserves 15 seconds for finalization
inside the 120-second session budget. Lost audio, malformed/replayed replies,
timeouts and failed queues cancel insertion and return typed status only.

Real private-pipe actor tests prove normal/canceled-busy/normal sessions, replay
rejection, control-character rejection and child reaping. These peers do not
recognize speech and do not capture a microphone. The separately documented
production-worker qualification uses public prerecorded JFK audio under the
actual sandbox. The controller is reached through the shell's existing shortcut and overlay stack.
`Super+Shift+D` toggles the request/finish operation; the rebindable finish and
cancel defaults are `Super+Alt+D` and `Super+Shift+Escape`. Escape cancels an
open dictation surface. The action provider registers `dictation.toggle`,
`dictation.finish` and `dictation.cancel` through the same configured action path.
A toggle never grants microphone access: it prepares the non-secret editable
field and opens a local one-session decision on the shared PermissionPrompt.
Session/persistent grants are absent, and pointer activation preserves the
original native or foreign field's keyboard owner. Accessibility decisions carry
the exact prompt session ID; a retired decision cannot start or cancel a successor.
Controls intercept their own pointer region and decision keys, rather than
withdrawing the client's seat focus. Lock and competing trusted prompts cancel
the request. The service wakes only while a target/actor exists, and UI phase
changes invalidate the retained frame. “Microphone off” appears only after the
actor drops and reaps capture. Language autodetection uses the loaded model's
supported vocabulary; no network recognizer, paid API or automatic licence
acceptance is introduced.


## Concrete speech release decision and staged provenance

The selected review candidate is **Whisper tiny multilingual**, rather than the
English-only `tiny.en`. This chooses the smallest unquantized multilingual
baseline reviewed with the CPU worker; it is not yet an accuracy-qualified default.
The [pinned upstream model card](https://huggingface.co/ggerganov/whisper.cpp/blob/5359861c739e955e79d9a303bcbc70fb988958b1/README.md)
identifies converted OpenAI weights and MIT licensing. The complete upstream
copyright and permission notice is preserved in `dist/local-models/whisper-tiny/LICENSE`.
No account, paid endpoint, extra runtime image or new trust identity is needed.

The remote review directory `/tmp/tilo-dictation-review-ce8` contains:

| File | Bytes | Verified SHA-256 |
| --- | ---: | --- |
| `ggml-tiny.bin` | 77,691,713 | `be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21` |
| `LICENSE` | 1,063 | `b5d65a59060e68c4ff940e1eddfa6f94b2d68fdf58ed7f4dd57721c997e35e9d` |
| `provenance.json` | 895 | `bd78051261f7c46309e4a52c8d3a8fdb6a31f6bd2c67e3fd17060c258b3b17e3` |

These are public review inputs under private directory permissions. The directory
is temporary builder storage, not a release destination or an installed model.
`tools/release/prepare_speech_catalog.py` prepares a complete review candidate
with unchanged U²-Net inputs and the exact speech receipts. Its offline tests
reject insecure transport, altered receipts, excess bytes and overwriting.

After the candidate is qualified, the concrete release work is:

1. Review `dist/local-models/catalog-source-speech-candidate.json`, the retained
   MIT notice, model receipt, CPU source receipt and public-audio qualification.
   Non-English recognition quality and older-device timings still need evidence.
2. Extend the existing protected `release-local-model-catalog.yml` allowlist to
   these **two exact** models, retaining `runtimes: []`, empty chat recommendation,
   immutable URLs, receipt checks and the existing trust key. General arbitrary
   models, new keys and ambient executables are outside this change.
3. Publish the weight and MIT notice to the pinned `whisper-tiny/<revision>`
   Forgejo package URLs already declared by the candidate. Independently read
   them anonymously and check the exact receipts before signing the catalogue.
4. Let the protected publisher create the signed catalogue and verify its
   unchanged public key. Review the resulting signed image input and reproduce
   it with the existing compiler; do not substitute a development signature or
   relax the publisher's comparison against reviewed image bytes.
5. Deliver those catalogue bytes through the normal image lifecycle, then use
   Settings' existing licence-acceptance and install action to obtain the model.
   Run the nonroot installed-worker journey and separately approved live
   microphone/denial/removal journeys before lifting the PR's hold.

Nothing in this work dispatches the publisher, publishes artifacts, modifies the
production catalogue, installs a model or approves microphone access. The pending
operator decision is approval of this exact reviewed release and its later image
qualification, not a vague grant of model or account access.
