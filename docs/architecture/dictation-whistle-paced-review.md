# Whistle release and paced-input review

Whistle remains a development candidate. Both native runtime variants tested on
2026-10-08 exceed the unchanged two-thread budget during initialization, before
audio is submitted. Source-paced CLI previews fit that budget, but emit incorrect
early hypotheses and miss names. These observations do not qualify resident
streaming, microphone use or universal field insertion.

The machine-readable receipts, limits and artifact digests are in
[dictation-whistle-paced-review.json](dictation-whistle-paced-review.json).
The [thread initialization follow-up](dictation-whistle-thread-review.md) traces
the host-CPU fallback, distinguishes idle compute workers from support tasks,
and compares the CLI's supported one- and two-thread settings without relaxing
the native admission guard.
Earlier failures remain in
[dictation-whistle-native-review.json](dictation-whistle-native-review.json).

## Exact release identities

The earlier native failure used the static `linux-x86_64/libneedle.a` from
[Needle3 revision 2ae11323](https://huggingface.co/Cactus-Compute/needle3/tree/2ae11323dc000f5e70c49f7403efa6af12ba9e67),
with [Whistle revision b358ddad](https://huggingface.co/Cactus-Compute/whistle/tree/b358ddadd89b7a713b5aa131f23032d3cca1b251).
Those archive, CLI, model and header digests remain unchanged at the official
repository revisions inspected for this comparison. The separate broader Cactus
source audit was not the runtime behind that failure.

The [official Python wrapper at af2654df](https://github.com/cactus-compute/needle/tree/af2654dffa4987f7e2cbbacdf59c2f854fd18302)
declares package version **3.1.0**, but Whistle selects engine generation 3 and
its fetch map chooses **3.2.0**. This resolves to a separate
`cactus_needle-3.2.0-py3-none-manylinux2014_x86_64.whl` in the pinned Needle3
repository. Comparing only the static archive would miss that distinction.
The four-member wheel was inspected without installing or importing it; only
the pinned `needle/libneedle3.so` was privately extracted for the native trial.
The repository licence notice was retained. No runtime is admitted for shipping.

With the same phase-marked adapter, model, PCM and limits, the static archive
completed load/model initialization in 8.15 ms and reached 12 native threads;
the 3.2.0 shared runtime completed that phase in 7.52 ms and also reached 12.
The observer stopped them at approximately 48 ms and 43 ms respectively, before
the first audio submission scheduled at two seconds. Neither supplies a
transcription or usable latency result. The inspected header and shared exports
offer no public thread setter; environment guesses did not enforce the limit.
No private ABI call, binary patch or relaxed guard was used.

## Source-paced CLI controls

Already exposed public FLEURS development fixtures were fed as elapsed PCM
prefixes: two seconds initially, then one-second increments, with backpressure
when decoding ran past the next interval. The official file CLI reloads the
model and resets stream state on every prefix. These are preview experiments,
not a resident streaming implementation. Complete-clip vendor `ttft_ms` and
language-token output are excluded from the first-word measurement.

| Single-pass case | First words received | First reference word matched | Completion after audio end | Final raw word errors |
| --- | ---: | ---: | ---: | ---: |
| English | 2.07 s | 2.07 s | 1.77 s | 4 / 19 |
| German | 2.06 s | 5.10 s | 3.23 s | 5 / 18 |
| French | 2.09 s | 3.08 s | 2.20 s | 0 / 20 |
| Spanish | 2.08 s | 3.07 s | 2.85 s | 2 / 29 |

The first French, German and Spanish hypotheses were unrelated thanks phrases.
Matching the first reference word is post-hoc fixture scoring, not a runtime
filter or proof that the whole draft is correct. Each result is one observation
on a shared AMD EPYC 9R45 builder, not a physical laptop measurement.

The French control changed only decoding mode: internally replaying two-second
chunks of each growing prefix versus decoding each prefix once. Completion
delay fell from 6.03 to 2.20 seconds and measured CPU time from 21.45 to 12.05
seconds. This avoids duplicate decoding in that harness; it does not establish
that the earlier 15.316-second first-word laptop problem is fixed.

The German control changed only two supported keyword cues, the already exposed
public names `Hongkong` and `Kowloon`. Final literal name recognition improved
from zero of two to one of two; word errors fell from five to four of eighteen.
This does not qualify an unseen personal dictionary or acceptable name accuracy.

All six CLI scopes passed their resource checks, covering 70 decoder invocations
with at most two native threads, roughly 40–44 MiB native RSS peaks and no high,
max or OOM events. The two native diagnostic scopes were rejected, not counted
as passes. All eight owned transient services are inactive. Network and home
access were denied; no microphone, package installation, new trust, signing,
deployment or production catalogue change occurred.

## Remaining admission gates

Resident native streaming needs supported thread control and auditable runtime
provenance before it can enter the existing confined worker. Repeated sessions,
silence and noise, cancellation, rolling audio beyond thirty seconds, an untouched
holdout and representative physical devices remain unqualified. Current CLI
previews do not meet the accuracy or lifetime requirements for a default engine.
The shared original-field insertion, privacy and undo boundaries remain required
for every engine; changing recognition does not grant another insertion path.
