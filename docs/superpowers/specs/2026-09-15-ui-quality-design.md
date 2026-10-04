# Tilo pleasant layout with minimal effort: proposed architecture and rollout

Status: implementation underway. Intrinsic spacing, host grid geometry, resize settlement, Text sizing, and content-aware cards/actions are implemented and independently reviewed. Automatic forms and guided overrides are implemented and independently reviewed; full manual measure/place, inspection, scaffolding, and the final audit remain required. See the framework layout implementation plan and progress audit.

## Goal and priorities

Make a pleasant, balanced, adaptive layout the result of very little application code. Developers describe content and grouping; Tilo supplies a carefully designed arrangement. Fully manual layout remains a first-class option for any screen, including ordinary forms and app chrome, not only specialized canvases.

Priority order:

1. Pleasant composition: spacing, proportions, alignment, readable widths, hierarchy, and stable adaptation.
2. Minimal developer effort to create and maintain that composition.
3. Fully manual control and selective overrides without abandoning shared widgets.
4. Supporting checks for functional defects, accessibility, and responsiveness.

This supersedes the earlier recommendation to seal standard regions, remove low-level exports, or restrict manual UI to custom-content adapters. Choosing manual layout needs no exception approval.

## Architecture: three composable levels on one engine

| Level | Developer provides | Framework provides |
| --- | --- | --- |
| Automatic | Content, order, meaningful groups, actions, and optional pattern choice | Spacing, alignment, widths, proportions, scrolling, toolbar overflow, adaptive arrangement |
| Guided | Automatic layout with selected overrides | Automatic decisions for everything not overridden |
| Manual | Explicit rows/columns/grids, dimensions, spacing, alignment, breakpoints, or rectangles | Standard widgets, theme, text measurement, interaction, accessibility, rendering services |

Each region can choose its level independently. An automatic page can contain a manual region; a manually composed screen can contain an automatic form. All levels share retained widgets, Taffy, and Vello. Manual layout must not require custom painting or reimplementation of standard controls.

Automatic means deterministic layout policies, not runtime AI guessing the interface. Authors still communicate order and meaningful grouping; the framework cannot infer information architecture from arbitrary fields.

## Existing foundations

- `ui/HIG.md` already describes spacing, hierarchy, visual craft, and adaptive patterns. Reconcile its enforcement language with first-class manual authoring.
- `ui/src/tokens/` and `ui/src/chrome/shell.rs` provide coordinated visual roles.
- `ui/src/widget/form.rs` already composes labels and controls.
- `ui/src/widget/column.rs` and `row.rs` expose padding, filling, wrapping, and intrinsic-size choices that currently require developer knowledge.
- `ui/src/widget/nav_shell.rs`, `nav_split.rs`, and `app_shell.rs` contain adaptive behavior with distinct purposes. Classify them before selecting the default public patterns.
- `crates/sdk/tilo-app/src/lib.rs` exposes the full toolkit. Preserve its capabilities while making the automatic path easier to discover.
- `ui/src/headless.rs` and `crates/dev/tilo-dev/src/main.rs` provide inspection, screenshots, scenarios, and responsive acceptance machinery.
- `ui/tests/design_token_audit.rs` checks some styling literals. Distinguish standard framework styling from intentional manual choices and domain geometry; a literal alone does not establish poor layout.

## 1. Encode pleasant composition in reusable policies

Start with form pages, collection/detail pages, and document workspaces. Avoid a universal layout that turns every screen into a grid of cards.

Automatic policies should handle:

- **Spacing rhythm:** label/control pairs are close; independent fields have more separation; sections have stronger separation.
- **Alignment:** related content shares edges and label/value columns align consistently.
- **Proportions:** forms and reading areas have comfortable maximum widths; workspaces can expand; auxiliary regions yield before primary content becomes cramped.
- **Density:** a few coordinated presets adjust the composition without requiring independent margin tuning.
- **Hierarchy:** typography and whitespace distinguish titles, sections, body text, and supporting text before adding borders or cards.
- **Content-driven dimensions:** text and controls grow where appropriate instead of being clipped by fixed heights.
- **Adaptation:** labels move above controls, collections reduce columns, and overview/detail layouts become separate screens when needed.
- **Action placement:** action groups align consistently and secondary actions overflow when necessary.
- **Stability:** small content changes do not reorder controls or make the whole screen jump. Test around breakpoints and preserve focus and selection.

