# Desktop parity: the old shell against the new desktop

Status: parity record, 2026-11-04.

The old shell (`crates/session/shell`, crate `shell`) was removed from `main`
in `5e0e173af2e`; its source is in git history at `5e0e173af2e^`. Everything
the person sees today is the new desktop:

- the session's desk, `crates/session/session/src/desk.rs`, which places
  `tilo_desktop::desktop_host::DeskLayer` over
  `tilo_desktop_surface::DesktopSurface` (`crates/session/desktop-surface`);
- the bar, menus and overlays of `crates/session/desktop`;
- the surface apps of the session: `tilo-topbar`, `tilo-quick-settings`,
  `tilo-lock-screen`, the notification banners and the sheets.

This page compares the two behaviour by behaviour, from the code. Rows name
the old file and function, what the new code has, and where the two stand.
"Old" is `crates/session/shell/src/…` unless it says otherwise.

## How to read a status

- **same** — the same behaviour, from a new file.
- **better** — the new code does more, or is more correct.
- **worse** — the behaviour exists but moves less, shows less or is harder to
  reach.
- **missing** — nothing in the new code does it.

## The two facts that explain most of the table

1. **The new session draws the portable surface, not
   `crates/session/desktop/src/composition.rs`.** `session/src/desk.rs` calls
   `tilo_surface_host::desktop_with`, which builds
   `tilo_desktop_surface::DesktopSurface`. That surface shows **one strip at
   a time**: `show_active_strip` swaps `self.shown` and marks its own
   structure dirty, so a strip or workspace change replaces the child row in
   the layout. The old motion lived in two widgets, `WorkspaceView` and
   `StripDeck`, built only by `desktop/src/composition.rs`; that file and the
   widgets are gone, and `surface/slide.rs` draws the change instead (§9).
2. **The old shell animated both.** Workspaces slid through
   `overlay_host/workspace_slide.rs` (`Drill::WORKSPACE`, parked workspaces,
   the leaving workspace drawn at a pose), and strips slid through the deck:
   each strip was a full-height screen in a `StripDeck`, and `StripDeck::show`
   set the deck's position on `motion::Snap`. Both were reachable by pointer,
   key and gesture.

`docs/architecture/window-management.md` §Motion is the contract the new code
should meet, and it already records part of the gap: "The workspace switch
swaps the desk's content in place today; the vertical desk slide that should
carry its direction cue is not drawn yet."

## 1. Workspace and strip navigation, and its animations

