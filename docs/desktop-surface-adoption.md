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
| Notification banners | Portable NotificationStack measures through ToastCard and now delegates completed clicks, cancellation and matching releases to shared PaintedTargets. Both desktop hosts forward the same pointer stream; existing shared close/choice faces receive hover and pressed state | [Measured-banner qualification](evidence/notification-banner-sizing/README.md) and [completed-click qualification](evidence/notification-banner-pointer/README.md) cover enlarged type, stale geometry, drag-out/Esc/focus/resize/retirement, repeated clicks and foreground history. Six error-banner variants preserve idle pixels and expose shared hover/pressed feedback; keyboard banner entry, quiet/locked and installed-device states remain |
| Launcher / search | Shared CommandPalette now retains refused-action query/focus/selection with existing accessible status feedback, ignores held-Enter repeats, reuses callback errors, reserves both theme scales and uses shared armed-row ink. [Recovery qualification](evidence/launcher-recovery/README.md): 12 shared cases, seven native cases, six inspected variants and private live search/Escape pass | Provider revisions now preserve the selected stable resource and invoke refreshed callbacks; [ranking qualification](evidence/launcher-ranking/README.md) covers keyboard, pointer, query correction, removal, resize and three inspected variants. Empty-workspace launchpad now delegates pointer activation, release ownership, focus loss, Escape and held-submit suppression to shared PaintedTargets; native Search opens only on completed clicks. [Launchpad qualification](evidence/launchpad-release/README.md). Empty/loading and real sandboxed app launch remain; VM/device coverage is blocked on this runner |
| Overview | #3057 and #3076 landed: shared creation/focus controls, compact scrolling, stable tab identity and interrupted activation. Exact #3076 merged-main checks passed 26 focused cases, seven native journeys and shell test compilation | Mixed-window preview and live device/workspace transitions remain; framing repair #3100 is integrated |
| Window chrome | DesktopSurface composes toolkit GroupRow/StripView/WindowGroup | Inactive/focused tabs, close/menu states, resizing and shared header controls |
| Restored app cards | Symbolic LocalApp notices decline scratch-document custody; matching launch failures/timeouts and orphaned exact-tab placements settle without removing saved tabs or record associations. Shared EmptyState recovery controls support keyboard, pointer and the native action door; focused Applications activation reuses the saved window | [Restore-card qualification](evidence/localapp-notice-custody/README.md): original masking, orphaned-opening and missing action-door regressions reproduced; dark/light/high-contrast, 200% text, focus and resize inspected. Exact-record adoption, repeated/stale recovery and Applications reuse pass. Installed repaired-layer acceptance and cold-launch attribution remain |
| Menus / dialogs | #3110 and #3205 landed: shared ListRowFace/TooltipHost and ContextMenu current-output placement. Exact #3205 merged-main passes three output/capture cases, keyboard, pointer open/dismiss, action parity and shell test compilation; all six capture variants match inspected originals | Additional dialog/long-label/error states and VM hotplug/input coverage remain |
| Emoji picker | Shared SearchField and ToggleGroup faces; interrupted pointer selection repaired after focus loss/search replacement, with scroll/layout cancellation | [Interruption qualification](evidence/emoji-picker-interruption/README.md); 200% text footer overlap (including 800 px skin-tone hints) and narrow group icon elision remain; installed-device coverage separate |
| Status / quick settings | Top bar already uses some ButtonFace controls; portable drill-in uses shared back face. The coherent People increment adopts shared input, measured controls, navigation row and ScrollRegion | [People qualification](evidence/people-panel/README.md); remaining primary-bar indicators/overflow and narrow Devices/Clipboard summaries, plus installed-device states |
| Workspace controls | Shared desktop model owns intent/state; workspace rename now paints with FieldFace/TextInput, including shaped selection, horizontal scrolling, idle elision and composed scale geometry; Create-invite uses shared primary ButtonFace and the footer reserves measured Caption space | [Shared name-field qualification](evidence/workspace-name-field/README.md); pointer/keyboard cancel, reopen, Unicode commit, held-submit repeat/release ownership, resize and selection captures; nine desktop/six native cases, compile and Clippy pass. Live workspace/device transitions and header states remain |
| Window switcher | #3159 landed normally at 6476c63a, conflict repaired: stable live identities, selected-close cancellation, moved-address refresh and stale-pointer guard. Fresh 11 desktop/seven native cases, compile and Clippy pass; six inspected variants | Exact merged-main 11 desktop/seven native cases and shell test compilation passed; live multi-workspace/preview/device coverage remains |

