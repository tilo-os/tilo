# App UI balance design

## Objective

Give every Tilo app a pleasant balance of useful information and breathing room with less layout work for authors, while preserving fully manual layout. Fix the concrete all-app review findings; do not redesign clear screens merely for uniformity.

## Evidence

[Complete baseline review](../audits/2026-09-15-all-app-ui-review/README.md). Review gate completed before fix delegation: 47 visual app reviews plus windowless Agent. The passing 48-journey UX bench misses these visual defects.

## Binding requirements

- Prioritize ordinary 100% text layout as well as narrow and 200% accessibility layout.
- Preserve primary actions, keyboard access, state and explicit manual dimensions. Disclosure must remain reachable by pointer and keyboard.
- Keep legacy renderer-relative font APIs compatible; theme-resolved fonts must scale once.
- Use actual text measurement for sizing and painting; do not shrink fonts to hide overlap.
- Reuse existing Card, Actions, Form, Toolbar, split and shell primitives before adding new abstractions.
- Do not globally change primitive padding or reinterpret explicit fixed sizes.
- No dependency upgrades.

## Composition rules

Primary content receives the flexible remainder. Empty optional docks start collapsed, and short request/query regions use a compact useful default with a manual resize route. Navigation lists precede secondary maintenance commands. Repeated metadata and headings collapse into one clear section header. Toolbar overflow retains every command. Related title/subtitle/action rows use a smaller gap than the separation between distinct sections. Card actions align within measured card bounds. A readable content width is appropriate for account lists/forms; image, map, document and editing canvases retain workspace.

Responsive decisions use the actual remaining content width and measured text, not window width alone. At insufficient space, use existing push/detail, wrapping or disclosure instead of overlapping controls. Keep normal-size density useful rather than multiplying every gap with text scale.

## Shared framework work

1. A public screenshot theme boundary makes custom and standard paths honor the same scale, palette and motion settings. Set renderer scale from that theme and use the renderer-aware layout path. Viewport aliases must be truthful.
2. Clarify base versus resolved text measurement/painting; repair shared empty states, editor text, grids/merge and toolbar where the review demonstrates failure.
3. Provide reusable measured composition/spacing through existing components and a small immediate-mode helper only where actual app migration proves reuse. One recipe should express related text, section spacing, a bounded header and action overflow without per-app magic offsets.
4. Demonstrate the default path in real apps, document the manual alternative, and verify interaction after reflow.

## App work packages

- Developer tools: API Tester, DB Client, Code Editor, Containers, Diff/Merge, Git, Hex, Logs, Typst.
- Browsing and preferences: Store, Settings, Themes, Feeds, Snippets, Chat, TOTP.
- Management: Firewall, Backups, Remote, VPN, Windows, OSK.
- Content and editing: Calendar, Notes, Music, PDF, Photos, Mail, Weather, Tube, Word Processor; recheck corrected captures for remaining apps.

Each package must address its marked findings, use shared fixes where available, and report every unresolved finding honestly. Shared framework changes land before dependent app changes. Implementation agents run sequentially with scoped independent reviews to avoid shared-checkout conflicts.

## Acceptance

Fresh wide 1200×800, compact 800×700, narrow 480×900 and true 200% 800×900 captures, supplemented by affected routes and short-height views. Inspect actual images, not only exit codes. Verify disclosed commands, pane switches, search/editing, selection and hit regions. Run focused tests for changed behavior and final relevant suites/UX matrix. Keep before/after evidence and a closure register for all 48 apps; state platform and harness limits. Preserve the VM initial suspended state after Linux verification.
