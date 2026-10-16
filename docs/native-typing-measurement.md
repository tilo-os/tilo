# Native Notes typing measurement

Use the normal signed Notes application, the compositor's physical input path
and standard Wayland presentation feedback. No screenshots or typed text are
needed in the diagnostic log. A semantic View receipt, canonical read, frame
callback, surface commit, render-worker return or unrelated desktop frame does
not prove that an edit reached the panel.

Enable `TILO_PROFILE_PRESENTATION=1` for the normal diagnostic compositor
launch. The normal launcher forwards exactly that boolean into the sandbox;
no native scenario, input gate, output path or GPU-profiling variable is forwarded.
For a greetd login, the greeter forwards the explicit host diagnostic boolean
into `StartSession`; arbitrary environment, scenarios and input gates are never
forwarded. The native GPU stage flags are a separate opt-in launch, described in
[native GPU stage diagnostics](native-gpu-stage-measurement.md). Keep them absent
for the primary typing acceptance run.
Do not enable `TILO_NATIVE_SCENARIO`: its scheduled-input logs include characters
and it does not test physical keyboard delivery.

Before measuring, use Notes' declared actions to create and open a disposable
scratch page. Verify that its actual editor exists, is focused, and that its
content-free observer agrees on selected page, main page, editor binding,
canonical body hash, rendered body hash and caret. Never use the person's Bugs
page. Let startup/catalogue traffic settle first. Send isolated character
presses through the existing physical input test path and wait for each changed
body's positive presentation feedback before the next press. Coalesced frames
cannot certify the visibility of every intermediate edit and fail qualification.

Collect the normal process diagnostic logs from the compositor and signed
Notes worker into one timeline. Each accepted sample must join:

1. `tilo-physical-delivery`: the original trusted kernel/libinput monotonic key
   timestamp and sampled compositor delivery timestamp, its Wayland serial,
   and the actual focused surface ID. No key code or character is logged.
2. `tilo-physical-input`: the same Wayland serial and a local input ID, client
   dispatch clock brackets, a random app worker lifetime UUID, diagnostic namespace
   PID, and strictly content-free
   Notes before/after state.
3. `tilo-physical-frame`: that same worker lifetime UUID, input ID, CPU frame ID and the actual Notes
   editor state observed after drawing the scene queued to the GPU worker.
4. `tilo-display-frame`: the same app worker lifetime UUID, CPU frame ID, render worker identity and frame
   index, with positive `wp_presentation` feedback for the exact new-buffer
   commit. CLOCK_MONOTONIC, hardware clock/completion and vsync evidence are
   required. Failed/discarded feedback never qualifies.

The Wayland client requests feedback while holding its native window state lock
across the exact GPU buffer presentation. `announce_frame` and
`pre_present_notify` enqueue frame callback requests; neither commits the main
surface. The lock also prevents configure/CSD activity from taking the feedback
for another commit. The compositor must retain exact committed/captured surface
ownership until its corresponding composed frame really completes a KMS flip.

Analyse the collected lines with the existing performance analyser:

```python
from input_latency import summarize_physical_notes_latency
report = summarize_physical_notes_latency(lines, scratch_page_uuid)
```

Normal application sandboxes isolate PIDs, so namespace PID alone cannot identify
a producer. Input, drawn-frame and feedback rows must join the same random
process-lifetime observer UUID. No host PID or sandbox relaxation is needed.

Missing/duplicate records or worker UUIDs, absent kernel timestamps, incompatible clocks,
wrong page/surface, unchanged body, incorrect caret, canonical/rendered mismatch,
an after-event body different from the drawn body, missing feedback and
coalesced inputs all fail. Do not discard these samples to improve percentiles.
The acceptance target remains key-to-present p50 below 20 ms and p99 below
50 ms. Report the actual sample count and both percentiles; a partial trace does
not pass. The measured interval includes kernel/libinput-to-dispatch backlog,
SDK state/layout/render work and KMS completion. It excludes peripheral scan
before the kernel/libinput timestamp and physical panel pixel response.

Historical `KEY-LATENCY` and `VELLO-STAGES` aggregates are diagnostic context,
not this acceptance proof. GPU stage samples must retain their own submission
identity and coverage; they cannot be reassigned to the most recent frame.

The app's own share of that interval needs no device: `cargo run --release -p
notes --example notes-latency` times typing, Enter, Backspace, the wheel and
opening a note on an owned 27-note, 838-block notebook, from the input through
the host's input path to the frame that shows it and that frame's GPU raster
([measured figures](app-performance.md#notes-input-latency-on-a-27-note-notebook)).
It is the regression signal between device runs and is likewise no acceptance
proof: it excludes kernel delivery, the compositor and KMS presentation.
