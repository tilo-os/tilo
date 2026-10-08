# Governed audio progress and PCM reply scheduling

The SDK's governed audio worker previously awaited its periodic `Status` RPC
before serving the PCM and control queues. A decoder's synchronous reply budget
starts when it enqueues a command, while the worker's service deadline starts
only when that command is dispatched. A slow progress observation therefore
consumed the decoder's reply budget before its write began.

An isolated native SDK test reproduced `Governed audio output reply was lost`
after 3.10 seconds. The production factory and command lane submitted a full
20 ms stereo S16 PCM packet through the Cap'n Proto channel. The test peer
accepted those samples, but delayed both the preceding status reply and the
write reply by two seconds each. Both RPCs were individually below the existing
three-second service deadline; their serialization exceeded the caller's
3.1-second reply budget.

The worker now polls one bounded progress request beside its command queues.
Progress no longer occupies the PCM lane, and urgent controls keep priority.
The same delayed-reply test passed in 2.04 seconds. Old-generation progress
replies are discarded, and same-generation progress snapshots cannot overwrite
newer command acknowledgments, including flush and pause. A current-generation
progress failure still becomes a terminal cached error. Service deadlines, lane capacities,
wire commands and host admission checks are unchanged.

Reproduce the SDK scheduling and existing host authority tests with Cargo or a
remote Cargo drop-in selected through `CARGO`:

```sh
"${CARGO:-cargo}" test --locked -p tilo-ui -p shell \
  --lib governed_output -j 8
"${CARGO:-cargo}" test --locked -p tilo-action-broker --lib \
  audio_output_raw_admitted_channel_cannot_open_or_write_host_pcm -j 8
```

This is a synthetic transport fixture, not a device-output benchmark. It opens
no microphone and does not exercise a physical audio device or installed VM.
Installed Music observations motivated the investigation: short-track EOF and
replay passed, while sustained playback and queue handoff reported governed
audio timeout or lost replies. Matching the error proves this scheduling defect
can cause the symptom; it does not establish that it caused those installed
failures. Sustained output and audible queue handoff still require installed
acceptance with the qualified SDK/runtime and matching host.

The final focused run passed nine SDK/host tests, including delayed progress,
stale acknowledgment fencing, generation-scoped failures, queue bounds,
ownership isolation, disconnect and local-session revocation. The separate
raw-channel admission rejection test passed. SDK all-targets and shell tests
compiled, and strict Clippy passed for those same targets. All compilation and
tests ran on the native remote builder. No installed playback, physical-device
latency or hardware throughput result is claimed.

## Admission checks and the host output registry

Installed acceptance on the matching source/runtime/host cohort at
`eaf2baa270a124d24b0158bfd32fe68a39df8b96` still failed: roughly 20 seconds
of audible output were followed by a governed-output timeout and a separate
six-second silent capture. The adjacent compositor iteration spent 4.143 seconds
after its pump. This is a timing correlation, not evidence that paint caused the
RPC timeout. The broker already runs on a separate thread.

Source investigation found another concrete blocking boundary: the host kept
its output-registry mutex while revalidating the retained caller. An admission
callback can wait on membership or identity state. While it waits, the same
mutex prevents the lock-screen callback from fencing playback and prevents
other commands or a replacement generation from accessing the registry.

The host now clones the retained caller under the mutex, revalidates outside
it, then reacquires it and checks the exact generation, local-session permit
and disabled state before applying the command. A late failure for an old
caller cannot fence a replacement generation. Admission is still checked for
every command; there is no authorization cache or longer deadline.

The causal regression blocks the production service's retained-admission
callback. It requires the production lock-screen fence to complete within a
300 ms observation window, before releasing that callback, and requires the
waiting PCM write to fail and its source to render silence. A companion test
replaces the generation during the wait, rejects the old command and verifies
that the new source still renders its own PCM. These are controlled admission
and synthetic-device tests, not measured compositor or installed-device tests.

Timeout diagnostics retain the existing user-facing error and log only the
operation, request/reply/retry counts and last queue/progress flags. They do
not log samples, paths, identities or route identifiers. Admission waits of at
least 50 ms also report their phase and duration. A real Cap'n Proto fixture
checks that stalled RPC replies and repeated full-queue replies produce distinct
transport evidence. Matching installed acceptance must collect that evidence
before attributing the remaining timeout to rendering, admission or device
backpressure.

The follow-up focused run passed 12 SDK/host audio tests in 4.184 seconds.
The separate raw-channel admission rejection test passed, and SDK all-targets
plus shell tests compiled. Strict Clippy passed for the same targets in
2 minutes 39 seconds. Its initial invocation had failed before Clippy started:
the remote session's JSON parser received a disk-triggered garbage-collection
status line while disk usage was 95.6%. A read-only observation subsequently
showed 93.17% usage, below the 95% critical watermark, and justified the retry.
No manual/shared cleanup, local build fallback or device operation was used.
Required PR CI and matching installed acceptance remain separate qualifications.