Policies use measured content, theme metrics, and available logical space. Preserve source order unless the developer specifies an alternate arrangement. Inspection should explain decisions, such as why a form switched to stacked labels.

Pleasantness is a designed and reviewed property of these policies, not a numerical beauty score. Compare candidates side by side with real content at multiple sizes.

## 2. Remove layout bookkeeping from the easy path

Automatic pages own outer gutters and viewport filling. Internal groups normally hug content. Scroll regions own intrinsic content measurement so developers do not need to remember `with_intrinsic_height()`.

Separate wrapping groups from single-line action rows. Avoid accumulating page padding on every nested container. Choose widths by pattern rather than assigning one maximum width to forms, tables, and canvases alike.

Keep existing primitive behavior compatible while introducing clearer recommended APIs; do not silently change `Column` or `Row` defaults across existing apps.

Proposed authoring vocabulary, not an existing Rust API:

```text
FormPage("Account")
  Section("Profile")
    Field("Name", name_binding)
    Field("Email", email_binding)
  Actions(Save)
```

This should produce a complete composition with no pixel values, nested-padding fixes, or handwritten narrow-window branch. Changing density or content width should not require taking over the entire screen.

## 3. Make overrides predictable

Precedence: explicit manual value, then region override, then pattern default, then theme default. An overridden property must not be silently replaced by an automatic policy.

Guided overrides include content width, density, label placement, column count, section spacing, and navigation arrangement. Overriding one property leaves the others automatic. Removing an override restores the default. Inspection identifies inherited and authored values.

Conflicting manual constraints still require resolution: report what cannot be satisfied instead of pretending the requested geometry was honored. Composition changes should preserve component identity and application state rather than regenerate the app.

## 4. Provide genuinely manual layout

Manual authors can:

- construct rows, columns, grids, and nested regions;
- choose numerical or token-based dimensions, gaps, margins, and alignment;
- author responsive rules or intentionally fixed compositions;
- measure a standard child and place it in a chosen rectangle;
- use theme defaults or deliberately override them;
- draw custom content when desired, without needing custom drawing merely to position standard widgets.

Provide shared child measurement and placement services so authors do not have to recreate node bookkeeping. When manual placement bypasses Taffy inside a region, the resulting rectangles must feed the same drawing, hit-testing, focus, and accessibility geometry. Coordinates remain logical unless explicitly transformed.

Manual layouts receive useful diagnostics for clipping or broken interaction, but are not rejected merely for custom spacing or departure from an automatic pattern. A fixed composition must define behavior below its supported minimum, such as scrolling or a minimum window size.

Code-first manual authoring is required. A drag-and-drop visual editor is a separate possible project, not assumed by this requirement. A lightweight preview and inspector benefit all three levels.

## 5. Optimize the iteration loop

Extend existing preview and inspection tools to display representative widths with realistic content. Show effective spacing, alignment edges, content width, scroll ownership, source location, and override origin.

Supply a small set of polished presets and complete examples. Developers should compare arrangements quickly, then tune only the decisions they care about. Preserve preview state where feasible. Rendering and policy selection remain deterministic and need no network or model call.

## 6. Keep checks supportive and concrete

Reuse existing `tilo-dev check`, inspection, scenarios, and responsive captures. Detect actual clipping, unreachable actions, missing accessible names, broken focus, and invalid standard color pairs. Exercise long labels, large text, narrow windows, supported themes, and locales.

Include effective clipping and intentional overflow in inspection so scroll content and overlays are not misclassified. Distinguish an automatic-policy defect from a manual constraint causing a problem, with actionable diagnostics for either.

Share functional interaction and accessibility contracts across all layout modes. Preferred spacing is not a release-blocking correctness rule. Human review judges hierarchy, balance, task usefulness, and visual craft. AI or automated screenshot review can suggest issues but cannot prove pleasantness.

