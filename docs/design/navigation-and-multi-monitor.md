# Navigation and multiple monitors

Status: design proposal for review, 2026-09-27. Nothing here is implemented
yet; section 5 lists the follow-up PRs. Where this page says "today" it means
`origin/main` at `1fe4a22eb6`.

Tilo's desktop is `Workspace → Strip → Window group → Tab`: groups scroll
horizontally inside a strip, strips stack vertically inside a workspace, and
workspaces are switched on the wheel disc. This page proposes one navigation
scheme for every input device (part A) and one multi-monitor model that fits
it (part B).

---

## 1. What exists today

### 1.1 Navigation

The routes below come from code, not from `docs/shell-navigation-keyboard-map.md`,
which has drifted (see 1.3).

| Level | Keyboard | Touchpad | Touch | Mouse | Controller |
|---|---|---|---|---|---|
| Tab in group | Ctrl+Tab / Ctrl+Shift+Tab (wraps) | — | — | click tab | LB / RB |
| Window along strip | Super+←/→ (walks every tab of the strip as one list, clamps; crosses display edges); Alt+Tab MRU over the whole workspace (wraps); Super+1…9 (Nth group) | 3-finger horizontal pans the strip 1:1, with no inertia and no snap | 3-finger swipe steps focus by one window | Alt+Shift+wheel; horizontal wheel over shell chrome; strip rail click | Guide+←/→ |
| Strip | Super+Shift+↑/↓ (clamps) | — | — | strip ribbon click | Guide+↑/↓ |
| Workspace | Super+PageUp/PageDown (fixed, clamps) | 3-finger vertical | 3-finger vertical | Alt+wheel; wheel disc | LT / RT |
| Overview | Super+Tab | 4-finger up/down; 3+-finger pinch | 3+-finger pinch | top-bar button | Select |
| Move tab | Super+[ / ], Super+Shift+Enter | — | drag | drag | — |
| Move group | Super+Shift+←/→ (within its display) | — | drag tab bar | drag tab bar | Guide+LB/RB |
| Move group to display | Super+Ctrl+Shift+←/→ | — | drag | drag | — |
| Move to workspace | palette `tab.move` (consent sheet), drag an overview preview onto a room | — | — | — | — |

Other facts that matter for the design:

- Super+Up maximizes the group. Super+Down is unbound, and because it is an
  editable-default shape it reaches the app.
- Every other Super chord is held back from apps even when nothing handles it,
  so Super+Shift+1…9, Super+Home/End and Super+Ctrl+arrows are silently
  swallowed today.
- An opt-in Vim layer maps Super+Ctrl+H/J/K/L. It has to use Ctrl because
  Super+H is workspace history, Super+L is lock (fixed) and Super+K is search.
- The workspace wheel (Super+S) has no keyboard navigation.
- No hot corners, edge push, drag autoscroll or shell edge swipes exist. Nothing
  moves a group to another strip or workspace by keyboard. Strips have no
  gesture.
- The F1 guide already adapts to the last-used input device
  (`preferred_hint_modality`): controller rows, touch gestures, or keys.
