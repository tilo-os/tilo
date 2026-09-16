# Shared Task 3 — opt-in full writing surface

## Outcome and scope

Implemented `Scrollable::with_content_min_viewport_height()` and the authorized one-line Scratchpad opt-in. Empty auto-height editors fill the measured viewport; long editors retain intrinsic overflow; default compact fields and explicit child heights retain their contracts. Lower-area clicks focus and place the caret, including a second click on an already-focused editor. First-wheel scrolling reaches the outer pane without losing the event to fractional inner overflow.

Initial HEAD was verified as `83c269dac5d2fa49078472887d9c4dda530c2ae6`. Concurrent unrelated work subsequently advanced HEAD (observed `396cf024b` before the final checks). No resets, pushes, merges, dependency upgrades, new build caches, or edits to other tasks' files were performed.

Owned commit paths:

- `ui/src/widget/scrollable.rs`
- `ui/src/widget/text_input.rs` (controller-authorized intrinsic-height correction only)
- `ui/tests/scrollable_test.rs`
- `.superpowers/sdd/2026-09-15-measured-ui-components/task-3-report.md`

Scratchpad remains pre-existing untracked work and is not committed. Original source was verified byte-for-byte against `.superpowers/app-ui-2026-09-15/scratchpad-original/lib.rs`, SHA256 `0875a6f0447707f20534e4123040fa39599024da0345c13238208d8cc6b0cce4`. Its only persistent source edit is `.with_content_min_viewport_height()` at the existing Scrollable chain. The exact app-only diff is saved in `task-3-scratchpad.patch` beside this report. All existing action bindings and app copy remain intact.

## Implementation and self-review

- The opt-in is absent by default. Existing Scrollable layout topology and intrinsic behavior stay unchanged when it is not used.
- The builder installs one presentational `ViewportContent` wrapper. Builder order and repeated opt-in are covered. The wrapper reuses Container's ordinary child lifecycle while registering that layout node as its own; it exposes the real child through normal traversal and creates no accessible control.
- A retained `Rc<Cell<f32>>` carries the measured viewport decision. Scrollable's adjustment changes only that decision. The wrapper changes only its own `min_size.height`. Neither adjustment mutates children or changes topology. Its Grid `minmax(min-content, 1fr)` row stretches short auto content while retaining long content. Explicit child sizing is never rewritten.
- The retained-tree test uses the production bounded solver through repeated narrow/wide, short/long/short transitions, verifies the child's full Taffy style is unchanged, and checks real scroll extents. The public Driver tests cover the host's rebuild/interaction lifecycle as well.
- Two independently reproduced interaction issues were corrected within controller-approved scope. TextInput's shaped multiline intrinsic height now rounds upward before Taffy rounds layout pixels, eliminating a phantom fractional inner scroll range. Fixed/manual dimensions and wheel-consumption logic are unchanged. Scrollable now assigns background focus only after an unhandled child press; explicit child focus survives, and scrollbar presses still focus the pane.
- Existing scrollbar/background keyboard behavior, default compact fields, explicit 72px child height, lower clicks and caret placement at 1×/2×, and the app's action/persistence routes all pass.

## RED evidence and investigation

All Cargo commands used `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0` unless explicitly shown otherwise. Full logs are retained beside this report under `task-3-logs/`.

1. Initial behavioral RED:
   `cargo test -p ui --test it scrollable_test -- --nocapture`
   — 6 passed, 2 failed. Empty editor ended at y=36 while the footer began at y=360, including the narrow resize fixture. `task3-red-integration.log` records it. A test-only no-op extension builder allowed a behavioral RED before the production API existed; it was removed when the real builder was implemented.
2. Initial wrapper GREEN attempt: 7 passed, 1 failed because a test assumed every long document exceeded 1000px. At 760px wide, the fixture legitimately measured 816px. Corrected that assertion to the actual requirement, exceeding the 400px viewport. The next run passed 9/9 (`task3-green2.log`). No production change was made for that test correction.
3. Expanded actual RED after app validation:
   `cargo test -p ui --test it scrollable_test -- --nocapture`
   — 8 passed, 2 failed (`task3-fractional-red4.log`).
   - Second lower click: text remained `lower click` instead of `lower click at end`. Scrollable's early focus fallback displaced the already-focused child because TextInput did not request focus again.
   - First wheel: 47 nonempty rows at font size 18.1, width 416, viewport height 300 produced a 1079px box with `content_height() == 1079.375`. The first wheel changed inner scroll from 0 to 0.375 while outer y remained 0, with the editor focused throughout.
