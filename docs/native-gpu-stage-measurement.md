# Native DRM GPU stage diagnostics

For an explicitly profiled compositor launch, set `TILO_PROFILE_GPU_STAGES=1`,
`VELLO_GPU_TIMESTAMPS=1` and `VELLO_GPU_DISPATCH_TIMESTAMPS=1`. The first flag
opts into consuming and logging the existing Vello dispatch samples. With the
flag absent, the bridge has no observer and makes no diagnostic calls or records.
This does not modify the scanout copy, damage, rendering or presentation paths.

Each `VELLO-STAGES` JSON record comes from an ordinary direct DRM paint. It carries
the actual process and renderer generation (which changes on recovery),
that paint's revision, success, latest attempted submission and its own GPU sample
submission ID. A sample can arrive after another frame has been attempted. Such a
sample retains its original ID and is marked stale relative to the current paint;
it must never be attributed to that paint's revision or input. A failed paint or
one that submitted nothing has no expected current submission.

Coverage counts describe paint attempts: matched current samples, missing current
samples and stale samples received. Matched plus missing equals attempts; stale
samples are a subset of missing-current attempts. These are not counts of all GPU
submissions, because an individual paint can retry. The reported status also exposes
unsupported, pending, invalid or failed timestamp mappings rather than fabricating
a duration. Stage durations are decimal nanosecond strings to preserve u128 values;
each stage retains its stable shader ID/name and dispatch count, including unknown
labels. Aggregate only distinct (process, renderer generation, sample submission ID)
identities and report missing/stale
coverage alongside any averages.

Timestamp queries and per-paint logging add diagnostic work. Collect these stage
profiles separately from the primary typing acceptance run and report the enabled
flags with the result.

These are GPU dispatch diagnostics, not key-to-present measurements. They do not
establish which scratch body was drawn or when a KMS flip showed it. The physical
Notes input/effect/frame/presentation observer supplies that separate acceptance
proof. Historical aggregate timings and a returned paint cannot certify typing
p50 below 20 ms or p99 below 50 ms.
