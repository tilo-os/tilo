# Native presentation feedback

Build the native probe with:

```sh
cargo build -p ui-examples --example ui-native-input-probe --profile thin-app --no-default-features --features presentation-feedback
```

Set `TILO_PROFILE_PRESENTATION=1` alongside `TILO_NATIVE_SCENARIO`,
`TILO_PROFILE_OUT`, and `TILO_UI_PRESENT=1`. The feature is currently implemented
only on macOS Metal. Unsupported builds explicitly report unavailable feedback;
absence of display records cannot qualify a workload. The normal feature-disabled
HAL retains its original surface layout and presentation path.

The narrow `vendor/wgpu-hal` patch observes the actual drawable used by the
existing presentation operation. It registers `addPresentedHandler` immediately
before presentation, with runtime API availability and duplicate-registration
checks. The worker releases the wgpu HAL guard before presenting. There is no
extra drawable, command buffer, fence, or presentation-policy change.

`tilo-display-request` identifies the render-worker generation, worker frame and
optional originating CPU frame. `tilo-display-frame` carries those identities,
the Metal drawable ID, backend and raw `presentedTime` host seconds. A bounded
nonblocking callback queue feeds a separate serialization thread. Missing rows,
including queue overflow or logging failure, remain missing evidence. The
collector waits for callbacks from the entire measured frame interval, with a
bounded drain timeout, before stopping its owned process.

[Apple's presentedTime documentation](https://developer.apple.com/documentation/metal/mtldrawable/presentedtime)
defines positive timestamps as the host time of onscreen presentation, and zero
as not presented or skipped. A callback for every discarded drawable is not
assumed. Keep positive, zero and missing feedback distinct.

`scripts/perf/presentation_feedback.py` checks request/callback pairing, unique
worker/frame and drawable identities, Metal provenance, finite timestamps,
monotonic presentation order, and complete expected CPU-frame coverage. It emits
interval statistics only for complete positive feedback. It never certifies
performance: workload coverage and state/pixel correctness are separate gates.

The input-associated cohort measures spacing between frames associated with
scheduled input. The full interval cohort includes intervening animation frames.
They answer different questions and must be reported separately. Raw Metal host
seconds are not an arbitrary Rust `Instant` epoch: input-to-presentation latency
requires explicit clock correlation before subtraction.

Observer-enabled runs include overhead. Request logging is on the render worker
immediately before present, within the existing `present_us` span; callback JSON
logging is off-thread. Compare equivalent instrumentation configurations.

Source provenance and modified upstream files are retained in
`target/runtime-proof/presentation-vendor-audit.json`. Four reporting tests pass,
both feature-enabled and feature-disabled `cargo check` pass, and independent
review found no ownership or lifetime blocker. The optional HAL package retains
its upstream licenses and a patch description in
`vendor/wgpu-hal/TILO-PRESENTATION-FEEDBACK.md`.

## Initial native results (2026-09-16)

All runs use the same optimized executable SHA-256
`bdc3eb3500614c17d8ec7483a483c9d96396b50da542c94afb75b941b56531ab`,
200 real text rows, 1260 wheel movements, a 16667 µs input schedule, and an
initial hover plus 60 wheel warmups. Hardware information is retained in
`target/runtime-proof/presentation-display-info.txt`.

- `native-presentation-20260916-111507`: failed presentation evidence. Of 1131
  measured input-associated worker-Presented frames, Metal reports 1008 positive
  timestamps and 123 zeros. Over the full active interval it reports 2015
  positives and 998 zeros. No callback is missing. This concretely demonstrates
  why the old worker outcome cannot qualify displayed FPS. The input workload
  also coalesced 69 measured inputs into two batches.
- `native-presentation-20260916-111600`: complete positive feedback for 1197
  input-associated frames and all 2393 frames spanning the active interval.
  Three input batches coalesced. Input-associated presentation interval p99 is
  25.000 ms (maximum 41.667 ms); all-frame interval p99 is 8.3334 ms.
- `native-presentation-20260916-111704`: complete positive feedback for all 1200
  input-associated frames and all 2392 frames spanning the active interval.
  No coalescing. Input-associated interval p99 is again 25.000 ms (maximum
  33.333 ms); all-frame interval p99 is 8.3334 ms, maximum 16.667 ms.

Both complete runs independently verify every wheel movement and retain raw
request/callback, input, CPU and worker evidence. They do not establish solid
60 Hz visual scrolling: intervening frames can present while the scroll offset
is unchanged. Do not substitute the faster all-frame percentile for the
input-associated cohort. Input-to-presentation clock correlation is still
absent, and requested input timing is not actual hardware-input delivery.
Investigate scheduler-send versus main-dispatch timing and frame admission
alignment before attributing the 25 ms tail to any one stage.

The parser now has four targeted tests and the complete Python reporting suite
has 29 passing tests. All 47 actual-app qualifications remain incomplete.


## Scheduler versus app dispatch (2026-09-16)

The scheduler now records its send timestamp immediately before proxy delivery.
Two repeated optimized runs use executable SHA-256
`133295ccc54dbb65ec90fa17663bb8a93be84e2800961ebe54fd19f9ae328cad`:

| Run | Scheduler lateness p99 | Send-to-dispatch p99 | Input-associated presentation interval p99 | Maximum interval |
| --- | ---: | ---: | ---: | ---: |
| `native-presentation-20260916-113835` | 4.110 ms | 0.036 ms | 16.667 ms | 25.000 ms |
| `native-presentation-20260916-113915` | 4.144 ms | 0.029 ms | 16.667 ms | 16.667 ms |

Both retain 1200 measured input-associated positive presentations and 2399
positive presentations across the active interval, with no zero or missing
feedback. Every one of the 1260 wheel movements is independently verified in
widget state. The first run has one warmup coalesced batch; neither loses
measured frames. The second run has a 4.433 ms maximum send-to-dispatch delay,
so its low p99 does not imply that every dispatch is equally fast.

These runs localize the earlier approximately 4 ms handler-entry lateness
primarily to the synthetic scheduler, not the app event queue. They do not
establish the cause of the previous 25 ms p99 presentation tail: presentation
phase, instrumentation changes and concurrent host activity remain uncontrolled.
No renderer optimization occurred between these measurements, and the better
presentation percentile must not be attributed to the timestamp instrumentation.
The original failed and slower runs remain evidence. Cross-clock end-to-end
latency, pixel assertions and all 47 actual-app qualifications remain open.

All 30 Python reporting tests pass. The optional presentation-enabled optimized
probe rebuild succeeded; raw logs, reports, hashes and per-gap timing analysis
are retained in each run directory under `target/runtime-proof/`.
