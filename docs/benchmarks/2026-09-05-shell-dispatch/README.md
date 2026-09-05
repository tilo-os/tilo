# Shell dispatch follow-up

Goal remains native 3840×2160, 1,000 actual redraw actions, mean ≤8 ms and p99 ≤30 ms under ordinary release defaults. No animation, action, deadline, or acceptance changes are permitted. The preceding committed checkpoint is `24ff389f5` and records improvement but failed gates and intermittent fence timeouts.

First bounded diagnostic sequence uses the preserved `shell-redraw-retained-default-release-20260905` executable (SHA-256 `e18dff0cf8ff6e81ab971ee7557ee776979dfdd30ddde4fb4b1063e898d34ab9`): deferred bump readback on; then deferred bump readback plus sparse coarse on. Both enable QOS_REDRAW_BENCH_PROFILE, which disables acceptance. This probes the remaining completion interval; these flags are not adopted as defaults or acceptance settings. All other flags are cleared. Keep all samples/failures and the established 15-minute process cap with 30-second TERM grace; the one-second renderer fence deadline remains unchanged. Do not overlap builds/tests/other benchmarks.

In parallel with read-only investigation and these preserved-binary diagnostics, Terra is adding opt-in, submission-matched GPU dispatch reporting to identify the actual remaining stages. Instrumented numbers cannot serve as acceptance results and missing/recovery samples must be reported explicitly. Any subsequent implementation and performance protocol will be documented before measurements.

## Initial diagnostic results (not acceptance)

| Preserved-binary option | Mean | p99 | Completion/map mean | Sparse / full / recovery |
| --- | ---: | ---: | ---: | ---: |
| Deferred bump copy | 41.169 ms | 111.238 ms | 35.832 ms | 999 / 1 / 0 |
| Sparse coarse + deferred bump copy | 29.464 ms | 89.105 ms | 24.045 ms | 999 / 1 / 0 |

Both complete all 1,000 native-4K redraw actions and keep the same 808,636-byte
average texture-copy payload and 33,177,600-byte full CPU destination writes.
Neither establishes the requested gates or justifies a default change. The
coarse-plus-deferred result is close to the preceding coarse-only diagnostic
(29.706 ms / 86.852 ms); no reproducible p99 benefit from deferral is shown.

Code inspection also found that each direct path-count and path-tiling stage
launches 128 workgroups ×256 lanes regardless of scene size. Both shaders
already stride over runtime GPU-produced counts, so any nonzero group count
still covers all work; changing it is a parallelism experiment, not a way to
drop paths. Terra is adding an initialization-time override for 1..=128
workgroups to the upcoming diagnostic binary, with the current default of 128
unchanged. Small counts must pass dense-pixel and overflow/failure-sentinel
tests before their timings can guide a production policy; large flattened
curves and real-GPU occupancy remain explicit risks.

## Planned release measurements

Build the combined diagnostic/override source using only:
`cargo build -p ui -p shell --release --example ui-perf --example redraw_bench --locked`.
Keep the normal profile and target configuration. Preserve/hash that binary
before any later default-policy change.

After the source-frozen tests and build complete, first run three shell
dispatch-timestamp diagnostics, in order: current default 128, override 8,
override 1. All enable QOS_REDRAW_BENCH_PROFILE=1,
VELLO_GPU_TIMESTAMPS=1 and VELLO_GPU_DISPATCH_TIMESTAMPS=1; sparse coarse and
deferred bump readback stay at their off defaults. The purpose is to locate
stage costs, not acceptance. Missing/stale/unsupported/recovery timing data
must remain visible and cannot be filled with zeros or attributed to another
submission. Stage quantiles are conditional on that stage being present and
must not be summed into a total-frame quantile.

Use the same native-4K 1,000-action workload and existing process/fence bounds
for each. No builds/tests/other benchmarks overlap performance measurement.
Any follow-up unprofiled comparison must explicitly distinguish an experimental
configuration from ordinary default acceptance. Only an actually adopted
default, rebuilt with normal release options and measured without diagnostic
flags, can establish the goal.

## Correctness observation before release timing

At one workgroup, the UI direct-versus-indirect test passes its 32,772-line
grid-stride fixture, empty scene, and clipped cubic curves. The real bump
overflow/recovery test also passes. However, the full shell benchmark test
sequence reports 10/11 passing: its exact native-4K oracle finds three XRGB
byte differences (maximum delta 1) at candidate 3, first at x=1894, y=14,
channel 2 (234 versus 233). An isolated one-group repeat passes, and the
128-group control also passes. The failed trial remains a correctness failure;
the exact comparison and tolerance are unchanged, and count 1 is not qualified
for adoption by that passing repeat.

