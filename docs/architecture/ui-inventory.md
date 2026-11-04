# Session UI inventory

Every surface the shell draws — sheets, menus, quick settings, the bar,
banners, the lock screen, setup — is supposed to look and behave like the
apps, because it is built from the shared components they are. This page is
the stock-take before the conversion: which surfaces still lay themselves out
by hand, what each draws, which shared component replaces it, and whether that
component exists yet.

The guard is `tools/policy/check_primitives.py` (the session-surface rules
below `hand-rolled`). Each existing instance is in its baseline
(`primitives_baseline.txt`), and a count only shrinks: a surface that starts
painting its own ground, words, geometry or scrolling fails the policy suite
with the component it should use named in the message. A surface converted
from this list must lower its line.

## The rules the guard enforces

| Rule | What it refuses | Build it from |
| --- | --- | --- |
| `hand-panel` | painting a panel, card, edge or shadow itself: `RoundedRect::from_rect`/`new`, `panel_ground`, `panel_border`, `panel_shadow`, `surface::card`/`card_on`/`shadow`/`elevation`/`panel` | `ui::widget::{paint_surface, SurfaceLevel, FloatingPanelFace, FloatingPanel, ListRowFace, QuickTileFace, Button, IconButton, Card}` |
| `hand-text` | a bare text run: `.draw_text`, `.draw_style`, `.draw_text_at` | the words go in a widget: `Text`, `TextStack`, `ListRow`, `Section`, `SectionHeader`, `PropertyList`, `Button` |
| `hand-row` | a painted shape within 12 lines of a bare text run — a row, tile or field composed by hand | `ListRow`, `ListRowFace`, `ControlRow`, `PropertyList`, `DisclosureRow` |
| `hand-metric` | a written-out radius, padding, inset, gap or a `RoundedRect` corner as a number | `theme.tokens.radii`, `ui::tokens::Spacing`, `ui::tokens::Metrics`, `ui::chrome::ChromeRole` |
| `own-scroll` | a scroll position the surface keeps and clamps itself | `Scrollable` (a body), `Scroll` (a list's rows), `ScrollRegion` (a painted host) |
| `own-draw` | a `Decorator` whose `draw` paints into the scene around the widget it wraps | the shared widget draws it; a wrapper only reads input or forwards `draw` |

The rules count under `crates/session/`, except `crates/session/compositor/`:
the compositor shapes a client's buffer (a foreign window's corners and
shadow) and has no toolkit tree to compose from. `apps/` keep the older four
rules only; the shell apps (`apps/launcher`, `apps/osk`, `apps/settings`) are
already composed and are checked when they paint by hand as any app is.

A rounded rect passed to `push_layer` is a clip mask (a fade or a reveal
around the wrapped widget), not a painted panel, and is not counted.

## How to read the table

Sorted by how visible the surface is to a person: what they meet without
asking for it first (the bar, banners, the lock screen, a sheet they open)
before what they go looking for (overview, switcher, pickers, desk widgets).
The counts are this tree's baseline for that file, across the six rules. A
file with one visible surface and several helpers is one row.

## Surfaces, most visible first

