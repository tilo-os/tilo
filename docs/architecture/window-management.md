# Window, workspace and strip management

Status: design note, 2026-09-29. Where it says **today** it describes
`origin/main`; **planned** rows are the work listed under "Delivery". It adopts
the navigation grammar of the owner's proposal in PR #1320
(`docs/design/navigation-and-multi-monitor.md`), which also carries the
comparison with niri, PaperWM, GNOME, macOS and Windows and the multi-monitor
defect list. This page is the contract the window manager implements: one
mental model, one binding for each operation on each input device, the action
behind each binding, and how every transition moves.

## One mental model

```text
                 ↑ strip above                     Ctrl+Super+↑, Super+1…9
   ← window   [ focused group ]   window →         Super+← / →
                 ↓ strip below                     Ctrl+Super+↓

   the frame:          maximize / restore           Super+↑ / ↓
   the plane you see:  windows along strips, spread across your displays
   one level out:      workspaces                   Super+Page Up / Page Down
   the glass:          which display                Alt+Super + arrow
```

A workspace is a **desk**: its strips stack top to bottom, a strip's window
groups run left to right, and each display is a window onto part of that desk
(every display shows the same workspace and holds its own groups; see
[displays-and-infinite-workspaces.md](displays-and-infinite-workspaces.md)).
Workspaces are separate desks, stacked in order. Nothing overlaps and nothing floats.

Every input device says the same three things:

1. **Direction is spatial.** ←/→ is the window axis, ↑/↓ the strip axis, and
   both keep their meaning across a display seam. Spatial moves clamp at the
   ends; lists (Alt+Tab, overview rows) wrap. A move never spills into the next
   level: Super+→ on the last window does not switch workspace.
2. **The level is a modifier, or a finger count.** Plain Super+← / → walks the
   windows of the strip, and plain Super+↑ / ↓ maximize and restore the focused
   group, as on Windows and GNOME; Ctrl+Super+↑ / ↓ is strips, and Super+1…9
   goes straight to a strip (1 is the top one); Super+Page Up / Page Down is workspaces; Alt+Super is
   displays. On a touchpad or a touchscreen three fingers work the plane
   (vertically, strips), four go one level out, and a pinch is the overview.
3. **Shift carries.** Every "go" has a Shift twin that takes the focused group to
   the same place and keeps it focused. Dragging is the pointer's Shift.

The frame of one group — its tabs, width and full screen — is not navigation,
so it gets letters and brackets rather than arrows; maximize and restore are
the exception people already know, on Super+↑ / ↓.

**Focus and view are different.** Focus is which tab receives keys; every
chord moves it. Panning a strip (a three-finger swipe, a scroll over the tab
bar) changes what is visible. When a pan settles, focus follows the group left
most visible, so a swipe followed by typing goes where you are looking.

## Every operation, on every device

Each row is one operation, published on the action door, so the palette,
`tilo-control actions invoke`, a screen reader, an agent and the chord are the
same door. Actions come in two forms:

- **relative**, on the shell provider (`window.focus-next`,
  `group.move-to-strip-next`): they act on the focused group of the focused
  display, and they are what a chord runs;
- **absolute**, on the `windows` provider next to today's `group.close:<group>`
  and `tab.move:<group>:<tab>:<workspace>`: the same operation on a named
  group, so an agent rearranges a desk without taking anyone's focus.

Chords are the rebindable defaults in `settings_store::hardware::ShortcutConfig`
unless marked fixed.

### Navigate