Code inspection finds a plausible mechanism, not a proven root cause: the
path-count shader atomically assigns per-tile segment positions, and fine
Area-AA sums floating-point contributions in that resulting order. A changed
dispatch grid can change allocation order and therefore last-bit rounding.
Both bridges in the failing test consume the same scene, so different
animation samples or independently recorded scenes do not explain that test.

The eight-group check also fails the exact shell oracle: candidate 5, the same
three bytes/max-delta-1 and first pixel/channel/value as above. Neither small
grid is qualified. Any planned release measurements of them remain explicitly
diagnostic evidence about stage cost, not eligible performance wins or grounds
for a default change while exact parity is unresolved.

## Benchmark instrumentation and acceptance separation

The shell benchmark now consumes Vello's bounded per-dispatch timestamp slot
after each completed action paint. It requires exact submission-id matching,
discards only the untimed setup frame, and reports missing/stale/unsupported,
pending/failed/invalid/overflowed samples explicitly. Earlier overflow retries
remain in whole-action latency and are separately counted; their GPU samples
are not assigned to the final submission. Per-stage quantiles include only
matched submissions where that stage actually ran.

Either GPU timestamp flag requires explicit diagnostic mode. A non-default
effective direct-path workgroup count also requires diagnostic mode; invalid
environment values retain the current 128-group default. Neither form of
instrumented/experimental run can print an acceptance pass. Normal redraws
do not read timestamp slots or initialize the per-stage aggregation storage.

The source-frozen default configuration passes all 12 shell benchmark tests,
including its exact native-4K retained-versus-dense oracle. This does not erase
the one- and eight-group pixel failures recorded above. Production rendering
defaults remain unchanged in this checkpoint.

Final validation also passes strict affected-library/example clippy, the
test-inclusive changed-line/compiler diagnostic filter (zero diagnostics),
workspace/vendor formatting, and diff checks. Earlier compiler/lint failures
and subsequent fixes are preserved in `validation.txt`; this is not a claim
that unrelated baseline test lints or the entire workspace are clean.

Before any measurements of the new binary, extend the fixed protocol with two
unprofiled default shell runs after the three timestamp diagnostics: preserved
`24ff389f5` control first, then the newly built candidate. Clear all experimental
and timing flags, retain 1,000 actions and the same limits, and report both
outcomes even on failure. This pair checks the unchanged-default runtime; it
does not establish a reproducible speedup from instrumentation-only changes.

## Next candidate, not implemented or adopted here

If stage costs support it, sparse coarse could dispatch only active native
bins while preserving native coordinates and per-tile draw/clip ordering.
Deduplicate bins with a bounded bitmap: row-major tile records do **not**
produce monotonically increasing bin IDs, because x-bin IDs repeat across
tile rows. Likewise, the current 65,536-tile limit does not imply 256 bins
when dimensions are narrow or partial.

A separate sparse-list shader permutation can pack bin IDs after the used
tile-bit words in the existing 8 KiB uniform, and fall back to the unchanged
sparse-coarse shader when the list does not fit. Failure propagation must
key off the first dispatched workgroup even when native bin zero is inactive.
Exact native-4K pixels, partial edge bins, clips, disconnected damage, capacity
fallback, and actual overflow recovery are required before any adoption.
Sparse coarse remains off by default; this is a candidate design, not measured
performance evidence.

## Review correction before measurement

The first ordinary release build passes in 12m40s. A concurrent read-only
review finds a diagnostic status race: reading status before taking a sample
can report Pending alongside a decoded match, or miss Invalid set during
dispatch decoding. That initial build is not measured. The follow-up changes
only benchmark reporting: capture the expected submission ID, take the sample,
derive Ready from a successfully decoded sample, otherwise query the resulting
status. It is tested and rebuilt before the fixed measurements above. Actual
frame timing, rendering, actions, resolution, and gate thresholds are unchanged.

The final source-frozen suite passes all 13 benchmark tests, including the
exact native-4K oracle, strict affected-library/example clippy, test-inclusive
changed-line/compiler diagnostics (zero), and formatting/diff checks. The final
ordinary combined release rebuild passes in 4m54s.

Preserved measured binaries:

