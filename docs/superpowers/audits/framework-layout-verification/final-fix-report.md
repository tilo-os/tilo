# Final scoped fix wave

Status: all three scoped fixes committed and verified. Controller-owned independent review, starter captures and final acceptance remain separate gates.

## Scope and environment

Work began on the existing `codex/macos-shell` checkout after base `5381612849d749c8cc0bb497c85689936f1c9fd1`. Existing unrelated dirty files were left untouched. No files under `apps/store` were modified. No push, merge or publication was performed.

Tests use the durable `.superpowers/sdd/2026-09-15-framework-layout-improvements/capture-source` verification snapshot authorized by the brief, excluding unrelated runtime commit `82afabfb7`. The final corrected snapshot contains only this fix wave's owned delta. An initial whole-file transfer of `ui/src/widget/mod.rs` also copied the unrelated `child_clip_rect` default from `82afabfb7`; the controller's byte-provenance check caught this. That method was removed from the snapshot only, leaving the working checkout untouched. Final checks were repeated because this was a concrete source mismatch. Commands run from `/Users/xaver/dev/qos-macos-shell`, with that directory's existing Cargo configuration, the snapshot manifest, and `CARGO_TARGET_DIR=/Users/xaver/dev/qos-macos-shell/target`. This verifies the framework changes in the durable capture source, not the unrelated concurrently changing runtime source in the shared checkout. The snapshot's prior framework provenance is recorded in `capture-recovery.json`; no new source manifest is claimed or fabricated here.

The test executable emits an existing macOS linker warning that `__eh_frame` exceeds the compact-unwind table's 16 MB encoding limit. This is not a Rust test failure.

## 1. Mutable resource-operation admission

Commit: `313ba237a` — `fix(ui): forward mutable admission through composition containers`.

Changed `ui/src/widget/composition_parts.rs`. The shared transparent-container macro now forwards the mutable document admission hook to children, returns the first handled result/error, and performs no second read-only fallback traversal. Regressions invoke through a direct Manual root and a nested Section → Manual root. They verify the exact invocation, operation kind, payload and supplied nested document path reach mutable admission once, with accepted and declining cases.

RED: both tests called the read-only route instead of mutable admission. GREEN: 2 passed.

## 2. Retained Manual policy-input invalidation

Commit: `a7c6da63c` — `fix(ui): invalidate retained Manual policy inputs explicitly`.

Changed `ui/src/widget/manual.rs`, `ui/src/widget/manual/tests.rs`, `ui/src/widget/composition_parts.rs`, `ui/src/widget/mod.rs`, `ui/src/app/manual_composition_tests.rs`, and `ui/MANUAL_LAYOUT.md`.

