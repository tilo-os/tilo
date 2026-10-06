# System dictation

Status: dictation components are implemented, but not assembled into a live
feature. There is no live shortcut, model installation or recognizer connection.
The microphone capture API is implemented but has not opened a real microphone.
Do not advertise “works in any field” or enable microphone capture based on this change.

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
also reaps the child. Source probing must run off the compositor thread.

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
- Callers must check liveness on each service tick and stop recording/inference
  on any failure. The insertion API does not own a microphone process.
- A request is bounded to 120 seconds and 16 KiB of draft UTF-8. Control characters,
  including line breaks, are rejected in this first boundary. Spoken paragraph
  commands require a field-aware extension; they must never submit a terminal.
- No transcripts, audio or surrounding-text strings are logged or persisted.
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
`WindowEvent::Ime(Commit)` after finalization. Reuse this bridge. It is not proof
of an app's selection, CRDT undo or accessibility routing.
Separate production-reader tests cover queue overflow, source EOF and owned-process teardown
using synthetic PCM/blocked pipes; they neither capture audio nor run inference.

## Candidate research (2026-10-05)

The user's exact bookmark URLs were not available. These are public candidates,
not confirmed bookmark identities. No upstream code or weights are imported.

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
reviewed. No weights or native dependency were installed. The reviewed
whisper-rs 0.16.0 `set_abort_callback_safe` erases its closure into a boxed trait
object, but its trampoline casts that pointer back to the generic closure type
and does not retain ownership. Do not use that helper. A raw C API adapter needs
an explicit callback lifetime and process-isolation audit; native CPU build
flags also need a portable baseline before adoption. The existing field bridge
reports purpose and caret, so identical-geometry native field switches require
an explicit field identity/edit generation from the shared host input contract.

## Remaining implementation and acceptance gates

1. Add exact speech runtime and model artifacts to the existing signed extension
   catalogue, including digest, revision, license/attribution, architecture and
   CPU/memory constraints. Do not auto-install packages from a mutable pip tag.
2. Assemble the trusted speech connection, with safe input-method arbitration
   alongside OSK and composing IMEs. Idle-seat field activation is implemented.
   Do not disconnect CJK composition or expose trusted globals to ordinary apps.
3. Integrate the implemented loss-detecting, bounded PipeWire microphone capture
   with the recognizer worker. Stop and reap only owned processes on cancel, lock, focus loss,
   device removal, timeout and backend failure. No persistent mic grant, audio
   file or transcript history by default. Permission denial stays a visible
   failure; retry requires another explicit start.
4. Add configurable hold and toggle gestures through the existing shortcut
   registry, plus semantic `dictation.start/stop/cancel/status` actions with
   local-seat authorization. Remote collaborators must not activate this mic.
5. Use shared themed controls for a non-focus-taking status surface: clear
   Listening / Finishing / Unavailable text, mic source, draft and Cancel action.
   Provide screen-reader announcements, touch/switch-access toggle operation,
   reduced motion and light/dark/high-contrast checks. Never depend on color,
   sound, or holding a physical key alone. Preserve the desktop's visual language.
6. Bind each recognizer result to its session ID and monotonic result sequence;
   reject unsupported language choices, stop on stream gaps, bound finalization,
   and unload idle models to leave desktop memory/CPU headroom. Corrections are
   explicit draft edits, not opaque LLM rewriting of the focused document.
7. Validate real journeys in text input, rich text, terminal and Chromium fields:
   selection replacement, a single undo unit, non-Latin text, focus changes within
   one window, cancellation/restart, IME/OSK coexistence, model crash, permission
   denial, unplugged mic and offline operation. Report unsupported clients
   honestly; XWayland and fields without text-input-v3 are still unproven.

No microphone was opened, permission granted, account connected, model installed,
device rebooted, or service deployed while developing this foundation.
