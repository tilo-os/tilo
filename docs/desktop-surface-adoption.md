# Desktop shared-component adoption

Inspected base: c3a50df (2026-10-06); design-standard merge #3025 and subsequent main 18c4f1e4 inspected for HIG changes and overlap. This inventory tracks targeted increments;
an entry is not verified merely because source inspection found a shared painter.

## Ownership after the split

- tilo-ui (ui-core) owns controls, typography, metrics, materials, focus painting
  and reusable chrome. ButtonFace is the retained Button's canvas-host face.
- tilo-desktop-surface owns portable notification/history and drill-in surfaces.
- tilo-desktop owns topology composition, top bar, overview, launchpad and workspace
  lenses. It reexports portable surfaces rather than duplicating them.
- shell owns platform effects, input routing, overlays and compositor integration.

Use theme roles and the existing material hierarchy. Preserve supported glass,
solid surfaces and reduced-motion behavior; no new desktop visual language.

## Remaining surface inventory

| Surface | Current observation | Next verification |
| --- | --- | --- |
| Notification center | #3039 landed: shared buttons, measured geometry, stable Clear/Undo focus. #3067 landed: shared Read control and wrapped ScrollRegion reading page, nested Back/Esc, accessible scrolling and covered-banner pointer precedence | [First increment captures](evidence/notification-center/README.md); [full-text reader evidence](evidence/notification-reader/README.md); reading, scrolling, cancellation, repeated actions, privacy, removal and resize |
| Notification banners | Portable NotificationStack owns geometry and native host supplies effects | Dismiss hit target, error/quiet/locked states and large text; review active banners behind the history center (native capture evidence) |
| Launcher / search | Shared desktop search already wraps toolkit CommandPalette; native providers and host effects remain shell-owned | Empty/loading/error states, keyboard ranking and repeated launch, narrowed search, Esc |
| Overview | #3057 landed: shared creation row and focus rings; correct synthetic workspace/window paths; keyboard creation; compact page scrolling and resize | #3076 ready: stable tab identity, interrupted activation and 26 focused / seven integration cases passed; CI waits for the separately owned lock-recovery fixture repair |
| Window chrome | DesktopSurface composes toolkit GroupRow/StripView/WindowGroup | Inactive/focused tabs, close/menu states, resizing and shared header controls |
| Menus / dialogs | Session power flyout now uses ListRowFace for action type, glyphs, elision, selection and focus; measured action labels widen its responsive layout. Tab-menu controls already delegate to the shared ContextMenu; workspace lens uses ButtonFace and SegmentedFace | [Power flyout captures](evidence/power-menu/README.md), keyboard/pointer/a11y parity, Esc, reopen and scaled resize verified; current increment reveals full secondary copy through shared TooltipHost; existing tab-menu keyboard cancel/focus/split journey passed, scaled long-label states remain |
| Status / quick settings | Top bar already uses some ButtonFace controls; portable drill-in uses shared back face | Status indicators, paging, focus/hover, offline states; no real device effects during evidence capture |
| Workspace controls | Shared desktop model and portable workspace settings own intent/state | Inline edit cancellation, resize/focus, repeated rename and workspace switching |
| Window switcher | Portable recency/preview surface owns Alt+Tab geometry | Alt release, Esc, interrupted windows, withheld previews, large text |

## Concurrency boundaries

Inspected Forgejo changed-file lists for Messages #2881, Dictation #2974 and
Files #3009. #2881 and #2974 both modify overlay_host/mod.rs: Messages adds its update pump; Dictation adds layer geometry, pointer routing, fixture setup and DictationSetup intent. These inspected UI hunks do not overlap reader, overview selection or the power-menu Session intent closing change. #3009 modifies shell broker_services/file_clipboard,
nearby_transfer and shared clipboard/broker/undo-adjacent paths. This increment
touches none of those paths.

## Evidence and completion gates

