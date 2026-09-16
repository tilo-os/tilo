# Task 1 independent review: custom screenshot capture fidelity

## Verdicts

- **Spec compliance: FAIL.** The standard screenshot path and 17 handlers satisfy the requested theme/scale plumbing, but Imager's preserved `--shots-dir` route does not honor the required `QOS_SHOTS_TEXT_SCALE` compatibility fallback.
- **Task quality: FAIL.** The implementation is otherwise consistent and well-scoped, but the standalone Imager path duplicated shared theme resolution and immediately drifted from its CLI/environment precedence contract. This is a blocking correctness issue for the task's stated capture oracle.

## Findings

### [P2] Route Imager batch captures through the shared scale precedence

**Location:** `apps/imager/src/main.rs:94`, `apps/imager/src/main.rs:246`

The preserved `--shots-dir` path parses only an explicit `--text-scale` and passes that `Option<f32>` to `standalone_shot_theme`. When the option is absent, `standalone_shot_theme` calls `with_text_scale(text_scale.unwrap_or(1.0))`; it never reads `QOS_SHOTS_TEXT_SCALE`. Therefore `QOS_SHOTS_TEXT_SCALE=2 imager --shots-dir <dir>` still produces 1x typography, even though the brief requires CLI scale to win over the compatibility environment scale and specifically requires the standalone Imager scenario route to use the same theme/renderer setup. The regular `--shots` path at `apps/imager/src/main.rs:145` correctly calls `ui::launch::shot_theme(args)`, so behavior now differs by capture entry point.

Use the same pure scale selection logic for the standalone path (or expose a shared resolver that accepts the parsed CLI value) so the precedence remains: valid CLI value, then `QOS_SHOTS_TEXT_SCALE`, then 1.0, with `Theme::with_text_scale` retaining final validation/clamping.

## Compliance and quality evidence

- The diff covers all 18 named custom handlers: Camera, Chat, Code Editor, Contacts, Disks, Slides, Sheets, Typst Editor, Video Player, Video Editor, Word Processor, Files, Maps, Monitor, Imager, Music, Terminal, and Store.
- Camera and Slides pass the resolved theme to `FrameHarness::new_with_theme`; the harness applies `theme.text_scale` to its renderer at `ui/src/headless.rs:659`.
- Every manual render path configures its `TextRenderer` from `theme.text_scale`, measures with `ui::widget::compute_widget_layout`, and draws with that same renderer instance. Files also rebuilds the admitted bindings before its second structural settling solve (`apps/file-explorer/src/main.rs:799`).
- Fixture and settling behavior visible in the diff is preserved: setup remains after initial sync where required; Code Editor retains scenario relayout; Files retains its second structural pass, priming draw, thumbnail warm-up, post-layout staging, and overlay; Video Player/Editor retain media warm-up; Store retains three renderer warm-up passes; Music retains its live settling flow; Word Processor retains the explicit 1000x700 history capture.
- `ui::launch::shot_theme` now uses `Theme::named`, keeps all three motion environment inputs, and applies explicit CLI scale before the compatibility environment fallback (`ui/src/launch.rs:822`). Its pure regression table contains all four required precedence cases.
- Files keeps all 17 scenarios and `--path`; `shot_viewport` correctly gives explicit `--size` precedence over standard `--viewport` (`apps/file-explorer/src/main.rs:146`). `--size` and `--path` remain screenshot-only because `LaunchSpec::allowed_options` values are rejected outside screenshot modes by `ui/src/launch.rs:990`; the positional live-directory and `--open-share-stdin` behavior remains intact.
- Named theme and reduced-motion behavior is preserved in the standard handlers. Music retains its `--theme`, `QOS_MUSIC_THEME`, and app-scoped motion overrides while inheriting shared text scale.
- The report records successful checks for all 18 packages, all binary tests, and UI tests (3064 passed, 2 ignored). No broad test suite was rerun. The source-level Imager failure is deterministic and does not require a focused runtime test to establish.
- The recorded macOS compact-unwind linker warning is an existing baseline and is not a finding. The visible 2x composition failures are explicitly deferred and are outside this capture-fidelity review.

## Scope

Reviewed only the supplied `47fc2425a..ef5f08db5` package plus narrow source checks needed to resolve the concrete Imager and Files compatibility risks. No production files or git state were changed.

---

## Round 1 re-review

### Updated verdicts

- **Spec compliance: PASS.** The Imager `--shots-dir` path now applies the required CLI → `QOS_SHOTS_TEXT_SCALE` → 1.0 precedence through the same public pure resolver used by `shot_theme`.
- **Task quality: PASS.** The fix is minimal, documented, regression-covered, and introduces no new issue in the two-file round-one diff.

### Finding closure

The prior P2 at `apps/imager/src/main.rs:94` and `apps/imager/src/main.rs:246` is closed. The batch path reads `QOS_SHOTS_TEXT_SCALE`, passes it alongside the parsed CLI value to `ui::launch::shot_text_scale`, and applies the result through `Theme::with_text_scale`. An explicit CLI value therefore wins, a valid environment value is used when CLI is absent, and the shared helper supplies 1.0 otherwise.

`ui::launch::shot_text_scale` is now public and documented at `ui/src/launch.rs:846`, allowing the standalone frontend to share policy without mutating process-global state. The new Imager test covers environment fallback and CLI precedence. The existing launcher table continues to cover invalid environment input and the final default.

### Verification assessment

The reported focused results are sufficient for this patch: both Imager binary tests pass, the launcher precedence test passes, `cargo check --no-default-features -p imager` passes, and `git diff --check` passes. Per review direction, no broad tests were rerun. The existing compact-unwind linker warning remains baseline noise rather than a new defect.