- Chord reservation: fixed chords, then any Super chord, then Print, F1 and
  Escape-over-chrome belong to the shell. Ctrl and Alt chords belong to apps,
  except Alt+Tab, Alt+`, Alt+F4, Ctrl+Alt+Delete, Ctrl+Alt+T and the VT keys.
  `zwp_keyboard_shortcuts_inhibit` suspends everything except the safety
  chords. The GlobalShortcuts portal can't grant a chord the shell owns.

### 1.2 Multiple monitors

In plain terms, as it works today:

1. **Every monitor shows the same workspace.** Switching workspace switches
   all of them. That matches Windows virtual desktops.
2. **Every monitor holds its own windows.** Each window group has one home
   monitor and is drawn only there. A strip is cut into one lane per monitor,
   and each lane scrolls on its own. A window never straddles a seam and is
   never drawn twice.
3. **Every monitor stacks all the strips** of the workspace, in the same
   proportions, whether or not it holds a group in them.
4. **Monitors are recognised by EDID** (make, model, serial), so a restart or
   swapped cables don't move anything.
5. **Unplugging** moves a monitor's groups onto the nearest remaining monitor,
   after its focused group. Each group remembers where it came from and goes
   back when that monitor returns, unless you moved it yourself in between.
6. **Focus** moves with the pointer, and with Super+←/→ past a monitor's first
   or last window. New windows open on the focused monitor.
7. **The partition is private to the device.** It lives in `state.json` and
   is never written to a room document.
8. **Mirroring** gives two connectors the same viewport. **Mixed DPI**: each
   output has its own scale, and a surface is told the scale of the output its
   top-left corner is on. **Presenting slides**: an app's `.audience` surface
   takes the big display, and the app window becomes the presenter view.
   Presenter focus and follow for collaborators is designed
   (`docs/presenter-focus-follow.md`) but not built.

**What works well.** "Each monitor has its own windows, and they come back
when I plug it in again" is exactly what macOS and Windows users expect. It
also avoids the hardest problems: seams, mixed DPI inside one window, and a
collaborator's screen shape leaking into anyone else's layout.

**What doesn't:**

| # | Problem | Where |
|---|---|---|
| M1 | Local partition changes leak into shared state. An unplug, or a move to another display, rewrites the strip's group order in the room document, so it reorders the strip for every collaborator. | `state.rs` `mirror_partition_reorder` |
| M2 | A mirror pair probably counts as two displays. `displays_of` removes duplicates by output id, not by rectangle, so partitioning switches on with two lanes over one image. There is no test for this. | `overlay_host/displays.rs:40` |
| M3 | The shell's primary display is the lowest `OutputId`, not the primary chosen in Settings. The wheel disc and full top bar may sit on the wrong monitor. | `displays.rs:152` |
| M4 | The arrangement canvas and Super+P "Extend" place monitors using physical mode size, while the compositor uses logical size. On a 2× laptop with a 1× monitor this leaves a gap the pointer may not be able to cross. | `apps/settings/src/panes/display.rs`, `display_mode.rs` |
| M5 | Remote cursors are published in this device's desktop coordinates, which mean nothing on a collaborator's differently shaped desktop. | `native.rs` pointer presence |
| M6 | Focus follows pointer *motion* across seams. Reaching for a button on the other monitor steals keyboard focus, and entering an empty monitor clears the strip's active group. | `overlay_host/mod.rs` `focus_display_at` |
| M7 | Every monitor reserves a band for every strip, even strips that have no windows on it. | `strip_bands` |
| M8 | Vertically stacked monitors are only reachable with Super+←/→, because reading order breaks ties on y. Up/Down never crosses a seam. | `partition.rs` `reading_order` |
| M9 | New groups, including a collaborator's, land on the focused monitor, far from their neighbours in the strip. The first time a second monitor appears, every group everywhere is homed to one monitor. | `place_unhomed` |
| M10 | Workspace, strip and window navigation have no idea of "which monitor I'm pointing at". Gestures act on the keyboard-focused monitor, not the one under the pointer. | `focus_lane` |
| M11 | There is one fullscreen slot for the whole session. | `compositor/state.rs` |
| M12 | Presenter follow says nothing about a follower who has several displays. | `presenter-focus-follow.md` |

### 1.3 Documentation drift to fix alongside

- `shell-navigation-keyboard-map.md` says Alt+Tab wraps at strip edges; it is
  MRU over the whole workspace. It also omits the move-group, move-to-display,
  resize, Vim, Super+Q and Super+Shift+/ bindings.
- The key table in `docs/architecture/compositor.md` still says
  Super+1…9 = workspace and Ctrl+Tab = strip.
- `displays-and-infinite-workspaces.md` names `display_outputs`. The code uses
  `display_profiles`.
- Comments in `overlay_host/mod.rs` and `gestures.rs` still call Super+Up a
  workspace switch.
- The F1 guide can Tab-focus the move-to-display rows, which it never draws.

---

## 2. Part A: one navigation model

### 2.1 The mental model

```text
            ↑ strip above              (Super+↑)
  ← window  [ focused window ]  window →   (Super+←/→)
            ↓ strip below              (Super+↓)

  workspace: a level beyond the screen (Ctrl+Super + any arrow)
  display:   the physical screen you are on (Alt+Super + arrow)
```

Four rules cover every device:

1. **Direction is spatial.** ←/→ is the window axis, the way groups sit in a
   strip. ↑/↓ is the strip axis, the way strips are stacked on screen. Arrows
   keep that meaning at monitor seams (part B).
2. **The modifier picks the level.** Plain Super means the plane you can see
   (windows and strips). Ctrl+Super means workspaces. Alt+Super means
   displays. The cheapest chord goes to the most frequent move.
3. **Shift takes the focused window with you.** Every "go" chord has a
   Shift twin that carries the focused group to the same place and keeps it
   focused. There are no exceptions.
4. **Finger count is the modifier on touch surfaces.** Three fingers work the
   visible plane (horizontal for windows, vertical for strips). Four fingers
   go one level out (vertical for workspaces). Pinch is the overview.

Workspaces are one-dimensional (previous or next), so at the workspace level
both arrow pairs work. Ctrl+Super+↑/↓ follows the wheel disc; Ctrl+Super+←/→
is Windows muscle memory. That is the only place two arrows mean the same
thing, and the modifier already makes it unambiguous.

**Edges.** Window and strip moves clamp: a spatial direction that wraps
around feels like teleporting. MRU lists (Alt+Tab, overview rows) wrap,
because they are lists. Workspaces clamp; the wheel disc shows the end.
Nothing overflows into the next level: Super+→ at the last window never
switches workspace, and a 3-finger swipe at the last strip never does either.
A rubber band and a one-line hint point to the next level instead.

### 2.2 Windows: frequent, easy, several equal routes

Moving to the neighbouring window has five equal routes, and all of them run
through today's `step_focus_along_strip`:

| Route | Go | Take the window along |
|---|---|---|
| Keyboard, spatial | Super+←/→ | Super+Shift+←/→ |
| Keyboard, by position | Super+1…9, Super+Home/End | Super+Shift+1…9, Super+Shift+Home/End |
| Keyboard, recent | Alt+Tab / Alt+Shift+Tab, Alt+` (same app) | — (a list has no place to carry to) |
| Touchpad or touchscreen | 3-finger swipe left/right: a direct pan that snaps to a group on release | 3-finger swipe while holding a group's tab bar (drag) |
| Mouse | Super+wheel; horizontal wheel over shell chrome; strip rail | drag the tab bar |
| Controller | LB/RB; Guide+D-pad ←/→; right stick pans | Guide+LB/RB |
| Overview | Super+Tab, then arrows and Enter | drag a preview |