4. Default Scrollable focus RED:
   `cargo test -p ui --test it scrollable_test::repeated_click_keeps_default_scrollable_editor_focused -- --exact --nocapture`
   — failed, actual `first`, expected `first again` (`task3-focus-red.log`). This proves the fallback correction also covers the default tree.
5. First-wheel correction GREEN:
   `cargo test -p ui --test it scrollable_test::fractional_height_editor_routes_first_wheel_to_outer_scrollable -- --exact --nocapture`
   — 1 passed (`task3-fractional-green.log`). Final all-Scrollable integration run passed 12/12.

Transient unrelated compile blockers were preserved and later resolved by their owners: `headless.rs` missing `sync`/ambiguous integer; Chromium `encode_command` test expected Value instead of Option<Value>; `native_scenario.rs` called App probe methods that were not yet present. Initial library attempts are in `task3-red.log`, `task3-existing-ui-tests.log`, and `task3-existing-ui-tests2.log`. They are compile blockers, not behavioral REDs. The final library build and affected tests are green.

The capture harness also required two fixture corrections: replace the unjustified height > 2× window check with true overflow; use Driver modifier state plus `key("End")`, since `key("Ctrl+End")` is a literal character string. Both helpers now use `Theme::dark().with_text_scale(scale)` to follow the normal resolved-theme contract. Earlier `task-3-captures`, `-v2`, and `-v3` directories are diagnostic artifacts, not acceptance captures. A repeated-wheel diagnostic did not resolve the actual issue and was removed; the final harness requires one downward wheel.

## Final verification

- `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui --test it scrollable_test -- --nocapture`
  — **12 passed**, 0 failed (`task3-final-integration.log`).
- `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui --lib widget::scrollable::tests -- --nocapture`
  — **34 passed**, 0 failed (`task3-existing-ui-final.log`). Includes scrollbar drag, paging, clipping/focus boundaries, gutters, manual heights, and compact max-height behavior.
- `target/debug/deps/ui-98aece3f3d10fdcd widget::text_input::tests --nocapture`
  — **99 passed**, 0 failed (`task3-text-input-final.log`). Uses the just-built library test binary from the preceding Cargo command.
- `target/debug/deps/ui-98aece3f3d10fdcd widget::code_editor::tests --nocapture`
  — **23 passed**, 0 failed (`task3-code-editor-final.log`). Includes the wrapper's theme/renderer measurement regression.
- `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 cargo check -p ui`
  — passed (`task3-check-final.log`).
- `rustfmt --edition 2024 --check ui/src/widget/scrollable.rs ui/src/widget/text_input.rs ui/tests/scrollable_test.rs`
  — passed.
- `git diff --check -- ui/src/widget/scrollable.rs ui/src/widget/text_input.rs ui/tests/scrollable_test.rs`
  — passed.

The final lib test link emitted the existing macOS `__eh_frame section too large` warning; compilation and tests succeeded.

## Actual Scratchpad validation and captures

Reproducible harness source: `task-3-scratchpad-check.rs` beside this report. It was temporarily copied to `apps/scratchpad/tests/task3_surface.rs` for compilation and removed after validation. No app test file was left behind or committed.

Final build:

```sh
CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p qos-scratchpad --no-run
```

Build log: `task-3-logs/task3-app-final-build.log`. It built the source including the one-line app opt-in and both confirmed framework fixes. Binary timestamps were September 16, 2026, 10:34:54 local:

- `/Users/xaver/dev/qos-macos-shell/target/debug/qos-scratchpad`
  SHA256 `14568d44002fdd5713266ce1040de916389fad1c57dc005326f200766d0a96b4`
- `/Users/xaver/dev/qos-macos-shell/target/debug/deps/task3_surface-b32c6a49fb72a68a`
  SHA256 `958b13c2e3b7c5687c7a9fcbbebf39bf487af45c43f7ccf59dcc33569008b177`

Runtime captures used isolated HOME and all four XDG directories, separately from the repository Cargo cache. With cwd `/Users/xaver/dev/qos-macos-shell`, the exact acceptance invocation was:

