# On-screen keyboard quality checks

The primary interaction under evaluation is swipe gesture typing. The production
keyboard is `apps/osk`. QWERTY selects the embedded English
predictor and AZERTY the French predictor. The geometric swipe decoder works
offline without downloaded weights; its `SwipeModel` seam permits evaluating
another decoder separately. Source, table and optional-weight provenance is
recorded in [`apps/osk/data/README.md`](../../apps/osk/data/README.md).

## Acceptance journeys

Run the public Cargo commands below on an approved remote builder when working on the laptop.
Use four build jobs for this suite. No personal text, captured typing or training
is needed. The touch fixtures are authored words and paths; the noisy-tap
benchmark uses the checked-in `typing_phrases.txt` and a fixed random seed.

| Journey | Required behavior | Production check |
|---|---|---|
| Two overlapping thumbs | Every key appears once in press order, regardless of lift order | `touch::overlapping_taps_type_in_press_order`, `touch::fast_alternating_thumbs_drop_no_keys` |
| Solo swipe | The prerecorded path composes its decoded word | `touch::one_finger_alone_on_the_keys_swipes_a_word` |
| Coalesced fast swipe | Down/up alone across adjacent letters composes the word | `touch::a_fast_adjacent_letter_gesture_uses_its_release_endpoint` |
| Layout identity change | Old contacts cannot type a new letter when rectangles are unchanged; a fresh contact works | `touch::changing_letters_without_changing_rectangles_cancels_old_contacts` |
| Swipe phrase correction | Replacing the second word preserves its prefix as preedit; the next gesture commits exactly one separating space | `widget::tests::swipe_correction_preserves_the_phrase_and_the_next_gesture` |
| Swipe capitalization | Sentence hints and Shift capitalize the first letter, Caps Lock stays active; candidate corrections match the word's case | `widget::tests::swipe_typing_obeys_sentence_capitalization_and_caps_lock` |
| Visible gesture feedback | The trail follows a solo contact, disappears on cancellation, and never exposes secret input | `widget::tests::gesture_feedback_is_transient_solo_and_never_secret`, `render_smoke::a_live_swipe_trail_paints_and_cancellation_removes_it_in_every_theme` |
| Focus changes while a finger is held | Old releases emit nothing; a fresh contact still types | `touch::a_field_change_cancels_contacts_before_their_release` |
| Rotation while a finger is held | Old coordinates cannot type a different key | `touch::rotation_cancels_a_contact_in_the_old_geometry` |
| Keyboard disappears during Backspace hold | No repeat deadline or deletion survives hiding | `widget::tests::hiding_cancels_a_held_backspace_before_its_deadline` |
| Long-press accent | The final lift position selects the accent even without a separate move event | `widget::tests::lifting_selects_the_accent_at_the_final_contact_position` |
| Secret-field touch | Taps commit literally, create no suggestions and never adapt touch offsets | `widget::tests::secret_touch_typing_never_adapts_the_touch_model` |
| Identifier fields | Email, URL and password taps ignore language and personal touch bias | `widget::tests::verbatim_fields_ignore_personal_and_language_touch_bias` |
| Display scale | One tap types exactly once at 1, 1.25, 1.5 and 2 times scale | `touch::scaled_touch_coordinates_type_once_at_the_drawn_key` |
| Language and orientation | English/French taps type correctly at 390×720, 720×390 and 960×600 | `touch::english_and_french_touch_typing_follow_phone_and_tablet_geometry` |
| Editing toolbar | Semantic Paste inserts literal clipboard text without learning it; caret moves commit preedit before the arrow; Hide cancels held contacts | `widget::tests::semantic_toolbar_edits_the_focused_field_and_hides_without_stale_contacts` |
| Accessibility | Editing keys have spoken names; semantic activation cycles announced shift state | `widget::tests::editing_keys_have_spoken_names_and_shift_announces_its_state` |
| Painting | Production layout and rendering paint in dark, light and high contrast; unavailable GPU is failure | `render_smoke::osk_renders_keys_and_toolbar` |