| Behaviour | Old | New | Status |
|---|---|---|---|
| Switch workspace by pointer (menu, banner) slides the whole view: the shown workspace leaves along the shared axis while the chosen one arrives from the side it lies on; only windows move, the bar and backdrop stay; input live from the first frame | `overlay_host/workspace_slide.rs` `WorkspaceSlide::start/poses`, `Drill::WORKSPACE` (`Slow` toward later, `Base` back), parked content drawn by `draw_leaving_workspace`, consumed by `paint_desktop::draw_workspace_sections` | `surface/slide.rs` poses the workspace shown before away on `Drill::WORKSPACE` while the chosen one is already laid out | same (see §9) |
| Switch workspace by key lands | `workspace_slide.rs` test `a_pointer_switch_slides_the_windows_and_a_key_lands`; `MotionSource::Keyboard` | `ShortcutAction::Previous/NextWorkspace` → `ChromeIntent::SwitchWorkspace(±1)` → `ChromeEffect::SwitchWorkspace`; no motion | same |
| Switch workspace by four fingers: gesture-triggered at the end of the swipe | `compositor` `gestures.rs` `swipe_action` → `switch_workspace`; `native.rs` `apply_shell_gesture` sets `MotionSource::Gesture`, which slides | `compositor/src/gestures.rs` identical; `session/src/keys.rs` `follow_gestures` → `chrome switch-workspace`; the change lands, because nothing draws a slide for a gesture either | worse → **fixed** (§9) |
| Strip deck: every strip laid at the deck's whole size, only the shown one drawn and taking input; going to another strip slides the deck | `StripDeck` (`begin_drag`/`drag_to`/`end_drag`, `Snap::SPEC`, `FingerPath` resampling, `COMMIT_SHARE` a quarter of the deck) | `surface/slide.rs`: the shown strip is laid out and takes input while the one before it is posed away | same (§9) |
| Switch strip by key (Ctrl+Super+↑/↓, Super+1..9, controller): `Snap` within `fast` | `desktop_host.rs` `go_to_strip` → `ShowStrip`; `composition.rs` `deck.show(display, index, source)` | `go_to_strip` → `ChromeEffect::ShowStrip` → `show_active_strip` swaps the row | worse → **fixed** (§9) |
| Switch strip by clicking the strip switcher / ribbon | `overlay_host/strip_switcher.rs`, `strip_rail.rs`; the click calls `state.focus_strip` and the deck slides | `desktop/src/strip_switcher.rs`, `strip_navigator.rs`, `strip_indicators.rs` (the sliding indicator moves on `ListChange::MOVE`); the switch itself jumped | worse → **fixed** (§9) |
| Three fingers up/down move the deck with the hand; release settles with momentum | `update_strip_deck_drag` → `StripDeck::begin_drag/drag_to/end_drag`; commit past a quarter or a flick | `GestureTracker` emits `StripDeckUpdate` per update; `surface-host`'s gesture reader hands every update to `DesktopSurface` | same (§9) |
| Horizontal strip pan follows three fingers 1:1 with rubber band and coast | `GroupRow::update_strip_pan` driven by `StripPanUpdate` | `GestureReader` drops the stream into `self.travel` and steps at the end; `DesktopSurface` has `route_swipe` (wheel and trackpad scroll) but no gesture-driven pan. `paging.rs` uses `update_strip_pan` for a wheel step | worse (still) |
| Focused window framed; a keyboard focus change slides the viewport only when the target was off screen, at `fast` | `navigation.rs` `begin_transition`, `scroll_group_into_view_animated` | `desktop/src/navigation.rs` `apply_navigation` (built only behind `composition.rs`); `desktop-surface` `scroll_group_into_view` jumps | worse |
| Strip indicator ribbon and rail fade and slide between strips | `StripIndicators` `Transient::CONFIRMATION`, `SlidingFocus::follow` | `desktop/src/strip_indicators.rs` same | same |

## 2. Window movement and resize

These come from the same widgets in both: the portable surface keeps
`GroupRow` / `WindowGroup`, so the rect springs, open and close, resize and
reorder are literally the same code the old shell drew.