| # | Surface | Files | Counts | What it draws by hand | Replace with | Exists? |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | **Top bar: status cluster (tray pill, badges, media indicator)** | `crates/session/desktop/src/topbar/tray.rs` | 11 (panel 6, text 4, row 1) | the tray pill's ground and critical-battery highlight, notification badge pill, media indicator pill, app-tray hit fills; every glyph's label painted as text | `BarControlFace` for the pill (already used for the selector), `BadgeFace`/`Avatar` `paint_overflow` for the count, `MediaTransport`/`ToastCard`-style card for the media pill, `IconWidget`/`Icon` for the glyphs | partly: `BarControlFace`, `BadgeFace.paint_overflow` exist; a **bar status-cluster component** (slots + count + media line as one control) does not |
| 2 | **Top bar: clock lane and its overlay** | `crates/session/desktop/src/topbar/clock.rs` | 8 (panel 3, text 4, row 1) | centre band's lift, the clock overlay panel and its bands, the clock's own labels | `FloatingPanelFace`/`paint_panel` for the overlay, `ListRow`/`Section` for its rows, `Text`/`TextStack` for the time, `Toggle`/`Button` for its controls | partly: `FloatingPanelFace` exists; a **clock/agenda popover** (a title, a date line, rows, a pause control) does not |
| 3 | **Top bar: people, sharing and invite chips** | `topbar/people.rs` (3), `topbar/sharing.rs` (2), `topbar/avatar_ask.rs` (2), `topbar/workspace.rs` (1), `topbar/controls.rs` (1) | 9 (panel 6, text 3) | avatar-chip pills, invite pill, watching chip, sharing and continuity pills, the ask dot, the Tilo mark's plate, focus ring | `ui::presence::FaceStack` (already used for the faces), `BarControlFace` for each pill, `Button`/`IconButton` for the ask and invite, `paint_focus_ring` for the ring | partly: `FaceStack` and `BarControlFace` exist; a **bar chip** (faces + label + press, one control) does not |
| 4 | **Top bar: strips and workspace name** | `topbar/strips.rs` (1), `topbar.rs` (3) | 4 (panel 2, text 1, metric 1) | the workspace-name field's label, the bar body's ground and border, the overlay surface a popover hangs on | `FloatingPanelFace` for the popover ground, `Text` for the field label, `Chrome`/`SurfaceRole::Bar` for the bar body (already used through `backdrop::fill_role`) | yes for the panel; the bar body has a shared `Chrome` component |
| 5 | **Notification banners (stack, toasts, "more" card, lock redaction)** | `desktop-surface/src/notifications/stack.rs` (2), `notifications/reader.rs` (3) | 5 (text 5) | the locked-session card's label, the "more" card's label, the reader page's paragraphs | `ui::widget::toast::ToastCard`/`paint_card` (already the cards' ground) for all three; the reader page from `Text`/`Section` | yes: `ToastCard`, `paint_card`; the **reader's body** needs no new component, only composition |
| 6 | **Notification centre (the retained list)** | `desktop-surface/src/notifications/center.rs` | 20 (panel 6, text 12, row 1, metric 1) | the panel's rows and groups, group chevrons and counts, the pager, the "cleared" band, the severity rail, the per-row controls | `ListRowFace` for a row (it already uses `ButtonFace` for its buttons), `SectionHeaderFace` for a group's band, `EmptyState` for the cleared state, `ScrollRegion` for the list, `Chip` for the count | mostly: `ListRowFace`, `SectionHeaderFace`, `EmptyState.paint`, `ScrollRegion` exist; a **notification-centre list** (a hosted stack with groups, paging and per-row controls) does not |
| 7 | **Lock screen** | `crates/session/sheets/src/lock.rs` | 0 | — (the one `RoundedRect` is a clip mask for the refusal shake and the idle fade, not a panel; the screen is `Text`, `TextInput`, `Button` and `PasscodeKeyboard` on `ui::auth_backdrop`) | — | yes |
| 8 | **"Join a workspace…" sheet** | `crates/session/sheets/src/directory.rs` | 0 | — (converted: `ModalDialog`, `ListRow`, `TextInput`; the decorator only reads Enter/Escape) | — | yes |
| 9 | **Sheets built on `ModalDialog`** | `sheets/src/{add_app,uninstall,portal/view,displays,shortcut_guide,workspace_settings/*}.rs` | 0 | — | — | yes |
| 10 | **Consent and question panels** | `sheets/src/display_confirm.rs`, `sheets/src/dictation.rs` | 0 | — (`FloatingPanel` + `Button`; the decorators read input only) | — | yes |
| 11 | **File chooser sheet** | `crates/session/sheets/src/file_picker/mod.rs` | 0 | — (`ui::widget::FilePicker`; the decorator reads input only) | — | yes |
| 12 | **Quick settings panel and its pages** | `crates/session/quick-settings/src/view/*.rs`, `faces.rs` | 0 | — (`FloatingPanel`, `QuickTile`, `ListRow`, `Section`, `Slider`, `Button`; `faces.rs` is plain data) | — | yes |
| 13 | **Power menu, window menu, workspace menu, Tilo menu** | `sheets/src/power.rs`, `window-menu/src/surface.rs`, `desktop/src/workspace_menu.rs`, `desktop/src/about.rs` | 0 | — (`ContextMenu`, `MenuItem`, `ModalDialog`) | — | yes |
| 14 | **Workspace settings lens (sidebar, panes, footer)** | `desktop/src/workspace_settings.rs` | 34 (panel 10, text 22, row 2) | the sidebar ground, hairline and identity plate, the tab rows, the pane intro, section headers, row cards, pills, the role cycler, captions, danger and primary buttons, the empty state, the footer | `FloatingPanelFace` for the lens, `ListRowFace`/`NavRowFace` for its tabs and rows, `SectionHeaderFace` for headers, `SegmentedFace`/`Dropdown` for the cycler, `Button`/`ButtonFace` for both buttons, `EmptyState.paint` for the empty panes, `Chip` for pills | rows, headers, buttons, segmented, empty state, chip exist; a **settings lens** (a sheet with a sidebar of tabs and scrolling panes, shared with the settings sheet) does not |
| 15 | **Workspace settings: agent access pane** | `desktop/src/workspace_settings/agent_access.rs` | 11 (text 11) | the pane's header, each app's answer row and its summary, the stop and note lines | `SectionHeaderFace`, `ListRowFace` + `SegmentedFace`/`ToggleGroup` for the answer, `TextStackFace` for the summary, `Text`/`Banner` for the note | partly: all exist; the pane composes them itself |
| 16 | **Workspace settings: general pane, members, emoji** | `desktop/src/workspace_settings/overlay.rs` (15), `workspace_settings/emoji_pane.rs`, `people_pane.rs`, `agents_pane.rs`, `default_apps.rs` | 15 (panel 5, text 10) | the lens card, nav pills, member rows, the grant composer's backdrop and marks | as #14, plus `ListRowFace` for a member and `Button`/`Toggle` for the composer | partly: a **settings lens** as above |
| 17 | **Workspace overview (Super+Tab panel) and its picture** | `desktop/src/overview_panel.rs` (5), `overview_panel/layout.rs` (6), `overview_panel/input.rs` (2), `overview_panel/paint.rs` (21), `desktop/src/overview_picture.rs` (3) | 37 (panel 10, text 13, scroll 1) | the panel's ground, edge and shadow, the "where you are" block, share rows, the go-to list, workspace rows and list rows, each tile and its strip label, the naming field, the face stack and app marks; and the shared picture's strip names, window titles and empty-strip words | `paint_surface`/`FloatingPanelFace` for the panel, `ListRowFace` for its rows, `TileHost`/`WindowPreview` for the pictured windows, `FaceStack` for the faces, `ScrollRegion` for its scroll (it clamps `scroll_y` by hand), `SearchField`/`TextInput` for the naming field | largely: `FloatingPanelFace`, `ListRowFace`, `TileHost`, `WindowPreview`, `ScrollRegion` exist; a **workspace-overview panel** (workspaces, each with pictured windows, one shared scroll) does not |
| 18 | **Window switcher (Alt+Tab)** | `desktop/src/window_switcher.rs` | 9 (panel 7, text 2) | the panel, its cards and their wells, the hover ring, the hint line | `FloatingPanelFace` for the panel, `Card`/`WindowWell` for a card and its window, `KeyHintsFace` for the hints, `Text` for labels | partly: `FloatingPanelFace`, `Card`, `WindowWell`, `KeyHintsFace` exist; a **window switcher card** (a well with a title and an app mark, lifted when chosen) does not |
| 19 | **Emoji picker** | `desktop/src/emoji_picker.rs` | 11 (panel 6, text 4, row 1) | the card's ground and edge, group segments, the grid's cells and their selection, the tone row | `FloatingPanelFace` for the card, `SearchField` (already used) and `ToggleGroup`/`SegmentedFace` for groups, `Card`/`ButtonFace` for a cell, `Popover` for the tone row | mostly: the picker's **grid cell** (a glyph, its name and its selection) as a shared component does not exist; `apps/osk`'s emoji panel draws the same thing and should share it |
| 20 | **Desk widgets (team, calendar, monitor, music)** | `desktop/src/desk_widgets/paint.rs` | 29 (panel 15, text 13, row 1) | each widget's glance ground, its open card's header, team rows, calendar grid, monitor readings, music art and transport, the grip, the pin and remove controls, the badge | `Card`/`ListRowFace` for the open card, `FaceStack` for the team, `DataTable`/`StatTile` for the monitor, `MediaTransport` for music, `ButtonFace`/`BarControlFace` for the controls, `Card`'s grip | partly: `Card`, `FaceStack`, `StatTile`, `MediaTransport` exist; the **desk-widget glance/expand frame** (a glance that becomes a card at a size stage) is specific to the shell and has no shared component |
| 21 | **People overlay and the peek** | `desktop/src/people_overlay.rs` (5), `people_overlay/peek.rs` (6) | 11 (panel 4, text 5, metric 2) | the overlay's pinned rows, the peek's card, its picture well, its badge and its Follow button | `FloatingPanelFace` and `ListRowFace` (already used for the roster), `WindowPreview` for the pictured windows, `ButtonFace` for Follow, `BadgeFace` for the count | partly: the roster is converted; the **peek** (a small picture of another person's screen with a caption and Follow) has no shared component |
| 22 | **Collaborator roster** | `desktop/src/collaborators.rs` | 7 (text 2, scroll 5) | the roster's own scroll offset (computed and clamped itself); two labels | `ScrollRegion` for the list (the rows are already `ListRowFace`), `Text` for the labels | yes: `ScrollRegion` |
| 23 | **Strip switcher and strip navigator** | `desktop/src/strip_switcher.rs` (2), `strip_navigator.rs` (7) | 9 (panel 1, text 1, scroll 7) | a label's ground, a label's text, the ribbon's own scroll position | `paint_surface`/`ChromeRole::Control` for the label, `Text` for its name, `ScrollRegion` for the rail | partly: the label should be a row (`ListRowFace` with `rail()`); a **strip rail/ribbon** exists as `ui::chrome::shell::{StripRail, StripRibbon}` |
| 24 | **Launchpad (first use)** | `desktop/src/launchpad.rs` | 6 (panel 1, text 5) | the empty workspace's cards' titles, subtitles and hints | `Card`, `TextStackFace`, `KeyHintsFace`, `EmptyState`; the "Open an app" button already uses `ButtonFace` | yes |
| 25 | **Hardware readout (OSD)** | `desktop/src/osd.rs` | 6 (panel 3, text 3) | the pill's level bar, its glyph, its name and its value | `ProgressBar`/`paint_level_gauge` for the level, `Icon` for the glyph, `TextStack` for the words | partly: `paint_level_gauge` exists; an **OSD pill** (a glyph, a device name, a value and a level bar on the floating material) does not |
| 26 | **Display "which is which" cards** | `desktop/src/display_identify.rs` | 2 (text 2) | each display's number and name (the card ground already comes from `floating_surface::paint_card`) | `Text`/`TextStack` on a `FloatingPanelFace` | yes |
| 27 | **Feedback picker** | `desktop/src/feedback_picker.rs` | 3 (panel 1, text 2) | the dim, the picked part's outline, the hint pill | `paint_see_through` for the bar (already), `Chip`/`Banner` for the hint, `KeyHintsFace` for its chords | partly: a **pick outline** (a lit part's bounds over the dim) is drawing, not chrome, and stays a painting surface |
| 28 | **Full-screen hint** | `desktop/src/fullscreen_hint.rs` | 1 (text 1) | the "press Esc" line over the window | `Text`/`KeyHintsFace` on the floating material | yes |
| 29 | **Drill-in pages (Wi-Fi, Bluetooth, VPN, sound, devices)** | `desktop-surface/src/drill_in.rs` | 6 (panel 2, text 4) | the shared control ground and its focus ring, list rows, the empty state's words | `ListRowFace`/`ControlRow` for the rows, `EmptyState.paint` for the empty state, `ButtonFace` for the back control | yes; `drill_in.rs` is exactly the kind of local copy the rules retire: it is the Quick Settings list language, drawn once for the panel and once for the notification centre |
| 30 | **Notifications over the strip** | `desktop-surface/src/surface/notices.rs` (1), `surface/arrivals.rs` (3) | 4 (panel 3, text 1) | the arrival hint's pill, its presence dot and its text | `Chip`/`Banner` for the hint, `Avatar`'s presence ring for the dot | partly: an **arrival hint** (a person's colour, a dot, a sentence, an arrow) has no shared component |
| 31 | **Hung-app card** | `desktop-surface/src/surface/hung.rs` | 2 (text 2) | its heading and body text (the card ground already comes from `floating_surface::paint_card`) | `TextStack` and `Text`; the two buttons already use `ButtonFace` | yes |
| 32 | **Strip paging and the desktop surface's own scroll** | `desktop-surface/src/surface/paging.rs` (2), `surface.rs` (2) | 4 (scroll 4) | the strip's pan position, clamped itself | `ScrollRegion` (a horizontal region) or `ui::motion` for the pan | yes: `ScrollRegion` |
| 33 | **Workspace title and panel geometry** | `desktop/src/composition.rs` (1) | 1 (metric 1) | a `with_gap(0.0)` on a column that should name its rhythm | `Rhythm`/`Spacing` | yes |
| 34 | **Onboarding pages** | `crates/session/onboarding/src/compose.rs` (2) | 2 (metric 2) | two written-out gaps in otherwise-composed pages | `Rhythm`/`Spacing` | yes |
| 35 | **Onboarding ceremony backdrop** | `crates/session/onboarding/src/ceremony.rs` | 0 | the dim and the two blooms are the backdrop itself, not chrome | `ui::backdrop` if it gains a ceremony backdrop; `auth_backdrop` covers the authentication screens | partly |
| 36 | **Capture bar** | `crates/session/capture-studio/src/surface.rs` | 1 (text 1) | one label on the see-through material | `Text` on `paint_see_through` (already used for the ground) | yes |

## Converted (no count against them)

These surfaces already compose the shared components and are the model for the
rest; they show that "the session's chrome is made of the same parts" is
reachable, not aspirational:

- the sheets (`ModalDialog`, `FloatingPanel`, `ListRow`, `TextInput`,
  `EmptyState`, `Section`, `Toggle`), the file chooser (`FilePicker`), the
  lock screen and the greeter (`Text`, `TextInput`, `Button`, `PasscodeKeyboard`,
  `auth_backdrop`);
- quick settings (`FloatingPanel`, `QuickTile`, `ListRow`, `Section`, `Slider`,
  `Toggle`, `Button`) and the whole top bar surface, banners surface, menus and
  the tray host, which compose the desktop's components;
- the collaborator roster and the people overlay's list (already
  `FloatingPanelFace` and `ListRowFace`), the about and workspace menus
  (`ContextMenu`).