Contact cancellation follows the current typed `follow_input_method(ImTransition)`
contract. Field changes reset composition and contacts; hiding cancels contacts
and timers; purpose changes cancel contacts before changing field layout. Stale
geometry is rejected before contacts act, rather than during passive painting.
The existing input-method owner and ordering contracts remain the integration
door for OSK and dictation; no competing injection mechanism is introduced.

## Synthetic touch baseline

The existing resolver uses a per-key Gaussian offset,
a context prior, a protected inner core and bounded local adaptation. A run on
the approved remote builder gave these deterministic benchmark results on
`4b1210dc7c`; qualification on current base `b941e4f47f` reproduces them:

| Gaussian spread in key units | Rectangle error rate | Context resolver error rate | Intended-key taps overridden |
|---|---:|---:|---:|
| 0.15 | 0.53% | 0.17% | 0.00% |
| 0.20 | 3.25% | 1.39% | 0.20% |
| 0.25 | 10.38% | 5.24% | 0.33% |
| 0.30 | 20.42% | 13.56% | 1.13% |

The test requires at least a 20% relative error reduction and fewer than 1.2%
overridden intended-key taps at every spread. These are synthetic per-letter
results, not human word-error rates. The cancellation and secrecy changes do
not tune or retrain this model.

Before the fix, the field-change journey emitted `q` into the next field and
the rotation journey emitted `w` from a contact that began on `q`. Both must
emit zero characters after cancellation.

## Commands and limits

```sh
cargo test --locked -p osk --lib -j 4 -- --nocapture
cargo test --locked -p osk --test it -j 4 -- --nocapture
cargo clippy --locked -p osk --all-targets -j 4 -- -D warnings
cargo check --locked -p shell --tests -j 4
```

Current-base qualification (`b941e4f47f`) passes all 158 normal library and
integration tests, including offscreen rendering in all three themes.

The normal library suite excludes three pre-existing opt-in benchmarks: FUTO
runtime decoding, a release-only latency budget, and the touch-parameter sweep.
Passing debug/offscreen checks does not certify touch-to-photon latency on Yoga
hardware, physical rotation delivery, a live screen reader, or another language.
Do not deploy or repeatedly launch a desktop to obtain those claims. A dedicated
approved device session and release measurement are still required.

## Optional gesture-data evaluation

