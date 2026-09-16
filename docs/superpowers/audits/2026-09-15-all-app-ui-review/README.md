# All-app UI review — 2026-09-15

## Review gate: complete before implementation

48 apps inventoried; 47 windowed apps visually reviewed using fresh current-source captures, plus the windowless Agent source review. Three review reports below mark every app and identify severity, source, ownership and verification. No implementation worker was dispatched before this gate.

- [Agent through Feeds](review-1.md)
- [Files through Scratchpad, including OSK](review-2.md)
- [Settings through Word Processor, including Windows](review-3.md)

Baseline source: `47fc2425a643a07ebf9c4f127fb5579ba42f77a7` plus the recorded pre-existing working diff. Capture commands, binary hashes, Linux source hashes and images live in `.superpowers/app-ui-2026-09-15/`. Native macOS captures cover 45 apps; fresh Fedora 44 ARM64 captures cover OSK and Windows. 228 images include main-route supplements. Reviewers inspected their listed images individually.

## What the benchmark says

Fresh desktop UX matrix: **48 passed, 0 failed, 0 errors, no coverage gaps reported**. Evidence: `.superpowers/app-ui-2026-09-15/bench-baseline/summary.json`. These journeys do not establish pleasant density, visual grouping, all-app coverage, or all pointer/keyboard routes. The visual findings remain open despite this result.

## Baseline findings at ordinary text size

- API Tester: maintenance buttons bury saved requests; a fixed split leaves unused request space above results.
- DB Client: short queries reserve a large empty editor region.
- Code Editor: empty Problems dock consumes 160 pixels by default.
- Containers and Diff/Merge: intermediate widths keep more panes/actions than fit.
- Store: actions exceed cards; redundant headings push browsing halfway down a narrow screen.
- Firewall and Windows: permanent administration controls displace the primary list.
- Settings, Themes and VPN: long preambles or fixed preview/header space delay controls.
- Chat, Backups and Feeds: related text lacks a clean measured gap or is separated too far.
- Git, PDF and Snippets: narrow controls overlap. Typst loses its medium-width Export route.
- Scratchpad: the primary editor collapses into a strip despite a large empty body.

## Framework causes and limits

Custom drawing combines already-scaled theme typography with renderer scaling, and app rectangles often remain fixed. Custom screenshot handlers sometimes ignore requested scale, hiding defects. Shared toolbar sizing estimates characters rather than measuring them. Nested default padding does not express the difference between related rows and separate sections. Automatic card/form APIs exist, but many apps still author geometry directly.

Capture fidelity defects are marked separately from actual live layout defects. Browser page content is a placeholder in this harness; its chrome was reviewed, not a real web page. Canvas/editor whitespace and OSK overlay remainder are not automatically defects. Apps without established defects should retain their useful composition.

## Closure policy

A finding closes only with source changes or an evidence-based ruling, targeted behavior verification, and a fresh relevant screenshot. A normal-size improvement cannot be replaced by a large-text-only fix. Collapsed tools must retain reachable actions. Current review files preserve the baseline; implementation results belong in a separate closure register.

[Controller addenda](controller-addenda.md) record additional verified findings and ownership rulings.

## Capture prerequisite implemented

Commits `ef5f08db5` and `4f0cc6b27` unify screenshot theme/scale/measurement across18 custom handlers and correct Files viewport handling. Independent code review passes after an Imager compatibility fix. Validation:18-package check, binary tests, UI3064passed/2ignored, focused fix tests. Controller built45native windowed apps and captured90images across18affectedapps with matching dimensions. This closes capture fidelity, not the newly exposed layout defects. [Corrected visual review, group2](corrected-review-2.md) records those findings.

[Corrected visual review, group1](corrected-review-1.md) completes the18-app matched100%/200% visual audit. Every standard capture route now visibly applies scale; app layout findings remain separately open.

## Implementation progress — 2026-09-16

The [closure register](closure-register.md) is the current status; the reviews above preserve baseline findings. [Accepted implementation evidence](implementation/README.md) retains the test reports and independent review results.

- Store's marked card containment, header density, artwork and scaled-filter findings are verified. Settings' excessive preamble and VPN's excess compact-header space are also verified.
- Shared Card, ContentCard and CardGrid now preserve their natural minimum height through intrinsic scrolling stacks. This fixes Store section overlap and Settings Region card overlap while retaining authored fixed rows and buttons. Commits `cec5c10c7` and `93b56dc1f` passed scoped spec and quality review, including real host Enter activation after scrolling and tabbing.
- Themes now opens narrow editable controls immediately and exposes a selected Customize/Full preview switch; the full specimen fits its scroll extent. Its scoped review passes. Settings wide form balance and the remaining apps/large-text findings remain assigned; this task is not complete.
- Shared verification: 3,070 UI library tests passed and 294 integration tests passed. Three integration failures also reproduce without the card changes: bound callback admission, an existing shell color audit, and typography gallery goldens. They are recorded as baseline failures, not silently counted as passing.
- Shared text and EmptyState sizing is implemented in `17110d403` and passed independent specification and quality review. Focused wrapping, scale, pointer, and host keyboard checks pass; fresh VPN screenshots show bounded empty-state text at normal, narrow, and 200% sizes. The separate enlarged Tor header capsule remains open. The task's broader UI run passed 3,090 library tests with one failure in concurrent Chromium changes and two ignored tests; integration tests were not reached.

Shared editor fixes `3f13fb4eb` and `83c269dac` passed independent review, including real wrapper scale propagation and CRDT undo/redo invalidation. CodeEditor23/TextInput99 focused module tests and the UI check pass. Root inspected9fresh post-fix Notes/Calendar/Typst images. Notes styled-glyph overdraw, Calendar scaled control heights and Typst toolbar/status bars remain separately assigned; these apps are not yet fully closed.

Final all-app captures, the final UX matrix, Linux checks and whole-change review remain pending.