## Shared components that do not exist yet

Each is named with the API a surface needs; none is built in this change.
Where two surfaces draw the same thing (the overview and the switcher both
picture a window; the emoji picker and `apps/osk` both draw a glyph grid), one
component serves both.

1. **Modal sheet with a body slot** — `ModalSheet` exists for painted hosts;
   `ModalDialog` covers composed ones. Neither offers a header search slot, a
   body with its own scroll and a footer of more than three actions, which
   the sheet a surface with a long list needs (a workspace directory, a
   chooser). API needed: `ModalDialog::search(field)`, `ModalDialog::body_scrolls()`,
   `ModalDialog::footer_action(&str, ModalActionKind)`. **File chooser, the
   directory sheet and the settings lens are the callers.**
2. **Settings lens** — `SettingsLens::new(tabs) -> Element`, `SettingsLens::page(tab, sections)`,
   `SettingsLens::footer(actions)`: a sheet with a sidebar of tabs, scrolling
   panes of `Section`s and one footer, on `FloatingPanelFace`. Replaces
   `workspace_settings.rs`'s hand-built sidebar, tab rows, section headers,
   row cards, cycler and footer, and is shared with the workspace settings
   sheet (`crates/session/sheets/src/workspace_settings/`), which draws the
   same shape composed. API: `SettingsLensTab { id, icon, label, count }`.