| Behaviour | Old | New | Status |
|---|---|---|---|
| A group's drawn rect springs to its laid-out rect (`springs.heavy`, interruptible) | `window_group_frame_spring.rs` `FrameSpring` on `WindowMotion::MOVE` | same widgets in `desktop-surface` | same |
| Open a window: width grows from 0, neighbours slide aside, content fades and scales 0.96→1 | `WindowGroup::start_spawn_animation`, `TileHost::spawn_progress` (`WindowMotion::OPEN`) | `desktop-surface/src/surface/open_motion_tests.rs` | same |
| Close: drawn from the client's last frame, reverse of open | `WindowGroup::start_closing` | `desktop-surface/src/surface/close_motion_tests.rs` (`the_window_right_of_a_closed_one_slides_into_its_place`) | same |
| Resize by an edge: direct manipulation, fixed opposite edge, size chip, outline lit | `group_row_edges.rs` `RowEdgeDrag`, `edge_resize.rs` | same | same |
| Move a group (reorder, between strips, between displays): springs to its new rect, neighbours close and open the gap | `GroupRow::record_shift_anim`, `window_move.rs` | same; a cross-strip move still slides the desk by `deck.show` | same |
| Maximize / restore: rect spring, siblings fade at `fast`; full screen as maximize with the chrome off | `WindowGroup::toggle_maximized`, `sync_sibling_fade` | same | same |
| Tab chips open and close (180 ms) | `tabs.rs` `close_fade` | same | same |
| Drag a tab across groups; a drag resting at an edge springs the strip across; a background tab comes forward after a dwell | `GroupRow` `DragReach`, `DRAG_SPRING_LOAD` | same | same |
| Held (another person's) window resists a drag and springs back | `GroupRow::resisted` | same | same |

## 3. Quick Settings

The panel is `crates/session/quick-settings`, an app of its own. The **app**
is the same generation in both: the old shell mounted it in-process
(`overlay_host/quick_settings.rs` `QuickSettingsPanel::mount`,
`QUICK_SETTINGS_WINDOW`), the new session starts it as a surface app
(`session/src/quick_settings.rs` `follow_quick_settings`, `Run::Standing`).
What differs is the host around it.

| Behaviour | Old | New | Status |
|---|---|---|---|
| Tiles: Wi-Fi (or the wired link), Bluetooth, VPN, night light, airplane, mobile data, do-not-disturb, rotation lock, power mode | `overlay_host/qs_state.rs` + `settings_store::QuickSettingsControl` (9 controls, `Wifi` required) | `quick-settings/src/faces.rs` `tiles`, `available`, `tile_on`, `tile_switch`, `section_of` — same nine, same order, same ids (`qs-tile-{id}`) | same |
| Arrangement editor: which tiles show, in the person's order, never dropping Wi-Fi | `settings_store::hardware::QuickSettingsConfig`, `view/edit.rs` | same, `Parts::Arrangement`, `qs-edit-*` | same |
| Sliders: brightness (one, or one per display, focused display first), volume with mute and the sound-output chevron | `qs_state.rs` `apply_fast_qs_reading`; `faces.rs` `Sliders::of` | `quick-settings/src/faces.rs` `Sliders::of`, same rows and ids; `qs-input-level`, `qs-stream-{node}-gain` for the microphone and streams | same |
| Sections: Wi-Fi networks with the password prompt and hidden-network form, Bluetooth accessories and nearby devices, VPN profiles, power modes, battery, sound outputs, the person's devices, the clipboard | `qs_device_rows.rs`, `qs_device_input.rs`, `faces::wifi/bluetooth/…` | `quick-settings/src/view/sections.rs`, `pages.rs`; `QsSection::ALL` (Sound, Wifi, Bluetooth, Vpn, PowerMode, Battery, Devices, Clipboard) | same |
| A section opened **in place**, unrolling under its tile on the disclosure motion | `DisclosureBand`, `qs-section-*` | each section **replaces the controls page**, `PageSlide` back and forth (`view/mod.rs` `panel`) | worse (a whole page per section instead of a band under it) |
| Notifications page (grouped by app, fold, actions, dismiss, clear with undo, hand to an agent) and one notification read in full | `overlay_host/quick_settings.rs` `quick_settings_notifications`, `view/pages.rs` | `view/pages.rs` `notifications`, `reader` | same |
| Panel material: floating surface, one inset, hairline, shadow | shell painted the card (`ui::surface::shadow`, `Material::Floating`, `ChromeRole::Panel.radius` 18), `quick_settings_settled_bounds` damage | `FloatingPanel` card with one inset (`83ca704d858`); damage left to the general frame | same |
| Panel appears from the tray (`Appear`) and its height follows what it shows (`Extent`) | `overlay_host/qs_motion.rs` `QsMotion` (presence, height, card side) | no `QsMotion`: the session places the window and shows it at once | **missing** |
| Panel stands under the bar at the trailing end, 380 wide | `QsPanelMetrics` 360 × text growth, margin `s2` | `desktop/src/overlays.rs` `Slot::QuickSettings` `Anchor::TopTrailing(8)`, `POPOVER_WIDTH` 380 | same (360 → 380) |
| The now-playing card beside or under the panel, changing sides by fading | `qs_motion.rs` `follow_card`, `CardSide` | the card is part of the panel's page (`view/now_playing.rs`) | worse |
| Every press acts or says why it could not | `quick_settings_dispatch.rs` validation, radio tickets, rollbacks | `quick-settings/src/live.rs` `run` returns a refusal the view shows (`ef644235281`); `verbs.rs` maps each effect to a resource action | better |
| Reads the device off the frame thread | `quick_settings_refresh.rs` tiered `FastReading`/`FieldReading` | `live.rs` worker thread, `session.rs` `Watch` over 15 resources | same |
| The panel runs as its own process | in-process mount | surface app started on open | better (isolation) / worse (first frame later) |
| Keyboard: Escape goes back then closes; Tab walks the controls; F1 to the guide; a press outside closes | `route_quick_settings_event` | Escape closes the slot; Tab and the other chords are the app's own; press-outside is the overlay stack's | worse |
| Opened at the caret to paste (`Super+V`) | `PasteOrigin`, `clipboard_paste_rect` | `PasteOrigin` survives in `state.rs`; nothing in the session places the panel at a caret | worse |

## 4. Top bar

| Behaviour | Old | New | Status |
|---|---|---|---|
| One bar 32 high: Tilo mark (opt-in), workspace pill and menu, strip name, the time or a live activity, the continuity chip, arrivals, waves, faces and invite, sharing, media use, app tray, own avatar, status cluster | `topbar_chrome.rs` + `overlay_host/topbar_host.rs` (mounted in-process); widgets in `crates/session/desktop/src/topbar*` | the same widgets, `crates/session/desktop/src/topbar.rs` and submodules, mounted as the `tilo-topbar` surface app | same |
| Workspace menu: switch, join, leave, rename in place, overview, new strip | `chrome_state` menus | `desktop/src/workspace_menu.rs` `WorkspaceMenuModel` | same |
| Strip ribbon and rail, on-screen windows lit, focus sliding | `overlay_host/strip_rail.rs` | `desktop/src/strip_indicators.rs`, `strip_switcher.rs`, `strip_navigator.rs` | same |
| Clock with Clocks' live activity over it, floating clock overlay with its controls | `topbar_chrome.rs` `ClockPoll`, `CLOCK_ACTIVITY_REFRESH` 250 ms | the bar's pill, reading every activity (`desktop/src/topbar/clock.rs`, `chrome::Activities`), and the floating layer's cards with the controls (`crates/session/activities`) | better (one layer for every activity; nothing floats over content) |
| Presence: faces, agents, spectator count, invite, the person's own card | `topbar_chrome.rs`, `desktop/src/topbar/people.rs` | `desktop/src/topbar/people.rs`, `avatar_ask.rs`, `profile_card.rs` | same |
| Status cluster: bell, volume, network, battery, and the bell alone with no Quick Settings (`TrayBell::only`) | `topbar/controls.rs` `TrayBell` | same, `TrayBell::only` intact | same |
| Bar reveals over a window covering it (Super, top-edge touch) | `reveal.rs` | `desktop/src/topbar/reveal.rs` `Reveals` | same |
| A compact bar on every display but the primary | `topbar_placement()` | `desktop/src/topbar/space.rs`, `desktop-surface` lanes | same |

## 5. Notifications

| Behaviour | Old | New | Status |
|---|---|---|---|
| Banner stack: top-right, springs to its slot, hover-to-hold with an aim delay, at most three, the rest folding into `+N more from the app` which opens the centre, never ages while held | `desktop-surface/src/notifications/stack.rs`, mounted by `overlay_host/banners.rs` | the same crate; `DesktopNotifications` installed by `session/src/desk.rs` `set_notifications` | better (the corner never fills; a fold leads to the one list) |
| Banners below window bars, shifting when a bar arrives under them | `stack.rs` `transient_slot`, `clear_top` | `DesktopNotifications` + `surface/notices.rs` `set_clear_top` | same |
| Notification centre: groups by app, one scrolling list, per-row actions, reader, agent hand-off, clear with an 8 s undo | `notifications/center.rs`, `history.rs`, `reader.rs` | `notifications/center.rs`, and the one door to it: the bell and the Quick Settings notifications row both open the desk's centre (`quick-settings/src/verbs.rs` `ShowCentre`) | better (one list, no paging; the panel keeps no second copy) |
| Quiet: Do Not Disturb, presenting, sharing, a focus schedule; banners held with a line saying why | `chrome_state` `notifications_quiet`, `QuietReason` | `NotificationsView::quiet`, `qs-notifications-quiet` | same |
| Machine-wide banners on the other displays; a locked session shows a redacted count | `stack.rs` `draw_machine_wide`, `draw_locked_card` | `DesktopNotifications::show_banners_elsewhere`; `surface/notices.rs` | same |
| Banners answer a call, reply inline, act on their sender | `banners.rs` `Ask::Banner/Reply/AnswerCall`; `notification.rs` `actions_for` | `notices.rs` `answer`, `DesktopNotifications::take_outcomes` | same |
| The chord moves focus into the newest banner (Super+A) | `handle_banner_keyboard`, `banner_owns_key` | the notification layer's own keys; the chord reaches the centre | worse (no focus-into-banner chord found) |

## 6. Window switcher, launcher and search

| Behaviour | Old | New | Status |
|---|---|---|---|
| Alt+Tab: MRU order, a quick flick switches without showing, a held Alt reveals the panel after 120 ms, Shift reverses, Escape cancels | `window_switcher.rs` (`REVEAL_DELAY` 120, `HISTORY_LIMIT` 256), `overlay_host/switcher_layer.rs` | `desktop/src/window_switcher.rs`, `session/src/overlays.rs` `REVEAL_DELAY`, `session/src/switcher.rs` | same |
| Switcher previews: live client picture, painted capture, withheld, or the app's mark in a 16:10 well; pages to the selection | `switcher_windows.rs`, `PreviewWell` | `window_flight.rs`, `desktop/src/window_switcher.rs` | same |
| Alt+` walks the app in front's windows | `SwitcherScope::FocusedApp` | `SwitcherScope::App` | same |
| Super palette: one card, field, filter band, scope token, parameter steps, secondary actions, a run row that says why it failed | `overlay_host/palette_*`, `search_state.rs`, `palette_flow.rs` | `desktop-surface/src/surface/search.rs`, `desktop/src/search_overlay.rs`, `desktop-search` | same |
| Empty palette: Happening now, Waiting on you, Continue, Apps (most used), All apps | `search/launcher.rs` | `desktop-search/src/palette.rs` (`the_empty_palette_leads_with_who_is_here`) | same |
| Scopes: this app, this workspace, everywhere | `SearchScope::{ActiveApp, Workspace, All}` | `ui::search::Scope` in the surface | same |
| Held Super peeks the field alone, then grows; a chord cancels the quiet stage | `input_policy.rs` (`SUPER_HOLD` 300, `SUPER_FULL` 600) | `desktop/src/shortcuts.rs` `page_chord` | same |
| A row runs the app's own action inside its window | `overlay_host/search` | `desktop-surface` `search_owners`, `unrouted_control` | better |

## 7. Lock screen, multi-display and touch

| Behaviour | Old | New | Status |
|---|---|---|---|
| Lock screen: clock, the person, one password field, refuse paste, verify on a worker, the unlock action, the refusal shake, idle blanking | `compositor/lock_screen.rs`, `lock_controller.rs`, `lock_auth.rs`, `lock_actions.rs`, and the `so.tilo.lock` client (`sheets/src/lock.rs`) | `crates/session/lock-screen` and `crates/session/sheets/src/lock.rs`, started as the session's lock surface | same |
| Media card on the lock: title, subtitle, previous/play/next, disabled what the player cannot do | `native/lock_media.rs` | `sheets/src/lock.rs` `Media` | same |
| Display identify: a numbered card on every display, in the corner the focused window leaves free, for 3 s | `shell/display_identify.rs` | `desktop/src/display_identify.rs` | same |
| Focused display, pointer moving focus between displays, menus and panels opening on the focused display | `overlay_host/displays.rs` (652-line module) | `desktop-surface/src/surface/displays.rs` `lanes()`, `display_at()`, `focus_display_at`; `desktop/src/overlays.rs` `display_for` | same |
| Each display shows a strip of its own and moves between strips alone | `strip_rail.rs`, `focus_strip` per display, `show_strip_on(display, strip)` | `lay_out_displays` places lanes, but `show_active_strip` keys off `tree.active_strip_id()`, so every display shows the same strip | **missing** |
| A window moved to another display, and a group carried across the seam | `move_focused_group_to_display`, `carry_focused_group_across_seam` | `desktop:desk` `move-to-display`; `surface/displays.rs` | same |
| Touch: a tap is one click where the finger was, a hold is a context menu, a drag on an edge resizes, the top edge reveals the bar | `compositor/touch_pointer.rs` `TouchPointer`, `ui-core/src/interaction.rs` `TouchPointerEmulator` | the same unchanged | same |
| On-screen keyboard: follows touch typing and tablet mode | `compositor/input_method.rs` `osk_wanted` | unchanged | same |
| Touchpad: three fingers the plane, four workspaces, a pinch the overview | `compositor/gestures.rs` | `compositor/src/gestures.rs`, unchanged; the **host** side lost the direct stream (§1) | worse (host only) |

## 8. The gaps, worst first

1. **No workspace slide** (fixed in §9).
2. **No strip slide** (fixed in §9).
3. **Three-finger strip and window gestures did not follow the fingers** (fixed
   in §9 for the deck; the horizontal strip pan is still stepped).
4. **The four-finger workspace switch had no direction cue** (fixed in §9).
5. **Quick Settings has no entrance and no height motion.** The old panel
   appeared from the tray and grew with what it showed; the new one is placed
   and shown at once.
6. **A Quick Settings section replaces the whole page** instead of unrolling
   in place under its tile.
7. **No per-display strip lanes**: two displays show the same strip.
8. **The now-playing card** is a page element instead of the panel's
   companion beside or under it.
9. **`Super+V` no longer opens the clipboard at the caret.**
10. **Touchpad horizontal strip pan** is stepped at release instead of
    following the fingers.
11. **Panel started on open**, so its first frame is later than an
    in-process mount's.
12. **No chord moves focus into the newest banner.**
13. **Focus framing on a keyboard focus change** jumps where the old shell
    slid the viewport within `fast`.

## 9. What this branch changed

**The strip and workspace slide (§1).** `desktop-surface` now draws the
change a person makes with a hand: `surface/slide.rs` holds the strip shown
before at its last laid-out place and poses it away on `Snap` (a strip step)
or `Drill::WORKSPACE` (a workspace step) while the strip chosen is already the
one laid out and taking input. A pointer and a finger slide; a key and an
action move within `fast`, as the design note says, and a change nobody made
still lands. Three fingers carry the deck: `surface-host`'s gesture reader
used to keep only the total and step once at release, and now hands every
update to the surface. Tests: `surface/slide_tests.rs` (a click slides across
frames and never cuts, a key's slide is over within `fast`, a finger moves the
strips one to one with its travel, a second change carries on from where the
first is drawn, a workspace switch travels the shared axis' 28 px).

**Quick Settings at scale (§3).** `quick-settings/tests/scale.rs` lays the
panel out at 2× and drives every tile, the volume mute, every slider and
every section through the targets the layout put them at, asserting the
effect that changes the device. `tests/session.rs` draws the panel on a
2944×1840 output at 2× and proves it paints its 380 logical pixels as 760
physical ones, inside the output. What the scale work found and this branch
did **not** change: the panel draws at the right density already (the
compositor hands a layer surface its logical rect and the client's buffer is
2×), so no compositor change was needed.

Still open, in the order the person meets them: the entrance motion above
(the panel still appears at once), a Quick Settings section replacing the page
rather than unrolling under its tile, the now-playing card, per-display strip
lanes, the horizontal strip pan following the fingers, and focus framing.
