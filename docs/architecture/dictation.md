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
The dictation stream sets `node.dont-reconnect`, `node.dont-fallback` and
`node.dont-move`: consent is pinned to the probed server-assigned source, rather
than a replacement microphone. These are the documented
[WirePlumber linking properties](https://pipewire.pages.freedesktop.org/wireplumber/policies/linking.html).
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
- A request has a 180-second wall-clock bound, with up to 105 seconds of capture,
  and 16 KiB of draft UTF-8. A final
  is limited to 4,000 UTF-8 bytes so one commit fits the standard Wayland
  4 KiB message buffer. Larger finals enter private recovery before sending any text; splitting
  would lose atomic selected-range replacement and undo. Native final delivery
  drains the existing IME bridge and dispatches it in the same compositor turn,
  before another edit can move the selection. An incomplete delivery disconnects
  that bridge instead of retrying after a focus or selection change. Line feeds
  are accepted only when the original field explicitly committed the Wayland
  Multiline content hint. The owned worker pipe carries that captured policy as
  local metadata; recognition results cannot enable it. Changing the field's
  content hint invalidates the session. Single-line fields reject line feeds;
  every field rejects carriage returns, tabs, escapes, NUL and other controls.
  Shared TextInput and CodeEditor now publish multiline intent through both
  the shell bridge and native Wayland window bridge. Wrappers inherit it only
  from one unambiguous active child at their IME caret; no text value or
  accessibility presentation is read for this policy. Block-document and
  deferred browser fields still need their own original-subfield policy before
  their paragraph journeys can be qualified. Spoken paragraph commands
  remain unbuilt; multiline never makes a Terminal-purpose field eligible.
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
  Errors contain typed status only. Private capture buffers, framed audio and
  drafts use zeroizing owners; explicit insertion and Copy necessarily export text.

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
An additional isolated test ran the immutable image's actual `pw-record` against
a private PipeWire/WirePlumber graph with hardware discovery disabled, as UID/GID
65534 in a read-only, network-free container without host devices or runtime
mounts. A public synthetic 440 Hz tone produced 94,208 PCM bytes. Removing the
virtual source originally left the recorder alive with no EOF; with the pinned
stream properties it exited with status 1 and EOF. This verifies the actual
source-loss boundary without using a personal microphone; physical-device
removal and older-device latency still need separate validation.

## Candidate research (2026-10-05)

### Wispr Flow comparison (official sources checked 2026-10-06)

[Wispr's feature catalogue](https://wisprflow.ai/features) advertises dictation
across applications, over 100 languages, spoken corrections, filler removal,
punctuation, dictionaries and context-dependent styles. Those are qualification
targets, not capabilities established by this branch. This implementation has
bounded partial recognition, final IME insertion, configurable toggle/finish/cancel
shortcuts, one-session local consent, a shared person dictionary and language
selection. A spoken correction interpreter, microphone source chooser and
hold-to-talk interaction remain unbuilt.

[Wispr's terminal guidance](https://docs.wisprflow.ai/articles/6478598909-using-flow-with-linux-wsl-and-terminal-applications)
describes clipboard recovery and cautions that contextual formatting can change
commands. Tilo keeps insertion on the original trusted IME connection and permits
line feeds only under that field's captured multiline policy. Dictation refuses
the text-input protocol's Terminal purpose:
process input does not identify an inner program's editable, command or secret
mode. A caret, terminal epoch or private delivery receipt alone cannot establish
that eligibility. The refusal applies at activation and every session revalidation,
without changing ordinary keyboard IME, emoji insertion or terminal paste.
Terminal support requires an explicit inner-field eligibility/privacy contract,
with edit-mode transitions invalidating the original grant, before live
qualification can admit it. No clipboard or key-injection fallback is permitted.
The current terminal/ush interface exports session effects and PTY bytes, not
an inner editable resource or an atomic replace-and-ack action. Foreground process
identity, echo/canonical flags and process-controlled OSC prompt markers cannot
stand in for that missing contract. A viable extension must bind the inner
resource and its lifetime/revision to the original hosted process, declare edit
versus command versus sensitive mode, revoke on mode/focus/process changes, and
acknowledge actual insertion without submitting a command. The inner editor must
own selection replacement and Undo. This is a source/protocol prerequisite,
not a microphone permission or a new persistent trust grant.

Code's source buffer is a separate action-owned editable resource. It now declares
Normal purpose rather than Terminal, publishes a keyed stamp over its buffer
identity/content and all selections, and refuses preparation during composition
or pending text dispatch. Unfocused Code ignores IME text while still clearing
disabled composition. The terminal PTY keeps its existing purpose and refusal.
Native fixture journeys drive the production IME event path and app actions:
Code keeps preedit out of the CRDT, commits Unicode with system Undo/Redo, and
rejects a commit after focus loss; Notes replaces a selected paragraph, undoes
and redoes the final once, and preserves it after reopening. Code's buffer,
same-caret selection/content and pending-dispatch invalidation are separately
asserted. These hosted app journeys do not establish standalone Wayland receipt
transport, microphone capture, or terminal inner-program support.
The shared CodeEditor used by Typst, Snippets, query and merge views follows the
same distinction: it delegates source purpose and private invalidation to its
existing TextInput, and publishes no dictation target while blurred or read-only.
Its production event-path regression replaces a selection with Unicode and
verifies that a read-only transition refuses a later IME final without editing.
No new input implementation or toolkit crate split is introduced.
An accepted private warm-up acknowledgement only prepares the original field.
When an existing input method owns the seat, activation preserves an explicit
terminal or sensitive-field refusal and drops the preparation rather than waiting
until timeout. Only a field that is still enabling may remain pending. The wire
regression changes content purpose while warm-up is outstanding and verifies
refusal without a Start or Final operation; no microphone or model is involved.

The paragraph boundary's real Wayland regressions verify that an explicitly
multiline field receives one Unicode final containing two paragraphs, with no
draft edits or synthesized keys, and that a changed hint or Terminal purpose
refuses delivery. The owned worker-pipe regression independently accepts LF
only under the captured field policy, rejects CR/ESC/NUL and single-line LF,
and observes every child being reaped. Four focused actor tests and seventeen
dictation wire tests passed on 2026-10-09. These synthetic peers do not establish
recognizer-generated paragraph commands or native/browser multiline propagation.

The subsequent native intent extension adds a boolean to the existing IME
window state and a Wayland window extension that forwards it as the standard
Multiline hint with Normal purpose. Password and Terminal purpose never gain
that hint. Native windows resend it on text-input enter and enable; shell
fields include it in their committed content type. No toolkit crate movement,
new text control, microphone grant or accessibility-value clone is introduced.
The browser runtime's semantics fixture explicitly remains single-line; a DOM
lease must establish browser multiline intent before that path is admitted.

Native-intent qualification on 2026-10-09 passed the real shell Wayland bridge
with plain TextInput, decorated TextInput and CodeEditor policy, literal Unicode
paragraph delivery once and zero provisional edits. The actual shared TextInput
IME-event test replaces a selection without firing its submit signal and checks
single-line, blurred and password intent. The existing decorator forwarding
contract, three shell content-type tests and all five production-output editing
regressions pass. The final required remote shell all-test compile passed in
1m05s. A live standalone winit window, browser paragraph edit, paragraph-only
input, paragraph-specific undo and microphone capture were not run by this
increment; those remain separate journeys.

The bookmarks behind this research were identified on 2026-10-07 from the
owner's X bookmarks (research comment on issue #3543). The candidates below are
those confirmed identities; earlier guesses are corrected. That initial review
imported no code or weights; the later CPU-worker qualification below records
the exact vendored runtime and the public test model used on the builder.

**Whistle** is [Cactus Compute's Whistle](https://huggingface.co/Cactus-Compute/whistle)
([announcement](https://cactuscompute.com/blog/whistle); bookmarked post
<https://x.com/cactuscompute/status/2106083041265562075>): a single 16.9 MB,
2–4-bit speech-to-text model for English, German, French, Spanish, Italian,
Dutch and Polish, with Apache-2.0 licensing. It runs on the dependency-free
[Needle](https://github.com/cactus-compute/needle) CPU engine
([platform builds](https://huggingface.co/Cactus-Compute/needle3)), whose speech
C API is `needle_load`, `needle_transcribe` and `needle_embed`. The one-pass API
accepts at most 30 seconds of 16 kHz mono audio; the pinned review below also
establishes a separate streaming API. Its model card's benchmark chart reports
4.31%/10.49% WER on LibriSpeech
test-clean/test-other, a 21.4% average over its seven FLEURS languages and
11.1 ms to the first token on an Apple M4 Pro CPU. Those are the publisher's
own measurements, not `speech_benchmark` results, and no older-CPU, Arm-board or
1 GiB worker-limit evidence exists yet. Keyword biasing is a candidate mechanism
for a personal dictionary. An earlier revision of this section guessed
[0xDAEF0F/whistle](https://github.com/0xDAEF0F/whistle), a Tauri dictation app
with DeepSeek text polishing; it is unrelated to the bookmark and is not a
candidate.

**Phonon-2** is confirmed: the bookmarked post
<https://x.com/yoitsmanan/status/2107505508655759729> announces its Core ML
package as the default model of Fermion's Detta app, and the
[model card](https://huggingface.co/FermionResearch/Phonon-2) states that
Phonon-2 is the model inside Detta. It is an English-only ASR model with
CC-BY-4.0 weights, distilled from NVIDIA Parakeet TDT 0.6B v3; its card reports a
5.21% average WER over seven English benchmark sets. Its published small
download is promising for older machines; download size does not establish
runtime memory or latency, and its CPU memory use is unpublished. Keep
attribution/NOTICE with the exact artifact. It is not a multilingual default.

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

### Pinned Whistle interface review (2026-10-08)

The [source review ledger](dictation-whistle-review.json) records exact model,
runtime, header and wrapper revisions, artifact sizes and hashes. That interface
review executed no upstream code. The subsequent
[CLI comparison](dictation-whistle-cli-qualification.json) executed only the
pinned native CLI in a network-free, hardware-free review namespace; no model
was installed. The pinned C interface includes
streaming process and stop calls: each call remains bounded to 30 seconds of
16 kHz audio, while successive calls maintain a stream. Its process-global state
requires serialized worker ownership; the header exposes no asynchronous
cancellation or thread-count setter. The existing owned worker must enforce
resource and cancellation bounds.

The pinned Python wrapper's top-level speech functions enable usage tracking by
default, recording an installation identifier and event/version/platform metadata.
Its source excludes prompts and outputs from that payload. Native binary telemetry
has not been established by this wrapper review. The inspected GitHub tree contains
no native engine source, so reproducible native provenance remains unqualified.
Use the existing confined speech worker and catalogue delivery rather than the
wrapper's automatic downloads. Preserve network denial independently of any
telemetry setting. Small artifact size alone does not qualify startup, recognition
quality, physical memory use or older-device performance.

The four exposed CLI cases measured 4/19 English, 7/18 German, 0/20 French and
2/29 Spanish raw word errors under the same conservative scorer as the baseline.
Peak resident memory was 43968–54220 KiB, with two observed threads and no guard
or high/max/OOM events. Accuracy improved in French and Spanish and regressed in
English and German; this does not justify a default change. The CLI emits only
committed pieces and suppresses early uncommitted drafts. Its first emitted
pieces refer to 4–6 seconds of already available audio. Offline pass and wall
timings do not measure microphone or source-paced first-word latency.

`tools/release/qualify_whistle_cli_scope.py` reproduces the pinned review inside
the unchanged transient-service envelope. It reads only four receipt-checked
runtime/model/licence files and one pinned public fixture in its native namespace.
Executable-inode tracking observes the owned decoder even when it changes its
thread name; missing process observation still fails. The direct static-library
C API adapter initially failed to link, then linked using a privately verified
runtime closure. It exceeded the unchanged native thread budget. The subsequent
[release and paced-input review](dictation-whistle-paced-review.md) distinguishes
that archive from the official wrapper-selected 3.2.0 shared runtime, which also
reached 12 threads during initialization before audio submission. Six bounded
source-paced CLI controls located duplicate decoding overhead, incorrect early
hypotheses and incomplete name accuracy; they do not qualify resident streaming.
The C API, cancellation, resident repeats, silence/noise, personal dictionary,
physical devices and production trust remain unqualified.

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

The source extension is implemented: `prepare_speech_catalog.py` validates and
stages U²-Net plus the three pinned MIT Whisper objects; the existing workflow's
`review-speech` mode compiles their candidate with the existing protected identity
and compares its public trust input with the installed key file. The publisher
also supports that four-model source, immutable speech package URLs and anonymous
receipt readback. No separate speech runtime image or new key is required for
in-process Whisper. Both workflow routes require `refs/heads/main`; the held task
branch cannot dispatch them as a substitute for review or integration.

The exact remaining sequence is: integrate the reviewed source; authorize the
protected `review-speech` dispatch using `TILO_MODEL_CATALOG_SIGNING_KEY`; review
its signed candidate and unchanged `keys.json`; activate the approved source and
signed catalogue as the image inputs; authorize the existing publisher using
`RELEASE_TOKEN` for immutable objects and exact signed-input reproduction; deliver
the resulting image through the established release route. The service then
requires the person's per-model/version/digest licence acceptance and explicit
installation. Those release, acceptance and installation actions have not run.
Signing material stays in the protected job, never in chat or agent output.
Do not dispatch publication while this work is on hold, add a trusted development
key, or substitute an unsigned executable path. Source/model evaluation and
prerecorded worker qualification do not require those protected actions.

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

1. Produce and review the exact speech model, licence receipt and signed catalog
   through the existing protected publisher. Candidate-review tooling is implemented;
   no protected signing, publication, trust change or model installation has run.
   Non-root worker CLI journeys passed with ephemeral review keys and stores in
   an isolated image container; these do not install a model or grant trust to
   the shipping image. The root-only development override remains refused.
2. Validate live microphone permission denial, device removal, cancellation and
   model failure on the image, followed by representative native text, rich
   document, Terminal and browser journeys with selected-range replacement and
   system undo. Current actor, worker-core and Wayland tests prove separate
   production boundaries, not a complete assembled microphone journey.
   Browser address fields use the shared input contract. Native-hosted Chromium
   DOM fields now use opaque original-selection leases with deferred private
   acknowledgement. The SDK waits for an outstanding original-field check before
   admitting one final; cancellation suppresses queued admission. The compositor
   retains the complete transcript through acknowledgement and offers explicit
   ten-minute Copy/Discard recovery on rejection or unknown delivery. Unknown
   delivery warns to inspect the original field before inserting again; there is
   no automatic retry, clipboard write or successor-field targeting. The need
   for private acknowledgement is pinned to the original session, so a missing
   deferred owner cannot be acknowledged as a synchronous edit.
   `apps/browser/chromium/README.md` documents the backend and mounted SDK
   qualification. Standalone native runners now have a surface-bound,
   content-free Wayland acknowledgement extension beside standard text-input-v3.
   The SDK advertises an opaque original-field stamp, enables the deferred field
   only after dictation preparation, waits for the private renderer's start ACK,
   and consumes the marked standard final before public app event dispatch.
   Focus changes cancel the original grant; a stale final is consumed without
   successor dispatch. Ordinary composition/OSK remains closed outside this
   voice session. Five real protocol tests, six bound native-dispatch tests and
   mounted renderer journeys qualify
   separate boundaries; a complete standalone native-window journey remains an
   acceptance gate. Search, telephone, URL and plaintext-only editable fields
   pass start/final ACK, Unicode replacement and native undo/redo in both direct
   and decorated mounted views (48 mounted journeys in total).
   Same-origin iframe input/textarea/rich-text
   finals now use private contexts pinned to their original frame/document, with
   native undo and removal/recovery qualification. Cross-origin/opaque frame
   ancestry and closed author shadow roots still need renderer lifecycle support
   before admission. Composition is observed in a private document-start world;
   absent observation refuses admission.
   Selection-less email/number inputs require the privately negotiated native-caret
   bridge described below; legacy clients retain refusal and recovery. A complete assembled
   browser microphone journey remains unqualified. Explicit consent now activates
   the same original lease/edit stamp once and waits for its private acknowledgement
   before recognition launches. Warm preparation and the consented session each
   have a three-minute bound (six minutes combined); checks cannot slide expiry.
   A real-clock journey proves survival past warm expiry and fixed active expiry.
   Cancelling an acknowledged start revokes its original receipt scope, and a
   delayed cancellation cannot revoke a freshly consented successor.

3. Extend the public six-language read-speech qualification below to noisy
   natural conversation, and measure an older CPU and Arm device. Three short
   probes per language and artificial long splices remain narrow evidence.
4. Complete live touch/switch access and screen-reader
   announcements. Toggle/finish/cancel work through shared controls; a hold
   gesture, current mic-source display and editable draft review remain product
   extensions. The shared language chooser and private live caption are implemented.
   No opaque LLM rewrite is implemented.
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
key or activating the image catalogue. The protected publisher has the explicit candidate-review mode described below.
Publication and normal image delivery follow the existing protected release workflow.

## Compositor worker controller

The shell shares only bounded worker wire types through `agent-provider-config`;
it does not link the decoder or its native build. An owned actor performs source
probing, recording, pipe I/O and child cleanup away from the render thread. It
executes only `/usr/bin/tilo-speech-worker`, uses private pipes and ordered framed
messages, bounds pending audio to 4 MiB and replaces private hypotheses at most
once per second. Model readiness alone never starts recording: an explicit
capture command follows the caller's local microphone decision and target check.
Cancellation is checked before and after the bounded source probe. The compositor
retains the busy actor until its capture and worker have actually been reaped;
a successor cannot overlap them. The actor allows 15 seconds for startup,
105 seconds of recording and 60 seconds for finalization inside the
180-second session budget. Lost audio, malformed/replayed replies,
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


## Speech model candidates and protected release

The engineering baseline is **multilingual Whisper base**, pinned to revision
`5359861c739e955e79d9a303bcbc70fb988958b1`: 147,951,465 bytes, SHA-256
`60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe`, MIT.
It uses the existing confined CPU worker, sealed descriptor loader, two threads,
256 MiB artifact cap and 1 GiB address-space cap. This choice requires no account,
paid service or new trust identity. It is the baseline for source integration and
prerecorded qualification; it does not change an installed model or production
default. Tiny remains a compact candidate; small remains a quality comparison.
The shared personal dictionary and conservative transcript cleanup remain part
of the same production recognition path, rather than a separate engine wrapper.

“Shipping admission” means the existing protected authority reproduces and signs
the exact reviewed catalogue, publishes receipt-checked weights and complete MIT
notices, and delivers it through the normal image lifecycle. The per-model,
version and digest licence acceptance remains an explicit Settings action before
installation. These signing, publication, licence-acceptance and installation
actions have not run. Model selection, source implementation, and isolated
public-fixture evaluation do not wait for those actions or for microphone consent.

`prepare_speech_benchmark.py --model base` stages only the selected weights while
preserving pinned public FLEURS provenance. The sealed-scope tool's explicit
`--whisper-public-review` path accepts the six bounded public language fixtures,
public vocabulary, synthetic noise and assembled auto-language transitions under
the unchanged scope limits. It excludes native-engine and frozen holdout modes.
These are exposed development probes, never a fresh holdout or natural noisy
speech claim. Actual execution and quality results must be recorded separately.

The [baseline qualification ledger](dictation-baseline-qualification.json)
records six actual production-worker sessions on pinned public audio after a
fresh remote rebuild. All completed under the unchanged limits, with aggregate
scope peaks of about 642–645 MB and first drafts around three seconds. Raw errors
remain visible: English 1/19 words, German 6/18, French 7/20, Spanish 4/29,
Arabic 8/15 and Mandarin 9/23 characters. These results qualify the multilingual
worker integration, **not high-accuracy release acceptance**. The unspoken
public dictionary hint “TiLo” appeared in neither German drafts nor final;
this alone does not qualify personal names or the live shared dictionary.
No cleanup was used to reduce the recorded error counts. Physical latency,
microphone capture and browser/terminal insertion remain separate journeys.
Two additional development sessions completed in the same scope: concatenated
English/German automatic recognition recorded 9/37 word errors, and German with
seeded 20 dB white noise recorded 6/18. The largest scope peak was 656 MB.
Concatenation is not natural code-switching, and synthetic noise is not a noisy
microphone recording. These results and their provenance remain in the ledger.

The descriptor worker now selects upstream Flash Attention for the pinned GGML
small architecture (768-wide, 12-layer audio and text states); tiny/base retain
their existing allocation path. The small header is read from the same immutable
verified descriptor, without reopening a path. This is an allocation decision,
not model admission: the complete native parser, receipt/catalogue checks,
256 MiB artifact cap, 1 GiB address-space cap and 960 MiB review headroom guard
remain intact. The compatibility byte loader uses the same decision.

The [small allocation ledger](dictation-small-allocation-qualification.json)
retains the failed non-flash production-worker attempt: the headroom guard stopped
it before final delivery at 1,028,264 KiB virtual size. An earlier probe parser
mismatch also failed before a decoder was observed; that was fixed rather than
counted as a model result. Six isolated Flash Attention review sessions and six
sessions on the actual conditional implementation completed within the same
limits. Small raw errors were English 1/19 words, German 0/18, French 2/20,
Spanish 1/29, Arabic 6/15 and Mandarin 11/23 characters. Mandarin worsened against
base's 9/23. The base control retained its prior French 7/20 result and roughly
3.1-second first draft; small first drafts took about 7.2–7.3 seconds. The largest
observed review scope peak was 1.09 GB, including witness/cache costs. Warmer
subsequent scopes do not establish lower physical cold-device memory use.

The six-language review used one allocator arena. A separate French session on
the actual conditional worker removed that harness override and passed with the
production default allocator: the same 2/20 raw errors, about 7.4 seconds to first
draft and 889,540 KiB peak virtual size. No production allocator setting changed.
This control qualifies that session, not all physical machines or languages.

Base remains the engineering integration baseline. This makes the existing small
candidate usable under the memory policy, with measured accuracy/latency tradeoffs;
it does not switch any model, accept a licence, change the shipping catalogue or
grant microphone access. Raw errors were scored without cleanup, the unspoken
German “TiLo” hint remained absent, and the shared dictionary path is unchanged.
These are exposed public source-paced batches, not continuous microphone,
natural noisy speech, physical older-device or universal accuracy acceptance.

The candidate catalog keeps U²-Net unchanged and adds three MIT multilingual
models at the same pinned revision: compact **Whisper tiny** (77,691,713 bytes),
**Whisper base** (147,951,465 bytes) and **Whisper small-q5_1** (190,085,487 bytes).
Small needs 2 GiB system memory; all workers retain the 1 GiB address-space cap
and two CPU threads. Small
improves the public probes below, while tiny remains a lighter short-dictation
option. Neither result establishes everyday reliability or Flow parity.
The [pinned upstream model card](https://huggingface.co/ggerganov/whisper.cpp/blob/5359861c739e955e79d9a303bcbc70fb988958b1/README.md)
identifies converted OpenAI weights and MIT licensing. Both checked-in receipt
directories retain the complete 1,063-byte upstream copyright and permission
notice, SHA-256 `b5d65a59060e68c4ff940e1eddfa6f94b2d68fdf58ed7f4dd57721c997e35e9d`.
No account, paid endpoint, extra runtime image or new trust identity is needed.

`tools/release/prepare_speech_catalog.py` accepts only the unchanged subject
release and these three exact speech recipes: immutable URLs, exact byte counts,
hashes, full notices, CPU capability and hardware requirements. It rejects
insecure transport, altered receipts, excess bytes and overwriting. Its output
is staging, not installation or permission to record.

The existing protected `release-local-model-catalog.yml` has two explicit modes:

1. `review-speech` stages public inputs without credentials, signs a candidate
   with the existing `TILO_MODEL_CATALOG_SIGNING_KEY`, compares the resulting
   public key with the installed image key, and uploads only signed catalog,
   public key, declarative source and review hashes. It publishes no weights,
   changes no installed image and needs no package token.
2. Review those exact artifact bytes and commit the matching source and signed
   image catalog through the normal protected PR path. A development key or
   removal of the reproduction comparison cannot bootstrap a release.
3. Default `publish` supports the prior U²-Net-only release or these exact four
   models. It stages verified speech inputs, publishes weights and full MIT
   notices using the existing `RELEASE_TOKEN`, and verifies anonymous readback
   before signing. Authentication, rate-limit or server read failures never
   become an upload; an existing receipt conflict fails without overwriting.
   It reproduces the checked-in catalog and key bytes before catalog publication.
4. The normal OS image lifecycle delivers that catalog; Settings' existing
   license-acceptance and installation actions obtain a model. Installation
   neither starts recognition nor grants microphone permission.

Both modes run only from protected main with the existing release key id
`tilo-local-model-release-2026-10`. Live microphone consent remains a separate
local decision for each recording. The source candidate and workflow extension
alone leave the currently installed catalog unchanged.

## Language, preview and correction

The missing-model prompt has a semantic Set up action through the normal Settings
launcher, with no installation or recording grant. Shared microphone symbols
distinguish listening from states in which capture is off.

Settings → Accessibility exposes a shared, persisted dictation-language dropdown
and the same semantic `SetDictationLanguage` action. Stable language codes are
separate from localized labels; unknown saved values fall back to automatic
detection. A session reads that preference when local consent is granted. The
continuity policy classifies the language as personal intent; no audio,
transcript, microphone identity or microphone grant accompanies it. The system
microphone is the source selected by Sound settings; no second per-app
microphone preference or permission is introduced.

While listening or finishing, the trusted local overlay shows the last 160
Unicode scalars of the provisional draft in the shared themed Text widget,
with a polite live accessibility caption. It is labeled Draft, does not enter
the field or workspace, and clears on retirement, cancellation, lock or recovery.
The complete private session draft is bounded and zeroized on replacement/drop;
words are absent from prompt diagnostics. Finish commits corrected text once to
the original selected range. Ordinary shared text editing and system undo/redo
provide correction; spoken replacement commands and a personal dictionary are
not yet implemented.

Consent and recovery explanations wrap in the shared permission prompt at the
selected text scale. Its height, buttons, trusted seal and accessible bounds
follow the complete explanation. The shared caption Container and Text widgets
own their background, themed inset and natural text height.

## Public multilingual and long-speech qualification

`tools/release/prepare_speech_benchmark.py` stages pinned Google FLEURS test
audio (CC-BY-4.0, Google FLEURS authors) and receipt-verified MIT models on a
build machine. It streams a bounded archive prefix without extracting paths,
rejects insecure redirects, and records original and PCM16-derived receipts.
`speech_benchmark` runs the production CPU decoder after the same worker
preparation and full confinement. Three first archive utterances per language
are scored without recognition-based selection. English, French and Mandarin
also have unique 70–90 second splices. These are artificial read-speech probes,
not conversational or population benchmarks. Reproduce with:

```sh
python3 tools/release/prepare_speech_benchmark.py target/speech-benchmark
cargo run --locked -p local-model-service --example speech_benchmark -- \
  --manifest target/speech-benchmark/manifest.json --model tiny > target/tiny.log
cargo run --locked -p local-model-service --example speech_benchmark -- \
  --manifest target/speech-benchmark/manifest.json --model base > target/base.log
python3 tools/release/summarize_speech_benchmark.py target/tiny.log target/base.log
```

The qualification runs on a build machine with no microphone or model install.
Results and per-fixture provenance are in `dictation-speech-benchmark.json`.
Normalization is NFKC/casefold and punctuation/whitespace cleanup only. It does
not hide number, accent or Chinese script errors. Mandarin uses CER because
space-token WER is not meaningful; Simplified/Traditional differences count.

| Three short probes | Tiny current WER | Base baseline WER | Small earlier bounded run WER |
| --- | ---: | ---: | ---: |
| English | 16.44% | 10.96% | 10.96% |
| German | 27.42% | 20.97% | 1.61% |
| French | 36.70% | 26.61% | 9.17% |
| Spanish | 16.67% | 9.80% | 2.94% |
| Arabic | 59.42% | 42.03% | 31.88% |
| Mandarin CER | 24.69% | 33.33% | 14.81% |

The Tiny column has now been verified by a complete 21-case run of the changed
decoder; the Base column remains the earlier upstream-parameter baseline. Small
rows preserve an earlier completed run of the bounded CPU implementation:
weights are loaded without decoder arenas, verified input is dropped before
readiness allocation, greedy fallback uses one candidate, and each transcription
gets fresh decoder state. Each probe runs in its own confined worker process.
That earlier run completed all 21 Small probes under the unchanged 1 GiB
address-space limit; each long final was repeated exactly after automatic-language
recognition. A new same-configuration Small comparison aborted with native
`std::bad_alloc` on `ar_eg-1`, after 13 completed probes. An isolated retry of that
case completed all three phases under the same cap. This failure is intermittent
and unresolved; the passing retry does not establish stability. The new C++ ABI
exception boundary and phase diagnostics have compiled, but the exception path
has not yet been reproduced with the boundary. The incomplete run is recorded
separately rather than presented as a complete comparison.

Current Tiny short fixed-language final medians are 0.58–0.74 seconds, versus
1.05–1.28 seconds with automatic detection. Its median cold load is 1.132 seconds.
Arabic WER remains 59.42% and Mandarin CER 24.69%; latency alone does not make it
a sufficient default for every language. Current Tiny long English/French WER
is 66.03%/53.45%, Mandarin CER 45.49%, with fixed-language final times of
1.75/3.14/4.20 seconds. Keep these Tiny results separate from the larger Small
candidate and from model release staging.

That Tiny object is SHA-256
`be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21`,
77,691,713 bytes. The 21 probes contain 18 short clips (three each in English,
German, French, Spanish, Arabic and Mandarin), plus three artificial long
splices. Exact short fixed-language medians span 584–739 ms. Tiny and Small use
the same receipt-checked audio and bounded decoder configuration, but the
current paired Small run is incomplete: only 13 corresponding probes completed
before `ar_eg-1` aborted. The earlier complete Small receipt is a separate run;
do not present it as a completed contemporaneous paired comparison.

External 20 ms process-memory sampling subsequently observed five completed
Small `ar_eg-1` probes with peak virtual size 1,043,140 KiB (about 1,019 MiB)
and resident high-water at most 512,452 KiB (about 500 MiB). The unchanged
address-space cap is 1,048,576 KiB, leaving only about 5 MiB at the observed
peak. [RLIMIT_AS limits virtual address space, including allocation mappings](https://man7.org/linux/man-pages/man2/getrlimit.2.html),
so low resident use does not establish allocation headroom. Address-space
pressure is a supported diagnosis, not proof of the original failing allocation;
the prior abort and the untested typed exception path remain in the receipt.

Controlled diagnostic allocator settings did not provide robust headroom.
Five single-arena probes peaked at 1,030,336 KiB; three fixed-128-KiB mmap
threshold probes peaked at 1,042,296 KiB. [Allocator arena and mmap thresholds
trade allocation reuse against memory consumption](https://man7.org/linux/man-pages/man3/mallopt.3.html).
The pinned upstream CPU Flash Attention path reduced resident use, but three
fully recorded probes decoded more slowly (fixed final median 5,698 ms versus
4,785 ms for the five default probes). Its requested five-probe batch was
interrupted by SSH transport after three complete receipts; the partial output
is preserved and is not a completed five-probe or accuracy qualification.
All experimental settings were reverted. The worker cap, decoder settings and
model selection remain unchanged. One same-clip Tiny observation peaked at
858,484 KiB virtual and 239,912 KiB resident (about 838/234 MiB); this single
resource observation does not establish Tiny memory stability or older-device
performance. `current_followup.allocation_diagnosis`
records every completed counter set and the interrupted log hash. Reproduce
inside the owned public-fixture review environment with:

```bash
python3 tools/release/probe_speech_memory.py \
  --manifest target/speech-benchmark/manifest.json \
  --benchmark target/debug/examples/speech_benchmark \
  --model small-q5_1 --case ar_eg-1 --repeats 3 --allocator default
```

Synthetic PCM can qualify an assembled browser journey without a physical
microphone or a new persistent permission. The existing private PipeWire
fixture disables hardware discovery and already exercises the real pinned
recorder. A non-root, network-isolated bubblewrap namespace probe passed on the
review builder. However, that builder lacks the immutable speech executable,
catalogue and trust inputs at the compositor's production paths; the current
core qualification's temporary witness is scoped to its child. An assembled
fixture must mount those disposable qualification inputs in its own namespace,
launch current compositor/browser builds against an owned profile, drive the
existing consent action, and assert original-field ACK, insertion, undo,
focus-change recovery and cancellation through `tilo-control`. It must not
introduce a production executable override, install a development key or alter
the user's grants. The synthetic assembly and its limits are recorded below. Real microphone,
device selection and physical-device acceptance remain separate gates.

Small's median cold load is 2.748 seconds. Short final medians are 4.37–4.79
seconds, with automatic language approximately another four seconds; long
English/French/Mandarin finals are 14.93/20.57/20.74 seconds, automatic finals
18.94/24.60/24.75 seconds. These remote x86_64 timings exclude capture completion;
older CPUs and ARM remain unqualified. A subsequent observation of the same
remote worktree/container reports AMD EPYC 9R45, 64 visible logical CPUs and
no cgroup CPU quota; the decoder used two threads. Original timing receipts
recorded only x86_64, so this is follow-up provenance rather than contemporaneous
CPU telemetry or a dedicated-core claim. An explicit language removes detection
work. The actor preserves bounded audio under decoder backpressure and allows
60 seconds for a final, rather than discarding these valid long results after
15 seconds. Cancellation remains responsive and retires the exact child.

The earlier Small long run reported English WER 17.31%, French WER 26.29% and
Mandarin CER 14.90%, compared with Base's earlier 64.74%/40.09%/42.75%.
These are one artificial splice per language, not
conversational accuracy. Small still omits the first French sentence, mishears
proper names, and sometimes hallucinates words in a short draft. It is an
improved local candidate, not proof of Wispr Flow parity. Draft revision scores
align a five-second draft to the best final prefix, allowing appended words;
they measure decoder revisions, not human correction effort.

The original eager loader failed native allocation on small even with a single
10.56-second clip. Deferred state preparation fixed that peak without raising
the cap. Default five-candidate fallback then exceeded the cap; upstream's -7
failure frees its state, so the wrapper now clears its pointer on that path.
Fresh state per transcription also prevents fallback RNG/caches from changing
repeated results. Public probes exposed and verified each fix. The signed-store
qualification separately asserts the production loader's exact bounded capacity.
Disabling timestamp prediction was tested and discarded because it worsened
French and Mandarin. No timestamp-policy change is included.

### Current intermediate-size qualification (6 October 2026)

The existing multilingual Whisper Base fixture was reverified, not downloaded
or installed: 147,951,465 bytes, SHA-256
`60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe`,
from the same pinned MIT model repository as Tiny. The unchanged production
configuration (two CPU threads, fresh state, 1 GiB address-space limit) completed
all 21 probes on the same 33-source FLEURS fixture. Cold-load median was 2,130 ms.

| Language | Short quality | Fixed-language median | Auto-language median |
| --- | ---: | ---: | ---: |
| English | 10.96% WER | 1,211 ms | 2,241 ms |
| German | 20.97% WER | 1,231 ms | 2,255 ms |
| French | 26.61% WER | 1,268 ms | 2,324 ms |
| Spanish | 9.80% WER | 1,282 ms | 2,290 ms |
| Arabic | 42.03% WER | 1,350 ms | 2,400 ms |
| Mandarin | 33.33% CER | 1,217 ms | 2,267 ms |

Three independent Arabic probes and one long Mandarin probe completed with
actual decoder peak virtual memory of 942.46–951.73 MiB, leaving at least
72.27 MiB of observed headroom; peak resident memory was 448.27 MiB. The initial
full-run supervisor counters were discarded because they did not observe its
isolated decoder children. These are sampled observations, not worst-case bounds.
Long English/French/Mandarin errors remained 64.74% WER / 40.09% WER / 42.75%
CER. Base improves several Tiny results and memory margin compared with Small,
but neither this quality nor three short recordings per language establish the
user's Flow-level goal. No catalogue, default or user selection changed.

The qualification examples now expose opt-in fixture entry points: a scoped
native WebView window on a loopback HTTP page, and a reviewed-model fixture held
only while its owner's stdin pipe remains open. Its deterministic test key,
accepted-license receipt and model store live in an automatically removed
temporary directory. They must only be mounted inside a disposable non-root
namespace; they are never installed into OS or user trust. Production worker
paths and microphone consent are unchanged. These entry points alone do not
prove the assembled audio-to-browser journey.

A disposable namespace run assembled the current headless compositor and the
sandboxed native Chromium fixture on loopback successfully. Actual DOM focus
and the correct Wayland keyboard/text-input surfaces were observed. The initial
harness incorrectly required standard IME enablement before private preparation
and used a test-double provider address. The production provider is `actions`;
invoking `actions/dictation.toggle` after focus completed original-field Prepare
admission, and only then did standard text input become active. This preserves
the prohibition on unguarded insertion; it is not a native focus defect.
Separately, the fixed-path real worker reached `Ready` inside the same namespace
layout and reported explicit `WorkerStopped` after owner EOF. These observations
were followed by a full synthetic capture journey. The initial missing audio
executables were supplied only in the disposable review container. Fedora's
PipeWire server package transaction failed at its sysusers hook; the cached,
signature-verified RPM's server executable/configuration were extracted into an
owned fixture directory instead of creating system accounts or running services
on the laptop.

The hardware-disabled private PipeWire graph fed public FLEURS `en_us-0` audio
through the actual pinned recorder and fixed-path worker. Explicit one-session
consent and original-field Start acknowledgement preceded capture. Finish removed
the recorder and inserted the corrected final into the original native Chromium
input at its existing caret. An assembled keyboard Ctrl+Z regression exposed a
missing WebView local-history capability. WebView now consumes the existing
system UndoRequest through an explicitly advertised native-history backend;
Chromium calls its focused frame's native Undo/Redo. Legacy hosts without that
capability retain their old protocol, and bounded queue admission does not claim
that an undo entry existed. Ctrl+Z restored the original value and Ctrl+Shift+Z
restored the dictated value in the assembled journey after this repair.
The existing Edit executor delegates to this same capability; native history
availability metadata in menu/action discovery remains unqualified.

A second recording canceled without altering prior text, and its recorder and
model worker exited. Starting a successor after that cancellation initially
failed even after observed worker exit. Two native-client scheduling defects
were identified: querying a pending reply's deadline advanced it another 50 ms,
and an idle guard poll did not republish private admission when it needed no
paint. The pending deadline is now owned by actual guard service; admission is
republished after an idle widget sync without forcing a rendered frame. A
production guard deadline regression failed before repair, then all nine lease
regressions passed. The assembled third recording then started after cancellation.

The first pointer-focus step did not move DOM focus, despite a trusted pointer
press reaching the correct second field at its current measured coordinates.
Public synthetic event diagnostics exposed stale Ctrl+Shift from the earlier
Undo/Redo sequence. Modifier-key releases are not mapped page keys, so the
WebView now forwards current host modifiers before pointer motion/buttons/scroll
through the existing backend method, including transparent wrappers. The
production pointer regression failed before repair; all seven WebView module
tests passed afterward. The assembled mouse-focus journey then retired capture
and left both fields unchanged, matching the ordinary native Tab-key journey.
Separate mouse and keyboard journeys then repeated the full insertion/history/
cancel/restart sequence with the public FLEURS player still streaming before
the successor field was focused. Both stopped the original recorder and left
both field values unchanged; these were active-audio cancellation assertions.

These hardware-isolated journeys used no physical microphone, external network,
user trust, published model catalogue or device permission. They do not establish
support in every text field, real-device latency, or Flow-level recognition.

### Missing-case and field-surface follow-up (6 October 2026)

Eight separate current-production Small probes completed the cases absent from
its aborted batch: Arabic 1/2, Mandarin 0/1/2, and long English/French/Mandarin.
The existing reviewed weights and corpus receipts were reverified; configuration,
two decoder threads and the 1 GiB confinement limit were unchanged. Actual child
memory counters were sampled every 20 ms. Minimum observed headroom fell to
**1.60 MiB** in the long probes, below even the earlier short-probe margin.

Together with the 13 successful observations preceding the original abort,
these observations reproduce Small's short English/German/French/Spanish/Arabic
WER of 10.96%/1.61%/9.17%/2.94%/31.88% and Mandarin CER of 14.81%. Long
English/French/Mandarin errors are 17.31% WER / 26.29% WER / 14.90% CER;
fixed-language final times are 14.84/21.41/21.65 seconds. This is an aggregate
of successful observations across two runs, **not a successful 21-case batch**.
The original `ar_eg-1` allocation abort remains in the qualification record;
passing its separate follow-up does not resolve that failure or establish
stability. No model catalogue, default, allocator policy or selection changed.

A focused actual CEF regression now observes focused `email` and `number`
inputs returning null selection endpoints. Both reject deferred preparation
with `not_editable`, preserve both field values and leave no lease blocking a
supported successor: a text input then completes original-field Start and
Commit acknowledgements. The test passes in a non-root namespace with no
physical devices or external network. It qualifies rejection and recovery,
**not support for these fields**. Removing the selection guard would weaken
original-caret binding and is not a compatibility fix. CEF's native
[selection-change callback](https://cef-builds.spotifycdn.com/docs/152.0/classCefRenderHandler.html)
is a candidate source of range information; its completeness, freshness and
binding to the renderer's original field require independent qualification
before enabling selection-less fields.

The assembled browser journey was also repeated with action enumeration after
successful insertion. Querying Undo returned only the shell's activity-history
`history.undo`; querying Redo returned no actions. Keyboard Undo/Redo still
passed. This fixture did not establish broker-acknowledged publication of the
application's resources, so its catalogue result cannot prove discovery
behavior in every registered app. Independently, the SDK host's
`focused_undo_status` still reads document history rather than the native
frame's history, and the backend's `native_edit_history` flag reports command
support, not available entries. Native menu/action availability remains
unqualified; enabling actions from queue admission would make an unsupported
promise. Production CEF's native
[context-menu edit-state flags](https://cef-builds.spotifycdn.com/docs/144.0/classCefContextMenuParams.html)
are scoped to the node on which that menu was invoked, so they do not alone
supply a continuously fresh focused-field history status.


### Allocation containment and native history follow-up

The Small failure is preserved. A separate test-only preload injects one
`std::bad_alloc` into the 3,200-byte FFT input allocation on the public
`ar_eg-1` draft. Pinned Whisper 1.8.3 creates a preprocessing thread before
running the same worker on its main thread; unwinding with that thread still
joinable aborts before the outer ABI catch. The controlled baseline aborted
with signal 6. Serial preprocessing through
`whisper_pcm_to_mel_with_state(..., 1)`, followed by
`whisper_full_with_state(..., nullptr, 0)`, now returns the typed recognition
failure and exits 1. Recognition still uses two threads, fresh state,
`best_of=1`, the default allocator and the unchanged 1 GiB cap. This identifies
and contains one real allocation failure path; it does **not** establish the
cause of the original intermittent abort or cover every native thread.
`tools/release/probe_speech_fft_failure.py` makes the public-fixture regression
repeatable without installing a preload or changing service confinement.

The subsequent default-allocator batch completed all 21 public cases. Draft,
final and automatic-language text matched byte-for-byte against the original
13 successful observations plus eight separate follow-ups, and against the
complete throw-observer diagnostic batch. Five actual-child memory probes
(two failing-case retries and all three long clips) completed. Short Arabic
virtual-memory margin increased to 17.8 MiB, while long Chinese still had only
1.60 MiB; long French retained 4.01 MiB. This remains inadequate evidence for
stability or a default model promotion. The production worker and benchmark
already release the verified input model copy before decoder allocation; no
second copy was found to remove.

The native history work uses the existing LocalUndo capability and canonical
`tilo.host:edit` resource. Original-focus Prepare/Check replies carry only two
history availability flags, accepted strictly with a 500 ms freshness bound.
Expired, canceled or changed reservations do not fall back to unrelated CRDT
history. The same availability reaches the host menu, palette, accessibility
controls and broker publication. A supported native history operation is
admitted as a bounded background job and reports success only after its
renderer acknowledgement; refusal or missing acknowledgement is non-retryable.
Native page history is local-device-only, like the private field context it
uses; it does not authorize another collaborator to undo private form edits.
Unsupported hosts do not advertise this capability, and withdrawing the
focused native owner removes its broker route.

**Chromium's history is page-wide, not per field.** A stronger actual CEF test
showed that Undo can restore another field's latest entry while the first field
is focused. The original failing field-ownership assertion is retained: a
field-scoped request must refuse without changing either field. The private
protocol therefore requires explicit `history_scope: "page"` for native
Undo/Redo, and the public action description states that another field may own
the latest change. The existing original-focus and frame lease still guards
that request; a focus or document change before dispatch refuses rather than
retargeting. No per-field native history support is claimed. The native
keyboard route keeps its existing page-history behavior.

The mounted audio fixture does not inherit the launcher's collaboration
service descriptor and certified route. Its earlier missing catalogue rows
remain an unqualified assembled broker-publication observation; direct
connection with an ephemeral signer cannot substitute for production launcher
admission. Native CEF effects and the production SDK resource-dispatch path
are qualified separately, without granting access or weakening attestation.

### Base comparison, segmentation and selection-less editor evidence (2026-10-07)

The Base/intermediate comparison **did run**: all 21 pinned public FLEURS
probes completed under the unchanged two-thread, 1 GiB worker limit, using
the existing receipt-verified MIT asset. Its earlier sampled children retained
at least 72.27 MiB address-space margin. Short fixed-language final medians
were 1.21–1.35 seconds; English/German/French/Spanish/Arabic WER was
10.96%/20.97%/26.61%/9.80%/42.03%, Mandarin CER 33.33%. Long
English/French WER remained 64.74%/40.09%, Mandarin CER 42.75%. The
`current_followup.base` record contains that complete comparison, not an
unexecuted plan. FFT exception containment does not solve either recognition
quality or the Small model's narrow memory budget.

These measurements selected a bounded Base segmentation experiment instead
of a larger model or a higher cap. Six fresh confined children compared the
same three long splices whole and with fixed 20-second chunks. Whole versus
chunked English WER was 64.74% versus 23.08% (4.655 versus 5.582 seconds),
French 40.09% versus 53.45% (6.236 versus 6.598 seconds), Mandarin CER
42.75% in both (6.365 versus 7.220 seconds). All six completed; sampled
address-space margins were 158.03–167.99 MiB. **Reject fixed cuts as a
general default**: they worsen French and do not improve Mandarin.
`current_followup.base_segmentation` stores paired scores, actual-child
memory, transcript hashes and raw-log digest. Benchmark-only
`--chunk-seconds` and `--chunk-silence` permit the next bounded hypothesis
without changing production, model selection or catalogue publication.

A real CEF probe confirms native insert/Undo works in email and number fields,
but distinct native caret positions yield identical DOM field/document
selection snapshots. The durable
`test_selectionless_dom_snapshot_cannot_bind_two_native_carets` asserts both
the ambiguity and different resulting native edits, then Undo restores the
original values. Existing unsupported-field refusal/recovery assertions remain.
A native atomic caret/owner bridge is required; admitting these fields by
value alone or by synthesizing keys would weaken the reservation contract.
The [HTML selection API](https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#dom-textarea/input-selectionstart)
limits which input states expose selection. The pinned CEF DOM interface's
collapsed-selection offsets also do not establish that private native caret.
No private field text, clipboard, microphone or credentials were used: these
were synthetic public fixtures in the non-root isolated native host.

The next bounded diagnostic searches for the quietest 100 ms within 25% of
each nominal 20-second boundary. Three isolated Base probes completed with
English/French WER 17.95%/34.91% and Mandarin CER 28.24%, final decoding
5.130/7.490/7.237 seconds. Unlike fixed cuts, all three improve on whole-audio
Base. This selects quiet boundaries for complete same-corpus qualification,
not a production promotion. The byte-for-byte short-output comparison uses
the original complete Base log, whose digest matches the committed receipt.

The added native-history dispatch originally broke browser-target compilation
by importing its canonical Edit operation identity from a native-only host
module. The identity now lives in the portable undo module; the host reexports
the same value. The exact CI wasm package/target check passes, as do native
history authority tests and the required shell integration compile.

The complete quiet-boundary candidate then ran all **21 primary probes** under
the same cap and default allocator. All 18 short cases' draft, fixed-language
final, auto-language final, references and sample counts match the earlier
complete Base run byte-for-byte. Long English/French WER was 17.95%/34.91%,
Mandarin CER 28.24%, with final decoding 5.093/6.820/6.718 seconds.
Each actual child was observed at 20 ms; the narrowest sampled address-space
margin was 163.24 MiB. The full scored summary, 21 child peaks, transcript
hashes and raw-log digest are recorded separately from the three exploratory
probes. Quiet boundaries are the selected next recognition improvement to
validate in production-worker journeys; they are still benchmark-only. This
is not evidence for noisy conversational accuracy, older CPU/ARM performance,
Small-model stability, or a microphone/model release approval.


### Private native caret binding for email and number fields (2026-10-07)

The new optional `native_caret_text_input` ready capability is strict and
per-tab. Only a capable original tab sends `native_caret: true` on private
Prepare; subsequent Start, Check, Commit and Cancel keep their original lease.
Legacy clients still refuse selection-less fields, and their original refusal,
ambiguity and recovery assertions remain unchanged.

The browser resolves the uniquely editable native node using the existing
internal DevTools connection and the same original frame's isolated world.
It grants no universal access and opens no inspector port. A captured native
selection getter binds node identity, direction, offsets, raw editor state,
value and original input ownership inside the renderer. The original native
snapshot is checked on every operation and again after `beforeinput`, before
native insertion. Only the opaque lease and existing history flags leave the
private protocol; field contents and caret offsets are not exported to the
client or recorded in its public action log. Temporary remote object handles
are released, and retired sessions clear private editor references.

Internal UA roots must not be treated as author-created open/closed roots:
[Chromium's mode getter](https://raw.githubusercontent.com/chromium/chromium/main/third_party/blink/renderer/core/dom/shadow_root.cc)
rejects the internal UA mode. Ownership is checked through the original native
input instead. The actual pinned-engine probe qualifies this distinction;
the implementation does not assume a portable HTML selection API for these
fields. Numeric finals are validated before and after page handlers so native
filtering cannot acknowledge deletion of the selected original value. Invalid
or non-finite numeric text requires correction; spoken-number/localized-digit
normalization is not qualified. Password, OTP, payment, masked and read-only
fields keep their privacy and eligibility guards.

The published `813d28ed` passed affected-package run 75139, including wasm,
clippy and every selected test shard, and fast-quality run 75142. App SDK run
75138 measured the merged release desktop module at 93,218,050 bytes, 25,858
above its existing budget. The additive dictation receipt follows the existing
ratchet's measured 1% headroom rounded to 64 KiB; that is a merged-tree total,
not an exclusive dictation cost. The native CEF caret adapter is excluded from
wasm. No consent or caller-custody checks are removed to reduce the size.


Exact integrated qualification completed all **39 actual CEF tests in
235.234 seconds**, including real fixed warm/active lease expiry, without
ignored tests or weakened assertions. The full matrix adds original email and
number replacement, empty fields, same-origin iframe and open-shadow-host
Unicode edits, native Undo/Redo, Cancel/repeated Start, focus/read-only/caret
revocation, post-handler length restriction, numeric filtering refusal,
sensitive-field recovery and malicious page-prototype isolation. The original
legacy refusal and two-native-caret ambiguity assertions remain. The structured
receipt records all four source digests and the raw result digest; remote
source/fixture bytes match the integrated candidate exactly.

Current-main integration preserved the exact saved resolved tree after
normalizing all 20 task commit messages under the newly introduced policy.
Both example targets and both EN/DE locale key sets remain; all commit/blob
policy inspections pass. This is native insertion qualification, not a
physical-microphone or universal custom-editor acceptance claim. The complete
certified app-broker journey and cross-origin/closed author editor lifecycle
remain unqualified. Native composition recovery without a trusted end retains
conservative refusal; microphone and release approval boundaries remain held.

## Comparable completed decoder batches

These are the same 21 public FLEURS cases: 18 short clips (three per language)
and three synthetic long splices. Fixed-language final decoder time excludes
cold loading, capture and field acknowledgement. All use two recognition
threads, the default allocator and the unchanged 1 GiB address-space cap; Small
uses the measured serial-mel containment. These are diagnostic inputs, not a
live conversation or older-device acceptance test.

Short error percentage / median final milliseconds; Mandarin uses CER,
other languages WER. Lower error is better.

| Language | Tiny | Base | Small q5_1, serial mel |
|---|---:|---:|---:|
| English | 16.44% / 739 | 10.96% / 1,211 | 10.96% / 4,428 |
| German | 27.42% / 703 | 20.97% / 1,231 | 1.61% / 4,614 |
| French | 36.70% / 696 | 26.61% / 1,268 | 9.17% / 4,878 |
| Spanish | 16.67% / 612 | 9.80% / 1,282 | 2.94% / 4,640 |
| Arabic | 59.42% / 716 | 42.03% / 1,350 | 31.88% / 4,833 |
| Mandarin (CER) | 24.69% / 584 | 33.33% / 1,217 | 14.81% / 4,652 |

Long error percentage / final milliseconds. These are one synthetic splice
per language, not statistical estimates.

| Language | Tiny | Base | Small q5_1, serial mel |
|---|---:|---:|---:|
| English | 66.03% / 1,753 | 64.74% / 3,740 | 17.31% / 14,900 |
| French | 53.45% / 3,135 | 40.09% / 6,165 | 26.29% / 21,038 |
| Mandarin (CER) | 45.49% / 4,199 | 42.75% / 6,308 | 14.90% / 21,397 |

Tiny cold-load median is 1132 ms; Base is 2130 ms; Small
serial-mel is 2788 ms. All three listed batches completed 21 cases without a failed child.
The earlier default Small batch aborted on Arabic case 1 after 13 successes;
that failure remains recorded and is not erased by this serial-mel batch.

Memory sampling has different coverage and must not be treated as matched
worst-case measurements. Tiny has one same-clip observation at 838.36 MiB peak
virtual / 234.29 MiB resident, leaving 185.64 MiB. Base has four independent
actual-child probes, worst peak virtual 951.73 MiB / resident 448.27 MiB,
minimum observed headroom 72.27 MiB. Small has five current serial-mel memory
probes, minimum headroom 1.60 MiB. RSS is not the RLIMIT_AS budget.

Next quality action: carry the measured quiet-boundary Base segmentation into
a bounded recognition-worker experiment, then repeat these cases and the
synthetic capture-to-field journey. The complete quiet-boundary Base batch
keeps all 18 short outputs byte-identical and improves long EN/FR/ZH to
17.95% WER / 34.91% WER / 28.24% CER, at 5,093 / 6,820 / 6,718 ms, with
163.24 MiB minimum sampled virtual-memory headroom over all 21 children.
Fixed 20-second cuts were rejected because French regressed. Small remains
unqualified for default use under the current cap. No cap increase, model
catalogue promotion or benchmark-audio publication accompanies this evidence.

## Composition observation before admission

The host now registers a minimal observer in the private text-input world at
[document creation](https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-addScriptToEvaluateOnNewDocument),
through the existing registration queue, before releasing navigation. Every
private text snapshot requires an existing observer with no active composition;
a late-created context cannot infer idle from a missed event. The page cannot
access this world. Native trusted and synthetic composition are separate, so
a forged page end cannot clear a native start. No field/preedit contents are
exported. Registration failure closes the host; an absent observer refuses
admission, and a newly observed document can recover without reviving leases.
The observer follows the initial iframe Window across its `about:blank`
transition and binds the current Document at its trusted load boundary. A
root-removal observer invalidates that binding when `document.open` removes
listeners; a surviving private-world marker cannot certify idle. The SDK
classifies this refusal as a confirmed no-edit receipt for Prepare, Start and
Final.

The full guarded suite passed 42 cases in 234.744 seconds, retaining its
original assertions and expiry waits with three added pre-admission/lifetime
cases. Four actual Blink IME journeys passed in the owned hardware-free
PID/network namespace: preedit before first Prepare in main and initial iframe
documents, forged end, started
lease retirement across focus/cancellation, and missing-observer refusal with
fresh-document recovery. These use actual Input.imeSetComposition and verify
that preedit changed the native field; they do not substitute synthetic events
for native composition. The fixture's debugger port is confined and production
opens none. Initial probes exposed that debugger Input.insertText emits an
untrusted composition-end and focus movement may emit no end. Neither is a
trusted idle proof; the guard stays closed until a fresh observed document.
Full ordinary composing-IME delivery and trusted cancellation recovery still
need the embedding's native IME path; the safe pre-existing admission gap is
closed without assuming those completions are trustworthy.

## Signed application capture and native Edit publication

A hardware-free assembled journey now uses the maintained SDK thin packager,
including its derived and signed resource kinds, verified runtime bundles and
the real launcher. The initial Browser package is 2,846,208 bytes and its rebuilt
payload is 2,847,232 bytes, both under the normal 8 MiB limit. The updated shared
SDK runtime is included in the verified bundle. A throwaway workspace governor
establishes custody before Shell startup; ordinary signed Join enrolls the session and normal Applications open
creates the primary Browser record. No authority or document-read refusal is
bypassed. The private namespace has its own PipeWire graph with hardware nodes
disabled, a pinned public FLEURS utterance, and no physical devices or external
network.

Certified broker navigation and page Read/Click finish with terminal receipts.
The page action changes DOM focus; the native toolkit also needs ordinary
window input focus. A measured compositor pointer click in the public original
field establishes that owner. Without it this fixture's preparation times out;
DOM focus alone is not claimed to prepare a native editable field. With normal
input focus, Prepare and Start are acknowledged before capture, the worker
records the private virtual source, and the final transcript reaches the
original field. The recorder is gone after Finish.

The original signed journey exposed only the primary document's CRDT Undo/Redo,
not the separate native Edit resource. It refused to substitute those document
actions or a keyboard-only history probe. A concrete SDK contract defect was
found independently: WebView handles `tilo.host.edit` but omitted its handler
declaration. The existing
renderer-acknowledgement and focus-change test now validates the real Edit
resource against WebView's handler table. It failed for both Undo and Redo
before the declaration and passed after it. The rebuilt package still lacked
the native Edit row. The actual bound-app adapter also intercepted LocalUndo
but inherited its default absent status and synchronous-completion flag.
Status and deferred completion now follow its existing callback binding into
the underlying owner, returning none/false while closing or closed. A focused
production-boundary regression reproduced the absent status; its closing-owner
assertions ensure metadata cannot cross a retired owner. All 14 selected native
history tests passed in 0.650 seconds.

With both fixes and the normal rebuilt signed package, the assembled journey
passed native Edit Undo and Redo with broker terminal acknowledgements: Undo
restored the exact original field, and Redo restored the exact dictated field.
A repeated session was canceled without changing that field; both recorder
and model worker exited before restart. Moving focus to the successor field
while public speech was actively streaming retired capture and changed neither
field. The confirmation run used diagnostic-free Shell at `791069d6e8`;
builder source digests match that qualified candidate. Subsequent main merging
adds device-display intent types without changing native history or dictation. Its temporary fixture signing key
was removed after both successful runs. Physical microphones, complete OS
rendering/fonts, older CPU/ARM and
noisy conversations remain unqualified. No permission, sandbox rule, package
limit, model default or release gate changes accompany these fixes.

### Paced worker qualification and conservative cleanup (2026-10-07)

The worker now fixes a quiet audio cut around 20 seconds after a bounded
lookahead, decodes each confirmed prefix once and revises only its unfinished
tail. It keeps the existing 120-second PCM, 16-KiB transcript and 1-GiB address
space limits. All 21 Base public batch probes finished under those limits;
their draft/final/automatic strings exactly matched the earlier reviewed
quiet-boundary experiment. Worst measured VM headroom was 164.03 MiB. This
qualifies that production segmentation path, not every incremental streaming
schedule.

`tools/release/probe_speech_stream.py` drives the actual framed worker with a
paced public source, retained audio under backpressure and the existing
ephemeral catalogue witness inside an unprivileged hardware-free namespace.
It records first partial, non-prefix revisions, finalization, user/system CPU,
20-ms sampled VM/RSS peaks and worker exit. Three before/after probes passed:

| Public input / affinity | First partial before / after | Finalization before / after | CPU seconds before / after |
| --- | --- | --- | --- |
| English short / two CPUs | 2.187 / 2.228 s | 1197 / 0.468 ms | 27.00 / 24.26 |
| English short / one CPU | 11.869 / 3.178 s | 24603 / 2.921 ms | 62.04 / 15.37 |
| English public splice / two CPUs | 2.119 / 2.144 s | 1161 / 2.692 ms | 141.70 / 137.21 |

The worker chooses at most two available CPUs before confinement, so a
one-CPU affinity profile no longer oversubscribes native inference. Finish
reuses a draft only when its audio is unchanged and its tail needs no additional
cut. Completion and failure retire and zeroize both transcript caches. The
final strings were byte-identical in each before/after pair and to the full
batch probe. The short English clip scored 5.26% WER and the public English
splice 17.95% WER under the existing conservative scoring rule. Optimized sampled
VM peaks were 872092, 862804 and 885608 KiB respectively. These are builder
affinity profiles, not physical older-hardware results. The two-CPU first
English hypothesis was incorrectly “Thank you.” Raw revisions and latency
are evidence of provisional output, not an accuracy or stability guarantee.
Full metric and digest receipts are in `dictation-speech-benchmark.json`.

Final-text cleanup is a bounded shared speech function with a personal literal
preference in the existing accessibility section. Settings exposes the
`SetDictationLiteral` action and its themed toggle. The preference is captured
when a session starts, so a later Settings change cannot change a recording's
meaning. Explicit comma-delimited numeric self-repairs are removed in selected English,
German, French and Spanish (including regional tags):
“Let me grab two, uh, no, three of these” becomes “Let me grab three of these.”
The replacement retains its spelling. Literal mode, quotations, names,
uncertain disfluency, decimal/padded numbers and unsupported languages are
preserved. Explicit chains keep the last corrected number. Localized markers
are narrow (`nein`, `non`, `no`); filler removal occurs only inside that numeric
repair. There is no general grammar rewrite, inferred name correction or
implicit numeric normalization. Automatic language retains only the English
rule; it does not infer a cleanup language from a potentially mixed transcript. Cleanup affects
only the final result and uses the existing original-field single-edit
insertion and native Undo path; drafts remain provisional. All 14 focused
headless frontend tests passed, including an actual private-pipe final through
the production compositor service: cleaned and literal modes each delivered
exactly one original-field commit. The Settings semantic literal action and
personal spelling file round-trip regressions also passed.

The following app-scoped spelling analysis predates the landed shared reader
integration described below. The existing spellcheck file now
preserves accepted capitalization and offers `user_spellings()` without a
second store; lookup remains case-insensitive and old lowercase files work.
Sandboxed text fields still have app-and-room-scoped config: the launcher
binds the package's room root as its config directory. OSK's learned predictor
uses a separate per-app personal record. Neither path is the shared system
dictionary, and scanning other apps' private roots would bypass their boundary. The existing shared text-assistance channel is
read-only for ordinary apps. Do not turn generic settings updates into a
personal-dictionary write bypass or bind the full host config directory into
apps. A typed shared dictionary service must connect existing Add-to-dictionary
actions, keyboard/autocorrect and recognizer lexical bias to the same normal
store with bounded words and the existing local authority. Recognition bias,
pronunciation alternatives and that complete journey remain unqualified.

A native-IME forwarding experiment is saved separately on
`codex/dictation-native-ime-recovery-wip` at `e03047dd9c`; it is not part of this
qualified increment. Its SDK dispatch/protocol tests passed. Actual CEF tests
kept all four existing safety cases green but failed both newly added ordinary
commit/cancel recovery cases: CEF emits an untrusted composition-end event,
and its range callback does not acknowledge completion. The observer keeps
refusing instead of allowing a page-generated end to clear trusted preedit.
A native backend completion proof is still required. No failing assertion was
ignored or weakened.

### Separate catalogue handoff

The selected **experiment** is multilingual GGML Base, not a new installed
default: `ggerganov/whisper.cpp` revision
`5359861c739e955e79d9a303bcbc70fb988958b1`, `ggml-base.bin`, 147951465 bytes,
SHA-256 `60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe`.
The checked-in `dist/local-models/whisper-base/upstream-receipt.json` identifies
the exact public artifact and complete OpenAI MIT notice: 1063 bytes, SHA-256
`b5d65a59060e68c4ff940e1eddfa6f94b2d68fdf58ed7f4dd57721c997e35e9d`.
The decoder is pinned whisper.cpp 1.8.3 from whisper-rs-sys 0.15.0; its archive
receipt is `native/upstream/whisper.cpp-receipt.json` (archive SHA-256
`6986c0fe081241d391f09b9a071fbcbb59720c3563628c3c829057cf69f2a56f`).
Public FLEURS fixtures retain their pinned CC-BY-4.0 attribution and receipts.

The review-only candidate now contains the unchanged U²-Net plus Tiny, Base
and Small. The **approved** `catalog-source.json` and signed image catalogue
still contain only U²-Net, so an ordinary installed desktop still refuses to
load speech until the reviewed release is approved. No new signing identity,
trust root, account, API charge or microphone permission is introduced.

The normal route is the existing manual
`.forgejo/workflows/release-local-model-catalog.yml` on `main`: `review-speech`
fetches exact upstream receipts and creates a review artifact using the
existing protected catalogue identity, with trust-key equality checked.
After explicit release approval and promotion of reviewed source/signed image
inputs, `publish` uses the existing protected package credential, immutable
model/license URLs and anonymous exact readback, then reproduces and compares
the signed image inputs before publishing the catalogue. Image deployment is
a separate authorized operation. Source support for the four-entry candidate
is prepared; none of these signing/publication/activation operations ran here.

The actual automatic-approval denial in this task was **removing PR #2974's
release hold**: forwarded authorization was insufficient in this thread. Do
not retry that removal or bypass it. Catalogue publication and deployment were
excluded by the earlier delegation; they were **not** separately denied by
automatic review. They still require explicit authorization. This release
boundary does not stop independent source engineering or synthetic acceptance.

### Shared personal vocabulary consumer

The authenticated session host supplies the dictionary owner's existing
`settings_store::personal_dictionary::Vocabulary` reader through
`State::set_dictation_vocabulary`. Replacing that binding cancels the current
recognition/field session and discards private recovery before a successor can
start. The host owns service lifetime, identity binding, migration and delivery;
voice creates neither another store nor an app/room config-file discovery path.
The native compositor obtains the service from the installed authenticated
settings source and subscribes to its change signal. Its watch holds only a
weak service reference and compares opaque reader binding epochs without
reading dictionary contents on the frame thread. Word edits preserve the
binding; account switching, including a coalesced same-person logout/login,
replaces it before input/result handling. Service destruction also wakes the
watch and revokes access. The shared service and its store remain the dictionary
owner's implementation, landed through PR #3302.

The actor captures accepted casing and exact language applicability off the
compositor thread. Automatic language uses only untagged words. It retains at
most 256 spellings / 16 KiB and rechecks revision and complete-content fingerprint
before semantic controls and replies, plus every 250 ms while inference is busy.
Any dictionary change, removal, read failure, revocation or service drop clears
its hints and stops/reaps the owned worker. It never silently substitutes an
empty dictionary for a failed authenticated reader. The themed status uses
English/German Fluent keys and asks the user to start a fresh session.
An ordinary edit is distinguished from losing the authenticated reader: it
retains the last already-visible draft for explicit copying, with a warning
that the draft may be incomplete. It never inserts a partial result or exports
it automatically. Revocation/read failure does not retain that draft; replacing
the person binding also erases any earlier recovery. New vocabulary applies to
a fresh session, without reusing an old decoder prompt.

The existing private worker configuration optionally carries accepted spellings
(max 32 KiB JSON, with no added field for empty vocabulary). Native inference
retains at most 64 ordinary BPE tokens at whole-word boundaries, within the pinned
Whisper text context; trailing words that do not fit are omitted. Special tokens
and control/whitespace-containing hints are refused. Hints affect decoder
context, never substring replacements in the transcript or executable
instructions. Configuration bytes/words, session hints and native token arrays
are zeroized; failed replacement clears previous native vocabulary. Empty
vocabulary keeps the decoder's previous parameters.

Qualification uses the actual shared Service and private actor pipe: removal,
rebinding to the same person, and service destruction stop a busy owned peer
without microphone access. A frontend regression checks binding replacement
retiring the original native delivery/recovery. The pinned Tiny/JFK worker probe
runs as UID 65534 in private PID/network namespaces with an empty device tree,
repeats final/cancel/restart, and refuses hallucinating an unspoken TiLo hint.
It additionally checks special-token rejection and exact baseline restoration
after failed vocabulary replacement. These are bounded integration proofs, not
rare-name accuracy, multilingual high accuracy, physical older-device timing or
a new full desktop journey. Model publication and release hold remain separate.

### Native composition cancellation remains excluded

The separate CEF experiment retains a stronger real controlled-form test: a
`beforeinput` handler replaces the composing input's value with `newer`, selects
that text and calls `preventDefault`. Native empty composition then erases it;
the observed `insertCompositionText` event is trusted and non-cancelable.
An earlier exact element/value/selection precheck fixes replacements made before
dispatch, but cannot protect changes made inside this event. No failing
assertion was removed and this recovery experiment is not in the candidate.

The exact pinned Chromium 152.0.7977.83
[renderer source](https://chromium.googlesource.com/chromium/src/+/refs/tags/152.0.7977.83/third_party/blink/renderer/core/editing/ime/input_method_controller.cc)
confirms the relevant ordering: composition update and beforeinput run before
the native deletion. Empty `SetComposition` also calls
`SetEditableSelectionOffsets` after `ReplaceComposition`. A safe renderer fix
must reject stale target/text/selection after event handlers and propagate that
refusal so the caller does not subsequently move the new selection. This fix
has not been implemented or qualified against the pinned CEF binary. An actual
Undo probe restored the original selected text but emitted no compositionend,
so it is not accepted as proof of native idle or as a cancellation substitute.
Changing to CEF's direct cancel API does not remove this primitive: the pinned
[offscreen CEF bridge](https://github.com/chromiumembedded/cef/blob/708dc14/libcef/browser/osr/render_widget_host_view_osr.cc)
delegates to
[RenderWidgetHostImpl::ImeCancelComposition](https://chromium.googlesource.com/chromium/src/+/refs/tags/152.0.7977.83/content/browser/renderer_host/render_widget_host_impl.cc),
which sends an empty `ImeSetComposition` with an invalid replacement range and
zero selection offsets. The guarded refusal must therefore live in the
renderer operation, rather than substituting this browser-host API.


### Initial-context and localized cleanup qualification

Public paced speech exposed an opening-context problem independently of native
field delivery. A fresh actor now anchors the first draft timer to successful
capture, rather than model startup or consent time. The worker independently
defers drafts until two seconds of recorded audio have accumulated. This does
not gate finals: stopping earlier decodes all available audio, and a deferred
empty draft is never cached as a final. Subsequent drafts retain the one-second
interval. Deferred input still advances the append-only audio check; shrinking
it retires the session. The production recognition regression covers early
finish, cache reuse and refusal of regressed audio.

The new public-only paced probe can assemble up to six receipt-checked clips,
select automatic recognition for language transitions, add accepted public
vocabulary, and inject seeded white noise at measured 10/20 dB SNR. It records
source/input digests, achieved SNR and clipping. Synthetic noise and concatenated
language transitions are stress probes, not real noisy/code-switch acceptance.
Unknown peer error strings are redacted; only the worker's typed error enum is
reported. Four Python tests cover reproducibility, actual SNR/clipping,
unobservable noise rejection and arbitrary-message redaction.

Before the worker gate, one public Base clip per language scored English 5.26%,
German 33.33%, French 35.00%, Spanish 13.79%, Arabic 53.33% WER and Mandarin
39.13% CER. German hints `Hongkong`, `Kowloon`, `TiLo` restored both spoken names
and reduced that clip's WER to 16.67%, without hallucinating unspoken TiLo.
This is a vocabulary integration benefit on one public clip, not general
rare-name accuracy. English at synthetic 20/10 dB scored 10.53%/21.05% WER;
the assembled English→German transition scored 24.32% WER. None meets the
very-high-accuracy requirement. An initial German draft failure did not
reproduce in the subsequent complete matrix; its failing attempt is preserved,
not removed from the qualification record.

Delaying the first request to two seconds reduced English revisions from five
to three on two CPUs (first text 2.349→3.165 s), and from four to one in the
one-CPU comparison (3.178→4.144 s). Both began with `However,` rather than the
incorrect `Thank you.` and kept the same final. CPU affinity on a build machine
is not physical older-device timing. Replaying a two-pass agreement policy (inspired by the
[upstream local-agreement approach](https://github.com/ufal/whisper_streaming))
over earlier transcripts instead delayed first text to 4.631/7.587 s and still
revised nine times on the long clip; it was not promoted. These measurements
expose the remaining accuracy/latency tradeoff, not stable streaming acceptance.

The actual context-gated worker was then rebuilt and run on English,
dictionary-biased German and Arabic through its confined private pipe. All
three completed; each returned an empty first draft below 32,000 samples and
never emitted text below that bound. English first text was 3.374 s with three
revisions; German first text was 3.610 s with seven revisions and both exact
name spellings, again without unspoken TiLo; Arabic first text was 4.879 s.
Final WER remained 5.26%, 16.67% and 53.33%, respectively. This independently
qualifies the gate's real wire behavior and preserves the accuracy limitation.
All three stayed below the unchanged 1 GiB address-space limit (peak
871,300–873,172 KiB); no decoder, permission or sandbox limit was relaxed.

Compact source/model/log receipts and scored results are committed in
`dictation-quality-probes.json`; raw public probe logs remain in the task evidence
worktree. No microphone or private dictionary contents entered these probes.

## Fixed backend comparison suite

`tools/release/speech_evaluation.py` freezes the same public inputs for alternative
local engines. On the owned remote builder, `prepare-public target/<corpus>` stages
exactly eleven members per language from the pinned FLEURS archives, without
fetching a model. `freeze target/<corpus>/manifest.json target/<suite>` verifies
source/converted-audio/transcription receipts and produces deterministic PCM16
fixtures. Previously exposed indices 0–7 remain development data; reserved
indices 8–10 form the holdout. Identical source audio, converted audio or normalized
same-language references across the boundary fail before suite publication.
An operator exposure ledger is still necessary: reserved means this tool did not
decode it, not that no person or upstream model has ever seen FLEURS. Model
training overlap is unknown. Never adjust configuration after observing holdout
results and continue calling those results holdout.

The fixed suite contains 21 development and 32 holdout conditions: six-language
read speech, deterministic 20/10 dB white-noise stress, and English→German and
French→Spanish assembled transitions. Development-only German vocabulary hints
are predeclared `Hongkong`, `Kowloon`, and unspoken `TiLo`; holdout vocabulary is
never mined from reference text. Neither noise nor concatenation is natural
conversation evidence. Four numeric self-correction text fixtures are explicitly
marked **without audio**. Real noisy speech, natural code-switching, spoken numeric
repairs, unseen spoken names and physical older-device qualification remain
required missing coverage. No synthetic cleanup fixture can satisfy those gates.

Each backend adapter must emit one JSON result per selected split case with exact
suite and PCM digests, engine/source/model/license/adapter/configuration receipts,
hardware/thread profile, raw final text, cumulative draft text/sample counts and
source-start wall times, stop-to-final duration, observed RSS/address-space peaks,
serial child CPU time and total wall time. No confidence field is required or used.
`score <suite.json> <results.json> --split development|holdout` rejects missing or
duplicate conditions, failed workers, altered suites/inputs, mixed configurations,
unobserved resource metrics and nonmonotonic timelines. Existing production-worker
logs can be converted with `import-stream <suite.json> <log> <engine.json>
<results.json> --split ...`; the conversion requires a complete selected split and
preserves failures rather than dropping them. The production probe needs a
receipt-checked model entry alongside suite cases; its existing ephemeral witness
and private namespace remain mandatory. Alternative-engine adapters must preserve
that hardware-free namespace and the unchanged 1 GiB experiment cap.

Scores remain WER per language, with no-space CER for Mandarin. Numbers, accents
and Simplified/Traditional differences remain errors. Results group by language
and condition, never a misleading cross-language average. First visible partial
is distinct from the earliest exact nonempty prefix that survives every subsequent
draft and the final. The latter also reports its unit count and the first stable
prefix of at least three words (six Mandarin characters). Stability is retrospective,
not a production confidence guarantee. Revisions include a changed final, so a
stable-looking last draft cannot hide a correction at Finish. A final-only engine
cannot claim streaming qualification from an empty timeline. Finalization includes
cached finals honestly; cold load, actual capture/field acknowledgement and physical
hardware remain separate measurements.

The first actual remote corpus freeze produced 53 conditions, suite SHA-256
`9995db50a190bcd818373886e06c404c9d9009cbb85a76cf166dd65daf8ef8fd`.
No reserved holdout clip was decoded. Audio and references stay in the owned remote
`target/speech-evaluation-corpus-v1` and `target/speech-evaluation-suite-v1`; no audio
or model weights are committed or installed. Offline regressions independently
exercise stable/final revision accounting, language-specific scoring, source
leakage, deterministic noise, stale/missing/failed comparisons and receipt checks.

### Bounded alternative-engine experiments

`probe_asr_candidate.py` and its two native fixtures reproduce the current
remote comparison. They admit only the exact frozen public suite digest and
explicit development cases, verify every model/audio receipt, unshare devices
and networking, restrict affinity to one/two CPUs and retain the production
1 GiB address-space cap. A 960 MiB live-address-space guard leaves headroom and
kills only the owned namespace process group. RSS and virtual size are distinct;
a guard stop is not a measured out-of-memory event or an accuracy result. The
probe refuses vocabulary conditions it cannot implement rather than silently
ignoring hints. No production model selection, catalogue, acceptance or trust
store changes. Native adapters retain raw decoder strings; cleanup cannot hide
ASR errors. The receipt ledger is `dictation-engine-comparison.json`.

The controlled Whisper comparison links the exact production bridge and existing
pinned 1.8.3 native archives, with greedy/best-of one, no previous-text context,
no VAD, serial mel preparation, the existing CPU baseline and identical public
input. It performs a whole-utterance pass before source-paced cumulative drafts.
Paced finals mirror the unchanged-input cache, so their near-zero finalization
excludes worker IPC and original-field acknowledgement. Cold load and source-start
clock are separate. Model revisions stay
`ggerganov/whisper.cpp@5359861c739e955e79d9a303bcbc70fb988958b1` (MIT):
Small Q5_1 SHA `ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb`,
Small Q8_0 SHA `49c8fb02b65e6049d5fa6c04f81f53b867b5ec9540406812c643f177317f779f`,
and Base Q8_0 SHA `c577b9a86e7e048a0b7eada054f4dd79a56bbfa911fbdacf900ac5b567cbb7d9`.

All 24 matched Base FP16/Q8 first-clip offline/paced conditions completed.
Offline final error fractions matched in each language: English 5.26%, German
33.33%, French 35%, Spanish 13.79%, Arabic 53.33% WER; Mandarin 39.13% CER.
German raw finals differed in one capitalized character; the other five were
byte-identical. Q8's sampled peak RSS was about 64 MiB lower. It is an efficiency observation,
not a high-accuracy default or an unseen-data result. In the paced German clip,
the first stable prefix of at least three words arrived at 19.860 s for Base and
18.261 s for Q8. Q8 produced fourteen non-prefix revisions versus twelve; this
retrospective stability measure supplies no confidence gate. Both Small
quantizations crossed the headroom guard before a final on the matched French
clip, on one and two CPUs, with both default and explicitly isolated one-arena
allocation profiles. These guarded experiments do not supersede earlier Small
quality observations or establish that every configuration exceeds the hard cap.

The second candidate is the official
[NVIDIA NeMo-Speech.cpp](https://github.com/NVIDIA/NeMo-Speech.cpp/tree/8642eaa5cc51efbc17ad0f3e433944ba858a873f)
CPU runtime, revision `8642eaa5cc51efbc17ad0f3e433944ba858a873f`, with required
pinned llama.cpp dependency and SentencePiece
`31646a467d2051eb904e0b45de3a73e91fe1c1e3`. ASR was built with microphone capture,
GPU backends and diarization disabled, two build jobs and AVX512 disabled.
Its Apache-2.0 and third-party notices are retained in the owned source/dependency
checkout. Official
[Nemotron 3.5 ASR Streaming 0.6B](https://huggingface.co/nvidia/nemotron-3.5-asr-streaming-0.6b/tree/ea30d66debe3740a08b573244286791d423d6b3e)
Q8 weights are pinned to `ea30d66debe3740a08b573244286791d423d6b3e`, 742090464 bytes,
SHA `3fc991d3badad7277c11030a7519832cddaf2057aafed6d4b25147e953a070b1`.
The [OpenMDW-1.1 terms](https://openmdw.ai/license/1-1/) and exact model card were
read and retained before download: license-page SHA
`54761c8fc8f5ba52f73ba50bdb48d8ad2ff6aff9be8be94cbe83abcef69b43ba`, model-card SHA
`a3344caadf796c084c6b90a9fa5978068fd45e3a019790bebe50489bb3c0f7b7`.
There was no account prompt, paid service, click-through or model redistribution.

The CLI offline path hit the virtual-space guard. Its file `--stream` mode is
unpaced and is deliberately refused as a paced measurement. The separate native
C++ adapter configures CPU threads and the language before stream creation,
one postprocessing worker, no batching, 160 ms new-audio chunks and an unchanged
source clock with retained backlog. The pinned public C ABI omits CPU thread
configuration, so this review fixture uses that revision's internal C++ API;
it is not a proposed stable application ABI. No confidence/stability output from
the engine is used as an admission gate. Actual native streaming startup under
the hard cap refused a **903046144-byte (861.21 MiB) CPU buffer** and exited with
an observed runtime error before a transcript. Accuracy, first partial, stop-final
and dictionary boosting were unmeasured at this stage. The subsequent separately
authorized 2 GiB characterization is recorded below; the production cap stays
unchanged. The existing production worker admits only Whisper
GGML artifacts up to 256 MiB; the 742 MB GGUF candidate also requires a separate
reviewed backend/admission design. The GGUF ran only in the experimental native namespace; no production
admission limit changed.

The first native build's compiler launcher failed; only that isolated CMake
configuration disabled caching and was rebuilt. The initial new wrapper's CPU
accounting measured bwrap rather than the decoder and is explicitly invalidated.
The corrected probe runs GNU time around the native child inside the PID
namespace. Guard kills report CPU as unknown plus a sampled lower bound. An
actual `/proc` process-disappearance race interrupted the matrix after nineteen
successful records; the sampler was repaired and only five missing conditions
were rerun. Both original failures remain in the evidence store. Missing process
observability fails the probe; it never produces acceptance by skipping it.

Two additional Base Q8 probes used the actual private production worker, one CPU
and the predeclared German Hongkong/Kowloon/TiLo hints. The ephemeral test helper
initially refused the missing Q8 receipt before recognition; exact size/hash
admission was added only to that helper and remotely rebuilt. Both one- and
two-second opening profiles completed, with final WER 16.67%, both spoken names
present and no TiLo in the final. They are **rejected for streaming quality**:
early drafts hallucinated URLs, and the two-second profile included unspoken
TiLo in a draft. Its first text arrived at 3.890 s, the first retrospectively
stable three-word prefix at 10.258 s, with four revisions, 19.093 CPU seconds,
295644 KiB peak RSS and 800220 KiB peak virtual space. Stop-to-worker final was
2.902 ms and excludes actual field acknowledgement. No default changed and no
heuristic filter or confidence gate was added to hide these failures.

Optional faster-whisper CPU INT8 has not been run. Its published eight-thread,
beam-five memory figure does not establish feasibility or infeasibility under
this one/two-thread, beam-one cap. Natural speech conditions, the 32 reserved
holdout conditions and physical microphone/older-device acceptance remain unrun.

### Memory policy and supported low-memory configurations

The 1 GiB cap originates in the candidate's `speech::worker::prepare`, introduced
by `b2ff613b2e`, as a fixed confinement policy alongside disabled dumps, tracing
and inherited descriptors. It is `RLIMIT_AS`, a per-process virtual-address-space
limit, not a measured requirement for a 1 GiB physical device. Shared libraries,
thread stacks and allocator reservations count against it. The production limit
remains unchanged; device fitness additionally needs the compositor, OS and other
apps measured together. Artifact admission's separate 256 MiB bound also remains.

`build_asr_memory_characterization.py` builds private experimental copies against
the same pinned 1.8.3 archives. It checks the exact original bridge digest, adds
public-only upstream allocation logging and `/proc` stage measurements, and
compares supported CPU `flash_attn` initialization. It downloads or installs
nothing. The default generated probes keep 1 GiB. The explicit
`--include-2g-characterization` flag generates separately labeled 2 GiB probes;
these cannot count as production qualification. `run_asr_memory_characterization.py`
rechecks available memory (minimum 8 GiB) and memory pressure (some.avg10 at most
5%) before each serial one/two-CPU development case. Missing observations fail.
The first matrix stopped at 5.45% pressure. After independent source analysis,
one reassessment found 1.35%; only four missing cases resumed. Both receipts remain.

The measured native allocation breakdown below uses upstream **decimal MB**, not
MiB or an estimate of resident memory. Compute pools are separate persistent
allocations; their sum is not an observed peak. KV is self/cross/padding combined.

| Engine/configuration | Native weights MB | KV MB | Conv / encoder / cross / decoder pools MB |
| --- | ---: | ---: | --- |
| Base FP16 | 147.37 | 28.31 | 16.28 / 85.88 / 4.66 / 96.37 |
| Base Q8 | 81.18 | 28.31 | 16.28 / 85.88 / 4.66 / 96.37 |
| Small Q5, default attention | 189.49 | 80.21 | 22.42 / 128.02 / 6.20 / 97.28 |
| Small Q8, default attention | 263.87 | 80.21 | 22.42 / 128.02 / 6.20 / 97.28 |
| Small Q5/Q8, CPU flash attention | 189.49 / 263.87 | 80.21 | 22.42 / 33.85 / 6.20 / 97.28 |

Full-context CPU flash attention saved 94.17 MB of Small encoder workspace.
Both quantizations completed French under the original 1 GiB limit, with 10%
WER. Q5 peak RSS was 385884 KiB and virtual space 887600 KiB; Q8 was 531080 KiB
and 959952 KiB. The earlier non-flash guard failures remain valid for that
configuration, not a rejection of all Small configurations. Separately labeled
2 GiB non-flash French characterization completed: Q5 495712 KiB peak RSS,
1027348 KiB virtual peak; Q8 568204 KiB RSS, 1099984 KiB virtual peak, both 10%
WER. Q5's virtual peak illustrates that the 960 MiB guard can reject a run that
would narrowly fit 1 GiB; Q8's observed peak exceeds 1 GiB. No cap was raised in
the deployed worker and no model/configuration was promoted.

Stage receipts confirm the existing loader briefly retains both verified input
bytes and copied weights, then releases input before allocating decoder state.
For Q5 flash, RSS went from 191144 KiB (input) to 385884 KiB (overlap) and
200252 KiB after input release. The existing bridge frees the old state before
creating a new independent draft state; old/new KV arenas are not intentionally
kept together. There is no persistent Whisper streaming encoder cache here:
cumulative audio is reprocessed per draft. PCM/float/mel storage and allocator
retention remain additional costs, bounded by the 120-second session limit.
Quantizing weights does not quantize these KV or compute pools. The supported
`n_max_text_ctx` field limits prior prompt tokens, not initial KV allocation.
`audio_ctx` is applied after initial state allocation, so it cannot by itself
avoid the cold full-size pools. An experimental rounded shorter context was
actually measured and rejected: French WER rose to 90% offline and paced, despite
first text at 2.630 s. It is available only as an explicit reproduction flag.

Full-context Q5 flash development finals were English 5.26%, German 0%, French
10%, Spanish 3.45%, Arabic 40% WER and Mandarin 47.83% CER. These exposed single
clips are not broad accuracy evidence; Mandarin regressed from Base. French
paced first text was 7.236 s on two CPUs and 12.183 s on one; meaningful stable
prefix 17.917/22.791 s, CPU 31.54/20.78 s. This is a plausible **memory** path for
a 4–8 GiB device, even under the current process cap, but cannot establish the
required real-time experience or whole-device fitness. Physical hardware,
production-worker dictionary/focus/privacy journeys and unseen speech remain
unqualified. Completing the same six-language full-context Q8 comparison matched
Q5 in five languages but improved Mandarin to 17.39% CER. Q8 offline peak RSS
ranged 530044–531680 KiB and virtual peaks 958992–962884 KiB. Q8 French first
text was 6.597/10.825 s on two/one CPUs, meaningful stable prefix 15.821/20.102 s,
CPU 27.57/18.18 s and final WER 10%. These remain slow, exposed development
observations, not an older-device or high-accuracy qualification. Detailed receipts
are `dictation-memory-characterization.json` (23 records, including retained
Nemotron failure and two rejected shorter-context runs).

Nemotron still failed startup under the separate 2 GiB budget, before accuracy
or latency measurement. Native memstats observed a 31.96 MiB model container,
then the same refused 903046144-byte CPU tensor buffer. Peak virtual space was
1612340 KiB and RSS 87772 KiB before refusal. Those are observed incomplete
allocations, not total engine memory. Complete weight/cache/decode-workspace
breakdown remains unavailable because startup failed; the 742090464-byte GGUF
alone is not its RAM requirement. Pinned `GGUFLoader` reads tensors into a bounded
largest-tensor scratch buffer and `Session::setup` releases it after loading;
it does not retain a mapped copy of the entire GGUF. Model tensor storage and
per-backend metadata arenas allocate before scheduler compute pools, which
reuse graph liveness. Source also includes a temporary F32-to-F16 conversion
buffer when needed. Metadata arenas reserve 64 MiB per default backend context;
the pinned public recognizer configuration does not expose that budget. Further
native allocation work is required; neither higher-budget failure rules out an
optimized future runtime. No known-over-2-GiB model was attempted.

### Streaming allocation diagnosis and bounded prototypes

The follow-up glibc allocator trace identified the original NeMo failure. The
pinned scheduler allocates **1323844080 bytes** of largely untouched metadata
for its internal graph capacity of 65536. Immediately before the 903046144-byte
model-container request, virtual space was 1546816 KiB and resident space only
48184 KiB. Existing virtual mappings plus that request total 2486985728 bytes,
above the observed 2147483648-byte `RLIMIT_AS`; `posix_memalign` returned
`ENOMEM`. The remote cgroup's `memory.max` was `max`, with about 8 GB current
usage and 167 GB available host memory. This failure is address-space exhaustion,
not a measured resident-memory ceiling or duplicate mapped weight file.
`ggml-backend.cpp` reserves scheduler split metadata in proportion to graph
capacity and maximum split inputs. The `65536` constant is internal to
`Session`; the pinned public recognizer has no graph-capacity option.

`characterize_nemo_scheduler.py` records a **review-only internal prototype**
with graph capacity 8192. It preserves near-capacity warnings/errors, single
stream execution and all resource guards; original source, binaries and build
intermediates are restored and hash receipts retained. Scheduler metadata
requests fall to 165480912 bytes each for two sessions. Observed graph sizes
were 2733 encoder nodes, 193 positional projection nodes, 63 RNNT nodes and
8 joint nodes. Fitting these exposed graphs does not qualify other utterances
or batching. This is an upstream optimization experiment, not a supported
configuration change or production backend.

The language-checked native fixture now rejects an unrecognized selected tag
before stream creation. The pinned language resolver otherwise silently falls
back to `auto`. Its alias list is not evidence of trained language coverage.
On the six exposed clips, two-CPU streaming produced EN 21.05%, DE 5.56%,
FR 0%, ES 6.90%, AR 6.67% WER and Mandarin 8.70% CER. Every observed draft
was a prefix of later drafts and final text, with zero non-prefix revisions.
First text ranged from 1.174 to 4.859 s; the retrospective stable prefix of
at least three words/six Mandarin characters ranged from 1.817 to 5.333 s.
These clocks begin at capture, include lazy streaming weight initialization,
and exclude the separately reported metadata constructor time. No causal
confidence gate is inferred from retrospective stability. English remains
worse than Base's matched 5.26% WER under the unchanged strict normalization.
One-CPU French also produced 0% WER, first text 2.992 s, stable prefix 3.620 s
and stop-final 289.636 ms. Default sixteen-slot peak RSS was approximately
942 MiB and virtual space approximately 1.50 GiB: **outside production limits**.

The supported `batching.state_arena_slots=1` setting was measured separately,
even with batching disabled. Original capacity 65536 still failed under 2 GiB,
this time refusing a 726098944-byte container. With the internal 8192 prototype,
the supported single-stream setting reduced model containers from
31.96 + 861.21 MiB to 31.63 + 692.46 MiB, a measured 169.08 MiB reduction.
French retained 0% WER, first text 2.933 s, stable prefix 3.571 s and
stop-final 152.491 ms; peak RSS 790520 KiB and virtual space 1394548 KiB.
It still does not fit the production 1 GiB address-space cap or 256 MiB
artifact admission limit. No default or cap changed.

Native pool telemetry observed encoder scheduler buffers growing from
0.45 to 12.66 MiB (12.33 MiB with one state slot), and decoder pools from
0.04 to 0.11 MiB. Graph metadata of 0.13–0.31 MiB and named graph device
arenas of 0–0.01 MiB are separate counters, not total compute/cache memory.
The cache-aware encoder's indexed K/V/convolution arenas and RNNT state reside
inside `model_tensor_container`; `make_state` leases an arena slot rather than
allocating a separate `SessionState`. Consequently the review's
`SessionState` diagnostic was not reached and is not a zero-byte measurement.
The observed sixteen-to-one capacity delta includes persistent state and
alignment. A complete tensor-by-tensor weight/state split remains unmeasured;
neither GGUF file size nor the graph's sum of tensor sizes is total live RAM.

Headroom checks stopped the first matrix at 11.7% memory pressure; only the
five missing cases resumed after pressure fell below 5%. Allocator and both
original-scheduler failures remain recorded. No reserved holdout, physical
microphone, dictionary boosting, natural noise, natural code-switching or ARM
acceptance was attempted in these comparisons.

### Smaller English-only streaming comparison

The official [sherpa-onnx model documentation](https://k2-fsa.github.io/sherpa/onnx/pretrained_models/online-transducer/zipformer-transducer-models.html)
identifies the English 20M streaming Zipformer. The original
[training weights](https://huggingface.co/desh2608/icefall-asr-librispeech-pruned-transducer-stateless7-streaming-small)
and [ONNX export](https://huggingface.co/csukuangfj/sherpa-onnx-streaming-zipformer-en-20M-2023-02-17/tree/d42f2d9f7ca24806fb667456a18a9f1b60f70d16)
declare Apache-2.0. Export revision is
`d42f2d9f7ca24806fb667456a18a9f1b60f70d16`; INT8 encoder/joiner plus decoder
total 45197026 bytes. This model supports **English only**; it cannot cover
the multilingual goal and is not an identified user bookmark.

The remote Python adapter uses exact public `sherpa-onnx`/`sherpa-onnx-core`
1.13.8 wheels and NumPy 2.4.2, with hashes in
`stage_zipformer_review.py`. Apache and permissive NumPy distribution notices
are retained in the isolated directory, with no global package install.
The hardware-free namespace has no microphone or network, cleared environment,
CPU provider, one/two CPU affinity, one OpenBLAS thread, 1 GiB `RLIMIT_AS` and
960 MiB headroom guard. Every extracted runtime file is checked against the
pinned wheel. Input chunks follow the actual 160 ms source clock; finish uses
the pinned API's documented 0.5 s zero padding, explicitly reported separately
from captured audio and included in stop-final work.

Clean English WER was **36.84%** on both one and two CPUs; synthetic 20/10 dB
white noise yielded **42.11/73.68%**. No number, accent or reference-aware
normalization was added. Peak RSS was 135576–135980 KiB, virtual space
445616–470724 KiB. Clean first text was 2.086/2.092 s, retrospective stable
three-word prefix 3.046/3.045 s on one/two CPUs; stop-final 11.932/23.154 ms.
Reported model construction is separate from the source clock. Streaming
memory/latency are promising, but these exposed accuracy results reject
promotion. No multilingual coverage or natural-environment claim is made.

`dictation-streaming-characterization.json` contains fifteen no-transcript
receipts: original failures, six-language prototype, one-CPU French, pool
telemetry, supported one-slot comparison and four English/noise cases.
No audio, raw transcript, weight artifact or installed model is committed.
Renderer cancellation is owned separately. Recognition quality, production
backend admission, broader unseen speech and physical-device journeys remain
engineering and acceptance work. All release holds remain in place.


### Serial scheduler reservation optimization under the original cap

A second isolated source prototype retains the original **65536-node graph
capacity**. In a single-backend, nonparallel scheduler every node and source
uses backend zero, so no cross-backend split or duplicated input copy is
needed. Reserving one split for that case removes the worst-case split
metadata reservation; multi-backend/parallel reservations remain unchanged.
An assertion fails if this serial path ever generates more than one split.
Default tensor metadata arenas are reduced from 64 to 4 MiB each; allocation
and graph-capacity guards remain in place. These are native source experiments,
not supported production settings. Original sources, libraries and object/archive
intermediates are restored after building, with exact hash receipts.

With the supported one-stream state slot, **all seven exposed development
runs pass the actual unchanged 1 GiB `RLIMIT_AS` and 960 MiB live-address-space
guard**. Two-CPU peak virtual space is 946764–947504 KiB (about 925 MiB),
with peak resident memory 792392–792880 KiB (about 774 MiB). The remaining
live-guard margin is about 35 MiB. RSS describes resident memory; virtual
address reservations are what this worker limit constrains. Neither quantity
establishes whole-device fitness on an older laptop.

| Language | Raw error | First text | Stable prefix | Stop-final |
|---|---:|---:|---:|---:|
| English | 21.05% WER | 1.651 s | 2.937 s | 151 ms |
| German | 5.56% WER | 4.856 s | 5.332 s | 101 ms |
| French | 0% WER | 3.004 s | 3.782 s | 150 ms |
| Spanish | 6.90% WER | 2.615 s | 3.572 s | 101 ms |
| Arabic | 6.67% WER | 1.173 s | 1.812 s | 158 ms |
| Mandarin | 8.70% CER | 2.613 s | 3.571 s | 150 ms |

One-CPU French also retains 0% WER, with first text 2.977 s, stable prefix
3.616 s and stop-final 279 ms. All observed drafts remain prefixes of later
text. These clocks use real 160 ms source pacing, include lazy streaming
initialization and report constructor work separately. Stability is measured
retrospectively and does not create a causal confidence gate.

English's four strict word edits include three edits from spoken-cardinal
versus digit rendering, plus a singular/plural substitution. The raw 21.05%
score is retained; no reference-aware normalization or altered acceptance
metric is applied. Formatting needs a separately tested language-aware path.
Pinned native ITN requires a build with `NEMO_SPEECH_WITH_NORM`, an enabled
Sparrowhawk normalizer and grammar assets. This reviewed build has no ITN
support or supplied grammar directory. Toggling `verbatim_transcripts` alone
would leave normalization disabled, so no such result is claimed.

`dictation-streaming-optimization.json` retains nine no-transcript receipts:
two separate 2 GiB review runs and seven runs under the actual 1 GiB limit.
`characterize_nemo_serial_scheduler.py` builds and restores the isolated source
prototype; `characterize_nemo_serial_matrix.py` replays only the six exposed
first clips plus one-CPU French. The 742090464-byte Q8 artifact still exceeds
production's unchanged 256 MiB admission limit, and this native adapter is not
the production worker. No model is installed or promoted. Unseen speech,
natural noise, natural language switching, broader session/cache growth and
physical-device validation remain to be measured; holdout speech is untouched.


The follow-up exposed stress matrix retains eight successful synthetic-noise
cases, all under the unchanged 1 GiB cap/960 MiB guard, with zero observed
non-prefix revisions. English stays at 21.05% WER at both 20/10 dB; German
is 5.56/22.22%, French 0/10%, and Spanish 6.90/3.45% at 20/10 dB. These are seeded
white-noise stress inputs, not recordings of natural noise. German first text
is 5.172/5.494 s, so the unchanged raw accuracy does not establish low latency.

Before Spanish 10 dB, PSI memory `some.avg10` rose to 7.05%, exceeding the
predeclared 5% guard. A later guarded resume stopped before decoding at 13.16%.
After recovery to 1.12%, the missing Spanish case completed; the next Arabic
case stopped before decoding at 9.47%. All three stops and all eight completed
cases remain in the ledger; six cases,
including Arabic/Mandarin noise and both assembled transitions, are pending.
No completed case was rerun, no guard was raised, and reserved speech remains
untouched. `characterize_nemo_serial_stress.py --resume-from` permits only the
predeclared exposed cases and resumes missing work under the same guards.

A smaller public FastConformer export was researched without downloading its
weights. The [exporter's source](https://github.com/k2-fsa/sherpa-onnx/blob/master/scripts/nemo/fast-conformer-hybrid-transducer-ctc/export-onnx-transducer.py)
loads the fixed `stt_en_fastconformer_hybrid_large_streaming_1040ms` checkpoint;
its [original card](https://catalog.ngc.nvidia.com/orgs/nvidia/teams/nemo/models/stt_en_fastconformer_hybrid_large_streaming_1040ms)
points to NGC terms. The English-only
[138 MB ONNX export](https://huggingface.co/csukuangfj/sherpa-onnx-nemo-streaming-fast-conformer-transducer-en-1040ms-int8/tree/8fa6b38d75fcaff6ecbf94f52f2e0e0b7395da22)
has no license card. It must not inherit the separately named CC-BY-4.0
`streaming_multi` model's license by assumption. Exact export file hashes were
read, but no artifact download, runtime qualification or user-bookmark identity
claim was made; source/license provenance remains to be reviewed.


### Native vocabulary fusion on the exposed dictionary case

The pinned RNNT decoder implements per-request context trees through
`AsrRequestOptions::speech_contexts`; it does not require a CTC language model.
`DecoderConfig::boosting_tree_alpha` controls fusion, with a default of 1,
per-token depth scaling 2 and a request boost clamp of 5. These are logits
used during actual recognition, not replacement of final text. The source
warns that high greedy boosts can insert or repeat words, so defaults need
measurement before any production adoption.

A separate receipt-checked adapter uses the predeclared exposed German case
and exactly its three vocabulary hints. It preserves the selected-language
check, actual 160 ms source clock, one state slot, CPU-only execution, two
CPU affinity, 1 GiB cap and 960 MiB guard. The canonical no-hint comparison
source/binary and original runtime remain untouched. `off` sets fusion alpha
0; `weak` sets 0.1 with request boost 1. Both complete with **5.56% raw WER**.
First text is 4.858/4.870 s, meaningful stable prefix 5.332/5.432 s for
control/weak. Neither inserts the predeclared unspoken TiLo hint in any draft
or final (case and optional spacing checked without rewriting the transcript).
This single exposed negative is not general hallucination acceptance, and
weak fusion shows no measured accuracy improvement here.

Native peak virtual space is 946972/947912 KiB and resident memory
793144/793312 KiB. Before building, pressure 6.76% stopped the adapter build;
it compiled successfully after recovery with pressure 0.77%. Default fusion
stopped before decoding at 6.77%, so its accuracy and false-positive behavior
remain **unmeasured**. `dictation-vocabulary-characterization.json` retains
both completed cases, both pre-work stops, exact source/binary/library hashes
and pending status. `build_nemo_vocabulary_review.py` and
`probe_nemo_vocabulary_review.py` reproduce this isolated experiment; explicitly
ignored/unavailable vocabulary requests fail rather than count as qualification.

The already integrated shared dictionary reader remains the production
vocabulary authority, with opaque binding epochs and revocation/cancellation
covered by actual frontend journeys. This fixed public native experiment
neither creates a second dictionary store nor promotes the larger backend.
The native Q8 artifact still exceeds production admission; no model, release
hold, recording permission or worker default changed.


### Supported model delivery and native vocabulary follow-up

The [delivery integration plan](dictation-model-delivery.md) separates the existing
constant-space verified downloader from the current Whisper consumer’s 256 MiB
admission boundary. Cached receipt verification now checks the same opened regular
file, refuses Unix symlinks and bounds concurrent growth; no model or cap is promoted.
The exposed German native vocabulary comparison is complete: default fusion scored
0% WER, versus 5.56% with off/weak fusion, with no unspoken TiLo in drafts or final.
First meaningful stable text remained about 5.33 seconds. The sanitized three-row
ledger preserves all pressure stops; this one clip does not qualify general names
or production shared-dictionary integration. Six native stress cases remain pending
a pressure-safe window.


### Prerecorded physical-laptop baseline

The [physical-laptop ledger](dictation-laptop-qualification.json) records the
exact remotely built worker and public French fixture on the connected ASUS
Ryzen 9 7940HS laptop with 32 GiB RAM. The hardware-free, network-free review
namespace exposes no microphone or user home. Two assigned CPUs occupy different
physical cores; the decoder thread setting and observed process thread peak are
recorded separately. The 1 GiB worker address-space cap, 960 MiB guard and bounded
transient service remain unchanged. The collected service is inactive with no PID.

The debug-worker control completed with 7/20 raw word errors, punctuation at
7.953 seconds and its first actual words at **15.316 seconds**, 870556 KiB peak
virtual space and 868438016 bytes scope peak. The earlier first-draft figure
counted an ellipsis. The corrected metric is derived from the unchanged raw log;
new probes report visible and lexical replies separately.
No high/max/OOM events occurred. This is materially slower than the roughly
builder controls and does not meet the intended interaction latency.
Native inference kernels already use effective `-O3` in this profile. The full
release worker built successfully through a remote Cargo drop-in in 9m19s, with a separately pinned
binary receipt. Physical comparison stopped before decoding at fresh headroom
checks of 7667608 and 7291344 KiB, below the unchanged 8388608 KiB requirement. The inherited raw tools label their
runs as builder experiments; the separate physical receipt corrects environment
identity without changing binary provenance. Future probe labels require an
external hardware receipt rather than assuming a host.

This single exposed prerecorded case does not establish natural speech, live
capture, microphone denial/removal, 4–8 GiB or Arm acceptance. A fresh scope
requires at least 8 GiB available host memory; insufficient current headroom
stops comparison before decoding rather than weakening admission.

### Startup, first words and bounded Base attention

The [latency ledger](dictation-latency-qualification.json) distinguishes visible
punctuation from lexical replies and records matched optimized/debug controls,
failed experiments and the actual native attention mode. On the shared builder,
the optimized control reached words at about 5.8 seconds and the debug control
at about 4.4 seconds. Both already use optimized native kernels; changing Cargo
profile alone does not explain the physical-laptop delay.

Actual Base Flash preparation measured about 102 ms verification and 90 ms
native load/allocation; its first inference took 1464 ms. The dominant observed
delay is inference. The reviewed header gate now checks heads and layers in
their separate pinned slots. Optional bounded worker diagnostics report the
attention parameter actually supplied to the loaded engine, without paths,
vocabulary, audio or transcript text. The private worker wire protocol is unchanged.

Base Flash retained the six exposed final error counts and reduced peak virtual
and resident space by about 85 MiB. Its French first words at 4.922 seconds do
not establish a speedup over the warmed standard control at 4.604 seconds.
Warmed scope page-cache charges likewise do not prove an allocator improvement.
No memory admission, address-space, artifact-size or thread guard changed.

The short-encoder-context experiment was reverted: it produced punctuation
quickly, but its first actual phrase contained 14 prefix errors across 16 words.
An earlier Base gate experiment never enabled Flash because it confused layer
and head slots; its apparent timing gain is excluded from qualification. All
recorded measurements here remain development evidence, with no physical
shipping-profile, live microphone or release acceptance claim.


### Whistle native thread admission

The review-only source fixture `tools/release/fixtures/whistle_stream_review.cpp`
links the pinned native C API with a private, signature-verified Fedora libc++
closure. No packages, trust keys or models were installed. The initial linker
failure remains historical evidence; this separately receipted closure links.

Under two-CPU affinity and the unchanged speech limits, the actual C API used
12 threads and reached the 180-second observer deadline. Its published header
has no thread setter; setting commonly used thread environment variables did
not limit this binary. The CLI's documented `--threads 2` is a distinct path.
This native result fails qualification and establishes neither first-word
latency nor successful silence/repeated-session behavior.

The observer now accepts an explicit native thread budget. An actual replay
stopped at six observed threads after 43.47 ms, with no high/max/OOM events;
this is a failed admission, not faster speech recognition. A real child-thread
test exercises the same termination route. The public CLI review now also
requires at most two observed native threads. The exact binary, package
signatures, source and failed-run receipts are recorded in
`dictation-whistle-native-review.json`. The first private native-output JSON
was overwritten by the guard replay; its captured sanitized scope log and
owned process diagnostic remain, and no speech metric is inferred from it.

The new guard also passed the pinned French CLI control with exactly two native
threads, 45300 KiB peak RSS, 8.57 seconds native CPU and 4.370 seconds wall time.
No guard or high/max/OOM event occurred. This validates bounded execution of the
CLI file path; it remains distinct from source-paced or microphone latency.

A separate five-file primary-source audit of Cactus revision
`2cfcdb8568eb9192323b27fd0012a834517b4419` does not repair native provenance.
Its inspected Needle test exercises LLM tool calling, and its transcription
code uses directory-based Whisper/Parakeet graph bundles. No compatibility
with this pinned Whistle `.cact`/C API is established. That repository has an
eligibility-restricted commercial licence, distinct from the Apache-2.0
Needle3 artifact licence; none of its code was incorporated or built.


### Native paragraph intent and current-main integration (2026-10-09)

Native TextInput, CodeEditor, decorators and embedded apps now carry the original
editable field's multiline policy through the shared IME state and Wayland
content hint. The compositor binds that policy to the original dictation target;
password, terminal and single-line targets remain ineligible for LF insertion.
No accessibility presentation inference or synthetic key fallback is used.

The held candidate integrates main 48fae48a334bba0677a893868eef603d858fdd21.
The four actual CI merge conflicts were resolved while preserving both wake
paths, generation-validated SHM mapping and the complete rootfs COPY contract.
The COPY union contains 301 rows with SHA-256
4bec7cf6742bff2a18ba086170146f0f8396c28929446347c11d971fd0e54c70;
its 156-instruction budget and all existing assertions remain intact.

Remote checks on the integrated source passed: shell --tests compilation;
9 pen/key integration tests; native multiline intent (plain input, decorated
input and code editor); 20 dictation integration tests; 5 public transcript
replays; the pending/detached SHM readback mapping guard; 17 opted-in speech
tests; multiline IME intent and the existing decorator forwarding contract.
The full rootfs COPY invariant script also passed. These are synthetic field
and worker checks, not live microphone or deployed-image qualification.
At this checkpoint, browser and block-document paragraph policy remained unfinished; subsequent sections record their qualification.


### Document paragraph transaction and browser qualification boundary (checkpoint db5d7fc7a8)

BlockEditor now advertises paragraph intent only for its focused writable
ordinary text/code field. Find, link/slash controls, visual formulas and tables
do not inherit that policy. A paragraph IME final routes through the existing
document replacement transaction, including a selection within one child line;
the transaction preserves selected-range prefix/suffix and inherited block
continuation rules. Pure LF and trailing LF also commit in multiline TextInput
without invoking Enter/submit or splitting a final at autocorrection boundaries.

The real Notes component regression passes Unicode two-paragraph, paragraph-only
and trailing-break finals through its actual field intent and compositor bridge.
Each replaces the original selection, enters owned persistent history once, and
restores the entire original edit with one undo and one redo. This remains an
output replay qualification, with no microphone or live model activation.

The SDK browser lease can carry a strictly parsed original-field multiline scalar
in equality and its keyed stamp; absent metadata defaults to single-line, and
changed or malformed metadata does not authorize insertion. Browser DOM paragraph
propagation is an unpublished prototype, not qualified or enabled by this commit.
Its real CEF build and three focused tests are recorded in
`dictation-browser-paragraph-native.json`: control-character and single-line-kind
rejection pass; the paragraph test fails on an exact native-event assertion.
Independent native controls establish four input notifications for one undoable
text command, plus three rendered LF for two requested LF in the rich fixture.
The alternate fragment command preserves breaks but loses inherited bold and
changes the native input-event type. Neither notification count nor that fragment
experiment establishes safe universal rich-editor paragraph insertion.

The failed browser assertion remains intact and its prototype is kept locally
for review. Browser paragraph acceptance needs a field/frame-bound native edit
path that preserves literal paragraph semantics, style, undo and cancellation;
the current DOM command is not sufficient evidence. No assertion was weakened,
failing test ignored, production browser sandbox relaxed or runtime deployed.

The final document routing passed all 35 existing BlockEditor unit regressions
and the shared multiline IME test, including literal paragraph-only breaks and
no submit. The 13 SDK browser guard tests also pass. The browser failure evidence
is explicitly separate from these passed source checks.

The final Notes paragraph regression was rerun after composition cleanup and
passed all three cases (1.322 seconds combined; remote compile 3m16s). The
existing staged formatting/contracts/secrets hook passed on the eight intended
publication paths. The failing DOM prototype is deliberately excluded.

The required final remote `check --locked -p shell --tests` also passed on the
exact document-routing source after cleanup (1m49s); no local build or fallback
was used. Latest-head CI status remains a separate result, not inferred from
these focused checks.


### Current-main dependency conflict follow-up

Current-head CI at db5d7fc7a8 found real conflicts with main d85c0c0363 in
Cargo.lock and the terminal manifest before Rust validation. The candidate
therefore integrates current main 426aa5cd2f52a5a9bbbed12b522e35947a5b0804,
which removes the deprecated tilo-xdg-paths shim. Resolution adopts tilo-paths
while preserving the native-editor tilo-process dependency and the optional
Whistle wasmi dependency. No third-party package/version is added, removed or
changed, and Cargo.lock was not regenerated. Workspace membership is now 280.
The complete rootfs COPY contract still passes with its 156-instruction budget.

CI OS run 85479 remains blocked by absent signed RPM snapshot
set-4cee6ac4651b25573a4c65e1dba081d96f0ecc825daa76e7cfa53c0eed58a63c;
its ensure-rpms job reports no main build-rpms.yml run producing the snapshot.
The image job has no log because it did not execute. This is an external image
prerequisite, not evidence of a dictation code/test failure; rerunning the same
non-transient job cannot produce it. The PR stays draft and held.

The current-main integration passed the required remote shell --tests compile
(1m33s), opted-in local-model-service all-target compile (4.17s), and staged
formatting/contracts/secrets checks. These results cover the manually resolved
dependency union; no inference metric is attributed to a new model run.

The actual Notes paragraph test also passed after the 426aa5cd2f integration
(0.543 seconds for the three cases, remote compile 2m57s), proving the newly
imported SDK view changes preserve the original field and owned undo transaction.

### Corrected native browser paragraphs and concurrent recognition (2026-10-09)

The original pinned-CEF rich-field failure is corrected through guarded native
line-break editing, rather than Chromium's block-paragraph insertion. The exact
sample now renders two requested LF as two LF, keeps both paragraphs inside the
original bold style and has one native undo/redo entry. Mixed-format selections
inherit Chromium's initial replacement style; the helper then carries that
collapsed style across breaks. Native bold, italic, underline, strike, sub/super,
font family, size, foreground and highlight state are preserved through native
format commands, without copying page markup or setting a field value.

The original failed prototype and assertion remain on local diagnostic branch
`codex/dictation-browser-paragraph-probe-20261009` at `1b8658e6c9`. The corrected
production regression asserts the exact native notification sequence
`insertText, insertLineBreak, insertLineBreak, insertText`, exact rendered text,
exact bold markup, one undo/redo, no repeated final and no site activation.
Four notifications are still one consumed original-field final. The pinned
native IME control reproduces the old defect; merely changing the host API is
insufficient. Chromium's [native insertion commands](https://raw.githubusercontent.com/chromium/chromium/main/third_party/blink/renderer/core/editing/commands/insert_commands.cc)
and [composition implementation](https://raw.githubusercontent.com/chromium/chromium/main/third_party/blink/renderer/core/editing/ime/input_method_controller.cc)
provide implementation context; the pinned-host tests establish behavior.

Every native step checks the original field's focus, writable/private/composition
state and fixed expiry. An input handler that changes focus interrupts the edit;
remaining text cannot move to another field. The partial original edit can be
undone, and the consumed lease cannot retry it. No automatic cross-field rollback,
clipboard replacement or field-value assignment is introduced.

Literal edge whitespace remains a bounded browser limitation. Normal HTML
whitespace rules can collapse an existing space or produce NBSP around a leading
or trailing break. Such rich-field requests fail before handlers or editing.
Textarea and explicitly whitespace-preserving rich fields (`pre`, `pre-wrap`,
`break-spaces`) qualify pure and trailing breaks exactly. The whitespace policy
is retained in the original private snapshot; it cannot be changed by model output.

The complete guarded CEF suite passed 49 tests on the boundary revision, including
its real 180-second fixed lease timer (245.880 seconds). The final mixed-format
revision passed all nine focused browser cases (9.269 seconds). The existing
mounted SDK executable passed direct and decorated ownership journeys; its rich
paragraph assertions are extended as part of this fix. A separate native typing
control verifies that dictation undo retains text typed immediately beforehand.
The 20 focused shell dictation integration cases also passed (0.502 seconds).

`qualify_dictation_browser_worker.py` runs an authenticated public-audio image
worker concurrently with a private real-browser field lease. Existing resource
and custody checks remain intact; bounded worker replies are forwarded as they
occur. The cases cover an original textarea final, focus-away-and-back retirement,
cancellation after actual recognition, and a fresh repeated input-field session.
Drafts never edit fields; successful finals insert once and undo/redo exactly.
The receipt distinguishes decoder status from the transient scope launcher and
checks all trailing cancellation replies. This is worker/browser concurrency,
not a live microphone or compositor shortcut journey.

Browser runtime references advance together to `2026.10.9-chromium` in SDK,
app manifest, package-contract fixture and image argument, so an installed older
bundle cannot satisfy the new editing protocol. No runtime or model is installed,
activated, signed for publication or deployed by this work. The PR remains held.

Field capacity is also part of the private original snapshot. Text inputs and
textareas validate the full prospective UTF-16 value against maxlength before
handlers; native email/number retain their existing complete-value checks.
Changing capacity during a paragraph interrupts remaining commands rather than
silently truncating an acknowledged final. Unobservable resource-scope control
files now fail explicitly as unverified controls before reading any artifact.

The final capacity-aware pinned CEF host passed 12 focused browser cases
(11.522 seconds). A fresh final concurrent run passed all four cases with the
same authenticated French final: original textarea (12.925 seconds), retired
focus (12.748 seconds), cancellation without a trailing final (3.922 seconds),
and a fresh repeated input session (12.689 seconds). The three complete workers
exit successfully with unchanged guard/cgroup limits; cancellation records no
unobserved decoder exit claim. An earlier stricter rerun was refused before its
fourth worker became ready by host pressure admission; that failure is retained
separately rather than hidden or counted as a pass. Detailed source/host digests,
exact outputs and limits are in `dictation-browser-paragraph-fixed.json`.

The final mounted SDK run passes for direct and decorated widgets, including
the corrected rich paragraph's exact text/markup and one native undo/redo.
Its first outer harness run passed the Rust assertions but raced asynchronous
helper shutdown during profile deletion. The final private PID-namespace init
waits for every owned native descendant to exit before removing profiles; the
whole run now exits successfully. Both logs remain in the receipt. No production
supervisor behavior or sandbox flag was changed to hide that cleanup failure.
The package-kind/browser-runtime contract passes on the new runtime reference
(1 test, 0.008 seconds), as do 3 package preparation and 4 scope-contract tests.
The final required remote shell all-test check passes (1.12 seconds cached).

### Nested rich fields and current-main conflicts (2026-10-09)

Further real CEF journeys qualify paragraphs inside code, links, list items and
table cells: literal breaks, inherited bold, unchanged link target/structure,
original-selection replacement and one native undo/redo. An original admission
regression found that the surrounding editable root admitted selection endpoints
inside, or a range crossing, a `contenteditable=false` island. Preparation now
checks effective endpoint editability and protected descendants intersecting the
complete range. It rejects locked content before handlers or editing; editable
text alongside the island still accepts paragraphs and owned undo normally.
The pinned host build passed in 11.951 seconds and all 15 focused tests passed
in 15.836 seconds. The original failure and corrected source/host hashes are
retained separately in `dictation-browser-rich-islands.json`.

Current-head CI for 5680a08243 was inspected once: check run 86055, lint 86058 and
app-SDK 86054 stopped during merge against main 8f6bfc2b00, before compiling the
candidate. Main a9d9b6c766 is integrated with both sets of localized keys retained.
The rootfs contract is the exact union of both 301-row parents: 302 rows, retaining
the cooperative Bash editor and adding the first-login Flatpak unit. Its SHA-256
is `7ae5fa508eee4062dda7e997189cd678c0d213eeed43990e3b9863245704fded`;
the unchanged 156-instruction budget passes. Cargo.lock is unchanged by this merge.
The required remote shell test-target compile passed (1m48s), 17 selected foreign
compositor dictation tests passed (0.624s), and the separate Notes integration
Unicode/selection/persistent undo case passed (0.280s). An initial Notes command
used an obsolete individual target name and ran no test; the corrected consolidated
`it` invocation produced that passing result.

Current-head OS run 86059 confirms the missing signed RPM snapshot
`set-4cee6ac4651b25573a4c65e1dba081d96f0ecc825daa76e7cfa53c0eed58a63c` and
absent main producer run; its image job did not execute. This non-transient
prerequisite is not rerun. No image or live microphone result is inferred.

`dictation-recorded-compositor-plan.md` prepares the bounded hardware-disabled
assembled audio journey and identifies exact model/runtime and one-session
microphone consent boundaries. Ordinary first-party installation is distinguished
from a security grant; this session's explicit deployment/activation hold still
applies to the real OS. The plan is prepared, not a current-head assembled pass.

After integration, the actual Notes three-case paragraph bridge regression also
passes (one selected test containing Unicode paragraphs, pure LF and trailing LF,
0.536 seconds). The full recorded-audio plan's preflight identifies missing old
Tiny/audio-tool fixture inputs explicitly; existing binary files are not claimed
to be compiled from the current source. No runtime activation is implied.

The complete guarded CEF suite passes all 56 tests on this final rich-selection
revision (247.630 seconds), including the real-clock fixed expiry case. This
result is retained in the rich-island receipt and is distinct from the previous
49-test boundary checkpoint.


## Recorded compositor history repair (9 October)

The hardware-disabled recorded Whisper Base compositor control now passes
selected original-field insertion, actual native Ctrl+Z/Ctrl+Shift+Z, cancellation
during public playback, restart after worker exit, and permanent retirement on
focus away/back. A release edge after a host-consumed Undo press had canceled
WebView's queued history before it reached Chromium. The shared widget now
invalidates on mapped presses; original-field checks still guard release-handler
mutations and deferred history. The regression also retains press/focus refusal
and waits for an actual history acknowledgement.

See `dictation-recorded-compositor-evidence.json` for failed traces, verified
public model/audio/tool provenance, exact source/binary hashes and actual output;
`dictation-recorded-compositor-plan.md` records the repaired journey and limits.
The first successful bounded run took 53.183 seconds overall, with a 37.951-second
inner journey under two CPUs, 3 GiB RAM, no swap and 256 tasks. Sampled memory hit
the cap with debug-runtime staging; this is not production footprint evidence.
The control is Whisper Base, not Whistle-specific runtime/accuracy or physical
microphone acceptance. The PR and real-OS deployment remain held.

## Recorded Whistle compositor qualification (9 October)

The already-reviewed Whistle weights and default-off Wasmtime AOT x86-64-v3
runtime now pass the assembled hardware-disabled French, English and Spanish
compositor journeys. German passes the same editor behavior but its unchanged
accuracy assertions fail. Exact outputs, source/binary/model/runtime hashes,
resource controls and failed setup/accuracy traces are in
`dictation-whistle-compositor-evidence.json`; exact historical drivers are in
`tools/release/fixtures/whistle_compositor_20261009/`.

| Public fixture | First visible preview after playback | Insertion after Finish | Sampled total peak | Raw development-fixture WER |
| --- | ---: | ---: | ---: | ---: |
| French | 3.801 s | 2.438 s | 2.226 GiB | 0.0% |
| English | 3.928 s | 1.630 s | 2.256 GiB | 5.3% |
| Spanish | 3.853 s | 3.138 s | 2.243 GiB | 10.3% |
| German — accuracy failed | 3.834 s | 3.743 s | 2.362 GiB | 27.8% |

Every case verifies original-field insertion, unchanged fields before final,
actual native Undo/Redo, cancellation during playback, observed live worker
identity and exit before restart, and permanent focus-away/back retirement.
The private preview's existing widget ID now publishes only its laid-out
rectangle through the existing `tilo-control` target registry. No transcript
text is added to control status or persisted by production code. Its layout,
accessibility, theme/scale and retirement regression passes, as does the required
remote shell all-test-target compile. The fixture witness accepts only the exact
reviewed Whistle digest and records its actual format, revision and Apache-2.0
licence; it never masquerades as Whisper GGML.

German produced “Franconk”/“Korlum” for “Hongkong”/“Kowloon” and misrecognized
part of the adjacent phrase. The preserved direct-audio worker control has
similar errors, suggesting a recognition limitation rather than evidence unique
to compositor capture. The failing assertions remain; their later placement lets
editor behavior and latency finish without counting the overall German case as
passing. WER uses the existing punctuation/space NFKC/casefold scorer without
reference-aware substitutions. English's `years` versus the reference's `year`
is still counted. These previously consumed public development recordings do
not establish general language accuracy or a holdout result.

All runs retain two CPUs, 3 GiB, zero swap, 256 tasks and a 240-second limit,
with no hardware nodes, host devices or network. Peak tasks are 156–159.
Memory includes debug/runtime staging, software rendering, compositor/browser,
preflight and file cache; these are not production or older-device footprints.
Preview timing observes appearance of a nonempty rendered draft, not correct
first words; audio-relative timing includes the recording's silence. Whistle's
root-owned-runtime loader correctly rejected the original one-ID fixture
namespace. The corrected disposable mapped image preserves that check and drops
the launcher to capability-free UID/GID 65534 before running any application.
No additional outer-container capability or persistent access was granted.

The earlier Whisper Base executable-link worker-exit helper skipped unreadable
processes. Preserve that result as the speech/insertion/history control, but not
strict worker-exit proof. These Whistle runs require an observed exact worker
command line before verifying its absence, and unreadable command lines fail.
No Whisper control was repeated or rewritten. Publication, persistent model or
runtime installation, real microphone consent and deployment remain held;
older devices, ARM and arbitrary complex editors remain unqualified.

### Matching-build component and direct recognition controls (9 October)

The fresh component profile at `c403ee7bf5f2660abadc20726697a5d810632ae1`
uses all four matching-build binaries and exact source guards. The earlier
component run's binary-identity refusal is retained. The actual pinned AOT
worker now returns empty draft/final text for two seconds of exact silence in
1.3/0.9 ms; the production shared recognition path skips inference only for
exact zero samples and still decodes appended nonzero quiet audio.

French insertion, Undo/Redo, cancellation, restart and focus retirement pass.
Preview appears 4.249 seconds after playback; insertion takes 2.137 seconds
after Finish. The transcript differs from the earlier control and scores 15%
raw development WER; this is neither an accuracy nor latency improvement.
Sampled group RSS peaks are 131.8 MiB for worker/runtime, 893.1 MiB for the
compositor and 896.9 MiB for browser/host. RSS sums double-count shared pages;
peaks occur at different times. Total cgroup usage reaches the 3 GiB bound and
includes debug executables, page cache, graphics and staging. Runtime and model
share one process, so their individual allocations remain unmeasured.

Fixed-audio direct German inference reproduces the wrong names with both auto
and explicit German (33.3% raw WER). The unchanged original compositor German
control remains failed at 27.8%. Capture is not required to reproduce the
recognition defect; the inspected bridge passes the language correctly and
calls the pinned decoder without reference-aware text replacement. A next
accuracy comparison has a specific existing candidate: pinned Whisper Small
Q5 (`ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb`).
The earlier [conditional allocation ledger](dictation-small-allocation-qualification.json)
records 0/18 German errors on this same clip, but first preview takes 7.246
seconds and worker RSS peaks at 446,896 KiB. This is a separate older source
and source-paced transport with the unrelated public spelling `TiLo`; it is
not current compositor acceptance. Non-flash strict-cap experiments also
failed, so neither blanket failure nor current success is warranted. Recheck
the current sealed worker on fixed German PCM with no vocabulary and unchanged
1 GiB worker/two-thread limits before selecting that accuracy/latency tradeoff.
No catalogue, runtime publication or persistent deployment is authorized.

See [component evidence](dictation-component-profile-evidence.json) and
[historical drivers](../../tools/release/fixtures/whistle_components_20261009/README.md).