3. **Bar chip** — `BarChip::new(id, label).faces(stack).icon(icon).on_press(..)`:
   one bar control that holds a face stack, a label and a press, on
   `BarControlFace`. Replaces the people, invite, watching, sharing and
   continuity pills and the tray cluster's shape.
4. **Bar status cluster** — `StatusCluster::new().slot(StatusSlot::Bell { count }).slot(StatusSlot::Volume(..)).slot(StatusSlot::Network(..)).slot(StatusSlot::Battery(..))`:
   the tray pill as one control with square slots, a badge and the
   critical-battery highlight. Replaces `topbar/tray.rs`.
5. **Window picture card** — `PreviewCard::new(window_id).title(..).app_mark(..).chosen(bool).hint(&[KeyHint])`:
   the well, its edge, its title, its app mark and its lift, shared by the
   window switcher, the overview's tiles and the people peek (which needs
   `PeekCard` = `PreviewCard` with a caption and an action). API:
   `PreviewCard::peek(caption, action)`.
6. **Workspace overview panel** — `OverviewPanel::new(workspaces).selected(..).scroll(ScrollRegion)`:
   each workspace a name, its strip labels and its window cards, with one
   `ScrollRegion`. Replaces `desktop/src/overview_panel/`.
7. **Emoji grid cell** — `EmojiCell::new(name, glyph).chosen(bool).secondary(line).remove(action)`:
   the cell the picker and `apps/osk`'s emoji panel both draw. API:
   `EmojiGrid::new(groups).search(SearchField).filter(query)`.