## Concurrency boundaries

Complete paginated changed-file lists for Messages #2881 and Dictation #2974
both include shell/overlay_host/mod.rs. Messages adds its update pump; Dictation
changes layer geometry, fixtures and setup routing. Those hunks do not overlap
overview selection, notification-reader routes or power Session-intent handling.
Neither PR changes the desktop switcher file or its native integration module.
Displays #3095 has no file overlap with these increments. Files #3009 owns
broker_services/file_clipboard, nearby_transfer, clipboard/broker and undo-adjacent
paths; none are edited here. No account sends or microphone/device effects.

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
- #3076 rebased onto integrated control framing repair #3100. Fresh focused validation and exact-head CI are required; no authentication/policy code is part of this UI increment.
- #3110 merged normally at 1161ca6aed22a58cb2904d8aa8ce784fabd3ee33. Shared TooltipHost reveals full power descriptions; wrapped measurement/paint, cancellation, inert presses, semantic actions and scaled resize passed branch validation. Exact merged-main postchecks remain required.
- All new screenshot evidence stays outside Git, in Library and PR attachments, with text manifests only. Full desktop/VM journeys and the remaining rows above remain open.

Power-description increment: verified source 2e9349c8d64f0252bf4ad6cdc43cd449c52c3dc9 uses shared TooltipHost and wrapped Caption painting. Nine power-menu cases, six tooltip cases, shell test compilation and all-target UI/shell Clippy passed. [Inspected before/after evidence](evidence/power-menu-details/README.md) covers six theme/scale/viewport variants. New capture PNGs remain outside Git. Displays #3095 changed-file inspection found no overlap with power or overview paths.

## Overview stable selection follow-up

Overview binds the shared focus ring and accessibility key to stable tab IDs. A surviving preview keeps focus when another closes; a vanished selection restores the current workspace and consumes one interrupted activation. [Original inspected evidence](evidence/overview-window-identity/README.md) is preserved with its original source revision and Library identities. Normal landing and exact merged-main verification are complete: 26 focused cases, seven native journeys and shell test compilation passed at ad449de1.
## Window switcher live candidate follow-up

Alt+Tab now reconciles the open candidate snapshot with live tab identities, preserves the intended selection, refreshes moved addresses and cancels a vanished armed window. Geometry changes consume old-position presses until paint. [Before/after evidence](evidence/window-switcher-live/README.md) covers themes, text/UI scales and a narrow output. Branch validation passed 11 desktop cases, seven native integration cases, shell tests compile and desktop/shell Clippy; normal landing and exact merged-main verification are complete at 6476c63a: 11 desktop cases, seven native journeys and shell test compilation passed.

## Tab menu current-output follow-up

Open tab menus now use the shared ContextMenu.constrain_to API on the current output before drawing. Both baseline production regressions fail; all three focused cases and the keyboard/cancel/reopen/split journey pass. [Inspected evidence](evidence/tab-menu-current-output/README.md) covers six themes/scales/output sizes and original Library IDs. The initial headless example used legacy Super+K on a fresh profile; its strict failure state/pixels are retained. Current default Super+Space and the corrected example pass; the example correction is a separate increment. Live VM/device coverage and remaining launcher/search/chrome/status/workspace states remain open. Issue3133 notification-service and Files3009 transfer/broker/undo ownership boundaries remain intact.