```sh
env HOME="$PWD/.superpowers/sdd/2026-09-15-measured-ui-components/task-3-runtime/home" \
XDG_CONFIG_HOME="$PWD/.superpowers/sdd/2026-09-15-measured-ui-components/task-3-runtime/config" \
XDG_DATA_HOME="$PWD/.superpowers/sdd/2026-09-15-measured-ui-components/task-3-runtime/data" \
XDG_CACHE_HOME="$PWD/.superpowers/sdd/2026-09-15-measured-ui-components/task-3-runtime/cache" \
XDG_STATE_HOME="$PWD/.superpowers/sdd/2026-09-15-measured-ui-components/task-3-runtime/state" \
QOS_TASK3_CAPTURE_DIR="$PWD/.superpowers/sdd/2026-09-15-measured-ui-components/task-3-captures-final" \
target/debug/deps/task3_surface-b32c6a49fb72a68a --nocapture
```

Result: **1 passed**, covering all four layouts (`task3-captures-final.log`). The same HOME/XDG environment, without QOS_TASK3_CAPTURE_DIR, ran:

```sh
target/debug/deps/scratchpad-43c30d0629dc06f2 --nocapture
```

Result: **7 passed**, 0 failed (`task3-existing-app-final.log`). This reuses the original `editor_uses_the_available_width_and_keeps_save_visible` and `typed_note_survives_save_close_and_reopen` tests, plus replay, save failure, exclusive profile lock, corrupt content, and CLI screenshot tests.

Capture root (absolute):
`/Users/xaver/dev/qos-macos-shell/.superpowers/sdd/2026-09-15-measured-ui-components/task-3-captures-final/`

| Prefix | Viewport / scale | Empty editor y range | Save y range | Persisted long bytes |
| --- | --- | --- | --- | --- |
| `wide` | 1000×700 / 1× | 122–581 | 599–639 | 2516 |
| `narrow` | 480×600 / 1× | 122–481 | 499–539 | 2518 |
| `double` | 640×480 / 2× | 179–346 | 364–404 | 2518 |
| `narrow-double` | 480×600 / 2× | 179–436 | 454–494 | 2525 |

For each prefix the exact files are `<prefix>-empty.png`, `<prefix>-saved.png`, `<prefix>-reopened.png`, `<prefix>-long-top.png`, `<prefix>-long-bottom.png`, and `<prefix>-empty.json`. Profiles are retained at `profile-<prefix>` under that root.

The harness clicks 25px above the lower editor edge, types Unicode text through the existing editor action, invokes the existing Save binding, drops/reopens the actual profile, and verifies exact persisted text. It then appends a long note through IME commit, verifies intrinsic overflow, scrolls to top then bottom with a single downward wheel, verifies the bottom equals the original viewport edge and the Save rectangle stays unchanged, saves, and reopens again to compare the complete body byte-for-byte.

Visual inspection: all four empty PNGs and all four long-bottom PNGs, plus `narrow-reopened.png`, were opened and inspected. The editor reads as a full writing surface; the final marker `Final line — the end is reachable.` is fully visible at the bottom in every layout; Save and the reminder stay visible. Header/helper copy was retained because the filled surface fixes the hierarchy without requiring an unrelated app redesign. Controller owns its separate baseline-matched 1200×800 / 480×900 / 800×900@2 final image gate.

## Remaining observations and limits

- Separate trailing-newline diagnostic for controller follow-up, not changed here: `TextInput::local().multiline(true).with_font_size(18.1).with_initial_value(&"A line of text.\n".repeat(46))` in the opt-in Scrollable, viewport 416×300, measured box height 1079px but `content_height()` after Driver drawing/clicking at (30,30) was 1056.75px (one row less). Text, width and 1× theme were unchanged; no edits occurred. Both before and after the first wheel had that content height. Exact diagnostic output is in `task3-fractional-diagnostic.log`. Ending with nonempty `Final line` produces 1079.375px drawn content and the confirmed fractional RED. The final regression uses that nonempty last line. This whole-row trailing-newline discrepancy merits a bounded separate investigation rather than an unsupported fix in Task 3.
- Captures use the shared production Driver layout/event/action path and offscreen renderer, not OS-level pointer injection into a live macOS window.
- No full unrelated UI/workspace suite was run; the brief's focused affected suites, compilation checks, and real Scratchpad persistence/capture routes are green.

## Controller archive note

The historical commands above retain their original paths. Before SDD workspace cleanup, the controller copied the final PNG/JSON captures, logs, reproducible harness and one-line app patch to `.superpowers/app-ui-2026-09-15/task3-accepted-evidence/`, preserving relative names. `manifest.json` records SHA-256 hashes for each copy. Disposable test profiles are not part of that archive. Baseline-matched controller captures remain in `task3-writing-20260916/`.