## Ordered rollout

### Phase 1 — Prototype composition and establish an effort baseline

Build realistic fixtures for small/long forms, collection/detail, and document workspaces. Record current authoring time, layout decisions, and iterations. Compare a small number of proposed spacing, width, and adaptation policies visually.

Likely areas: `ui/src/widget/form.rs`, `column.rs`, `row.rs`, `container.rs`, `scrollable.rs`, tokens, and preview fixtures. Keep prototypes separate from existing defaults.

Gate: agree on pleasant wide and narrow compositions with real content. Certification is supporting work, not the first product deliverable.

### Phase 2 — Deliver automatic form layout

Build form-page composition, field alignment, section rhythm, width, actions, and scrolling using existing controls and lifecycle.

Gate: a useful form needs no pixel values or manual responsive branch, handles long labels and 200% text, and is visually balanced. Adding a field does not require retuning geometry.

### Phase 3 — Deliver guided overrides and full manual placement

Add local overrides, reset-to-default behavior, and standard-child measurement/placement. Verify nesting automatic and manual regions in both directions.

Gate: rebuild the form fully manually using standard controls. Authored geometry is respected, hit-testing/accessibility match, and a local override does not disable automatic behavior elsewhere. This phase is required before the authoring model is considered complete.

### Phase 4 — Extend to adaptive workspaces and inspection

Apply the model to collection/detail and document workspace patterns using existing shell widgets. Extend developer tools with source-linked inspection and multi-width previews.

Gate: state/focus survive resize, navigation follows region purpose, and developers can discover and alter a decision without tracing Taffy internals.

### Phase 5 — Adopt through templates and targeted checks

Update `tilo-dev new`, examples, and serialized UI construction to recommend automatic layout while clearly documenting guided/manual use. Add layout-policy regressions and functional checks. Preserve the low-level SDK and migrate incrementally.

Gate: new apps start with pleasant defaults; existing apps remain compatible; automatic and manual pilots pass shared functional checks.

## Success criteria

Primary measure: time to a visually accepted, adaptive screen. Compare against the current toolkit on equivalent tasks with developers of comparable experience. Choose a numerical improvement target after collecting the baseline.

Also measure:

- layout-specific decisions and follow-up adjustments per screen;
- absence of pixel values and breakpoint branches in the automatic form pilot;
- whether adding fields, changing text, or resizing requires extra layout code;
- human assessment of spacing, alignment, hierarchy, and proportions;
- local overrides without rewriting surrounding composition;
- practical fully manual authoring with standard controls;
- preserved focus, selection, and correct input/accessibility geometry.

Low line count alone is not success. The result must look and behave well, and the manual path must be practical.

## Research informing the proposal

- [Libadwaita adaptive layouts](https://gnome.pages.gitlab.gnome.org/libadwaita/doc/main/adaptive-layouts.html) demonstrates reusable width constraints, wrapping, adaptive dialogs, and split behaviors. Borrow component-level policies that eliminate repeated layout decisions.
- [React Aria quality](https://react-aria.adobe.com/quality) demonstrates shared interaction and accessibility with visual customization. Preserve those services under manual layout.
- [GOV.UK Design System accessibility](https://design-system.service.gov.uk/accessibility/) notes that components alone do not make a service accessible. Evaluate actual screens and tasks.
- [W3C evaluation-tool guidance](https://www.w3.org/WAI/test-evaluate/tools/selecting/) explains automated evaluation limits. Keep aesthetic and workflow judgments in human evaluation.

The three-level model, override rules, and rollout are proposed engineering choices for Tilo, not claims that the cited systems implement this exact architecture.

## Implementation status — 2026-09-15

The approved seven-task [implementation plan](../plans/2026-09-15-framework-layout-improvements.md) is complete: reliable primitives, shared resize/measurement lifecycle, automatic cards/forms, guided overrides, first-class retained manual composition, inspection, examples and starter adoption. The broader rollout directions and human productivity study above remain future work. No elapsed authoring-time improvement or human acceptance study is claimed.
