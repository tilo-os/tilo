# Desktop UX review and bench acceptance cases

Status: initial review; bench implementation awaits design approval. No claim of
complete UX acceptance or a reproduced hardware scroll fix.

## Evidence collected

- Inspected current shared driver, scrollable, macOS event conversion, shell
  overview input, and native runner source.
- Ran the existing `target/debug/shell` with isolated profile and Files
  directories under `/tmp/qos-ux-review-20260915`, using `--shots`.
- Process exited successfully and produced `desktop.png`, visually inspected at
  1280 × 800. This executable was already built; its source revision was not
  established. The capture is evidence about that executable only.
- Existing unrelated modifications were left in place.

## User goal alignment

The captured fresh Home screen presents Discuss, Notes, Tasks, Plan a meeting,
Invite people, and Catch up. The heading is “Make something together”. Opening
an arbitrary app is explained in a small bottom shortcut hint. For a person
trying to open a file or launch an app, the primary choices emphasize a different
goal. This is an information hierarchy concern, not a measured failure of those
controls. A full journey must establish whether the visible launcher is easy to
identify and how many interactions it takes to reach the intended content.

The Home wheel occupies substantial left-hand space. Small secondary text and
the patterned background warrant contrast and readability measurements; visual
inspection alone does not establish a contrast ratio. A screenshot cannot prove
click cost, keyboard reachability, successful launch, or scrolling behavior.

## Input findings requiring regression cases

1. **Nested scrolling:** `ui/src/widget/scrollable.rs::handle_event` handles
   MouseWheel and returns before its child dispatch. For nested retained
   Scrollable widgets, the outer instance intercepts wheel delivery, including
   when its own offset does not change. Test a long inner region inside both a
   scrollable and a non-overflowing outer region. Require inner movement first,
   then explicitly specified boundary handoff. Source finding; no runtime
   reproduction recorded yet.
2. **Display scale:** macOS winit converts precise logical deltas into physical
   PixelDelta using the view scale. The shared Scrollable converts PixelDelta
   directly to offsets, whereas some shell regions divide by scale. The
   headless driver always dispatches with scale 1.0. Test equivalent physical
   inputs at 1× and 2× and compare logical displacement. Trace the native path
   before declaring the observed user bug reproduced.
3. **Driver pointer contract:** `Driver::scroll` says no cursor placement is
   needed, but Scrollable rejects the event outside its content viewport. Every
   bench scroll must establish and record pointer location explicitly.
4. **Input fidelity:** the convenience scroll driver and shell socket command
   generate LineDelta with phase Moved. PixelDelta and Started/Ended/Cancelled
   need explicit cases. Native host gesture arbitration runs before root widget
   dispatch; a widget-only result does not certify that arbitration.

## Recommended bench design

Extend the existing driver and inspection infrastructure. A separate screenshot
script would miss input semantics; an action-only runner would bypass visible
navigation. The integrated approach reuses actions as stable operation identities
and outcome checks, while visible control input supplies usability evidence.

Each scenario specifies the user's goal, isolated starting data, visible route,
completion predicate, interaction budget, and checkpoint expectations. Record
pointer clicks, keyboard actions, text entry, scroll gestures, and direct action
invocations separately. Direct invocations do not count as zero-click UX success.
Use fixture setup outside the measured portion. Evaluate outcomes after bounded
state-based waits; a successful input dispatch is not completion.

### Initial journeys

- Fresh desktop → open an app → verify its usable content is visible.
- Files → locate a seeded file → open it → verify the intended document.
- Workspace A → Workspace B → return → verify identity and preserved content.
- Desktop → setting → change → verify visible state and persistence where
  promised by that setting.

Budgets are explicit product decisions per route, recorded alongside the
scenario. Do not set the budget automatically to the observed click count.

### Measurements

- Geometry: visible clipped bounds, target sizes, unintended overlap, declared
  alignment, and relationships between label/control, field, and section gaps.
  Evaluate only explicitly related elements; intentional overlays are valid.
- Contrast: resolved foreground/background pairs and compositing context, tied
  to visible elements. Report unresolved gradients/images/unknown paint as
  unmeasured. Never substitute theme-token checks for actual rendered coverage.
- Scrolling: cursor location, target region, before/after offset and bounds,
  direction, maximum range, nested handoff, content under the pointer after
  movement, and unchanged unrelated workspace/strip state.
- Journeys: completion, click and key budgets, unreachable controls, unexpected
  navigation, and recoverability using Back/Escape where applicable.

### Execution and reports

Run the real shell tree with deterministic fixtures through the existing driver,
then add native macOS input acceptance for scale and gesture arbitration. The
macOS runner currently exposes `--shots` but does not connect the documented
Linux test socket. This requires an explicit supported driving seam.

Exercise laptop and narrow windows, light/dark themes, enlarged text, and 1×/2×
display scales. Each run records supported coverage; missing required evidence
fails acceptance rather than silently passing or disappearing as a skip.

Export JSON measurements plus a browsable report with screenshots, annotated
failures, input trace, expected/observed values, and executable/build identity.
Keep subjective clarity and visual balance as review findings alongside
mechanical checks. Automated measurements alone do not establish pleasantness.

## Completion evidence still required

- Executable bench integrated with the shell/action infrastructure.
- Tests proving deliberate geometry, contrast, budget, and scroll defects fail.
- Driven results for all initial journeys, with real completion assertions.
- Native macOS scale and input evidence, including the reported scroll issue.
- Inspected output report and documented repeatable commands.

None of these implementation deliverables is claimed complete by this audit.