8. **Notification centre list** — `NotificationList::new(rows).groups().page(..).empty(EmptyState)`:
   grouped rows with a per-row control set, paging and a cleared band, on
   `ListRowFace` and `SectionHeaderFace`, with a `ScrollRegion`. Replaces
   `desktop-surface/src/notifications/center.rs`.
9. **OSD pill** — `Readout::new(icon, name).value(label).level(f64).tone(Tone)`,
   on the floating material, with `paint_level_gauge`. Replaces
   `desktop/src/osd.rs`.
10. **Desk widget frame** — `DeskWidget::new(id).glance(child).card(child).stage(Stage)`:
    the glance-to-card behaviour and the frame a desk widget's card stands on.
    Replaces the frame in `desktop/src/desk_widgets/paint.rs`; the cards'
    contents then use `FaceStack`, `StatTile` and `MediaTransport`.
11. **Quick Settings list language** — promote `desktop-surface/src/drill_in.rs`
    (`control_fill`, `control_radius`, `paint_control`, `DrillInList`,
    `draw_empty`) into `ui::widget` as `QuickList`/`QuickRow`, so the panel,
    the notification centre and any settings page share one list, and the
    local copy goes.
12. **Arrival hint** — `ArrivalHint::new(text).presence(rgb).action(..)`:
    the pill at a strip's edge saying who opened what. Replaces
    `desktop-surface/src/surface/arrivals.rs`'s `draw_hint`.
13. **Clock/agenda popover** — `AgendaPopover::new(now, date).rows(runs).pause(action)`:
    the clock's overlay, on `FloatingPanelFace` with `ListRow`s.
14. **Row-level field** — `SearchField` exists, but a sheet's embedded field
    (`FloatingPanelFace`'s header field) and the strip's naming field
    (`strip_switcher::draw_field`) draw the same thing by hand; a
    `FieldFace`-backed embedded text widget on `FieldFace` is needed, or the
    existing `FieldFace` adopted at both sites.

## How this list shrinks

A surface is converted when its file's baseline lines are lowered (or the
lines drop out) and the shared component it uses is the one named in its row;
a component lands in `crates/sdk/ui-core` with its own catalogue entry
(`crates/dev/ui-graph/components.json`) and tests, and the local copies above
are deleted in the same change. A new surface that hand-lays itself fails
`tools/policy/run.sh` before it can land.