## Landed verification checkpoint

Overview #3076 merged normally at ad449de191107ccf9f9c88037052604f4228206b; exact merged-main checks passed 26 overview cases, seven native journeys and shell test compilation. Power descriptions #3110 merged at 1161ca6aed22a58cb2904d8aa8ce784fabd3ee33 with exact merged-main nine power cases, six shared-tooltip cases, one native Quick Settings journey and shell test compilation passing. Alt+Tab #3159 landed normally at 6476c63a8e0dc6ef03a2bcb7bd58c1e44ff6b18a from conflict-repaired head 38e6d15fb04d2c28ba00db2cf6a426088fd27dbf: fresh 11 desktop and seven native cases, shell test compilation and all-target desktop/shell Clippy passed; exact merged-main postchecks passed 11 desktop cases, seven native journeys and shell test compilation. The separate live named Open an app → global palette → typing → Esc → reopen/type/cancel journey passed in a private CPU headless session with inspected state/pixel evidence. The original Super+K probe used a legacy binding on a fresh profile. The current default Super+Space passes strict modifier-mask and held-key journeys; no desktop preference or policy change is required.

VM verification was attempted on the runner through tilo-vm: the configured dev VM refuses SSH at 127.0.0.1:2222, and doctor reports “cannot open /dev/kvm for reading and writing.” No VM/device, credentials or trust configuration was changed; no VM pass is claimed.

## Final verified checkpoint

Tab menus #3205 landed normally at 499f0740d44bca4af2da986839489ed230d4877c from fully green head 8f628b5154a0ae5b1de044e97bbba266e9bac2cd. Exact merged-main verification passed all six focused cases, shell test compilation and byte-identical comparisons of all six inspected render variants. A freshly rebuilt compositor at that exact main commit passed the corrected held Super+Space example and a repeated pointer open/type/Esc journey, with private profiles/sockets cleaned up. [Default-search example evidence](evidence/headless-default-search/README.md) records retained assertions, actual pixels and original Library provenance. The remaining rows above and VM/device blocker remain open.

Launcher recovery #3277 merged normally at 4e2c99589a8d669e2626018a0f7b30a7183cfe2f. Exact merged-main checks passed 12 shared palette tests, seven native shell cases, shell tests compilation and a rebuilt private compositor search/Escape journey. Merged high-contrast, combined-scale and live captures were inspected. Ranking is the next independent increment; remaining inventory and VM/device limitations stay open.

The live full-loop example now positively checks painted query pixels within the named input bounds. An actual nonblank closed-desktop capture is rejected; the production private search/Escape journey passes all existing and new assertions. [Qualification](evidence/headless-palette-visibility/README.md) distinguishes this observability improvement from an unproven animation failure.

## Top-bar editor increment

The editor adopts shared ButtonFace and ListRowFace, measured title/hint/rows and
wrapped Add controls. Actual production regressions qualify drag cancellation,
release activation, stable keyboard focus and submit ownership; theme changes
invalidate the retained scene. [Qualification](evidence/topbar-editor/README.md).
Primary chrome at 320px still collides above the editor and remains the next
independent increment alongside status/device/offline and live window states.

The action catalogue uses dedicated shell English/German Fluent entries; CI's
all-action catalogue regression is retained and strengthened across both locales.
The locale files overlap Dictation #2974 only at file level in independent hunks;
the top-bar entries are beside overview, and Dictation owns its later namespace.
Messages #2881 and Files #3009 still have no edited-path overlap.

## Owned overview clock fixture (2026-10-07)

On base 411d51e9, the overview harness's ambient clock pin was ignored by the
native source's deliberately unknown fixture clock. HostedTileInputs now carries
an explicit fixture-only ClockSnapshot; the harness supplies 09:41 without
changing the fixture's Files services or the live clock. Five native chrome tests
and thirteen overview interaction/pixel cases pass. The overview pixel oracle
now requires an offscreen adapter rather than silently asserting only state.

