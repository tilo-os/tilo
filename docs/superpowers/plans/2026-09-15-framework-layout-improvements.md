# Pleasant framework layout implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use subagent-driven-development to implement and independently review each task. Complete one reviewable framework change at a time.

**Goal:** Improve automatic layout and manual composition in the Tilo framework, demonstrate their effect on the unchanged Store, and notify the user after every completed change for review.

**Architecture:** Repair the shared retained-layout contracts before adding higher-level composition. Keep Taffy, Vello, and existing manual APIs. Add automatic patterns and selective overrides alongside them; do not rewrite Store code to manufacture an improvement.

**Tech stack:** Rust, Taffy 0.10.1 (verified in the workspace manifest and lockfile), Vello, existing headless inspection and screenshots.

**Spec:** `docs/superpowers/specs/2026-09-15-ui-quality-design.md`

**Evidence:** `docs/superpowers/audits/2026-09-15-store-layout-framework-review.md`

## Global Constraints

- Do not modify any file under `apps/store/`.
- Preserve fully manual layout and standard widget behavior; do not seal off raw APIs or impose a strict design profile.
- Each task needs focused red/green behavioral tests, independent review, and an unchanged-Store capture or explicit evidence that the task has no visible effect there.
- Notify the user after each completed change with before/after evidence. Give their review time to arrive before beginning the next behavioral change; independent investigation and preparation can continue.
- Do not push, merge, or publish as part of this execution. Local commits are review checkpoints.
- Keep geometry available before paint and consistent for rendering, input, focus, and accessibility.
- No app-specific heuristics in the framework: no Store identifiers, catalogue names, or screenshot-specific geometry rules.
- Existing fixed sizing remains an explicit manual option. Automatic composition uses measured content and predictable overrides.

## Task 1: Honor gaps in content-sized columns

**Files:** `ui/src/widget/column.rs`; focused layout regression tests in that file or an existing registered integration-test module; `ui/src/widget/card.rs` only if its framework-owned gap workaround needs adjustment to preserve intended spacing.

**Consumes:** `Column::with_intrinsic_height`, `Column::with_gap`, retained child/node mapping.
**Produces:** a stable gap contract across intrinsic and filling column modes without losing scroll extent, child identities, or explicit margins.

- [x] Add a real layout regression: an intrinsic, unpadded column containing two 10-pixel children with a 20-pixel requested gap has child y offsets 0 and 30 and content height 40. Observe the existing failure (second child currently at 10).
- [x] Cover zero gap, default theme gap, three children, explicit margins/padding, and content taller than a scroll viewport. Derive expected geometry by hand rather than calling production helpers to compute expected values.
- [x] Implement the smallest coherent fix. Preserve correct intrinsic growth; do not casually restore flex behavior that truncates scroll content. Keep host widget/node traversal aligned and repeated layout idempotent. Explain any chosen Taffy workaround in the code.
- [x] Run focused tests, then relevant Column/Card/Scrollable layout regressions. Use `cargo test -p ui --lib widget::column::tests -- --nocapture` and the directly affected additional filters; register any new integration module in `ui/tests/it.rs`.
- [x] Self-review and commit task-owned files only. Report exact red/green commands and results, changed behavior, and remaining limitations.
- [x] Controller: independently review the commit, render Store wide/narrow/detail captures, compare with baseline, and notify user before Task 2.

## Task 2: Bring CardGrid children into the host layout lifecycle

**Files:** `ui/src/widget/card.rs`, targeted `ui/src/app.rs` tests only if needed for real focus traversal; registered framework tests.

**Consumes:** Task 1 geometry contract and current Widget/Taffy traversal.
**Produces:** CardGrid children with valid geometry and host identities after layout, before paint; no hidden inner tree built every draw.

- [x] Reproduce a grid of standard buttons missing from App focus traversal and having zero rectangles before draw.
- [x] Implement grid layout using the shared host tree and intrinsic measurement. Preserve supported explicit minimum widths, gap, and fixed-height API behavior; do not change Store.
- [x] Verify all card buttons appear in focus order, input reaches the intended button, before/after-draw rectangles are identical, and reflow updates row count/height when width changes.
- [x] Check retained structure changes, long contents, and scroll clipping against existing CardGrid tests. Ensure painting is not rebuilding layout.
- [x] Commit, independently review, render unchanged Store captures, and notify user.

## Task 3: Make viewport-dependent composition settle on resize

**Files:** `ui/src/app.rs`, `ui/src/widget/mod.rs` only if a general lifecycle contract is required, shared headless layout runner/tests.

**Consumes:** retained composition, host layout with viewport dimensions, and Task 2 host-tree semantics.
**Produces:** a generic resize/composition lifecycle that handles widgets deriving their composition from allocated geometry without per-app breakpoint invalidation patches.

- [x] Use a framework test widget with composition changes at more than one threshold; compare fresh and resized output at 1200, 920, and 600 logical pixels. Current Store evidence is 1200-to-920 retaining a 1120-pixel grid.
- [x] Define a bounded settle/recomposition mechanism or tracked viewport contract that fixes existing geometry-dependent roots. Explain how it avoids unbounded loops and needless recomposition on scroll/paint-only frames.
- [x] Preserve focus/selection and scroll state when resize changes topology. Exercise live-runner-equivalent and headless paths, not only fresh construction.
- [x] Commit, review, and capture unchanged Store at fresh 920 and resized 1200-to-920. Notify user with a visible comparison.