- `shell-redraw-dispatch-profile-default-release-20260905`: SHA-256
  `fe86c79768871ff2d479ca2b38a94e127088ca025db90e2b5e0a61720bf193ce`.
- `ui-perf-dispatch-profile-default-release-20260905`: SHA-256
  `e3bf83357fbf4ac2554adc3efe366337aaa1dfed9ed06a3bf5640cd08059385d`.

Only the shell binary is timed in this follow-up protocol. Both reside in
`target/ui-perf-comparison/`; that local artifact directory is not committed.

## Completed dispatch diagnostics (not acceptance)

All three predeclared runs finish 1,000 native-4K actions: 999 sparse and one
full render, no recovery. Every dispatch sample matches its submission exactly:
1,000/1,000, with all missing/stale/unsupported/pending/failure counters zero.
The reported 2,048-byte allocation is specifically the dispatch timestamp
resolve/readback buffers, not total renderer memory or total instrumentation.

| Direct workgroups | Whole-action mean | Whole-action p99 | Exact native-4K pixel status |
| --- | ---: | ---: | --- |
| 128 (unchanged default) | 49.591 ms | 132.849 ms | Default oracle passes |
| 8 (diagnostic only) | 48.683 ms | 124.234 ms | **Fails**; not qualified |
| 1 (diagnostic only) | 52.793 ms | 118.382 ms | **Fails**; passing repeat does not erase failure |

Selected measured GPU stage means:

| Stage | 128 groups | 8 groups | 1 group | Matched stage samples/run |
| --- | ---: | ---: | ---: | ---: |
| Sparse fine | 11.967 ms | 12.250 ms | 11.967 ms | 999 |
| Coarse | 11.284 ms | 11.053 ms | 11.207 ms | 1,000 |
| Backdrop | 2.774 ms | 2.567 ms | 2.746 ms | 1,000 |
| Direct path tiling | 2.566 ms | 2.697 ms | 5.132 ms | 1,000 |
| Direct path counting | 2.332 ms | 2.096 ms | 3.010 ms | 1,000 |

The lone full-fine action takes 281.370 / 295.883 / 281.803 ms respectively;
it remains in the whole-action sample set. Stage statistics are conditional
on presence. Per-stage p99 values are in the raw logs and are not summed.
Timestamp instrumentation can perturb execution: these are not unprofiled
stage costs or acceptance results. All three runs intentionally return exit 1
with `DisabledDiagnostic`.

Neither smaller grid merits adoption. Eight groups do not materially reduce
the major costs, and one increases direct path work; both also fail exact
pixels. Coarse and sparse fine remain the dominant measured stages. The
active-native-bin design above targets one of these costs, but cannot by
itself establish the complete 8 ms mean / 30 ms p99 goal.

Normal wall-clock animations remain enabled, not replayed at identical times.
Average texture readback payload is 808,636 / 807,591 / 807,591 bytes per action;
full CPU destination writes remain 33,177,600 bytes per action in all three.
These are API payloads, not physical memory-bus traffic. No new RAM or GPU
memory reduction is claimed from this diagnostic-only checkpoint.

## Unprofiled default check

The predeclared control-then-candidate pair also completes all 1,000 redraws
at native 3840×2160 with no timing or experimental flags, 999 sparse / one
full attempt each, and no recoveries. Both use ordinary release defaults.

| Binary | Mean | p99 | Maximum | Peak process RSS | Gate |
| --- | ---: | ---: | ---: | ---: | --- |
| Previous committed `24ff389f5` | 39.100 ms | 101.620 ms | 317.367 ms | 538,012 KiB | **Fail** |
| This diagnostic-instrumentation candidate | 37.252 ms | 97.533 ms | 363.958 ms | 537,484 KiB | **Fail** |

These are single observations, not repeated evidence of a new speedup. The
current change does not promote a rendering optimization, and the results
must not be compounded with prior improvement percentages. RAM is effectively
unchanged in this pair. CPU user+system totals are 136.17 versus 131.26 seconds;
those include process setup. Earlier failed gates and fence timeouts remain
in the previous checkpoint's evidence.

The full objective remains unmet: mean ≤8 ms and p99 ≤30 ms, as well as the
broader constant-60-FPS / old-laptop requirement. This software Vulkan adapter
does not qualify physical GPU/KMS scanout or 2010 hardware. The checkpoint
adds tested measurement/acceptance safeguards and rejects an unsuitable tuning
direction; it does **not** claim success on that objective.
