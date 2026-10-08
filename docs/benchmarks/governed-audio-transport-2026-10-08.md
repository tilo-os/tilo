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
