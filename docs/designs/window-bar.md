# The window bar: one bar, and only when reached for

Status: proposal for the parts marked **Decide**; the mechanism is built
(`ui::widget::top_edge_reveal`, `WindowHeader::set_window_bar`,
`NavShell::with_header_in_window_bar`).

## What a maximized window looks like now

- The client fills the window: no lane above it for the decorations and no
  margin below it. The desktop's top bar stays above (fullscreen still covers
  the whole display).
- The window's bar (title, Full screen, Restore, close) hides at rest and
  fades in over the window's top edge when the person reaches for it. The
  intent model (`crates/sdk/ui-core/src/widget/top_edge_reveal.rs`):
  - only the top 6 px of the window summon it; the app's own top row (a
    browser's tabs, a toolbar) stays the app's;
  - a fast climb that brakes in that band is a push for the edge and shows the
    bar at once; otherwise a 150 ms rest there, counted from when the pointer
    settles;
  - passing through (fast, not braking) is transit and never counts;
  - a click in the band while the bar hides goes to the app and cancels the
    reveal until the pointer leaves the band;
  - shown, it stays while the pointer settles on it or heads for one of its
    controls (menu aim), gives an overshoot into the desktop's top bar 300 ms
    to come back, and hides at once otherwise;
  - keyboard (`pin`) and a touch swipe down from the edge reach it too.
- In a Tilo app with a header row, the bar is that row: the window's
  controls stand at its trailing end, in room the row keeps for them, as
  three of the row's own icon buttons. No second bar.
- Notes and Passwords put their header *in* the bar: maximized, the header
  hides with the bar and comes back over the content with it. Files and
  Messages keep a permanent header, with the window's controls appearing in
  its trailing slot.

## Decide: which apps keep a permanent header

| Option | For | Against |
| --- | --- | --- |
| A. Header in the bar for every app (default on in `NavShell`) | Maximized means all content; one rule everywhere | Sidebar toggle, Back and app actions are one reach away; discoverability of actions drops |
| B. Header in the bar for document/reading apps (Notes, Photos, PDF, Word, Video), permanent for navigation apps (Files, Mail, Settings) | Keeps the path bar / folder actions visible where they are used constantly | Two behaviours to learn |
| C. Permanent everywhere (status before this change) | Nothing hidden | The "two bars" the request was about, minus the stacked decorations |

Recommendation: **B**, with the rule written as "an app whose header holds
navigation the person uses while working keeps it; an app whose header
holds occasional actions puts it in the bar". It also applies to tiled
windows if you want: the same `with_header_in_window_bar` row could hide
under a tiled window's tab row (not built; today it hides only maximized).

## Decide: where Search and History live

Today both are system services (AGENTS.md: apps never draw a search field;
the host offers History to the header row). Their entry points:

| Option | For | Against |
| --- | --- | --- |
| 1. In each app's header (today: the app adds a `search_button`, the host adds History) | Discoverable where you work; scoped to the app by default | 20 copies of the same two buttons; hidden with the header in option A/B |
| 2. In the window bar, beside Full screen / Restore / close, owned by the shell | One place, same for foreign apps (a terminal gets History and Search too); appears with the bar | Only visible when reached for; the bar grows from 3 to 5 controls |
| 3. In the desktop top bar only (Super+K / Super+H), scoped to the focused window | Truly OS-level; zero per-window chrome | Far from the content on large screens; scope (this app vs everything) must be explicit |

Recommendation: **2 + 3**: the desktop's Search/History entry is the global
one; the window bar carries the two as window-scoped shortcuts (they already
exist as tab-pill chips for tiled windows). Apps then drop their own search
and History buttons, which makes option A/B above cheap. This needs the
shell to own both controls in the merged bar (the `WindowBar` offer would grow
from 3 to 5 controls) — not built yet.

## Not done yet

- Out-of-process Tilo apps (Settings and most `.tapp` apps) are Wayland
  clients: the shell cannot reach their header row in-process, so today they
  get the floating chips. Merging needs a small Tilo Wayland protocol (the
  shell offers `controls_width` + `revealed`; the client answers with its row
  rect), implemented in `native_runner` like `tilo-input-context-v1`.
- The desktop's own top bar is gaining an auto-hide setting; it uses the
  same `TopEdgeReveal` so that, when both are hidden, approaching the top
  reveals the top stack together (desktop bar, then the window bar under it).
  While the desktop bar shows over the window, the shell passes its height to
  `WindowGroup::set_top_overlay_inset` and the window's bar (and a header in
  it) stands below it.
- No keyboard shortcut is bound to `pin` yet (candidate: the shortcut that
  focuses the top bar, or F10 as GNOME uses for the primary menu).
