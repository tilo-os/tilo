# Desktop shared-component adoption

Inspected base: c3a50df (2026-10-06); incoming main d534036e / #3025 inspected for HIG changes and overlap. This inventory tracks targeted increments;
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
| Notification center | First increment replaces hand-painted action controls with ButtonFace; aligns accessible action focus/bounds and prevents stale Clear all focus after pointer group activation | [Before/after captures](evidence/notification-center/README.md), Clear/Undo, interrupted Undo, resize/focus, light/dark and enlarged text; production overlay journey exercised |
| Notification banners | Portable NotificationStack owns geometry and native host supplies effects | Dismiss hit target, error/quiet/locked states and large text; review active banners behind the history center (native capture evidence) |
| Launcher / search | Native launcher and search providers remain shell-owned; shared desktop search owns portable result contracts | Empty/loading/error states, keyboard ranking and repeated launch, narrowed search, Esc |
| Overview | Portable desktop overview/panel has custom card geometry | Workspace/group selection, pointer/keyboard parity, narrow layout, cancellation |
| Window chrome | DesktopSurface composes toolkit GroupRow/StripView/WindowGroup | Inactive/focused tabs, close/menu states, resizing and shared header controls |
| Menus / dialogs | Tab menu is shell overlay; workspace lens already uses ButtonFace and SegmentedFace | Focus return, Esc, interrupted flows, long labels; keep grant/security semantics unchanged |
| Status / quick settings | Top bar already uses some ButtonFace controls; portable drill-in uses shared back face | Status indicators, paging, focus/hover, offline states; no real device effects during evidence capture |
| Workspace controls | Shared desktop model and portable workspace settings own intent/state | Inline edit cancellation, resize/focus, repeated rename and workspace switching |
| Window switcher | Portable recency/preview surface owns Alt+Tab geometry | Alt release, Esc, interrupted windows, withheld previews, large text |

## Concurrency boundaries

Inspected Forgejo changed-file lists for Messages #2881, Dictation #2974 and
Files #3009. #2881 has no shell paths. #2974 modifies ui-core text-input and
presentation integration. #3009 modifies shell broker_services/file_clipboard,
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