| Operation | Relative action | Keyboard | Touchpad and touchscreen | Pointer | Status |
|---|---|---|---|---|---|
| Previous / next window (tabs walked as one list, across displays) | `window.focus-previous` / `-next` | Super+← / → | 3 fingers horizontal: direct pan, snap on release, focus follows | click; Super+wheel | today; snap and focus-follow planned |
| Nth / first / last group of the strip | `window.focus-nth` (`index`) | Ctrl+Super+1…9, Super+Home / End | — | click | 1…9 today |
| Strip above / below | `strip.focus-previous` / `-next` | Ctrl+Super+↑ / ↓ | 3 fingers vertical | strip ribbon; click the strip | today |
| Nth strip, from the top | `strip.focus-nth` (`index`) | Super+1…9 | — | strip ribbon | today |
| Previous / next workspace | `workspace.previous` / `-next` | Super+PageUp / PageDown (fixed) | 4 fingers vertical, following the fingers | strip switcher | PageUp / PageDown, 4 fingers (one step, not yet following) and the action today |
| Neighbouring display | `display.focus-previous` / `-next` | Alt+Super+← / → | — | point and click | planned |
| Overview | `overview.toggle` | Super+Tab | pinch in / out; tablet posture: swipe up from the bottom edge | top-bar button | today, except the edge swipe |
| Strip switcher | — | Super+S (fixed), then ↑ / ↓, Enter, F2 rename, Esc | — | hover-aim the right-edge strip ribbon; click in the card | today |
| Recent windows | — (a held modal) | Alt+Tab, Alt+` (fixed) | — | — | today |

### Discover tab operations

`tab.menu` (shell provider), or the rebindable Super+F10 chord, opens the
focused tab's context menu at its tab pill. The first action is focused
immediately; Up/Down choose a row, Enter invokes it, and Escape cancels.
Right-click still targets the tab under the pointer. A single-tab group offers
Close only; multi-tab groups also offer Split right and Move to new group.
The menu uses the existing action dispatch, permissions and close history.
Settings and the shortcut guide publish the same binding. App Context Menu
and Shift+F10 keys continue to reach the app's resource menu.

### Carry the focused group

| Operation | Relative action | Keyboard | Touch and pointer | Status |
|---|---|---|---|---|
| One slot left / right, walking across a display seam | `group.move-left` / `-right` | Super+Shift+← / → | drag the tab-bar ground into a gap | today within a display |
| To the strip above / below | `group.move-to-strip-previous` / `-next` | Shift+Ctrl+Super+↑ / ↓ | drag onto another strip | today, except the drag |
| To the previous / next workspace | `group.move-to-workspace-previous` / `-next` | Super+Shift+PageUp / PageDown | — | planned; one tab today via `tab.move` |
| To the neighbouring display | `group.move-to-display-previous` / `-next` | Alt+Super+Shift+← / → | drag onto the other display | today on Super+Ctrl+Shift+← / → |

A workspace is the sandbox and sharing boundary. Carrying a group there moves
each tab through the window-transfer consent sheet exactly where `tab.move`
would show it; a chord opens the sheet and never bypasses it.

### The frame of one group

| Operation | Relative action | Keyboard | Touch and pointer | Status |
|---|---|---|---|---|
| Maximize, restore | `group.maximize` (toggle), `group.restore` | Super+↑ maximizes, Super+↓ restores (on any strip); Super+M toggles | double-click or double-tap the tab-bar ground; frame button | today; the maximize animation planned |
| Full screen / restore | `group.fullscreen` | Super+F | — | today |
| Narrower / wider, 5 % steps | `group.narrower` / `-wider` | Super+- / = | drag either edge ([Resizing a window](#resizing-a-window)) | steps and edge drags today |
| Previous / next tab | `tab.previous` / `-next` | Super+Ctrl+Shift+Tab / Super+Ctrl+Tab (Ctrl+Shift+Tab / Ctrl+Tab where the app leaves them unhandled) | click a tab | today |
| Tab into the neighbouring group | `tab.move-to-group-previous` / `-next` | Super+[ / ] | drag the tab onto the group | today |
| Tab into a new group | `tab.move-to-new-group` | Super+Shift+Enter | drag the tab into a gap | today |
| New tab, close, reopen, duplicate | `tab.new.terminal.here`, `tab.close`, `tab.reopen`, `tab.duplicate` | Super+T, Super+W, Super+Shift+T, Super+D | `+` and × in the tab strip | today |

**Maximize** makes the group fill its display's work area. The top bar and the
strip ribbon stay, because they say where you are; the group's title,
restore and close chips appear when the pointer nears its top edge. **Full
screen** hides all chrome on that display. The two are separate states today:
a maximized group stops at the leading edge gutter, where the strip's first
window starts, and only a client's full screen covers it. Both still hold one slot
for the whole desktop; each display getting its own, so a group full screen on
a projector leaves the laptop's chrome alone, is planned.

## Gestures

The shell owns three or more fingers; one and two belong to the app
(`compositor::backend::drm::gestures::claims`). Natural scrolling inverts both
swipe axes. A touchpad gesture acts on the display under the pointer; a
touchscreen gesture on the display the touchscreen is mapped to.

| Fingers | Horizontal | Vertical | Pinch |
|---|---|---|---|
| 3 | pan the strip 1:1; release carries a fling and settles on a group edge | strip above / below | overview in / out |
| 4 | reserved | workspace, following the fingers | overview in / out |

The pinch covers the overview, so four fingers take workspaces and three take
strips, the same level split as the modifiers. Today a vertical swipe steps
once, the moment it passes its threshold; following the fingers is planned.
"Following the fingers" means the desk slides with the hand and the release
decides: past a quarter of the display height, or a flick, commits; less
springs back. A gesture during the settle catches the motion where it is.

A touchscreen gets the same table. The compositor claims a touch sequence as a
gesture when a third contact lands within 250 ms of the first, and cancels it
for the client (`wl_touch.cancel`), so an app's two-finger scroll is never
taken. One finger stays direct: on a window it is the app's; on a tab it drags
the tab; on the tab-bar ground it drags the group; on a window's edge it resizes it
as a pointer would (planned: today edge resizing is pointer-only), with a hit
area at least 24 px wide under touch.

## Pointer

- **Tab**: drag to reorder, onto another group to join it, into a gap for a new
  group, onto another strip, display or overview preview.
- **Tab-bar ground** (the empty part of the bar): drag moves the whole group to
  the same targets; double-click maximizes.
- **Either edge of a window**: drag resizes that window, and only it; Shift
  trades width with the window across the seam. See
  [Resizing a window](#resizing-a-window).
- **Super + drag** anywhere on a window moves its group; Super + right-drag
  resizes it. Super chords are shell-owned, so no app loses a binding.
- **Wheel**: over the tab bar it pans the strip; Super+wheel steps windows.
- **Every drop target is drawn while it would accept**: an insertion bar in a
  gap, a lift on a group to join, the target strip's ribbon segment, the target
  display's outline. Dragging against a display's outer
  edge scrolls that lane, faster the closer to the edge; resting a drag on a
  background tab for half a second brings that tab to the front.

## Resizing a window

The strip is infinitely wide, so resizing is never zero-sum unless the person
asks for it. One rule decides every case, and the code holds it in one place,
`crates/sdk/ui-core/src/widget/edge_resize.rs`: `seam_owner` says whose edge a
point is, `EdgeDrag::resize` says what a drag sets. The row
(`group_row_edges.rs`) only feeds them geometry: the hover highlight, the press
and every frame of the drag ask the same two functions.

- **You resize the window whose edge you grab, and only that window.** Every
  window owns both of its edges. The seam between two windows (each one's
  6 px grip plus the 8 px gap) is split down the middle, each half being the
  edge of the window it touches, except that **the focused window owns the
  whole seam**: the seam is too small to aim at halves, and the window you are
  working in is the one you mean. Grabbing Files' left edge, where it meets a
  terminal, resizes Files when Files is the window in use, wherever on the
  seam the press lands.
- **Before pressing**, the edge of the window that will change shows its grip
  (with Shift held, both windows' grips at that seam), and the pointer shows
  the resize cursor. The seam's own rule never lights up, so it never suggests
  that the gap or both windows move.
- **A plain drag** changes only the grabbed window. Its width follows the
  pointer 1:1, in whole pixels, and its *opposite* edge holds still on screen.
  Everything beyond the dragged edge moves with it at its own width: dragging
  a left edge left pans the strip by what the window grew, so the windows
  before it slide away and its right edge stays put. No other window changes
  size.
- **Shift+drag** is zero-sum between the two windows at that seam: what one
  gains the other loses, both outer edges stay put. On an outer edge there is
  no one to trade with, and it is a plain drag. Shift is read at the press.
- **Limits stop the edge** and nothing else makes up for it. The narrowest a
  window goes is its app's real minimum (`AppWidget::min_window_width`): a
  Tilo app's compact layout (`metrics.window_min_width`, 320 px), a client's
  `xdg_toplevel` min-size hint, and for an app that states nothing the
  strip's floor (`WIDTH_FLOOR_PX`, 160 px, which also bounds the others from
  below, since the window's own chrome needs it). A client's max-size hint
  stops it the other way; there is no maximum of ours that can be felt.
- **The pointer stays on the edge**: the drag is anchored where it was
  pressed (no jump), and a pointer resting at a display edge auto-scrolls the
  strip, so a window can grow past the screen while its edge stays under the
  pointer. The scroll only runs while it still changes a width.
- **Live**: Tilo apps lay out again every frame of the drag. A client is sent
  a configure each frame its size changes; until it answers, its last buffer
  is drawn at the size it was drawn, against the fixed edge and clipped to the
  tile, never stretched (`ForeignEntry::frame_rect`; input maps through the
  same rect). The window's chrome follows the pointer at once.
- **Release** stores the width as the share of the strip it landed on and
  hands it to the shell to persist. Escape or losing focus cancels the drag:
  both windows and the strip go back to where they were. While the pointer is
  down the strip holds the fixed edge even if that cuts a window at the
  screen edge, and letting go moves nothing: no window moves to be framed,
  even when all of them would fit on screen and one is cut off. Only a pan
  the drag carried past the strip's start or end eases back into its range.

### The strip never scrolls by itself

The strip pans for what the person does, and only then: explicit navigation
(focusing another window, Super+arrows, a swipe or wheel, a strip or window
chosen in a switcher) and bringing a window into view that has just opened or
been focused. It never scrolls on its own to "frame" windows: not after a
resize, a close, a window changing width, or any other layout change. There
used to be such a rule ("if all windows fit on screen, scroll so none is cut
off; otherwise never cut one at each edge"), applied after every layout
change and when a resize drag was let go; the user rejected it on 2026-10-18
("no that is BS. i dont even know what that means") after Files' right edge
moved from 1280 to 1351 px on releasing a resize. The only automatic
correction left is the clamp that keeps the pan inside the strip's legal
range, for example when a close at the strip's far end leaves less to show.
Pinned by `group_row.rs`'s `after_releasing_a_resize_no_window_moves`
(`USER REQUIREMENT`) and its width-change tests.

Pinned by `edge_resize.rs`'s unit tests (the rule), `group_row.rs`'s row
tests (hover highlight, push, Shift, limits, cancel, auto-scroll) and the
pointer tests in `crates/session/shell/tests/desktop_env_interactions.rs`.
The tests that state the user's own words carry a `USER REQUIREMENT` marker
and may not lose an assertion without their OK (`docs/policies.md`).

**Why it kept regressing.** Seam ownership used to be nobody's decision. The
seam handle was keyed to its left neighbour (its drag set "left width + dx"),
so the infinite-strip change of 2026-09-20 kept only the left window moving,
and the 2026-10-12 change that added leading edges wrote that down as the
rule ("the seam is the first window's trailing edge") and pinned it in a test
(`[40, 0, 0]` for the middle window's left edge). Each fix stacked another hit
target (window grips, a leading grip only at a lane's start, the seam), and
which window moved fell out of dispatch order. The same handle drag also left
the strip's re-framing on, so the pan moved under the pointer. Now the owner
is decided by a named function with tests per case, the seam widget resizes
nothing, and the user's words are pinned.

## Where things open

Things open next to where the person is working, and going away returns
them there.

- **A new window** (a launch, a search result, Super+T) opens as its own
  group right of the group you are in; a plain launch from inside a tabbed
  group joins it as a tab, a search result never does. With nothing focused it
  takes the border between windows that is on screen.
- **Its width**: when it and the window it opens beside would not fit on
  screen together, it takes the room beside that window if that is at least
  480 px, else the two share the display equally; only a display too narrow
  for two usable windows pans instead (`width::share_beside`).
- **Closing** the focused window goes back to the window you came from (per
  strip, most recently focused first), as closing a tab goes back to the tab
  you came from; only without such history does the neighbour take focus.
- **Clipboard history** (Super+V) opens at the caret of the field you are
  typing in (else over the focused window), and choosing an item pastes it
  there. Browsed from Quick Settings it opens under the tray and only copies.
- **Readouts** (volume, brightness, layout) sit bottom centre unless that would
  cover the caret; then they sit top centre.

## Overlays

An overlay summoned from the keyboard opens on the display with keyboard
focus; one summoned by pointer or fingers opens on the display under them.

- **Overview** (Super+Tab, pinch): the current workspace's whole hierarchy —
  strips per display, groups, tabs — beside a rail of workspaces. Every preview
  is a drop target and a drag source. Entering zooms each window from where it
  is drawn to its card, tracking a pinch while the fingers are down; leaving
  reverses it.
- **Strip switcher** (Super+S, or hover-aim the strip ribbon): the ribbon
  expands into a card listing the workspace's strips — names, avatars of who
  is on each, click to switch, + New strip, rename — and a Workspaces section
  to switch workspace or add one. Keyboard: ↑ / ↓, Enter, F2 renames, Esc
  closes.
- **Window switcher** (Alt+Tab): most recently used first, every display,
  shown once Alt is held 120 ms.
- **Strip ribbon**: the right edge of the content, one segment per strip
  (HIG §9.11).

## Motion

Two rules. **Everything is drawn moving from where it is to where the layout
now puts it**, and **motion follows the input**: how far that drawing is
allowed to travel depends on what asked for the change.

- **Direct manipulation** (a drag, swipe or pinch) animates continuously.
  While fingers are down the surface is welded to them; release hands their
  velocity to the settle spring.
- **A pointer click** may animate, at `fast` or `base`, or on the rect
  spring below. It shows where the thing the person aimed at went.
- **A keyboard shortcut** applies its result on the next frame. A shortcut is
  a frequent expert action with a known result, so animating it adds latency
  to every repetition. Only spatial context may move, at `fast` at most: a
  viewport sliding an off-screen target into view, a direction cue. Repeated
  presses never queue; a press mid-slide retargets from where the slide is
  drawn.
- **Agents and scripts** (the action door) follow the keyboard rule: nobody
  watches a hand move, and the caller acts on the result at once.

Each transition is tagged with its source (`ui::tokens::MotionSource`:
`Keyboard`, `Pointer`, `Gesture`, `Action`) where the host runs the
operation: a chord and the action door run the same handler and name their
source there (`ShellOverlayHost::run_navigation_shortcut`). The desktop
holds that source until the layout that shows the change has run
(`DesktopSurface::begin_transition`), and `tokens.motion` picks the duration
per source: `Motion::result` for what changes what is shown (zero after a
shortcut) and `Motion::context` for what only says where something went
(capped at `fast` after a shortcut).

- **Rects spring.** Each group's drawn rect (x, y, width, height) follows its
  laid-out rect on a critically damped spring (`tokens.motion.springs.heavy`:
  ζ = 1, settled in about 0.26 s, no overshoot past a window edge). A new
  target mid-flight keeps position and velocity, so every transition is
  interruptible and repeated presses become one motion instead of a queue.
- **Opacity tweens** on `tokens.motion.durations` (HIG §9.7): `fast` for drop
  hints and sibling fades, `base` for panels.

| Transition | Pointer or gesture | Shortcut or action |
|---|---|---|
| Open a window | its group's width grows from 0 at its slot while neighbours slide aside; the content fades in and scales 0.96 → 1 | the same: the window arrives when its app is ready, frames after the chord |
| Close a window | the reverse, drawn from the last frame the client presented, so a dying client never flashes empty | the same, for the same reason |
| Move a group (reorder, strip, display) | the group springs to its new rect; neighbours spring to close and open the gap | in its new slot on the next frame; the viewport slides (`fast`) only if the group left the screen |
| Resize | an edge drag is direct manipulation: the width follows the pointer every frame, a Tilo app lays out again every frame, a client is configured at frame rate and its last buffer is shown unscaled against the fixed edge until it answers ([Resizing a window](#resizing-a-window)) | the new width on the next frame |
| Maximize / restore | the group's rect springs between its tiled rect and the work area; siblings on that display fade (`fast`) rather than being squashed | the end state on the next frame, no fade |
| Full screen | as maximize, and the chrome slides off that display's edges | as maximize |
| Switch tab | the tab chips open and close (180 ms); the content is swapped, never crossfaded | the content and chips on the next frame |
| Focus another window (Super+←/→) | the strip glides on the rect spring to frame it | the focus shows on the next frame; the viewport slides (`fast`) only when the target was off screen |
| Focus another strip | no content moves; strips share the height | the same |
| Switch workspace | a whole-view change (`slow`), gesture-triggered at the end of the swipe | the same change with only a `fast` direction cue |
| Overview in / out | each window zooms between its live rect and its card | lands at once |

The workspace switch swaps the desk's content in place today; the vertical
desk slide that should carry its direction cue is not drawn yet (#5248).

**Reduced motion** (`theme.reduce_motion`) stays the global override: it
collapses every duration to 0 and jumps springs to their targets, whatever
the source. Finger-following remains: a surface under the
hand is direct manipulation, not animation.

**Cost.** A transition changes transforms, not layout. An animated frame does
no Taffy relayout, no text shaping and no client reconfigure; it is the static
scene with other transforms and opacities. Budget: at most 1 ms of scene
building per frame above the static frame, so a 144 Hz panel (6.9 ms per
frame) keeps its cadence on an integrated GPU. Each transition is measured
headless with `TILO_FRAME_TRACE`, and its first animated frame must equal the
last static one: no jump at the start, none at the end. The keyboard rule is
held by `the_first_frame_after_a_chord_shows_the_final_layout` in
`crates/session/shell/tests/strip_navigation.rs`.

## Delivery

Small PRs, each carrying its action, chord, test and doc row together:

1. This note.
2. The strip axis: Ctrl+Super+↑ / ↓ focus strips (Super+↑ / ↓ until
   2026-10-17), Shift+Ctrl+Super+↑ / ↓ carry a group to
   a strip (a new model operation), maximize on Super+M, with a saved shortcut
   table's retired defaults following the new ones. The window-management
   operations are published as relative actions.
3. Synthetic touchpad gestures on the control socket, so gestures are driven
   and verified headless like keys; then gestures by finger count.
4. Maximize keeps the chrome; maximize, full screen and restore animate.
5. Open, close and move transitions on the rect spring, with frame-trace
   numbers. **Today** the x of every group springs when an open, close or
   reorder moves it, retargeting mid-flight from the drawn position and
   velocity, and an armed pass eases the strip's pan with it. A launch still
   cuts the pan so the new window is on screen in the frame that opens it.
   Widths (open from 0, close from the last frame, resize) and maximize are
   next; layout still runs on every animated frame.
6. Carrying a group to another workspace through the consent sheet, and the
   display level on Alt+Super.

## Open questions

1. Four-finger workspaces are vertical, because the workspace list is. macOS and
   Windows users expect horizontal. Keep vertical?
2. Decided 2026-10-17: Super+↑ / ↓ are maximize and restore, as on Windows and
   GNOME; strips moved to Ctrl+Super+↑ / ↓ and Super+1…9, and windows by
   number to Ctrl+Super+1…9. Super+M keeps toggling maximize.
3. Shift+Ctrl+Super+↓ on the last strip: create a strip (niri-like), or clamp like
   every other spatial move? This note clamps, and creating a strip stays a
   palette action.