The controller approved the bounded `Manual::invalidation() -> ManualInvalidation` handle before production changes. Calling its `invalidate()` after changing captured inputs marks the region for another solve; the author must also schedule an ordinary layout frame (for example, return true from the owning widget's update). Arbitrary `Rc<Cell<_>>` changes are not observed automatically, and this handle does not wake a native window itself. Children are retained.

The policy snapshots the requested revision before evaluation and records that revision only after successful policy execution. Real `apply_layout` acknowledges the solved revision. Hypothetical probes do not acknowledge actual placement; failed solves remain pending; an invalidation during evaluation is not lost. Parts' existing application code was factored into a helper so Manual can acknowledge actual placement without a new host protocol.

The App regression paints and marks the tree clean, checks that an unchanged same-viewport layout frame performs no policy calls, changes only the captured X input and invalidates, then verifies a 105.5 px movement, matching hit/accessibility bounds, unchanged retained node IDs, text, selection and focus. It covers direct Manual, Form under Manual, and Manual under Form. Additional lifecycle coverage checks probes, failed solves and invalidation during evaluation.

RED: X delta was 0 instead of 105.5. GREEN: manual filter, 29 passed.

## 3. Automatic NumberInput sizing and stepper centering

Commit: `7bebe26b6` — `fix(ui): measure automatic NumberInput size at text scale`.

Changed `ui/src/widget/number_input.rs`, `ui/src/widget/text_input.rs`, `ui/src/app.rs` (test module only), and `ui/src/app/number_input_layout_tests.rs`.

The supplied starter screenshot was inspected. At 200% text, the fixed 140×32 footprint crops the value and the glyphs are vertically misplaced: glyph width is measured at renderer scale, while vertical centering uses an unscaled line-height constant.

The initial scaling-only proposal was superseded after RED measured `15 min` at 44.3125 px at 100%, plus 24 px embedded field padding, exceeding the existing 68 px field width. The controller rejected tuning a new fixed width around the fixture. Approved correction: retain the original scaled footprint as a minimum, use the existing renderer-aware settlement hook to measure the formatted value plus actual field padding, steppers and gaps, and center glyphs from measured renderer height. Explicit `with_size` and Manual min/max allocations retain their existing semantics. TextInput changes are limited to internal visibility of its shared font-size constant.

## Exact validation commands and logs

All log paths below are relative to `.superpowers/sdd/2026-09-15-framework-layout-improvements/final-fix-checks`.

Common prefix (run from repository root):

```sh
CARGO_TARGET_DIR=/Users/xaver/dev/qos-macos-shell/target cargo test --manifest-path .superpowers/sdd/2026-09-15-framework-layout-improvements/capture-source/Cargo.toml -p ui --lib
```

| Suffix | Log | Result |
| --- | --- | --- |
| `composition_parts::tests` | `01-admission-red.log` | 0 passed, 2 expected failures |
| `composition_parts::tests` | `02-admission-green.log` | 2 passed |
| `manual_policy_input_moves_clean` | `03-manual-red.log` | 0 passed, 1 expected geometry failure |
| `manual` | `04-manual-green.log` | 29 passed |
| `widget::number_input::tests` | `05-number-red.log` | 17 passed, 2 expected sizing/glyph failures |
| `number_input` | `05-number-measured-red.log` | 18 passed, 3 expected failures: value fit, glyph line box, retained App width |
| `number_input` | `06-number-green.log` | 21 passed |
| no filter | `09-full-ui.log` | 3041 passed, 2 ignored; superseded source provenance (extra unrelated default hook) |
| no filter | `11-isolated-full-ui.log` | **3041 passed, 0 failed, 2 ignored** on corrected exact isolated source |

Each behavior was reported to the controller after its scoped commit; production changes for the next behavior waited for the controller's user-review interval clearance. Starter execution and final visual recaptures remain controller-owned.

Additional exact checks:

```sh
CARGO_TARGET_DIR=/Users/xaver/dev/qos-macos-shell/target cargo clippy --manifest-path .superpowers/sdd/2026-09-15-framework-layout-improvements/capture-source/Cargo.toml -p ui --lib -- -D warnings
rustfmt --edition 2024 --check --config skip_children=true ui/src/widget/composition_parts.rs ui/src/widget/manual.rs ui/src/widget/manual/tests.rs ui/src/widget/mod.rs ui/src/app/manual_composition_tests.rs ui/src/widget/number_input.rs ui/src/widget/text_input.rs ui/src/app.rs ui/src/app/number_input_layout_tests.rs
```

Both initially completed with exit 0 (`07-clippy.log`, `08-fmt.log`). `07-clippy.log` shares the superseded extra-hook provenance; final corrected isolated clippy completed with exit 0 in `10-isolated-clippy.log`. The same rustfmt command, run from the corrected `capture-source` directory instead of the repository root, completed with exit 0 in `12-isolated-fmt.log`. `git diff --check` on owned paths also passed. Clippy checks the UI library, not unrelated workspace crates or every integration-test target.

NumberInput compatibility: automatic minimum remains 140×32 at 100% and 280×64 at 200%, with width enlarged using measured committed text and rounded upward to avoid subpixel clipping under host pixel rounding. The existing target-height minimum is preserved. Explicit `with_size` disables automatic measurements/scaling; exact Manual min/max allocations still win. The embedded field receives the final allocation and retains typing/commit behavior. The real App accessibility SetValue path schedules a layout frame and expands the existing control for a longer committed value, without replacing its node or focus. Font size comes from TextInput's existing internal constant; drawing uses actual renderer-measured glyph height for centering. Theme changes use the existing host rebuild contract.

The measured automatic width does not impose a maximum-width/wrapping policy for arbitrarily long numeric values or suffixes. Authors may still choose explicit allocations (with the existing internal horizontal scrolling), larger regions, or their own surrounding layout policy. Starter at 340 px/200% has 292 px available, and the focused fixture fits that width. Actual generated-starter rendering remains controller validation.

## Self-review and remaining handoff

Reviewed owned diffs for transparent forwarding, retained placement revision ordering, exact Manual min/max compatibility, NumberInput scale ownership, action-driven invalidation, and preservation of unrelated files. No production App lifecycle rewrite, new general solver or Store migration was introduced. No scoped paths had unrelated dirty hunks at start; whole-file staging was restricted to those owned paths. The pre-existing dirty Column formatting was never staged or changed.

Controller owns the independent final scoped review, final source manifest, starter tests/visual recaptures and user acceptance. This report does not claim those gates are complete.

### Verification source correction

The hook entered the snapshot when the Manual production files were transferred. Logs `01-admission-red`, `02-admission-green`, `03-manual-red`, and `05-number-red` precede that transfer. Logs `04-manual-green`, `05-number-measured-red`, `06-number-green`, `07-clippy`, and `09-full-ui` used the extra unrelated default hook; no implementation or test relies on it. Those logs remain the actual observed TDD history and are not relabeled as exact isolated-source evidence. The repeated final clippy/full UI run on the corrected snapshot is authoritative for final verification. No source manifests were generated by this agent; the controller performs that comparison separately. Documentation was also synchronized to the snapshot.

Controller reports its final source verifier passed after correction: isolated composed tree `eeb640b08d05b44f6c4748d16f26cb7796a56a87`, 39 framework files matching exact composed bytes and 26 Store files unchanged in both root and snapshot. The controller-owned record is `final-source-evidence.json`.

Final verification completed: corrected isolated UI library test run exited 0 with **3041 passed, 0 failed, 2 ignored**; corrected isolated clippy and rustfmt both exited 0. Working-tree owned source files have no uncommitted changes. Final commits are `313ba237a`, `a7c6da63c`, and `7bebe26b6`.