Kept, and why:

- **Super+←/→ still walks tabs as one flat list.** The window that holds
  keyboard focus is a tab, and one chord that visits every focusable thing in
  order is easier to learn than separate group and tab chords. Ctrl+Tab stays
  for cycling within a group.
- **Super+1…9 stays "Nth group of the strip"**, counted in on-screen order
  across displays (left monitor first). Super+Home/End jump to the first or
  last group.

Changed:

- **Touchpad strip pans get inertia and snap to a group.** Today a pan stops
  exactly where the fingers leave it, which often leaves two half windows on
  screen. A fling carries on, and on release the strip settles on the nearest
  group edge, reusing `tick_snap_at`. The pan also sets focus to the group
  that ends up most visible, so a swipe followed by typing goes where you are
  looking.
- **A touchscreen 3-finger swipe becomes the same direct pan** instead of a
  one-window step. One gesture should behave the same on both surfaces.
- **Holding Super for 500 ms shows numbers on the groups** (1…9) and the chord
  legend for the visible plane. Releasing Super after a hold does nothing: a
  tap still opens search, and a hold is never a tap.

### 2.3 Strips: less often

| Route | Go | Take the window along |
|---|---|---|
| Keyboard | Super+↑/↓ | Super+Shift+↑/↓ (past the last strip it creates a new one) |
| Touchpad or touchscreen | 3-finger swipe up/down | drag the group onto another strip |
| Mouse | click the strip ribbon; click the strip | drag |
| Controller | Guide+D-pad ↑/↓ | Guide+LT/RT |
| Palette | "Next strip", "Previous strip", "Go to strip…" | "Move window to strip…", "Move window to a new strip" |

Moving to a strip lands on the group in the target strip that is closest
horizontally to the one you left, the way niri moves between windows in a
column. Only if that strip's remembered active group is still on screen does
it win instead. Super+Shift+↓ on the bottom strip creates a new strip, which
makes "put this beside my work, but on its own row" one chord. A new
`SessionIntent::MoveGroupToStrip` does this; today only ReorderTab and
ReorderGroup exist.

In a workspace with one strip, 3-finger vertical and Super+↑/↓ rubber-band.
The first few times this happens, a hint says "Four fingers switch
workspace" (or "Ctrl+Super+↑/↓ switches workspace"), in the modality of the
device used.

### 2.4 Workspaces: less often

| Route | Go | Take the window along |
|---|---|---|
| Keyboard | Ctrl+Super+↑/↓, Ctrl+Super+←/→, Super+PageUp/PageDown (fixed), Ctrl+Super+1…9 | Ctrl+Super+Shift+arrows, Super+Shift+PageUp/PageDown, Ctrl+Super+Shift+1…9 |
| Touchpad or touchscreen | 4-finger swipe up/down | drag onto the wheel disc (spring-loaded) |
| Mouse | Ctrl+Super+wheel; wheel over the wheel disc | drag onto the wheel disc |
| Controller | LT/RT | — (use the palette; transfers need a consent sheet) |
| Wheel disc | Super+S, then ↑/↓ and Enter (new; today only Escape works) | — |
| Palette | "Switch to {workspace}" (exists) | "Move window to {workspace}" (exists as `tab.move`) |

Workspaces are Tilo's sandbox and sharing boundary, so taking a window to
another workspace is not a cosmetic move. It always goes through the existing
window-transfer consent sheet whenever that sheet would appear for the palette
action. The chord opens the sheet and doesn't bypass it. When nothing needs
consent (same grants, a solo workspace), the move is immediate.

Ctrl+Super+1…9 selects workspaces in wheel-disc order. Super+1…9 stays with
windows: windows are the frequent level, and the cheap chord belongs there.

### 2.5 Displays: a physical fact, not a level you have to manage

A display is a slice of the window axis (part B), so Super+←/→ and
Super+Shift+←/→ already cross from one monitor to the next. Alt+Super exists
only to jump:

| Go | Take the window along |
|---|---|
| Alt+Super+←/→/↑/↓: focus the neighbouring display in that direction, on its last-focused group | Alt+Super+Shift+arrows: throw the focused group onto that display, next to its focused group |

This matches Windows (Win+Shift+←/→ moves a window to the other monitor):
Super+Shift+→ held down walks a window to the right and across the seam. The
Alt form skips the walk.

### 2.6 Other surfaces

**Overview (Super+Tab, pinch).** Keep Windows' Task View chord. Arrow keys
already walk previews and rooms. The overview grows a small map of the
workspace's strips, drawn per display, so it explains the model it is named
after (today it shows neither strips nor displays). It opens on the display
under the pointer.

**Mouse.**

- Super+wheel steps windows, and Ctrl+Super+wheel steps workspaces. Alt+wheel
  and Alt+Shift+wheel are removed, because Alt+wheel belongs to apps (GIMP,
  browsers, terminals). There is no strip wheel binding; the ribbon is a
  one-click target.
