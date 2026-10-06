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
   ancestry, selection-less email/number inputs, closed shadow roots and composition
   already active when the isolated world starts need renderer lifecycle support
   before admission. A complete assembled
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

The candidate catalog keeps U²-Net unchanged and adds two MIT multilingual
models at the same pinned revision: compact **Whisper tiny** (77,691,713 bytes)
and **Whisper small-q5_1** (190,085,487 bytes). Small needs 2 GiB system memory;
both workers retain the 1 GiB address-space cap and two CPU threads. Small
improves the public probes below, while tiny remains a lighter short-dictation
option. Neither result establishes everyday reliability or Flow parity.
The [pinned upstream model card](https://huggingface.co/ggerganov/whisper.cpp/blob/5359861c739e955e79d9a303bcbc70fb988958b1/README.md)
identifies converted OpenAI weights and MIT licensing. Both checked-in receipt
directories retain the complete 1,063-byte upstream copyright and permission
notice, SHA-256 `b5d65a59060e68c4ff940e1eddfa6f94b2d68fdf58ed7f4dd57721c997e35e9d`.
No account, paid endpoint, extra runtime image or new trust identity is needed.

`tools/release/prepare_speech_catalog.py` accepts only the unchanged subject
release and these two exact speech recipes: immutable URLs, exact byte counts,
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
3. Default `publish` supports the prior U²-Net-only release or these exact three
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
the user's grants. This fixture has not been assembled or run; namespace and
recorder feasibility are not end-to-end capture evidence. Real microphone,
device selection and physical-device acceptance remain separate gates.

Small's median cold load is 2.748 seconds. Short final medians are 4.37–4.79
seconds, with automatic language approximately another four seconds; long
English/French/Mandarin finals are 14.93/20.57/20.74 seconds, automatic finals
18.94/24.60/24.75 seconds. These remote x86_64 timings exclude capture completion;
older CPUs and ARM remain unqualified. An explicit language removes detection
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