The original tracked overview PNG remains byte-identical (SHA-256
06d93114eaf00a4a6ad297e9dd3b6f78873f2ec18caadfe12c433c1acd651406), with tolerance 4.
This runner reproduces 7,082 differing pixels before and 6,807 after; the clock
region now matches exactly, and all 275 before/after differences are confined
to that region. The existing Home focus ring, shared creation row and fixture
avatar differences remain for maintenance review. No baseline has been refreshed.
The ready source PR carries inspected original PNG attachments and qualification
results; protected landing and exact merged-main checks remain pending.

Remaining primary-bar work is separate: collisions at 320 px, retained shared
overflow controls and status/offline states. The qualified top-bar editor
increment remains frozen while the Files CI prerequisite is resolved.

## People Follow identity follow-up

People Follow now resolves the painted participant identity against the live roster.
Keyboard focus follows a surviving person across reordering and clears when that
person departs or becomes unavailable; the Follow button uses shared ButtonFace
focus painting. [Qualification](evidence/roster-follow-identity/README.md) covers
stale pointer/keyboard activation, accessible focus, repeated Follow/Stop and
Escape/reopen. Broader People text scaling, status-entry controls, paging and
installed-device acceptance remain in the inventory.

## People completed activation follow-up

People now uses shared PaintedTargets for completed pointer gestures and
ButtonFace pressed states. Stable person targets cancel across movement,
departure/return, resize, focus loss and dismissal/reopen. Held Enter/Space
repeat and key-up stay owned after Activity opens; a repeat cannot reverse
Follow's existing local-key handoff. [Qualification](evidence/people-completed-activation/README.md)
records native baseline failures, actual post-input screenshots and reuse of
unaffected light/dark/high-contrast identity captures. Existing prompt and
capability-gate tests retain their assertions at release. Follow-label elision
at 125% text, status-entry controls, broader viewport/paging and installed-device
acceptance remain open; protected landing and exact merged-main postchecks are
independent integration gates.

## People status draft lifecycle follow-up

Closing People now ends its status edit lifecycle before hidden input, hidden
paint or reopen can resurrect the discarded draft. Completed Activity/Invite/
Join/dismiss actions cancel the edit without publishing it. Existing shared
EditCore-backed selection, IME, navigation, explicit Enter submission and
two-step Escape behavior remain intact. [Qualification](evidence/people-status-lifecycle/README.md)
records four production baseline failures, native journeys and two inspected
light/dark125 screenshot pairs. The portable component and broader existing
render suites are unchanged. Status field/control rendering, text scaling,
viewport/paging and installed-device acceptance remain open.

## Coherent People panel follow-up

The current-main delivery preserves the authored #3736 source and combines the
remaining People viewport, paging, status-rendering and scaling work. Shared
ScrollRegion, FieldFace/TextInput, measured ButtonFace/SegmentedFace and
ListRowFace keep controls reachable, selection accurate, labels legible and
empty/error/disclosure content in separate measured bands. Native acceptance
includes repeated paging, stationary-pointer reopen, pointer/semantic scroll,
Unicode editing, Esc, focus loss, resize cancellation and visible-only scroll
fade scheduling. [Qualification](evidence/people-panel/README.md) records the
bounded checklist and production capture matrix. Protected integration and
exact merged-main postchecks remain independent gates; installed-device and
the broader remaining rows above stay open.

## Emoji picker measured-layout follow-up

The enlarged-text footer and narrow group glyph defects observed during #3719
are repaired through shared Caption shaping and SegmentedFace target metrics.
Names and hints stack only when needed; narrow group controls wrap while
preserving keyboard/pointer identity and localized accessible bounds. Resize
keeps the selected emoji visible. [Focused evidence](evidence/emoji-picker-responsive/README.md)
reuses the unchanged inspected hints and qualifies the failing 200% / 320 px
states plus normal comparison. Installed-device and extremely small viewport
coverage remain separate from this source layout correction.
