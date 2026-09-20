# Native presentation feedback

The optional `presentation-feedback` feature records macOS Metal drawable
presentation callbacks for a native workload. Build the probe with:

```sh
cargo build -p ui --example ui-native-input-probe --profile thin-app \
  --no-default-features --features presentation-feedback
```

Set `QOS_PROFILE_PRESENTATION=1` together with `QOS_NATIVE_SCENARIO`,
`QOS_PROFILE_OUT`, and `QUICK_UI_PRESENT=1`. Unsupported platforms report that
feedback is unavailable. Missing display evidence cannot qualify a workload.

The `vendor/wgpu-hal` patch observes the drawable already used by the normal
presentation operation. It registers an `addPresentedHandler` immediately
before presentation and does not create another drawable, command buffer,
fence, or presentation policy. The feature-disabled path retains the original
surface layout and behavior.

`qos-display-request` identifies the render generation, worker frame, and
optional originating CPU frame. `qos-display-frame` records those identities,
the Metal drawable ID, backend, and raw `presentedTime` host seconds. A bounded,
nonblocking callback queue feeds a separate serialization thread. Queue
overflow, logging failure, timeout, a zero timestamp, and an absent callback are
distinct outcomes and remain visible in evidence.

`scripts/perf/presentation_feedback.py` validates request/callback pairing,
identity uniqueness, backend provenance, finite timestamps, monotonic order,
and expected CPU-frame coverage. It emits interval statistics only for complete
positive feedback.

Report two cohorts separately:

- **input-associated intervals** contain frames associated with scheduled input;
- **full active intervals** also contain intervening animation frames.

A fast full-frame cadence does not prove that scroll or edit state changed at
that cadence. State or pixel assertions remain required. Raw Metal host time is
not a Rust `Instant` epoch, so input-to-presentation latency requires an explicit
clock correlation before subtraction. Observer-enabled runs also include
instrumentation overhead and must be compared only with equivalent builds.

The vendor patch description and license provenance live in
`vendor/wgpu-hal/QOS-PRESENTATION-FEEDBACK.md`. This facility supplies display
observations; it does not independently certify workload coverage, native input
delivery, application correctness, or a frame-rate target.