- Drag autoscroll: dragging a tab or group against a display's outer edge
  scrolls that lane, using ui-core's existing `reorder.rs` edge-scroll helper.
  Pushing the bare pointer against an edge does nothing, because accidental
  scrolling is worse than an extra gesture.
- Spring-loaded wheel disc: hovering a drag over a workspace on the disc for
  600 ms switches to it, so the drop lands there (with consent if needed).
  Today the disc accepts no drops.
- Hot corner: one optional corner, top-left of the primary display, opening
  the overview. It is **off by default**: macOS ships it off, Windows has none,
  and GNOME's on-by-default corner is its most common complaint. It never
  fires during a drag, in fullscreen, or while a game holds pointer lock.
- Back/Forward buttons stay in-app (#775).

**Touch.**

- 3 fingers pan windows and strips and 4 fingers switch workspaces, exactly as
  on the touchpad. Pinch opens the overview.
- The side screen edges stay with apps, for in-app Back.
- In tablet posture only (no keyboard attached), a swipe up from the bottom
  edge opens the overview. That is the one edge phone and tablet users
  already know.
- A touchscreen's gestures act on the display it is mapped to.

**Controller.** The Guide button is Super.

| Input | Action |
|---|---|
| LB/RB | previous/next window (same as Super+←/→) |
| LT/RT | previous/next workspace (kept) |
| Guide+D-pad | spatial: ←/→ windows, ↑/↓ strips |
| Guide+LB/RB | take the group left/right (kept) |
| Guide+LT/RT | take the group to the strip above/below |
| Right stick | pan the focused lane freely, like 3 fingers |
| Select, Start, Guide tap, X, Y, A, B | kept (overview, guide, Quick Settings, palette, Enter, Escape) |

LB/RB used to cycle tabs within a group. They now walk windows, and because
that walk already includes every tab, no separate tab control is needed. The
cheap buttons go to the more frequent move.

**Command palette.** Every action in this document is a palette action
labelled with its current chord, read from the catalogue (never declared
locally). New actions:

- Go to window… (every window in the workspace, labelled with its display
  when there are several)
- Next or previous strip
- Move window to strip… / a new strip / display… / workspace…
- Focus display…
- Present on…

### 2.7 Reserved keys

The rules stay as they are and become explicit:

1. **Super chords belong to the shell.** That includes unhandled ones, so a
   later binding never breaks an app that learned to use one.
2. **Ctrl and Alt chords belong to apps.** Tilo takes only Alt+Tab,
   Alt+Shift+Tab, Alt+`, Alt+F4, Ctrl+Alt+Delete, Ctrl+Alt+T, Ctrl+Alt+L (new,
   see 2.8) and the VT keys. This proposal returns Alt+wheel and
   Alt+Shift+wheel to apps.
3. **Ctrl+←/→ and Ctrl+↑/↓ are never captured.** They move by word and
   paragraph. This is why Tilo cannot copy macOS's Ctrl+←/→ for Spaces.
4. **Keyboard-shortcut inhibit** (VMs, remote desktops) suspends rules 1 and
   2. It never suspends the safety chords: lock, VT switch, Super+Esc and the
   release chord.
5. **The GlobalShortcuts portal** can never grant a chord covered by rule 1 or
   2.
6. **Digit chords match the physical key.** Super+1…9 and every modifier
   variant match the digit row's keycode, not the keysym. Otherwise Shift
   turns 1 into `!`, and AZERTY layouts need Shift to type a digit at all.

### 2.8 Discoverability

- **F1 and Super+Shift+/ open a guide laid out as the model.** It is a grid:
  rows are levels (window, strip, workspace, display), columns are "go" and
  "take along". It is drawn for the last-used device, as today:
  - keyboard: chords;
  - touchpad: finger diagrams;
  - touchscreen: gestures;
  - controller: buttons.
  Key labels follow the active layout (`ui::keycaps`, as today).
- **Holding Super** shows the group numbers and the plane legend (2.2).
- **Coach hints** appear only when a user does something the hard way three
  times, for example:
  - dragging a group across displays teaches Super+Shift+→;
  - a 3-finger swipe with nowhere to go teaches 4 fingers.
  They use the device's modality, appear at most once per concept per device,
  can be dismissed and are stored device-locally.
- **Palette and menus** show each action's chord inline, for example in the
  tab menu and window-group menu.

**Vim keys.** The existing opt-in setting becomes a pure alias: H/J/K/L
replace the arrows in every chord above, with the same modifiers. This
retires Super+Ctrl+H/J/K/L, since Ctrl now means workspaces. It frees
Super+L as follows: lock gains Ctrl+Alt+L as a permanent second fixed chord
(an old GNOME and KDE default), and while Vim keys are on, Super+L means
"right". Workspace history (Super+H) is reachable from the palette and the
top bar while Vim keys are on. Super+K search moves to Super+Space (fixed
already) in that mode.

### 2.9 Complete binding table (proposed)

"New" means unbound today; "changed" means it did something else.

| Chord | Action | Kind | Status |
|---|---|---|---|
| Super+← / → | focus previous/next window (tabs flattened), across displays | rebindable | kept |
| Super+Shift+← / → | take group one slot left/right, across displays | rebindable | extended across displays |
| Super+↑ / ↓ | focus strip above/below, across stacked displays | rebindable | **changed** (was maximize / unbound) |
| Super+Shift+↑ / ↓ | take group to strip above/below; past the edge makes a new strip | rebindable | **changed** (was strip focus) |
| Super+1 … 9 | focus Nth group of the strip | rebindable | kept |
| Super+Shift+1 … 9 | take group to slot N | rebindable | new |
| Super+Home / End | focus first/last group | rebindable | new |
| Super+Shift+Home / End | take group to first/last slot | rebindable | new |
| Ctrl+Super+↑ / ↓, Ctrl+Super+← / → | previous/next workspace | rebindable | new |
| Ctrl+Super+Shift+arrows | take group to previous/next workspace (consent sheet if needed) | rebindable | **changed** (←/→ was move-to-display) |
| Ctrl+Super+1 … 9 | switch to workspace N | rebindable | new |
| Ctrl+Super+Shift+1 … 9 | take group to workspace N | rebindable | new |
| Super+PageUp / PageDown | previous/next workspace | fixed | kept |
| Super+Shift+PageUp / PageDown | take group to previous/next workspace | fixed | new |
| Alt+Super+arrows | focus neighbouring display | rebindable | new |
| Alt+Super+Shift+arrows | throw group to neighbouring display | rebindable | new (replaces Ctrl+Super+Shift+←/→) |
| Alt+Tab / Alt+Shift+Tab | MRU window switcher, whole workspace, all displays | fixed | kept |
| Alt+` | MRU across the focused app's windows | fixed | kept |
| Super+Tab | overview | rebindable | kept |
| Ctrl+Tab / Ctrl+Shift+Tab | next/previous tab in group | rebindable | kept |
| Super+[ / ] | move active tab to neighbouring group | rebindable | kept |
| Super+Shift+Enter | move active tab to a new group | rebindable | kept |
| Super+M | maximize/restore group | rebindable | **new home** (was Super+↑) |
| Super+F | fullscreen | rebindable | kept |
| Super+- / = | narrower / wider | rebindable | kept |
| Super+S, then ↑/↓/Enter | workspace wheel with keyboard navigation | fixed | extended |
| Super+P | display layout | fixed | kept, with a Present entry (B.6) |
| Ctrl+Alt+L | lock (second chord) | fixed | new |
| Super held 500 ms | number and legend overlay | fixed | new |
| Super+wheel | previous/next window | — | new |
| Ctrl+Super+wheel | previous/next workspace | — | new |
| Alt+wheel, Alt+Shift+wheel | returned to apps | — | **removed** |

Everything else in `fixed_chords.rs` is unchanged.

| Gesture | Action | Status |
|---|---|---|
| 3-finger horizontal (touchpad and touch) | direct pan with inertia and snap; focus follows | pan kept, snap and inertia new, touch changed from step |
| 3-finger vertical | strip above/below | **changed** (was workspace) |
| 4-finger vertical | workspace previous/next | **changed** (was overview) |
| 4-finger horizontal | reserved | kept unassigned |
| 3+-finger pinch | overview | kept |
| bottom-edge swipe up, tablet posture | overview | new |

### 2.10 Compared with others

| | Windows | Workspaces | Carry a window | Touchpad | Kept from it | Changed |
|---|---|---|---|---|---|---|
| **niri** | Mod+←/→ columns, Mod+↑/↓ within a column | Mod+PageUp/PageDown, Mod+1…9 | Mod+Ctrl+…; Mod+Shift+Ctrl+… to monitor | 3-finger horizontal columns, vertical workspaces | scrollable strip, spatial arrows, clamp, 3-finger pan | Tilo's ↑/↓ are strips (niri: windows in a column); Shift carries (niri: Ctrl); Super+1…9 are windows, not workspaces |
| **PaperWM** | Super+,/. and arrows | Super+PageUp/PageDown | Super+Shift+… | horizontal scroll, vertical workspaces | Shift carries; PageUp/PageDown | per-monitor workspace stacks (see B) |
| **GNOME** | Alt+Tab (apps), Super+1…9 (dash apps) | Super+PageUp/PageDown, Ctrl+Alt+←/→ | Super+Shift+PageUp/PageDown; Super+Shift+←/→ to monitor | 3-finger horizontal workspaces, up overview | Super+PageUp/PageDown, Alt+`, pinch overview | hot corner off by default; Ctrl+Alt+arrows left to apps |
| **macOS Spaces** | Cmd+Tab (apps), Cmd+` | Ctrl+←/→, Ctrl+1…9 | drag to the screen edge | 3/4-finger horizontal Spaces, up Mission Control | per-app MRU (Alt+`), numbered workspaces (Ctrl+Super+N) | Ctrl+arrows stay text keys; workspaces are vertical (the wheel), so 4 fingers go vertical |
| **Windows desktops** | Alt+Tab, Win+1…9 (taskbar) | Ctrl+Win+←/→ | Win+Shift+←/→ to monitor | 4-finger horizontal desktops, 3-finger up Task View | Ctrl+Win+←/→, Super+Tab overview, Win+Shift+←/→ crossing monitors, Win+P | Win+Up maximize becomes Super+M (↑ is spatial in a strip desktop) |

**The one deliberate break from macOS and Windows** is the touchpad
workspace axis: both use horizontal 4-finger swipes, and Tilo uses vertical.
Tilo's windows already own the horizontal axis at three fingers, and its
workspaces are drawn vertically on the wheel disc. Vertical keeps "the
fingers move the thing you see" true. Section 6 lists horizontal as an
alternative for review.

---

## 3. Part B: multiple monitors

### 3.1 The target in one sentence

> **All your monitors show the same workspace, side by side like one long
> desk; each monitor holds its own windows in every strip; the arrows walk
> across monitor seams; and windows go back to their monitor when you plug it
> in again.**

That is today's partition model, kept and repaired. The alternatives were
weighed as follows:

| Option | Verdict |
|---|---|
| **Each monitor is a slice of every strip (today, kept)** | Windows-like ("desktops span all monitors") and macOS-like ("each display has its windows"). Collaboration-safe, because the split is private to the device. No seams. Left and right stay continuous. |
| Each monitor shows its own strip | Rejected. Strips are shared document structure: a collaborator with one screen would see strips appear and disappear with *your* hardware. Super+↑/↓ would mean "strip" on one desk and "monitor" on another. |
| One strip viewport spanning all monitors, windows free to cross seams | Rejected. A window across a 2× and a 1× panel has no right scale. Bezels cut windows in half. Unplugging shifts every window. |
| Each monitor pins its own workspace (macOS "separate Spaces") | **Offered as an opt-in only** (3.7). By default, one workspace means one set of people and permissions on every screen, which answers "where does my new window go and who can see it". |

### 3.2 Left and right across monitors

- A strip's order is the shared order. Each monitor shows the part of it that
  is homed there. Monitors are walked in physical order from the arrangement,
  so Super+→ past the laptop's last window lands on the external monitor's
  first window.
- ↑/↓ are strips *within* a monitor. Past the top strip of a monitor that has
  another monitor physically above it, Super+↑ goes to that monitor's bottom
  strip, on the group nearest horizontally (fixes M8). The neighbour is found
  in 2D by overlap on the perpendicular axis, not by reading order, so
  L-shaped layouts behave.
- Carrying (Super+Shift+arrow) follows exactly the same path as going. A
  window walks off one monitor's edge onto the next.
- Alt+Super+arrow jumps between monitors directly (2.5).
- **Keyboard input acts on the focused monitor.** Wheel, gestures, hot corner
  and overview act on the monitor **under the pointer**, as on macOS. Moving
  the pointer across a seam no longer moves keyboard focus (fixes M6). A click,
  a scroll-to-focus, a gesture or a keyboard move does. "Focus follows mouse"
  becomes an opt-in setting for people who want it.

### 3.3 How windows move between monitors

| Way | Result |
|---|---|
| Drag the tab bar onto the other monitor | lands in the gap you drop it in (today) |
| Super+Shift+←/→ | walks slot by slot, across the seam |
| Alt+Super+Shift+arrow | jumps next to the target monitor's focused group |
| Palette "Move window to display…" | same as the jump |
| Unplug | onto the nearest monitor, after its focused group, remembering its origin (today) |

A window keeps its **width fraction** when it changes monitor: half the laptop
becomes half the external. That is what "the same window" means across a 13"
and a 27" screen. A group you moved yourself stops belonging to its old
monitor (today's rule, kept).

**Placing new windows** (fixes M9):

- A window you open lands on the focused monitor, next to the focused group
  (today).
- A group that appears for another reason goes to the monitor that holds its
  left neighbour in the shared strip order. That covers a collaborator's
  window, a restored session, or a window opened while undocked whose
  neighbour is on the external. The window then appears next to what it was
  opened beside.
- The first time a monitor is seen, nothing moves. It starts empty, as on
  macOS and Windows. A one-time hint says "Move windows here: drag, or
  Super+Shift+→".

### 3.4 Strip bands per monitor

A monitor gives height only to the strips that have groups on it, plus the
focused strip, as a slim row you can drop into (fixes M7). Side-by-side
monitors of equal height still align, because they usually hold the same
strips. When they don't, each monitor uses its own space rather than showing
empty bands.

### 3.5 Hotplug, unplug and docking

Kept:

- Monitors are identified by EDID, not by port.
- Unplugging moves windows to the nearest monitor. Replugging brings back
  every window that still belongs to that monitor.
- Display layouts are remembered per set of monitors (`display_profiles`).

Added:

- **Plug and unplug are announced**: "3 windows moved to this screen", and
  "3 windows moved back to Dell U2720Q". The announcement uses the same toast
  as other shell receipts. Focus stays on the same window, which is scrolled
  into view wherever it now lives.
- **Closing the lid with an external monitor attached** ("clamshell") counts
  as unplugging the panel, and opening it counts as plugging it back in. Both
  use the same origin-and-return path.
- **An unknown monitor set extends** (the macOS and Windows default). The
  toast offers "Mirror instead · Super+P", and the choice is remembered for
  that set.
- **Fixes:**
  - the shell's primary follows the primary chosen in Settings (M3);
  - a mirror pair is one display to the partition (M2);
  - the arrangement editor and Super+P use logical sizes (M4).
- **The partition never writes the shared strip order** (M1). Display order
  is derived from the shared order and filtered by home monitor. Only a
  deliberate reorder within a monitor rewrites the shared order, and then only
  the relative order of the groups involved.

### 3.6 Presenting

"Presenting" means three different things, and each gets one obvious path:

1. **Presenting slides on a projector** (exists, #828). The app's audience
   surface takes the big display, and its window stays on the laptop as the
   presenter view. Added: while an audience surface is up, Tilo holds
   notifications on every display (they queue for afterwards) and never
   blanks or locks the screen.
2. **Showing a window on the projector.** Super+P gains a **Present** entry,
   which works just like "Present on…" in the palette. It moves the focused
   group to the chosen display, shows it fullscreen there without chrome, and
   turns on the same notification hold. Your other windows stay on the
   laptop, and ending the presentation brings the group back. This needs a
   fullscreen slot per display (M11).
3. **Presenting to collaborators** (`presenter-focus-follow.md`). Followers
   navigate their own desktop to the target. Added rule (M12): the follower's
   compositor scrolls whichever of *its* monitors holds the target group. If
   the target is unhomed on the follower's device, it goes to the follower's
   focused monitor. Nobody's partition is ever changed by someone else.

**Mirroring** stays output duplication: a mirror pair shares one viewport and
counts as one display. When aspect ratios differ, the smaller image is
letterboxed, never stretched.

### 3.7 Pinning a monitor to a workspace (opt-in)

This is for the "chat always on the left screen" desk. In Settings →
Displays, a monitor can be set to "Always show {workspace}". A pinned monitor:

- ignores workspace switching, and the wheel disc marks it;
- makes its workspace the input workspace while it has keyboard focus. New
  windows, the palette and Alt+Tab then use that workspace;
- is shown by the overview as a separate row.

This is deliberately last in the follow-up list (section 5) and is optional.
It is the only feature here that changes the rule "one active workspace per
device", so it needs its own review.

### 3.8 Mixed DPI

- Each monitor keeps its own scale, derived from its physical size and
  overridable per monitor (today). The goal is that text has the same physical
  size on every screen.
- Tiled windows never straddle a seam, so a window always has exactly one
  right scale. When a group changes monitor, the client gets the new preferred
  fractional scale, and the old buffer is scaled for the frame or two until it
  redraws.
- A floating surface that straddles a seam uses the scale of the monitor
  holding most of its area, not its top-left corner.
- The arrangement editor draws and snaps monitors at their **logical** size,
  so a 2× laptop next to a 1× 4K monitor looks the way the pointer
  experiences it (M4).

### 3.9 Collaboration

| Shared (room document) | Private (this device) |
|---|---|
| strips, the order of groups in each strip, tabs | which monitor holds a group, lane scroll, widths, maximize, active workspace, monitor pinning |

Consequences to state plainly:

- A collaborator with one ultrawide monitor sees your strip as one row. You
  see the same groups split across two monitors. Both of you see the same
  *order*.
- Presence, follow and remote cursors refer to groups, tabs and positions
  inside a surface, never to monitors or desktop coordinates (fixes M5).
- A collaborator's new window appears on your monitor that holds its
  neighbour (3.3). It never takes your focus.

### 3.10 Scenarios

**1. At the desk.**

- Setup: a 2× laptop on the left, a 1.5× 27" monitor to its right, and
  workspace "Thesis" with one strip. The laptop holds Chat. The monitor holds
  Editor, Browser and Terminal.
- Super+← from Editor lands on Chat on the laptop. Super+→ walks Browser,
  then Terminal, then stops.
- Super+Shift+→ on Chat carries it across the seam to become the external
  monitor's first window.
- 3 fingers on the touchpad pan whichever monitor the pointer is on.

**2. Unplug and walk to a meeting.**

- The external's three windows slide onto the laptop after Chat, and a toast
  says so. Terminal kept focus and is scrolled into view.
- Super+→ still visits Editor, Browser, Terminal in the same order.

**3. Present.**

- A projector is plugged in, and the display set is unknown, so Tilo extends
  and the toast offers Mirror.
- In Slides, Present puts the audience view on the projector and keeps the
  presenter view on the laptop. Notifications are held.
- Later, Super+P → Present shows Browser fullscreen on the projector. The
  presentation ends, Browser comes back, and notifications are delivered.

**4. Back at the desk.**

- The dock is plugged in. Editor, Browser and Terminal return to the
  external. A note opened during the meeting stays on the laptop, because it
  never lived anywhere else.
- A toast says "3 windows moved back to Dell U2720Q".

**5. A collaborator joins "Thesis" from a single tablet.** They see Chat,
Editor, Browser, Terminal and Note as one strip. When you spotlight Editor,
their strip scrolls to it. Your monitors don't change.

**6. Clamshell.** Closing the lid moves Chat and Note to the external. Opening
the lid brings them back to the laptop.

**7. Two strips, stacked monitors.**

- Setup: the monitor sits above the laptop.
- Super+↑ from the laptop's top strip lands on the monitor's bottom strip.
- Ctrl+Super+↓ switches workspace on both monitors at once.

---

## 4. Conflicts with existing chords

| Existing | Fate | Why |
|---|---|---|
| Super+↑ maximize | moves to Super+M | ↑ is the strip axis. Windows users lose Win+Up; a strip desktop has no floating windows to maximize, so it is rarely needed. |
| Super+Shift+↑/↓ strip focus | becomes "take group to strip" | Shift always carries. |
| Super+Ctrl+Shift+←/→ move to display | becomes Alt+Super+Shift+←/→; the old chord takes a group to a workspace | Ctrl means workspace; Alt means display. Plain Super+Shift+←/→ now crosses monitors anyway. |
| Super+Ctrl+H/J/K/L Vim layer | becomes Super+H/J/K/L (opt-in); lock gains Ctrl+Alt+L | one alias rule instead of a second modifier scheme |
| Alt+wheel / Alt+Shift+wheel | removed; Ctrl+Super+wheel / Super+wheel | Alt+wheel belongs to apps |
| 3-finger vertical workspace | becomes strips; workspaces move to 4 fingers | finger count is the level |
| 4-finger vertical overview | overview stays on pinch, Super+Tab and bottom-edge swipe | frees 4 fingers for workspaces |
| Controller LB/RB tabs | LB/RB step windows (the walk includes tabs) | the frequent move gets the cheap button |

Chords that are silently swallowed today are free to assign, so they break
nothing. That covers Super+Shift+1…9, Super+Home/End, Ctrl+Super+arrows,
Alt+Super+arrows and Super+Shift+PageUp/PageDown. Super+↓ reaches apps today;
taking it is consistent with rule 1 in 2.7.

Pre-alpha: no migration of user rebinding data. Rebound chords that now
collide are reset to the new defaults.

---

## 5. Follow-up PRs

Each item is a focused PR to main, in dependency order. The fixes come first
because they are bugs in today's model regardless of this proposal.

1. **fix(shell): keep display partition changes out of the shared strip order** (M1). Stop `mirror_partition_reorder`, derive display order from the shared order, and test with two devices.
2. **fix(shell): a mirror pair is one display** (M2). Dedupe by scene rectangle or `mirror_of`, with a test.
3. **fix(shell): the shell's primary follows Settings** (M3).
4. **fix(settings): arrange and extend displays in logical size** (M4).
5. **fix(shell): publish remote cursors relative to the surface** (M5).
6. **docs: resync the keyboard map, the compositor key table and the displays doc** (1.3), and fix the guide's hidden Tab stops.
7. **feat(shell): strip axis on Super+↑/↓ and carrying to strips.** Adds `SessionIntent::MoveGroupToStrip` (with new-strip-at-edge) and nearest-group landing, and moves maximize to Super+M.
8. **feat(shell): workspace level on Ctrl+Super.** Arrows, 1…9, Shift carry through the window-transfer sheet, and Super+Shift+PageUp/PageDown.
9. **feat(shell): positional window chords.** Super+Shift+1…9, Super+Home/End and Shift variants, all matched by keycode.
10. **feat(shell): display jumps on Alt+Super, and Super+Shift+←/→ across seams.** Retires Super+Ctrl+Shift+←/→ as the display chord.
11. **feat(compositor): gestures by finger count.** 3-finger vertical for strips, 4-finger vertical for workspaces, touch 3-finger direct pan, inertia and snap on release, and gestures act on the display under the pointer.
12. **feat(shell): mouse routes.** Super+wheel and Ctrl+Super+wheel (dropping Alt+wheel), drag autoscroll, the spring-loaded wheel disc and an optional hot corner.
13. **feat(shell): controller mapping** (2.6).
14. **feat(shell): keyboard navigation in the workspace wheel.**
15. **feat(shell): navigation and move actions in the palette** (2.6).
16. **feat(shell): focus stays put when the pointer crosses a seam**, with focus-follows-mouse as an opt-in (M6).
17. **feat(shell): 2D display neighbours and ↑/↓ across stacked monitors** (M8).
18. **feat(shell): per-monitor strip bands** (M7).
19. **feat(shell): new groups land beside their strip neighbour; first-seen monitors start empty with a hint** (M9).
20. **feat(shell): hotplug and lid announcements, and clamshell as unplug.**
21. **feat(compositor): a fullscreen slot per display** (M11).
22. **feat(shell): Present on a display, and hold notifications while presenting** (3.6, items 1 and 2).
23. **feat(shell): the guide as a level grid, the Super-hold overlay and coach hints** (2.8).
24. **feat(shell): Vim keys as a pure alias, and Ctrl+Alt+L lock.**
25. **feat(shell): overview map of strips per display** (2.6).
26. **feat(shell): multi-display rule for presenter follow**, once focus/follow is built (3.6, item 3).
27. **feat(shell): pin a monitor to a workspace**, opt-in and separately reviewed (3.7).

---

## 6. Open questions for review

1. **Workspace touchpad axis:** vertical 4 fingers (proposed: it matches the
   wheel and keeps one axis per level) or horizontal 4 fingers (macOS and
   Windows muscle memory)?
2. **Maximize on Super+M**, giving up Windows' Win+Up, or keep Super+↑ for
   maximize and put strips on Super+Shift+↑/↓ with carrying on
   Ctrl+Shift+Super? The second option breaks the Shift rule.
3. **Super+←/→ over tabs:** keep the flat walk through every tab (proposed),
   or step by group and leave tabs to Ctrl+Tab and LB/RB?
4. **Should pinning (3.7) exist at all**, or is "every monitor shows the active
   workspace" enough?
5. **Vim keys taking Super+L and Super+H** while on: acceptable, or should
   Vim keys stay on a separate modifier?
