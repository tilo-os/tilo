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
