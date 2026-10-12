# Whistle endpoint and resident session review

The compiled public decoder can remove the reproduced German streaming hallucination
without modifying recognized text: keep every streaming segment provisional, then
replace it with the supported full-audio `needle_transcribe` result at finish.
This is a source-only qualification adapter, not production model admission or a
finished OS dictation backend. The existing production pipe and compositor already
separate draft updates from final insertion; regressions exercise those paths.

## Raw recognition versus committed output

The same pinned Google FLEURS public fixtures and model/module from the
[compiled review](dictation-whistle-v8-review.md) are reused. These are exposed
development fixtures, not a held-out accuracy benchmark. Scoring uses the existing
NFKC/casefold/punctuation/whitespace scorer, with no number or accent cleanup.
Every committed value equals the selected raw full-audio result exactly.

| Public case | Raw streaming errors | Raw endpoint / committed errors | Audio seconds | Endpoint delivery seconds |
| --- | ---: | ---: | ---: | ---: |
| German | 7/18 | 6/18 | 17.94 | 21.506 |
| German, existing two-name cue | 6/18 | 4/18 | 17.94 | 21.750 |
| French | 0/20 | 0/20 | 10.20 | 12.860 |
| English | 4/19 | 4/19 | 10.56 | 12.297 |
| Spanish | 2/29 | 2/29 | 12.84 | 16.580 |

German streaming confidently emits and marks stable the unspoken `Vielen Dank.`;
its word probabilities are 0.938 and 1.0. Agreement across two streaming passes
is insufficient evidence of correctness. The endpoint result drops the prefix
naturally. Without cues it also introduces a spelling error, so the reduction
is 7 to 6, rather than simply subtracting two wrong words.

The existing newline-separated `Hongkong\nKowloon` cue improves Hongkong, but
Kowloon remains `Korlum`. Automatic detection returns German and exactly the
same full-audio text as the selected German language. Disabling word timestamps
also produces identical text on the four cued/multilingual controls. Neither
control fixes the name error. No confidence threshold, dictionary substitution,
hidden decoding parameter or reference-aware filter is used.

Selected endpoint delivery is emitted before the diagnostic automatic-language
and timestamp calls. CPU time and process lifetime include those diagnostics;
the delivery latency does not. All five scopes retain the previous resource
limits, observe one native thread, and have no high/max/OOM event increments.
The complete raw JSON, source receipts and observations are in the
[endpoint ledger](dictation-whistle-v8-endpoint-review.json).

## Resident lifecycle and production regressions

One model instance handles French speech, three seconds of generated zero-valued
silence, a canceled three-second speech prefix, and repeated French speech.
Silence commits nothing; cancellation discards its result and commits nothing;
the repeated final result equals the initial final result. Drafts never carry
committed text. The observed resident process uses one native thread, 197724 KiB
RSS high-water and 742496 KiB virtual peak; CPU time is 21.38 seconds. Cancellation
occurs between completed decoder calls, not during active inference.

The production worker regression passes an actual framed pipe through
`serve_decoder`: its controlled decoder returns `Vielen Dank.` provisionally,
then `Für Hongkong — nein, für Kowloon. 東京 café` at finish. The exact final
string replaces the preview; ending the pipe before finish emits no final reply.
The compositor regression exercises the native input bridge and verifies one
literal IME commit without appending the provisional hallucination. These tests
prove transport and insertion behavior, not spoken correction recognition accuracy.

## Reproduction and limits

Use `tools/release/qualify_whistle_v8_scope.py` in the existing admitted transient
scope, with the authenticated artifact bundle and public manifest. `--mode
endpoint` is the default; `--mode lifecycle` runs the four resident sessions.
`--name-cues` is admitted only for the existing German public fixture. Bundle
ownership, sizes, SHA-256 hashes and checked-out adapter source are verified
before decoding. Missing resource or session observations fail qualification.

The historical compiled ledger remains unchanged. To reproduce its original
streaming measurements, check out commit
`1f102809554b28a3bea932e368b99437ca8f2165`; its source hashes differ intentionally.
The endpoint ledger preserves the initial endpoint, four-language diagnostic,
and final lifecycle source profiles separately. The final JavaScript changes
only the lifecycle branch to use the same endpoint policy; it does not rewrite
historical measurements. A pressure admission refusal before decoding is retained
in the private review log; admission thresholds were not relaxed.

The adapter accepts at most 30 seconds of public audio. It does not change the
production 120-second limit or establish a policy for long dictation. Full-audio
endpoint decoding incurs extra latency and may introduce other errors. Real
microphone input, active-inference interruption, assembled browser/terminal
journeys, older-device/ARM behavior, and production V8 worker integration remain
unqualified. No microphone permission, model activation, installation or new
runtime dependency is introduced. The candidate remains held.

## Validation

Executed: five focused production worker tests and one native compositor endpoint
commit regression passed through ${CARGO_DROP_IN}. C++ compilation used strict warnings;
JavaScript syntax, Python Ruff/py_compile, Rust formatting and diff whitespace
checks passed. The required `shell --tests` compile check passed in 35.49 seconds, and strict
all-target Clippy for model-service and shell passed in 3m07s. Clippy initially
rejected an assertion inside the mock decoder's Result-returning method; the
assertion now lives in the test and all five worker tests passed again. The current-source
harness passed both endpoint and lifecycle modes under unchanged budgets, with
one observed native thread and no memory-limit/OOM events. Missing repeat output
and a canceled commit were rejected by the actual session validator. Eight owned
services from this round are inactive. The reproduction ledger hash records the
exact input ledger before its resulting observations were appended here.