The capture example is fixture-only and calls the production NotificationCenter
paint path. It includes an application error, action focus, Undo and restored
states in light/dark and 200% text. Render it before and after the source change.

Required checks: focused notification-center tests; desktop-surface examples
compile; cargo check --locked -p shell --tests; staged pre-commit checks; affected
package plan; actual screenshot inspection. A ready PR follows successful checks
and shareable evidence. Normal protected landing and postchecks remain required.

The first increment also uses toolkit elision for fixed-line previews, grows row
action targets with the shared control height and keeps focused rows visible on
resize. The production shell overlay render matrix also passed at compact size,
enlarged text and high contrast/UI scaling. Live compositor/VM capture remains
a distinct follow-up layer; current evidence is offscreen production rendering
and native overlay input routing.

The second increment adopts ListRowFace in the session power flyout. It retains
the status-rooted Panel material, quiet idle rows, animation and session policy.
Before/after fixtures cover dark/light/high contrast, 200% text, combined 150% UI
and 130% text, and a narrow output. Full action names take precedence over
secondary copy when height is constrained; complete details remain accessible.
The current full-text increment adds a visible keyboard/pointer route to complete
notification recovery text using ButtonFace and ScrollRegion. History keeps compact
previews; reading wraps the complete title/body and exposes native accessible
scrolling. Reading performs no default action or agent handoff. Back/Esc restores
history selection, and a second Esc leaves history. Hidden previews remain hidden.
The native journey found and fixes a covered-banner pointer interception; actual
history presses now match the foreground paint order. Review power secondary copy
at narrow/high-scale sizes remains open.

Capture delivery: all 24 representative and matrix PNGs are saved to native ChatGPT
Library image files. Small evidence manifests retain their identities, dimensions,
source revisions and checksums; the current source tree contains no capture PNGs.

## Protected landing and merged-main postchecks

- #3039 merged normally as 1b1fa48ac5d016f6fb410dffa3cd3924f8651bb5.
- #3057 merged normally as 18c4f1e472fffb39e7334c1160f6e6d5d27efba4.
- At the latter merged-main commit: all 22 overview cases, six power-menu cases,
  eight notification-center cases and cargo check --locked -p shell --tests passed.
  Actual overview/power render fixtures passed with WGPU_BACKEND=vulkan and only
  the Mesa CPU ICD selected; no DRM access was retried.
- Full-text reader integration and its independent exact-head CI/main postchecks
  are a separate gate; the broad remaining-surface inventory stays open.

## Current verification (2026-10-07)

- #3067 merged normally at ae67fc35d70b479b9a24a8c3a1426e289cbc6a12. Exact merged-main checks passed: 12 center cases, one portable reader case, two native reader cases, four notification integration cases and shell tests compile.
- #3076 head 38942a3da31cf468616e07364bb66b505559e0a2 remains ready. Its only failing shell CI case is the separately owned session_lock_recovery fixture; no authentication/policy code is part of this UI increment.
- Power-menu details: baseline captures confirm ellipsized recovery copy at 200% text. The shared tooltip regression fails on ae67: single-line sizing clips the complete description. Current work wraps measurement/paint at the available width and adopts TooltipHost for keyboard/pointer reading; cancellation, inert hint clicks, semantic actions and scaled resize are verification gates.
- All new screenshot evidence stays outside Git, in Library and PR attachments, with text manifests only. Full desktop/VM journeys and the remaining rows above remain open.

Power-description increment: verified source 2e9349c8d64f0252bf4ad6cdc43cd449c52c3dc9 uses shared TooltipHost and wrapped Caption painting. Nine power-menu cases, six tooltip cases, shell test compilation and all-target UI/shell Clippy passed. [Inspected before/after evidence](evidence/power-menu-details/README.md) covers six theme/scale/viewport variants. New capture PNGs remain outside Git. Displays #3095 changed-file inspection found no overlap with power or overview paths.