FUTO's donated English-QWERTY gesture dataset has a separate
[MIT license](https://huggingface.co/datasets/futo-org/swipe.futo.org/blob/main/LICENSE).
That license does not cover the keyboard app, model weights or arbitrary tap
corpora. Its [dataset card](https://huggingface.co/datasets/futo-org/swipe.futo.org)
provides donor-session-separated training/validation/test sets; preserve that
separation for an independent evaluation. Review privacy and provenance, pin
revision and checksums, and keep exact notices before importing anything.

The independently written import tool reads only a bounded validation prefix
on the existing remote runner. It pins dataset revision
`d71bf5fd7f45b3e7c2ed2d76a21b0dbd3b4ba566`, verifies the MIT notice and QWERTY
layout SHA-256 values, and keeps the notice alongside extracted fixtures. It
retains the original validation membership, removes session IDs, sentences and
timestamps from evaluator input, and reports only aggregate session counts.
No raw donated gesture fixture, FUTO app source, model output or model weight
is committed to Git or shipped in the OS image. No training was performed.

Run this concrete production-decoder evaluation on the remote builder:

```sh
cargo run --locked -p osk --example swipe_eval -j 4 -- target/osk-eval --fetch-validation 256
cargo run --locked --release -p osk --example swipe_eval -j 4 -- target/osk-eval
```

The importer caps the prefix at 1000 rows and 16 MiB of source bytes. The
256-row prefix covers five original donor sessions. It contains 246
multi-letter gestures, of which 201 target words are in the embedded dictionary.
On the historical base `4b1210dc7c`, the baseline and endpoint-indexed decoder both recover exactly 160 top-1,
184 top-3 and 195 top-8 targets. Out-of-vocabulary words count as misses;
one-letter/one-point records are explicitly excluded. These prefix results
are exploratory, not a representative held-out accuracy claim.

The endpoint index applies the existing first/last-letter pruning before
scanning vocabulary, preserving the scoring rule. On the same remote debug
run, median decode time changed from 38.034 ms to 30.599 ms and p95 from
82.230 ms to 76.854 ms. Release timing is the relevant decoder latency check;
these measurements never certify touch-to-photon latency on a real device.
The indexed release decoder measured a 3.185 ms median and 7.950 ms p95 on
this same prefix, below the 30 ms decoder budget. Accuracy counts were unchanged.
The two debug aggregate reports and the release report are adjacent to this document and include
source-prefix and extracted-fixture hashes for reproducibility.

On current base `b941e4f47f`, the release decoder reproduces those accuracy
counts and measures 3.326 ms median and 8.320 ms p95. Its aggregate evidence is
[`osk-swipe-release-current.json`](osk-swipe-release-current.json). These numbers
measure the decoder only; no touch-to-photon or representative accuracy claim
is implied.

Future training or CPU-intensive evaluation must run on the approved existing
remote runner with a bounded resource budget. Keep donor-session partitions
intact, leave the test set untouched during tuning, and audit any separately
introduced code, weights or dictionaries on their own licenses.

## Session-isolated swipe qualification

The offline geometric matcher now reranks the original geometric shortlist with bounded
elastic path alignment. Its 64 candidates and 32 samples use a six-sample
alignment band, endpoint weight 0.15 and dictionary-prior weight 1.0. It uses
the existing English/French dictionaries, learns no parameters or text, and
requires no downloaded model. Letter-key dimensions define its scoring unit;
editing-key width cannot change recognition of an otherwise identical trace.

The complete original validation split contains 54,269 gestures in 692
anonymous sessions. Before inspecting errors, SHA-256 of the original session
identifier assigned sessions to analysis (`first byte < 51`) or qualification.
Only anonymous numeric ordinals enter the evaluator; the identifier, sentence
and timestamp are discarded. The analysis cohort contains 9,602 records and
the qualification cohort 44,667. Original train/dev/test membership is retained.
The original training split was not read. No qualification or test errors are
exported or inspected, and invalid target labels are not filtered to improve
scores. One-letter/one-point records are excluded explicitly.

Analysis revealed that the original shortlist already recovers 98.7% of known
lexical targets, while top-1 ranking is much weaker. A three-value prior sweep
(0.25, 0.50, 1.00) on analysis sessions selected 1.00 before qualification and
test import. All trials, including the weaker settings, are recorded in
[`osk-swipe-analysis-trials.json`](osk-swipe-analysis-trials.json). No neural
training or vocabulary expansion was needed for this improvement.

| Qualification metric | Original matcher | Frozen elastic matcher |
|---|---:|---:|
| Literal top-1, 43,448 eligible gestures | 65.51% | 73.13% |
| Lexical top-1, 43,431 eligible gestures | 74.17% | 83.48% |
| Lexical top-3 | 85.55% | 90.32% |
| Lexical top-8 | 89.71% | 91.83% |

Literal scoring retains lowercase source target spelling. Lexical scoring
separately folds accents and removes non-ASCII letters from targets and
predictions; the same transformation applies to both matchers. Normalization
is a reporting convention, not a model improvement. Unknown words count as
misses. Candidate recall and eligibility are identical in the paired comparison.

The lexical top-1 gain is 9.31 percentage points, with a paired session-cluster
95% bootstrap interval of 8.66–9.93 points (10,000 replicates, fixed seed).
Resampling whole sessions avoids treating gestures from one session as
independent participants. This is session isolation, not verified separation
of human identities across sessions. The frozen matcher measures 3.143 ms
median and 7.792 ms p95 for diagnostic top-64 decoding on qualification;
these timings measure decoding, not physical touch-to-photon latency.

Aggregate source and session-count evidence is in
[`osk-swipe-validation-baseline.json`](osk-swipe-validation-baseline.json),
[`osk-swipe-validation-elastic.json`](osk-swipe-validation-elastic.json) and
[`osk-swipe-validation-paired.json`](osk-swipe-validation-paired.json).
The dataset's exact [MIT notice](osk-swipe-data-LICENSE.txt) accompanies these
reports. The pinned source SHA-256 is
`8d9d34ec1c12f564d02dcb69cf132ba19d45b75632a205af90d3a81290e24972`.
The [collection paper](https://arxiv.org/html/2606.25247v1) describes a donated
English QWERTY corpus with formal encyclopedic stimuli;
it does not certify conversational typing, all languages or all devices.

Run the full comparison on an approved remote builder, with four build jobs:

```sh
cargo run --locked --release -p osk --example swipe_eval -j 4 -- target/osk-validation-full --fetch-full-validation
cargo run --locked --release -p osk --example swipe_eval -j 4 -- target/osk-validation-full --elastic-prior 100 --qualification-only
cargo run --locked --release -p osk --example swipe_eval -j 4 -- --compare docs/testing/osk-swipe-validation-baseline.json target/osk-validation-full/report-elastic-100-qualification.json target/osk-validation-full/paired-qualification.json
```

The full importer is bounded to 512 MiB, 60,000 rows, 256 KiB per line and a
900-second import deadline. It verifies the full source hash, row count,
license/layout hashes and the stripped fixture hash before evaluation.
`--fetch-full-test` imports the separately pinned original test split after
freezing the matcher; it never exports test errors. The source metadata query
`--source-info` reproduces the published split checksums without reading gestures.

The separately pinned original test split contains 49,970 records in 697
sessions (48,660 lexical-eligible gestures). The frozen matcher reproduces the
qualification improvement:

| Original test metric | Original matcher | Frozen elastic matcher |
|---|---:|---:|
| Literal top-1 | 65.54% | 73.08% |
| Lexical top-1 | 74.07% | 83.06% |
| Lexical top-3 | 85.37% | 89.98% |
| Lexical top-8 | 89.45% | 91.60% |
| Decode median | 3.020 ms | 3.190 ms |
| Decode p95 | 7.725 ms | 7.920 ms |

The test lexical top-1 gain is 8.99 percentage points; its paired session-cluster
95% interval is 8.51–9.46 points. Top-1 improves in every reported orientation
and word-length bucket. The smallest orientation bucket has only 48 eligible
gestures, so it cannot support a strong hardware-specific conclusion. Source
SHA-256 is `d4e42e34ced1cae925a5ec94672bdaf48b4532598a1e39d72884f9c7b588721e`;
the importer verifies it against the pinned revision's published LFS checksum.
No test-based retuning occurred. The aggregate reports are
[`osk-swipe-test-baseline.json`](osk-swipe-test-baseline.json),
[`osk-swipe-test-elastic.json`](osk-swipe-test-elastic.json) and
[`osk-swipe-test-paired.json`](osk-swipe-test-paired.json).

```sh
cargo run --locked --release -p osk --example swipe_eval -j 4 -- target/osk-test-full --fetch-full-test
cargo run --locked --release -p osk --example swipe_eval -j 4 -- target/osk-test-full --elastic-prior 100
cargo run --locked --release -p osk --example swipe_eval -j 4 -- --compare target/osk-test-full/report-classic-all.json target/osk-test-full/report-elastic-100-all.json target/osk-test-full/paired-test.json
```

A separate production-event regression covers a fast adjacent-letter gesture
with only touch-down and touch-up. Before the classifier fix, `a`→`s` inserted
`s`; after it, the same asserted journey composes `as`. Both classification
and decoding now use letter-key scale; non-letter panels retain their previous
scale behavior. This delivery fix changes neither the frozen ranking weights
nor its corpus-scoring calculation. Existing secrecy, focus cancellation,
multi-thumb, correction, casing and theme journeys also exercise the updated geometric
matcher. The corpus reports record the exact decoder source used for measurement.


The live decoder chooser still prefers the existing optional FUTO runtime when
its libraries and caller-provided model assets are available. This increment
qualifies the geometric fallback and does not change that chooser, download
weights, or claim FUTO-runtime accuracy. The identical-rectangle layout test
also checks that a fresh contact types the new letter after cancelling the old
one; it uses the production preference path, not a geometry-only helper.

Final geometric qualification passes 162 normal library/integration tests, the
existing release-only decoder latency guard, all-target clippy and the shell
integration compile. FUTO runtime decoding, the parameter sweep, physical Yoga
input, live screen-reader operation and physical touch-to-photon latency remain
unrun; no desktop launch or deployment was used for these results.