## Task 4: Make text sizing and shared capture configuration coherent

**Files:** `ui/src/widget/text.rs`, `ui/src/text_render.rs`, shared offscreen/headless framework helpers and registered tests.

**Consumes:** Text's theme-resolved sizing and TextRenderer scaling/measurement.
**Produces:** consistent theme-scaled Text regardless of an app-local renderer starting at its default scale; exact-once scaling with the live renderer; shared configured preview/capture entry points.

- [x] Reproduce Text with a 14-pixel scaled base and theme scale 2 drawing at 14 instead of 28 when the renderer remains at scale 1.
- [x] Fix the ownership of resolved versus renderer-relative text sizes in framework APIs. Do not add global mutable theme state or silently double-scale custom canvas text.
- [x] Verify measure/draw consistency, base and semantic roles, explicit unscaled text, plugins/custom text engines, and scales 1, 1.25, 1.5, 2.
- [x] Verify the existing Store screenshot path renders genuinely larger Text with its existing 200% setting without changing Store code. Report any separately scoped non-Text control behavior that remains.
- [x] Commit, review, capture Store normal/large text, and notify user.

## Task 5: Add content-aware card composition with explicit manual sizing

**Files:** `ui/src/widget/card.rs`, `ui/src/widget/button.rs` for measured content-sizing/centering prerequisites, focused companion module if needed, widget exports, framework fixtures/tests.

**Consumes:** host-integrated CardGrid and measured content.
**Produces:** automatic row sizing and reusable identity/body/action-footer composition, with an explicit fixed-height/manual alternative.

- [x] Add behavior tests for unequal description lengths, row height matching the largest content minimum, footer alignment, and expanded text without clipping.
- [x] Correct the demonstrated Button fit-content width estimate and scaled text centering; provide true intrinsic standard-action sizing for automatic patterns while preserving explicit fixed dimensions. Cover long labels/icons at 200% text.
- [x] Introduce the automatic API alongside compatible fixed sizing. Document precisely whether existing methods express a fixed size or minimum; do not silently reinterpret explicit manual contracts.
- [x] Verify zero/one/many cards, reflow, and intrinsic content height under scrolling.
- [x] Demonstrate the new automatic composition with a framework fixture. Capture unchanged Store and distinguish improvements from capabilities that require later app adoption.
- [x] Commit, review, and notify user.

## Task 6: Add automatic page/form composition and local overrides

**Files:** new focused modules under `ui/src/widget/`, widget exports, `ui/src/widget/form.rs`, framework examples/tests and UI documentation.

**Consumes:** reliable column/grid/scroll sizing, tokens, standard controls.
**Produces:** page-owned gutters/readable width, intrinsic sections, field alignment and action regions; density/width/spacing overrides that affect only the requested property.

- [x] Build a realistic framework fixture with name/email fields, multiple sections, supporting text, validation, and actions; keep all Store files unchanged.
- [x] Add geometry tests for common alignment edges, label/control separation, readable maximum width, narrow stacking, natural scroll growth, and long text at 200%.
- [x] Implement a small discoverable API with content/order/grouping as required inputs. Use deterministic policies, semantic typography, and shared controls, not runtime AI or app-specific inference.
- [x] Support selective overrides and resetting them; precedence is explicit manual value, region override, pattern default, theme default. A local override must not disable adaptation elsewhere.
- [x] Produce an automatic fixture requiring no pixel sizes or authored breakpoint branch, plus a guided variant. Measure authoring decisions versus the existing primitive composition.
- [x] Commit, review, capture fixtures and unchanged Store, and notify user with honest adoption boundaries.

## Task 7: Complete manual composition, inspection, examples, and final audit

**Files:** focused shared measurement/placement API and tests, headless inspection, UI examples/docs; `crates/dev/tilo-dev` scaffolding only after the new APIs are stable.

**Consumes:** Tasks 1–6 and the existing manual Widget API.
**Produces:** documented standard-child measurement/placement, mixed automatic/manual layouts, helpful preview inspection, and examples/scaffolds favoring the low-effort path.

- [x] Build the same form manually with standard widgets, exact logical rectangles, and explicit responsive rules. Verify final rectangles drive drawing, hit-testing, focus, and accessibility.
- [x] Test an automatic page containing a manual region and a manual page containing an automatic form. Preserve state/focus when a local override or viewport changes.
- [x] Expose layout decision/override origin and effective geometry in inspection without duplicating the existing tooling. Provide multi-width fixture captures and practical docs for all three modes.
- [x] Update new-app examples/scaffolds only where they can consume the new API correctly; keep existing low-level exports and compatibility.
- [x] Run affected suites and one broad final independent review. Verify Store source hashes against the initial manifest, aggregate before/after evidence for every change, and notify user.
- [x] Audit the actual objective against the spec and all seven tasks. Do not claim completion while any required implementation, review, verification, or user-requested capture remains.

## Final acceptance

All seven tasks complete through `7bebe26b6`.
