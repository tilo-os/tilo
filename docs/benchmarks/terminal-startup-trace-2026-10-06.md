# Terminal startup: hosted and packaged paths

The previously reported 90-second Terminal first-frame failure in
[PR #2957](https://code.tilo.so/tilo/tilo/pulls/2957) used the wrong observer.
`actions:window.new.terminal` mounts Terminal inside the compositor.
`AppPlaceholder` is its persisted tile kind, and `foreign_frames` only observes
foreign Wayland clients. Neither observation establishes a rendering failure.
This report supersedes that conclusion; the original failed probe remains
useful evidence of an observation error, not an application startup benchmark.

[Completed journey and raw observations](terminal-startup-trace-2026-10-06.json)
record a fresh disposable COW of developer image
`registry.tilo.so/tilo/tilo-vm@sha256:ec4aff79035255d6aec7f9cf3c3e7555f26a8a1890b3d24c8bc95310708c228e`,
source `12f4dfb2d535f0ea9c64278b66a9bef435d2e4b3`. These are the installed
application binaries, not the candidate patch. The remote builder has no KVM
or DRM render node: QEMU TCG, four vCPUs, 4 GiB RAM, 1280×720, and llvmpipe.
The private headless compositor used a marked developer-only throwaway identity,
peer sync disabled and an isolated non-activating D-Bus broker. No physical
machine was rebooted or modified and no persistent authority was granted.

## Hosted Terminal and the actual missing-service bug

With no supervisor in the private runtime directory, the hosted action completed,
the `terminal` target laid out at 664×528, and a private PNG contained the actual
Terminal UI. The exported `inspect-session` action was polled beyond its initial
accepted receipt to a successful final response. It reported:

```json
{"lifecycle":"fixed","input_available":false,"startup_reason":null,
 "transport_reason":"Terminal process has not opened — input unavailable"}
```

The UI incorrectly described this as a shell exiting without output. This is a
cause-reporting bug: `spawn_hosted` correctly refuses a missing required `ush`
service, but the live resource constructor catches that error and substitutes
`spawn_headless`, a capture-only terminal. Input is already refused; there is
no evidence of an unsafe command sink or a rendering failure.

Starting the image's real `/usr/libexec/tilo-ush serve` against an owner-private
socket, restarting the private compositor, and explicitly opening a hosted tab
changed the completed inspection to `lifecycle: open`, an environment target,
a session identifier, and `input_available: true`. The snapshot also shows an older restored tab reporting a resource-unavailable
error, and the supervisor logged a broken pipe. The newly opened tab inspected
here did open; these partial failures remain visible in the evidence and are
not treated as proof of a timeout defect. No command roundtrip was measured
and no timeout was increased.

The candidate retains the required-service error in a live hosted terminal's
`no_runtime` state and status, while keeping input unavailable. Capture and
read-only behavior remain intact. The public strict `spawn_hosted` constructor
still refuses missing services; no host fallback or access grant is added.

A regression mounts `terminal::definition()` through the shell's production
`InProcessApps` and `FrameHarness`, lends unavailable services, and invokes the
actual bound resource actions. It asserts the lifecycle, specific `ush` cause,
and refusal of even confirmed input. With only the test added, the unchanged
application code failed (`fixed` versus `no_runtime`). With the patch, it passes.
The pinned-image journey establishes the original behavior; the remote component
test establishes the candidate behavior. The candidate was not deployed into a
running guest or device.

## Packaged Terminal admission and presentation

The separately discovered `applications:so.tilo.terminal` action runs the
packaged application through the normal launcher. The guest trace showed the
installed shared runtime, required supervisor transport, a Terminal process,
and Landlock `FullyEnforced`, `no_new_privs: true`, with network restrictions.
The private bus cannot activate systemd scopes; optional filesystem/media
services were absent. Those limitations are retained in the log and do not
justify weakening admission or sandboxing.

| Observation after invocation | Time |
| --- | ---: |
| Successful action receipt | 16.10 ms |
| First foreign surface | 3,242.68 ms |
| First nonzero foreign frame | 27,455.36 ms |
| First correctly sized, presented frame | 27,457.89 ms |

The final frame identifies `so.tilo.terminal`, `frame_seq: 1`, no rendering error,
a 664×592 buffer matching its rect, and `presented: true`. The compositor's
10-second opening deadline briefly made the tile visible with `frame_seq: 0`;
that state did **not** qualify as a rendered frame. An action receipt alone also
did not qualify. This observation proves package admission, process creation,
frame production and compositor presentation in this fixture. It does not prove
that the packaged app's PTY accepted a command or that normal system services
were available.

The application's existing `APP-STARTUP` trace reports 26,454 ms total, including
20,631 ms at `render-worker-first-blit-copy-done`. This is one cold launch on
TCG/llvmpipe, with software GPU initialization/compilation included. It is not
hardware latency, a Clear Linux comparison, a before/after improvement, or a
justification for changing CPU features or shader policy. Hardware frame and
interaction measurements remain with the separate device-testing work.

## Reproduction and verification

Use the immutable image above and the existing `os/dev/disk-image.sh` plus
`os/dev/vm.sh` fixture, with a read-only base and disposable writable COW.
Set `TILO_ACCEL=tcg`, `TILO_SMP=4`, `TILO_MEM=4096`, `TILO_XRES=1280`,
`TILO_YRES=720`, and `TILO_VM_HOSTFWD=restrict=on`. Run the image's native
headless shell in a private profile/runtime directory. Discover actual published
action IDs; poll broker-backed invocations with `actions_status` to a terminal
state. For hosted tabs, use `inspect-session`, `list_targets`, and a private
snapshot. For packaged apps, explicitly discover the `applications` provider
and require a new nonzero foreign frame whose dimensions match its rect and
whose presented flag is true. Keep the original 90-second observation bound.
See [headless testing](../headless-testing.md); never screenshot the real desktop.

Executed sequentially through a remote Cargo drop-in, with `--no-fallback`
and `-j8`:

- Negative control and fixed regression: shell library filter
  `hosted_terminal_missing_ush_reports_startup_failure_and_refuses_input`.
- Required integration compile check: `check --locked -p shell --tests` (passed).
- Existing service discovery/no-fallback test: Terminal library filter
  `hosted_service_sockets_are_probed_and_never_fall_back`.

The raw JSON retains completed receipts, targets, frame history, snapshots'
hashes/sizes and relevant log tails. Temporary observer errors from earlier
attempts were corrected before drawing conclusions. The guest, private
compositor, supervisor, writable COW and disposable test-account base are
removed after the trace; the original immutable production baseline is retained.
