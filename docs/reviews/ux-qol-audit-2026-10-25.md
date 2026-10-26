# UX / QoL audit — missing affordances, motion and visual consistency

Audit base: `origin/main` @ `3203631d96` (worktree `ux-qol-pass`), 2026-10-25.

This is a source + rendered-frame audit, not a claim that every device, every theme and
every locale has been exercised. It is a **product/UX** list, deliberately separate from
`docs/hand-rolled-ui-audit.md` (which tracks app-side toolkit migration) and from
`docs/ux/desktop-affordances.md` (which tracks focused-tab operations). Where a finding
overlaps either document it says so.

## How this was produced

1. **Source review** across `crates/sdk/ui-core` (toolkit), `crates/session/{shell,desktop,
   desktop-surface,desktop-model}` (compositor + chrome) and all 71 directories under `apps/`.
   Every finding cites `file:line` and was checked against the surrounding code; absences were
   grepped for before being claimed.
2. **Cross-check against the repo's own ledgers** — `docs/hand-rolled-ui-audit.md`,
   `docs/ux/desktop-affordances.md`, `docs/reviews/live-path-audit.md` — so this list adds
   rather than restates.
3. **Visual review of real rendered frames** (production offscreen renderer, not mockups):
   `crates/session/shell/tests/snapshots/e2e/*` and `apps/{settings,monitor,passwords,maps,
   git-client,calculator}/shots/*`. Frame-specific observations are in **Part F** and are
   labelled as coming from a capture, because a capture cannot show motion, hover or latency.
4. **Motion inventory**: every `Appearing`/`Presence`/`Disclose`/`MotionValue` call site, plus
   the places that paint a state change with no interpolation. Part C.

## Severity key

| Severity | Meaning |
|---|---|
| **blocker** | A person cannot complete a task, or the system tells them something false about their data. |
| **notable** | A common task is much harder than it should be, or two surfaces disagree in a way a person notices. |
| **medium** | A refinement most people would hit weekly; wrong-feeling but recoverable. |
| **minor** | Polish. Noticeable to a careful eye; cheap to fix. |

Each finding is numbered so it can be referenced from a PR. Prefixes: `SHELL-`, `CORE-`,
`MOTION-`, `CHROME-` (app-drawn chrome vs the system bar), `DUP-` (cross-app duplication),
`VIS-` (rendered-frame), `APP-<app>-`.

## Coverage

Parts A–E and G cover all 71 entries under `apps/` plus the shell, desktop and toolkit.
The per-app coverage table is in **Appendix A**; **Appendix B** lists the things that were
checked and found genuinely well handled, so the list is not read as "everything is broken".

## Contents

| Part | Subject | Findings |
|---|---|---|
| 0 | Executive summary — the twelve that matter | — |
| **A** | Shell, window manager and system chrome | 17 (`SHELL-*`) |
| **B** | `ui-core`, the shared toolkit | 15 (`CORE-*`) |
| **C** | Motion, animation and visual polish | 17 (`MOTION-*`) + per-app motion measurements |
| **D** | Structural: app-drawn chrome, toolkit bypass | 7 (`CHROME-*`) + the shell-bypass table |
| **E** | Cross-app duplication | 9 (`DUP-*`) |
| **F** | Visual review of rendered frames | 7 verified (`VIS-*`); 6 of 7 old-artifact leads killed |
| **G** | Individual apps, by domain (G1–G5) | ~75 (`APP-<app>-*`) |
| **H** | Prioritised backlog, 6 tiers | 75 ranked items |
| Appendix A | Coverage: every app, chrome class, depth | — |
| Appendix B | What is genuinely well handled | — |
| Appendix C | What this audit did **not** check | — |

**170 distinct finding ids.** By severity tag: **1 blocker**, 54 notable, 109 medium, 63 minor
(some ids carry two findings, and the positive `VIS-04`/`VIS-07` entries are not problems).

## Part 0 — Executive summary

If only twelve things get done, these are the twelve. They are ordered by
(person impact × how many surfaces it fixes) ÷ effort.

| # | Finding | Why first |
|---|---|---|
| 1 | **`ToolbarLocation::edited` carries a live value in only two apps** (`DUP-04`) | Seven document apps show no unsaved-work marker at all. It is one boolean per app, already painted, already announced. |
| 2 | **Only two apps opt into the framework's close admission** (`DUP-04`) | Save / Discard / Keep editing *and* retry-on-save-failure exist in `ui-core`; the rest of the document apps hand-roll a prompt and dead-end on failure. |
| 3 | **db-client and api-tester cannot cancel a running request** (`APP-db-client-02`, `APP-api-tester-01`) | Both apps are *fully* blocked with no way out but killing the window. |
| 4 | **Files has no search of the current folder** (`APP-file-explorer-01`) | 5 000-file folders are navigable only by scrolling or first-letter typeahead. |
| 5 | **Logs "follow" never follows** (`APP-logs-01`) | The app's headline feature visibly does nothing. |
| 6 | **Calendar month grid has no hover state and no transition when paging** (`MOTION-06`, `MOTION-07`) | ~30 % of that app's surface is dead under the pointer, and its one direct-manipulation gesture (swipe) has no follow-through. |
| 7 | **Six shortcut sheets, four of which print chords that are wrong on the person's keyboard** (`DUP-03`) | The keyboard reference is the one place a person goes to learn a chord. Two omit rows the others show. |
| 8 | **Nine zoom clusters, six meanings for Ctrl+0** (`DUP-02`) | Muscle memory is wrong in five of six windows; `ContentZoom` exists and exactly one app implements it. |
| 9 | **The autocomplete suggestion list and the display-change card appear with no motion at all** (`MOTION-02`, `MOTION-03`) | Every neighbouring surface animates; these two pop. One of them is a destructive-flow confirmation. |
| 10 | **The lock screen has a hand-rolled credential buffer with no IME** (`SHELL-01`) | A passphrase typed through an IME cannot be entered — a hard lock-out, not a nicety. |
| 11 | **Firewall promises enforcement it never performs, and does not say so** (`APP-firewall-01`) | Safety-relevant: the person closes the app believing rules are live. |
| 12 | **A narrow window silently deletes ~22 calculator keys** (`VIS-01`) | Verified by rendering at this commit: the pad drops to 4 columns with no overflow key, no hint and no way back to the scientific layout. The same shape will exist in any app that picks a compact layout by width alone. |

Two structural findings sit behind several of the above and are worth their own line:

- **`MOTION-01`: the gesture vocabulary is applied unevenly** — the catalogue is excellent and
  gate-enforced, but eleven surfaces were never wired to it, so the same action animates in one
  place and cuts in another. Details in Part C.
- **`CHROME-01`: 3 of 70 apps draw a header the shared window bar is supposed to carry**
  (`ebook`, `typst-editor`, `canvas`), and five more declare nothing so their bar carries only a
  title. The mechanism works and 53 apps use it; the exceptions are what a person sees as
  "this app feels different".

---

# Part A — Shell, window manager and system chrome

The shell is the biggest *adopter* of the toolkit in the tree — the top bar, quick settings,
window menu, power menu, notification banners/centre, overview, file chooser and onboarding
are all on shared widgets (Appendix B). Its gaps are therefore narrower and sharper than
expected: they cluster in security surfaces, in bar chrome that bypasses `control_targets()`,
and in attention/undo feedback.

## A1. Security and credentials

### SHELL-01 — blocker — lock screen hand-rolls its credential field, so IME input is impossible

`crates/session/shell/src/compositor/lock_controller.rs:85` keeps `buffer: String`, backspace
is one grapheme truncate (`:141-145`), and `shell/input_policy.rs:2556-2560` maps exactly five
keys: `Enter`, `Escape`, `Backspace`, `Space`, `Character`. The whole event conversion bails
out unless the event is `KeyboardInput`:

```rust
// crates/session/shell/src/shell/input_policy.rs:2549-2551
let WindowEvent::KeyboardInput { .. } = event else { return LockKey::Ignored; };
```

There is no IME path to the lock buffer at all: `grep -rni "ime\|text_input\|input_method"` over
`compositor/lock_controller.rs` and `compositor/lock_screen.rs` returns nothing (every hit is
`time`/`dimensions`), so `WindowEvent::Ime(Ime::Commit)` — which is not a `KeyboardInput` — never
reaches the buffer. The field itself is painted by hand (`compositor/lock_screen.rs:1472`,
`:1533`), and `windows_password.rs`'s cousin in quick settings shows what the toolkit path looks
like.

**Impact.** A passphrase entered through an IME (Japanese, Chinese, Korean, and any layout
composed that way) cannot be typed at the lock screen: the person is locked out of their own
machine. Secondary: `Delete` does nothing, `Ctrl+Backspace` deletes one character not one word,
there is no selection, no caret motion and no paste.

**Fix.** Adopt `ui::widget::TextInput::with_secret(true)`, which already exists and is adopted by
imager and db-client, inside the lock's existing retained tree. `SecretPrompt` with
`Presentation::Inline` is the working model two files away (`overlay_host/wifi_password.rs:165`)
— it routes real pointer, IME and key events. **Prerequisite:** `TextInput` has no
clipboard-off switch (grep for `with_clipboard`/`no_paste` in `ui-core/src` → nothing), and
lock-screen paste is a policy question; add an edit-policy option rather than keeping the
`String`.

### SHELL-02 — notable — a refused focus request is silently dropped and nothing marks a window that wants attention

`compositor/state.rs:6556-6566` logs `"refusing xdg-activation focus request"` at debug level and
returns. `shell/tab_binding.rs:288-291` deliberately refuses to steal the strip:

```rust
// Front the tab, but move focus no further: stealing the strip because somebody else
// opened something is exactly what the room model rules out.
group.activate_tab(target);
```

There is no attention concept to carry the news: `WindowMenuFacts`
(`shell/window_menu.rs:115-125`) has no attention field, `TabBar`/`WindowGroup` have no such
field, and grep finds no X11 `WM_HINTS.urgency` handling.

**Impact.** The classic pair: "my click did nothing" and "where did my window go". A long build
finishing and asking for input is invisible; a link opened from a background app raises a window
in another strip with no signal.

**Fix.** Add `Tab::attention`; set it when `evaluate_activation_request` refuses, when an X11
client sets urgency, and when a parented dialog maps into a non-active strip/workspace. Paint it
as a dot on the tab pill (`Badge`/`BadgeFace` already exists and is unused by the strip); clear on
focus; offer "Go to this window" in the window menu; one-shot OSD line for the panned-off case.

### SHELL-03 — medium — app tray icons are pointer-only: no hover, no keyboard, no a11y node

`crates/session/desktop/src/topbar.rs:3563` (`draw_app_tray`) fills an image and a warning dot.
The hit rects are stored at `:3610` and consulted at `:3715`, but `control_targets()`
(`:1764`) — the list that drives keyboard focus *and* the accessibility tree — never includes a
tray item, and `accessibility()` (`:4556`) maps `control_targets()` one node per control.

**Impact.** Assistive technology sees a `Toolbar` with no tray children; Tab cannot reach the
icons; there is no hover or press state, while the pill 40 px away gets
`paint_bar_selector(… SelectionState::new(false, hovered, false) …)` (`:2534-2544`).

**Fix.** A `ui::widget::BarControlFace` per icon plus a `control_targets()`/`control_rect()`/
`control_name()` entry each — the treatment `CONTROL_MEDIA` already gets. Escape hatch exists
(tray items are published as palette actions, `shell/search/providers/tray_items.rs`), which is
why this is medium and not notable.

### SHELL-04 — medium — desk widgets have no keyboard path and no accessibility at all

`shell/overlay_host/desk.rs:727-731` accepts only `MouseInput{Left}`; `grep` finds zero
`AccessNode` and zero `a11y::Role` in the file. Opening, closing and pinning a desk widget is
pointer-only, and the pin is a bare rect (`:773-777`).

**Fix.** `PaintedTargets` + `AccessNode` per slot (the pattern `notifications/stack.rs` already
proves), and paint the pin through `ButtonFace`/`ToggleFace`.

## A2. Window management

### SHELL-05 — medium — closing a whole window offers no undo, though the close *is* undoable and workspace deletion does offer it

`shell/window_actions.rs:607-637` — `close_group` captures the tabs
(`closed_tabs::capture_group`) and calls `remember_closed_tab_after_removal(entry)`, then
returns. Nothing is pushed to the person. Contrast deleting a workspace, which is far more
destructive, in `shell/chrome_state.rs:2691-2706`:

```rust
toast.dismiss_after = crate::shell::closed_tabs::UNDO_WINDOW;
toast.actions = vec![("default".into(), t("workspace-deleted-undo"))];
self.push_notification_full(toast);
```

**Impact.** "Close window" from the window menu (`window_menu.rs:344-350`) takes five tabs and
the desktop says nothing; the person assumes the work is gone. The chord that would restore it
(`Super+Shift+T`) is only discoverable from the guide.

**Fix.** Push the same 8-second undo toast from `close_group`, labelled with the group's active
title ("Closed 5 tabs — Undo").

### SHELL-06 — medium — no way to equalise or preset window widths from the UI

`shell/overlay_host/tree_harvest.rs:586-589` has `set_group_widths(permille)`, and the model has
presets (`desktop-model/src/width.rs:128-135` `Narrow | Normal | Wide | Fraction`), but the only
caller of `set_group_widths` is the test-control door (`compositor/native.rs:5287-5288`), and the
window menu offers no width rows. The keyboard path is a crude ±50 ‰ step
(`overlay_host/keyboard.rs:494-495`).

**Impact.** After dragging one seam, two windows are 43 %/57 % with no "reset to equal" and no
"make this half". Recovery is a pixel-precise drag or many chord presses.

**Fix.** Window-menu rows — Half width / Narrow / Normal / Wide / Equalise all — bound to
`set_group_widths(1000/count)` and `set_width(WindowWidth::…)`; snap the pointer drag to halves
and thirds within a few pixels.

### SHELL-07 — minor — stepping past the first/last workspace does nothing at all, not even a repaint

`shell/chrome_state.rs:3565-3581` returns `changed = false` at the end of the list and does not
`bump()`; `switch_strip` (`:3336-3341`) saturates the same way. The chord is still swallowed as
handled (`overlay_host/keyboard.rs:300-320`).

**Impact.** The person cannot distinguish "I'm at the end" from "the chord didn't register" from
"this app ate my key" — and on a single-workspace desktop the chord is a permanent no-op.

**Fix.** On a refused step, flash the workspace chip / show the brief OSD the shell already has
(`ChromeState::acknowledge`, `chrome_state.rs:3591-3596`).

### SHELL-08 — minor — the window menu cannot move a window to another display, though a chord can

`shell/window_menu.rs:233-254` (`move_rows`) offers only `MoveToStrip` and `MoveToWorkspace`, and
`WindowMenuFacts` has no displays. The capability exists:
`overlay_host/displays.rs:684 move_focused_group_to_display`.

**Impact.** The menu enumerates every strip and workspace — the discoverable "where can this go?"
list — but not the second panel sitting on the desk.

**Fix.** `WindowMenuAction::MoveToDisplay(OutputId)` + a submenu named from
`settings_store::display_identify::display_names`.

### SHELL-09 — medium — six chrome text fields are painted by `TextInput` but do not accept a pointer press

`crates/session/desktop/src/text_entry.rs:1-16` documents the split, and three sites prove the
pointer does nothing:

```rust
// crates/session/desktop/src/strip_switcher.rs:883
Target::Field => return Press::Consumed,
// crates/session/shell/src/shell/directory_sheet.rs:536
Target::Search => DirectoryOutcome::consumed(),
```

with `Hit::Field` falling into `_ => {}` in `emoji_picker.rs:1106-1125`. The six sites:
`strip_switcher.rs:1411`, `topbar/strips.rs:205`, `emoji_picker.rs:1006`,
`directory_sheet.rs:457`, `overlay_host/cursor_chat.rs:412`, `overlay_host/overview_grid.rs:1142`.

**Impact.** These fields *look* like text fields and accept typing, but click-to-place-caret,
drag-select, double-click word select and the right-click edit menu are all missing. A person who
clicks mid-word to fix a typo watches the caret stay where it was.

**Fix.** Forward pointer events into the `TextInput` these owners already hold (cheapest), or give
`TextInput` a `LocalState`-style binding so it can own the model outright.

## A3. Chrome and bar affordances

### SHELL-10 — minor — the shortcut guide is invisible to assistive technology as anything but one flat string

`shell/overlay_host/accessibility.rs:321-369` publishes a single `Role::Dialog` whose description
is a pre-joined `"label: chord"` list, and `focus_accessibility` (`:456`) returns nothing for
`LayerId::ShortcutHelp`. Meanwhile the ~20 chord rows are real rebind targets for pointer
(`shortcut_help.rs:589-600`) and keyboard (`:618-629`).

**Impact.** A screen-reader user can hear the shortcuts but cannot rebind one, and the Done button
is unrepresented. The menu (`tab_menu.rs:348-419`) shows the shape this should take.

### SHELL-11 — minor — lock-screen media transport is AT-clickable but keyboard-unreachable

`compositor/lock_screen.rs:699-710` publishes per-transport `AccessNode`s with `Click`, and
`compositor/native.rs:11164-11171` honours them. `shell/input_policy.rs:2556-2567` maps no `Tab`
and no arrows. Switch-access, which synthesises `Tab` (`native.rs:4643`), is therefore also
excluded.

### SHELL-12 — medium — the projection ("cast") sheet has no hover state anywhere

`shell/projection_overlay.rs:1930-1948` paints a keyboard focus ring and a focus tier, and
`:970-973` swallows `CursorMoved` as `consumed(false)`; the word "hover" does not occur in the
file. Every other shell list lifts the row under the pointer
(`quick_settings/sections.rs:756`, `overview_paint.rs:1093`).

### SHELL-13 — minor — three shell surfaces paint `ButtonFace` without `.hovered()`

- `shell/directory_sheet.rs:806-816` — and the sheet never routes cursor moves at all
  (`:363-388` is press-only; `grep -c hover` → 0). This is the "Join" button of the room list.
- `shell/overlay_host/shortcut_help.rs:262-267` — Done.
- `shell/display_confirm.rs:134-152` — Keep / Revert, the affirmative buttons of a countdown.

The neighbours in the same files do pass it (`paint_top.rs:508-514`, `qs_device_rows.rs:237`,
`app_notice_card.rs:222`), which is what makes these three stand out.

### SHELL-14 — minor — `desktop-surface` bypasses the shared scrim

`crates/session/desktop-surface/src/surface/search.rs:720`:
`ui::vello::peniko::Color::from_rgba8(0, 0, 0, 110)` — a literal scrim, in a tree whose own
module doc says "A scrim is never a literal colour and never an opaque fill"
(`ui::overlay::scrim`). The shell does it correctly at `overlay_host/mod.rs:1563`.

## A4. Notifications

### SHELL-15 — notable — notifications carry no app identity: the icon argument is discarded

`shell/notifications.rs:1600` binds the freedesktop `app_icon` argument to `_app_icon` and never
reads the `image-data`/`image-path`/`desktop-entry` hints; `shell/notification.rs:327-378`
(`from_incoming`) never calls `.with_icon_kind(…)`; the default
(`desktop-surface/src/notifications/model.rs:46-57`) is `Generic => None`, i.e. no glyph. Painted
at `stack.rs:714`.

**Impact.** Every third-party banner looks identical. In a stack of three, the first question a
banner must answer — which app wants me — is unanswered.

**Fix.** Carry `app_icon`/`image-data`/`desktop-entry` through `NotifyRequest` and resolve the
verified `app_id` through `crate::app_icons` (the lookup `ShellWindows::app_icon` already uses);
keep the wire's `app_icon` as a lower-trust fallback (`TrayIcon::Letter` establishes that
pattern).

### SHELL-16 — medium — a notification from an app the shell did not launch has no name and is filed as "System"

`shell/notification.rs:344-348` deliberately drops the sender-controlled `app_name` and shows only
launcher-derived attribution; `shell/notifications.rs:1429-1438` proves `app_id` only for Flatpak
callers; `desktop-surface/src/notifications/history.rs:25-31` then falls back to
`t("notification-group-system")`, and "Open" on such a row is `HistoryActivation::Unavailable`
(`stack.rs:1272-1276`).

**Impact.** Scripts, CLI tools, self-started daemons, Wine/X11 apps and anything launched from a
terminal collapse into one indistinguishable "System" pile, and clicking the row cannot take you
back to it.

**Fix.** Attribute by process as well as by sandbox — the tray already has this proof
(`tray::owner_resolver` → `verified_flatpak_app_id(pid)`
`.or_else(verified_package_for_descendant(pid))`). Where attribution still fails, show the
sender's own claim explicitly ("App says: Foo").

### SHELL-17 — medium — per-app notification badge counts are collected and filtered and never drawn

`shell/chrome_state.rs:3977-3983 notification_badge_for()` has one caller, a test
(`shell/notification.rs:1390`). What is drawn is the aggregate total
(`topbar_chrome.rs:243` → the bell). The tray item gets a status dot but never a number
(`desktop/src/topbar.rs:3592-3603`).

**Impact.** The bell says "12" with no way to tell which app owns them, and a per-app `badges`
policy toggle changes a number that is never rendered — the setting reads as broken.

**Fix.** Paint `notification_badge_for(app_id)` on the app's tray item and its tab pills
(`Badge`/`BadgeFace` already clamps at 99).

---

# Part B — `ui-core`: the shared toolkit

The toolkit is the highest-leverage place in this list: one fix here lands in every app. It is
also in better shape than the audit's premise assumed — focus-visible, pointer-target minimums,
slider keyboard models, text-selection granularity, table keyboard navigation, disabled-row
skipping and empty-state text are all done properly (Appendix B). The gaps below are the
remaining sharp edges.

## B1. Keyboard vocabulary

### CORE-01 — notable — the file picker has no Home/End/PageUp/PageDown and no Ctrl+A select-all

`widget/file_picker.rs:4290-4295` — the *only* list-navigation arms are `ArrowDown` and
`ArrowUp`. The Ctrl chord table above (`:4248-4257`) handles only `"l"` and `"f"`, so `Ctrl+A`
falls through to `_ => return`; the Places column (`:4267-4272`) is likewise arrow-only.

**Impact.** In a 400-file folder, End and PageDown do nothing, and multi-select
(`with_multiple`, `:947`) is reachable only by Ctrl/Shift-clicking. Nine app crates use
`FilePicker` today (Backups, Publisher, Imager, Photos, PDF-viewer, Print-centre, Store, Tube…).

**Fix.** Add the four paging keys reusing `move_selection` with a page length from the visible
row count, and a `Ctrl/Cmd+A` arm calling `select_all()` when `multiple`.

### CORE-02 — notable — date picker PageUp/PageDown page the *view* but not the *selection*, so Enter commits an off-screen date

`widget/date_picker.rs:517-523` binds PageUp/PageDown to `retreat_month`/`advance_month`, and
`:173-187` documents that these deliberately do **not** move the selection. The only "where am I"
cue is the selection disc (`:749-757`), which is only painted for the six visible weeks.

**Impact.** Page forward twice from a January date and nothing on screen is selected; `Enter`
(`:525`) then commits the invisible January date. Arrow keys behave the opposite way (they move
both, `:223-231`), so the two key families disagree with each other.

**Fix.** Move the selection by one month (same day-of-month, clamped) via `commit_user_date`, as
GTK's `GtkCalendar` and `<input type=date>` do; keep the plain view flip on the header chevrons.

### CORE-03 — notable — autocomplete swallows Escape even when no suggestion list is open, and blurs the field

`widget/autocomplete.rs:1143-1146`:

```rust
Key::Named(NamedKey::Escape) => { self.cancel(); *ctx.handled = true; }
```

and `cancel()` (`:1043-1047`) both closes the list and does `self.input.set_focus(false)`. There
is no `if self.open` guard (contrast `search_field.rs:815`, which guards on a non-empty value);
the only gate above is `if !self.is_editing() { return; }` (`:975-977`).

**Impact.** In a dialog containing an autocomplete field, the first Escape does not dismiss the
dialog — it only removes the caret, and `*ctx.handled = true` stops the dialog from seeing it.
People learn to press Escape twice.

**Fix.** Guard the arm on `is_open()` and let a bare Escape fall through to the host.

### CORE-04 — medium — tree view Shift+Home / Shift+End move focus without extending the selection

`widget/tree_view.rs:2690-2695`:

```rust
Key::Named(NamedKey::Home) => self.focus_at(0),
Key::Named(NamedKey::End) => self.index.len().checked_sub(1).is_some_and(|index| self.focus_at(index)),
```

`on_key_inner` receives `toggle`/`extend` (`:2626`, called with `extend = modifiers.shift_key()`
at `:4499`) and every other movement applies them — arrows (`:2650-2680`), PageUp/Down
(`:2688-2689`). Home/End ignore both.

**Impact.** Shift+End should select everything to the bottom (Finder, Explorer, Dolphin,
`GtkTreeView`); here focus jumps and the selection stays, so the next Shift+Arrow over-selects
from the wrong anchor. Affects Backups (`view.rs:901`), Canvas layers
(`panels/layers.rs:176`) and Tasks (`list.rs:97`).

### CORE-05 — minor — context menus have no Home/End and no type-ahead

`widget/context_menu.rs:1730-1760` handles Escape, ArrowDown/Up (`move_hover`), Enter and
ArrowRight, then `_ => {}`. `move_hover` (`:1200-1222`) is careful — it skips separators,
captions and disabled rows — but nothing consumes `Key::Character`, although
`crate::type_ahead` exists and both `Dropdown` (`dropdown.rs:1281`) and `TreeView`
(`tree_view.rs:2705`) use it.

**Impact.** A 15-item right-click menu (browsers' tab menus, editor context menus — 73 app files
reference `context_menu`) cannot be reached from the bottom or jumped to by typing a letter.

### CORE-06 — minor — an open `Dropdown` list supports only ArrowUp/ArrowDown

`widget/dropdown.rs:1250-1270`. Type-ahead and filtering are already correct (`:1272-1295`), but a
long option list (timezone, font, language, currency) must be walked one row at a time.

**Fix.** Home/End → `0` / `n-1`, PageUp/PageDown → `highlight ± visible_rows`, then
`ensure_visible`. 49 app crates construct a `Dropdown`.

### CORE-07 — minor — `NumberInput` steps by one only; no PageUp/PageDown, no Home/End

`widget/number_input.rs:980-1000` binds ArrowUp/ArrowDown to `self.step`. `Slider` in the same
toolkit binds PageUp/Down and Home/End (`slider.rs:699-709`); a GTK spin button pages by its page
increment.

**Impact.** Reaching 500 with `step = 1` needs 500 presses, and there is no keyboard way to jump
to min or max.

## B2. Pointer, targets and hover

### CORE-08 — medium — `SearchField`'s clear button is a hardcoded 22×22 target and ignores pointer precision

`widget/search_field.rs:44-46` (`CLEAR_GUTTER = 28.0`, `CLEAR_D = 14.0`) with
`:729-733` `self.clear_rect = cr.inflate(4.0, 4.0)` → 22 logical px, hit-tested at `:564`.
`SearchField` contains no `InteractionEnvironment` at all, while `Button` grows every hit rect to
the environment minimum (`button.rs:1533-1540` → `interaction.rs:216-221`: 32 px fine, **48 px
coarse**) and `TextInput`'s clear accessory is a full field-height square
(`text_input.rs:4001-4018`).

**Impact.** The most-clicked glyph in a filter box is the smallest target on screen, and on a
touchscreen it is under half the toolkit's own declared minimum. Settings, Chat and the OSK emoji
panel use it.

### CORE-09 — medium — the coarse-pointer 48 px minimum is declared but not applied by tabs, date pickers or context menus

`InteractionEnvironment::minimum_target()` returns 48 for `PointerPrecision::Coarse`
(`interaction.rs:216-221`); `Button`/`ListRow`/`Chip`/`Dropdown` honour it. These do not:
`tabs.rs:3148`/`:3874` clamp to the theme token (32) or a constant; `date_picker.rs:340-341`
(`CELL_SIZE: f64 = 32.0`, `CHEVRON_WIDTH: f64 = 32.0`); `context_menu.rs:1238` (row height 32);
`TabBar` has no interaction field at all.

**Impact.** On a convertible/touchscreen, day cells, menu rows and tab chips stay finger-hostile
at 32 px while the buttons around them grow to 48. The toolkit already has
`minimum_collection_row()` (`interaction.rs:270-276`) for exactly this.

### CORE-10 — minor → notable in aggregate — a tab cannot be middle-clicked to close, and no keyboard close exists for a non-active tab

`widget/tabs.rs:4204-4238` handles Right (context menu) and Left only; `MouseButton::Middle` does
not appear in `tabs.rs` at all. `handle_keyboard_navigation` (`:2603-2624`) binds arrows and
Home/End, with no `Delete`/`Ctrl+F4`.

**Impact.** Middle-click-close is muscle memory in every browser, editor and terminal; here the
press is not even handled, so it falls through to the app. Closing tab 5 of 9 requires activating
it first. Blast radius: every tabbed app plus the shell's own window bar.

### CORE-11 — medium — a disabled control can never explain why it is unavailable

`widget/button.rs:364-382` has one string for the accessible name, the hover tooltip and the menu
label; `grep -rn "disabled_reason\|disabled_hint\|why_disabled"` in `ui-core/src` → nothing.
Meanwhile disabled widgets are dropped from the tab order entirely
(`focus_navigation.rs:82`: `if widget.is_focusable() && widget.is_enabled()`).

**Impact.** The classic dead end: "Save" is greyed out, the pointer user sees a tooltip that just
repeats the label, and a keyboard user cannot even reach the control. Nothing says a required
field is empty. Tooltips *do* still fire on disabled widgets
(`widget/mod.rs:1796-1808`), so the one available channel is wasted.

**Fix.** `Widget::disabled_reason() -> Option<&str>`; `TooltipHost` prefers it while
`!is_enabled()`; append it to the accessible description.

## B3. Text and forms

### CORE-12 — medium — there is no maximum-length facility anywhere in the toolkit

`grep -rn "max_length\|maxlength\|max_chars\|char_limit" crates/sdk/ui-core/src/` returns nothing.
`TextInput`'s builder offers placeholder, clear button, trailing accessory and `on_submit`
(`text_input.rs:3716-3800`) but no cap; `form_composition.rs:427 with_error` is the only
validation channel and needs a rebuild to show copy.

**Impact.** Any field with a server- or schema-imposed limit (display name, tag, per-app name,
mail subject) silently accepts text that will be rejected later, with no counter and no
truncation feedback. Every form in all 70+ apps.

**Fix.** `with_max_chars(n)` clamping in `apply_edit`, plus an "n/N" trailing hint and an
over-limit state on the control edge.

### CORE-13 — minor (latent) — `Checkbox::radio()` ships radio semantics with no radio-group behaviour

`widget/checkbox.rs:368-377` sets `Role::RadioButton` (`:821-825`), but `a11y.rs` has no group
role, every radio is an independent tab stop (`bool_control.rs:337-339`), and arrows are not
consumed (`bool_control.rs:157-186`). Meanwhile `ToggleGroup` — what apps actually use — does
implement arrow + Home/End (`toggle_group.rs:1927-1935`).

**Fix.** Either add a `RadioGroup` container owning the roving tabindex, or delete
`Checkbox::radio()` and point callers at `ToggleGroup`. Currently only
`examples/design_gallery_shots.rs:343` calls it.

### CORE-14 — minor — the in-place rename field paints a caret but ignores Left/Right/Home/End/Delete

`widget/name_field.rs:1-27` states the limitation ("no selection, no clipboard, no caret
movement"), `:196-210` and `:518-524` show the accepted keys, and a test pins `ArrowLeft` →
`Ignored` (`:519`). The caret is still drawn (`:265-269`).

**Impact.** Fixing a typo in the middle of a version name means deleting back from the end, while
the visible caret implies otherwise. Used by activity/history rename
(`activity_timeline.rs:1103`, `:1116`).

**Fix.** Swap for `EditableLabel`, which wraps a full `TextInput` (`editable_label.rs:551-556`).

### CORE-15 — minor — the autocomplete list discards wheel magnitude

`widget/autocomplete.rs:1266-1274` reduces the delta to `signum()` and scrolls one row;
`Dropdown` converts the same event with magnitude (`dropdown.rs:79`, `:1225` — `*y * -LINE_PX`
with `LINE_PX = 60.0`).

**Impact.** A three-line wheel notch or a trackpad flick moves the suggestion list by one row; it
feels stuck and lags every other list in the app.

---

# Part C — Motion, animation and visual polish

## C0. What is already right (so the findings are read in proportion)

The motion system is the strongest part of this codebase and the findings below are gaps in
*adoption*, not in design:

- One catalogue: `crates/sdk/ui-core/src/motion/named.rs` (1 438 lines) — 19 named motions, every
  pace from `tokens::motion`, every reduced-motion variant declared, every shortcut rule declared.
- Closed-form springs with retargeting that preserves value **and** velocity
  (`motion/spring.rs:110-160`), so an interrupted motion continues instead of restarting.
- A settled motion asks for no frames (`MotionValue::settles_at` → `want_frames_until`), and the
  clock is a deadline list, not a polling tick (`motion/clock.rs`).
- The gate passes clean: `python3 tools/policy/check_motion_library.py` reports nothing — no
  `Curve::`, no `Spring::new`, no hand-rolled easing in `crates/sdk`, `crates/session` or `apps/`.
- Reduced motion is honoured centrally at `MotionSpec::timing` (`motion/spec.rs:93-125`), and
  `Appear::pose`/`Drill::pose` drop their spatial channel under `motion.reduced()`
  (`named.rs:74-80`, `:136-141`).
- Loading has a real policy with a grace period and a minimum-shown floor
  (`arriving.rs:41-46`: `GRACE 150 ms`, `MIN_SHOWN 300 ms`), so fast loads do not flash.

## C1. Per-surface motion matrix

`✅` animates, `⚠️` partially, `❌` hard cut. Evidence is the call site, not the intention.

| Surface | Motion | Evidence |
|---|---|---|
| Popover / menu / dropdown / context menu / tooltip | ✅ `Appear` | `popover.rs:1098`, `dropdown.rs:1716`, `filter_menu.rs:505`, `context_menu.rs:1619`, `tooltip.rs:388` |
| Quick Settings (appear, drill, height) | ✅ `Appear`+`Drill`+`Extent` | `overlay_host/qs_motion.rs:1-31` |
| Dialog (overlay `ModalDialog`) | ✅ `Appear`, scrim cut out | `modal_dialog.rs:2314-2325` |
| Dialog (embedded `ModalSheet`) | ⚠️ **in only** — leaves in one frame | `modal_dialog.rs:1009-1010`, doc admits it `:878-881` — **MOTION-01** |
| **Autocomplete suggestions** | ❌ none | `autocomplete.rs:1217-1224` — **MOTION-02** |
| **Display-change confirmation card** | ❌ none | `display_confirm.rs:600-614` — **MOTION-03** |
| **Calendar period nav / swipe** | ❌ none | `calendar/src/app.rs:2725-2733` — **MOTION-06** |
| Window open / close / move | ✅ `WindowMotion::OPEN/MOVE` + width spring | `window_group.rs:97-104`, `:883-890`, `tile_host.rs:28-44` |
| Workspace switch (incl. swipe) | ✅ `Drill::WORKSPACE` | `workspace_slide.rs:40` |
| Tab strip insert/close/move | ✅ `ListChange`+`Disclose`+`Snap` | `tabs.rs:1040-1057` |
| Tab content cross-fade | ✅ `CrossFade` | `window_group.rs:347-348` |
| Sidebar / drawer / inspector / flap | ✅ `PanelSlide` | `sidebar.rs:785-799`, `flap.rs:173` |
| Accordion body | ✅ `Disclose` reveal | `primitives/disclosure_row.rs:610` |
| Accordion **chevron** | ⚠️ rotates in one widget, snaps in three others | **MOTION-09** |
| Toast / notice | ⚠️ strip fades, but the bar height jumps | `window_toolbar_row/strip.rs:211-216` — **MOTION-04** |
| Notification banners | ✅ `PanelSlide` | `desktop-surface/notifications/stack.rs:33` |
| List item arrival / reorder | ✅ `ListChange`+`CrossFade` | `arriving_rows.rs:38`, `tree_view.rs:3592-3610` |
| Checkbox / switch / slider / badge | ✅ `Mark`/`Thumb` | `checkbox.rs:106-115`, `toggle.rs:340-372`, `slider.rs:200` |
| Scrollbar fade | ⚠️ linear, instant appear | `animation/idle_fade.rs:60-77` — **MOTION-15** |
| **Hover tint (chrome vocabulary)** | ❌ binary both ways | `chrome/shell.rs:188-196` — **MOTION-08** |
| **Press scale on chrome** | ❌ one-frame transform snap | `chrome/shell.rs:195`, `tabs.rs:1267-1277` — **MOTION-10** |
| Determinate progress fill | ❌ steps between values | `progress_bar.rs:146-160` — **MOTION-16** |

## C2. Hard cuts a person actually sees

### MOTION-01 — notable — modal confirmations arrive with motion and leave with none

`widget/modal_dialog.rs:1009-1010`:

```rust
// It arrives from its centre; the scrim fades with it, cut out where the sheet is drawn.
self.appear.follow(true, theme.motion());
```

It never calls `follow(false, …)`, and its own doc says so (`:878-881`: *"It leaves when its host
stops painting it, at once."*). The host then drops the paint immediately:
`action_palette/confirm.rs:409-411` returns early once `self.confirm` is `None`. The overlay
variant does it correctly (`modal_dialog.rs:2314-2318`), which is what makes the asymmetry visible.

**Impact.** The action palette's "are you sure?" — a high-frequency surface, 210 ms in — blinks
out. Confirming a destructive action is exactly the moment an abrupt disappearance reads as
"did that work?".

**Fix.** Keep drawing while `appear.is_drawn()` even when the host reports closed (mirroring
`PopupSurface`'s `leaving` snapshot so the body still paints), and have `paint` take the open
state instead of hardcoding `true`. Cheaper interim: hosts keep calling `paint_message` for
`Appear::SPEC.settle_time(Exit, …)` after the answer. Blast radius: every app's palette
confirmation, `code-editor/src/app.rs:350`, `chat/src/channel_sheet.rs:85`.

### MOTION-02 — notable — the autocomplete suggestion list is the only floating list that pops

`widget/autocomplete.rs:1217-1224` gates on `if !self.open { return; }` and paints
`SuggestionList::paint` (`:544-563`) at full opacity with no origin and no exit. Compare the
directly analogous control 200 px away, `dropdown.rs:1716-1723`:

```rust
self.appear.follow(self.open, theme.motion());
if !self.appear.is_drawn() { self.list_pose = crate::motion::Pose::REST; … return; }
```

**Impact.** Typing in the File Explorer path bar (`path_bar.rs:73`), Maps endpoint editor
(`endpoint_editor.rs:74`), Weather search (`core.rs:251`), the peer-sync name field
(`peer_sync_overlay/sheet.rs:543`) and any form field (`form.rs:268`) pops a panel in, while a
dropdown next to it animates from its caret.

**Fix.** `appear: Appearing` on `SuggestionList`; `follow(self.open, theme.motion())` at the top
of `paint`; draw through `appear.pose(list_rect, anchor.center())`; keep painting while
`is_drawn()`.

### MOTION-03 — notable — the display-change confirmation appears in one frame, under the pointer

`shell/display_confirm.rs:600-614` paints with `paint_panel(…, theme, 1.0)` and the file contains
zero motion references. This is the "Keep these settings? / Revert" card that appears whenever a
resolution or scale change risks blanking a screen, with a live countdown already running, over a
scrim covering every display (`:630`). The Super+P path reaches the same information through an
animated sheet — and the file's own doc says the two should look identical
(`display_confirm.rs:16-18`).

**Fix.** `Presence` on `Appear::SPEC`, anchored at the band's own centre (as
`modal_dialog.rs:2314-2325` does), gate `draw` on `is_drawn()`, and pass
`appear.pose(…).opacity` instead of `1.0`. Reduced motion already resolves to `Fades`.

### MOTION-04 — medium — the toolbar notice fades in while the bar height (and the window content) jumps

`widget/window_toolbar_row/strip.rs:211-216` returns the full measured height as soon as `open`,
while `strip.rs:287-290` only varies **opacity**. `slot_row.rs:220-228` folds that height into
layout immediately.

**Impact.** Content shifts up under nothing for ~100 ms, then the notice fades in. The push is
larger than the thing that pushed; it reads as a glitch rather than a reveal. Affects every
compact-width window showing a `ToolbarNotice`.

**Fix.** Drive `height()` from a `motion::Extent`/`Disclose` progress, the way
`primitives/disclosure_row.rs:610` does.

### MOTION-05 — medium — the calendar month grid's cells and event chips have no hover state at all

`apps/calendar/src/app/month_view.rs:176-183` pushes `Hit` rects for every day cell and
`:329` for every chip, but the cell is painted with exactly two grounds (`:191-196`: selected or
`c.surfaces.content`), `grep -ci hover month_view.rs` → **0**, and nothing in `App` records the
pointer for this view. The same app *does* hover its agenda rows, task rows and editor controls
(`agenda.rs:155,222`, `tasks.rs:356`).

**Impact.** ~42 day cells plus their chips — roughly 30 % of the app's surface — are dead under
the pointer. The person cannot tell that a day cell is a button (it opens a day view) from empty
grid gutter.

**Fix.** Track `last_cursor` and paint a lift on `cell.contains(pointer)` using
`chrome::shell::selection_tier_on`, which exists for this.

### MOTION-06 — notable — calendar period navigation, including a real one-finger swipe, is a hard cut

`apps/calendar/src/app.rs:2725-2733` (`step_period`) reassigns `self.anchor` and sets
`self.dirty = true`; the swipe path (`:2703-2716`) feeds the same thing. `month_view.rs` contains
zero motion references, and the whole app's motion usage is one line (`app.rs:4425`
`faces.needs_redraw`). The shell performs the same gesture *with* motion
(`workspace_slide.rs:40`).

**Impact.** A direct-manipulation gesture that swaps the entire screen with no follow-through is
the single most jarring thing in the app: the hand expects the grid to move with it. The same
swipe feels different in Calendar and on the desktop.

**Fix.** Two layers — offset the grid by the drag delta while the finger is down (the
`Fingers`/`Trajectory` machinery is already there), then cross-fade/slide on `Drill::WORKSPACE` at
release with `want_frames_until`. Keyboard/chevron navigation gets at least a `CrossFade`.

### MOTION-07 — minor — logs "follow" appends rows without revealing them

`apps/logs/src/view.rs:268` replaces `bridge.rows` and the shared table keeps its view anchor
(`data_table.rs:1783 capture_view_anchor`); the only `reveal_row` call is for selection
(`view.rs:549`). Rows are appended in ascending time (`app.rs:978`).

**Impact.** With Follow on, new lines land below the fold and the visible page is frozen — the
toggle looks broken. (`journalctl -f` behaviour is the whole point of the button.) Fix: reveal the
last row when at/near the bottom, else a "N new entries — jump to newest" pill.

## C3. Hover and press — the chrome vocabulary

### MOTION-08 — medium — `motion::Feedback` (the named hover/press tint) has no real call site; every hover wash is a hard cut both ways

`motion/mod.rs:29` advertises `Feedback` — "press and hover tints" — and `named.rs:1073-1105`
implements it (on at once, spring back to 0 over `fast`). Whole-codebase consumption: two
references in `apps/photos`, and the one that exists (`navigation.rs:324-333`) is a map-camera
settle cue, not a tint. The actual hover paints are boolean:

```rust
// crates/sdk/ui-core/src/chrome/shell.rs:188-196
let fill = if lifted { colors.lift(1.0) }
           else if state.hover { colors.lift(HOVER_LIFT_T) }
           else { Color::TRANSPARENT };
```

(same shape in `chrome/nav_row.rs:1053-1059`).

**Impact.** A hover wash that vanishes the instant the pointer leaves is the loudest "cheap" tell
in an otherwise fanatically interpolated system — and it is on the most-hovered pixels in the
shell: tabs, nav rows, selectors, toolbar buttons.

**Fix.** Hold a `Feedback` per hovered surface and scale the lift by `feedback.level(now)`, then
`want_frames_until(feedback.settles_at())`. Keep instant-on (that is correct); only the release
gains the 130 ms fade.

### MOTION-09 — medium — the disclosure chevron snaps in three widgets while it rotates in the fourth

`primitives/disclosure_row.rs:520-528` is the reference implementation:

```rust
// The chevron turns a quarter, from pointing at the row's end to pointing down,
let turn = Disclose::reveal(self.progress(), 1.0).turn;
let rotated = ctx.transform * Affine::translate(centre.to_vec2())
    * Affine::rotate(turn * std::f64::consts::FRAC_PI_2) * Affine::translate(-centre.to_vec2());
```

The same affordance is an icon swap in `tree_view.rs:1038-1045` (`paint_disclosure`: `if expanded
{ ChevronDown } else { ChevronRight }`), `data_table.rs:3011-3018` (tree column), and
`chrome/nav_row.rs:1044-1052` + `:1384-1394` (section bands).

**Impact.** The rows below the chevron *do* animate (`tree_view.rs:3592-3610` moves them through
`KeyedTransitionSet`), so the affordance lags the thing it describes by snapping while the content
glides. In the Settings sidebar this happens dozens of times a session.

### MOTION-10 — medium — press-scale is a one-frame transform snap

`chrome/shell.rs:195` and `:233`, `chrome/selector.rs:148`:

```rust
scale: if state.pressed { PRESS_SCALE } else { 1.0 },   // PRESS_SCALE = 0.97
```

consumed raw (`tabs.rs:1267-1277`), and it snaps back on release in the same frame as the click's
effect. The catalogue has `ControlChange`, `Thumb`, `Mark`, `Attention` — no scale channel.

**Impact.** A 3 % spatial jump on the largest clickable chrome object (a tab pill, a window-group
selector) is perceptible as a pop rather than a press.

**Fix.** Add a `Press` motion (critically damped spring on scale, `Fast`, `Reduced::Lands`) and
read `SelectionTier::scale` from it.

### MOTION-11 — minor — determinate progress fills step between values

`widget/progress_bar.rs:146-160` computes the fill straight from `self.progress`, and
`set_progress` (`:311-319`) only sets `dirty`. The same face backs the sidebar/QS "level" bars
(`chrome/nav_row.rs:1041-1046`).

**Impact.** Resource meters and level bars step in visible chunks while every other numeric
channel interpolates. Fix: a `MotionValue` on the fraction retargeted in `set_progress` — cheap,
because a settled bar asks for no frames.

## C4. Reduced motion and theme knobs

### MOTION-12 — medium — the theme's `motion.character` setting is a dead knob

`theme.rs:32-42 apply_motion_character` rewrites only `springs.snappy` and `springs.bouncy`; grep
for `.snappy|.bouncy|.subtle` over production code returns `theme.rs` only. `MotionSpec::timing`
(`motion/spec.rs:112-124`) uses `SpringSpec::settling_within(pace_duration(pace, motion))` and
reads only `springs.heavy`.

**Impact.** A theme author setting `"motion": {"character": 0.0}` ("calm") or `2.0` ("lively")
ships a theme whose motion is bit-for-bit identical to stock, while `theme_document.rs:108-115`
validates the field and `theme.rs:1644-1667` tests that the spring moves. Silence here is worse
than any of the three fixes.

**Fix.** Route a pace through `snappy`, or scale `Durations` in `apply_motion_character`, or delete
the knob and `SpringPresets::{subtle,snappy,bouncy}`.

### MOTION-13 — medium — one time-driven fade bypasses reduced motion entirely

`widget/capture_studio.rs:2976-2992` passes `EffectiveMotion::default()` — which is
`new(Motion::default(), false)`, i.e. reduced motion **off** — into
`Transient::exit_starts`/`opacity`, although `Transient::READOUT.reduced = Reduced::Lands`
(`named.rs:819-824`) means it should disappear at once. Every sibling in the same file guards on
`theme.reduce_motion` (`:2254-2258`), and `theme.rs:741-743` explicitly forbids the pattern.

### MOTION-14 — minor — three widgets turn a specified fade into a cut under reduced motion

`floating_window.rs:342-353` (`if reduce_motion { return target; }`), `tile_host.rs:2669-2671`
(`if theme.reduce_motion { self.spawned_at = None; }`) and `window_group.rs:1312` (drops the
size-label fade) each early-return where the spec says `Reduced::Fades`. Defensible as a choice,
but not the documented one (`tokens/motion.rs:262-268`), and it makes windows appear dead rather
than settled.

**Fix.** Stop passing `EffectiveMotion::default()` into the timing helpers
(`floating_window.rs:56-82`, `tile_host.rs:28-61`, `window_group.rs:75-104`) and let
`MotionSpec::timing` pick the `fast` fade.

## C5. Durations and holds

### MOTION-15 — minor — the idle chrome fade is the only linear fade in the codebase, with three unrelated hold constants

`animation/idle_fade.rs:60-77` ramps linearly (`1.0 - since_hold.as_secs_f32() / self.fade…)` with
an instant appear ("flashes into view", per `scrollable.rs:156`). The holds are per-surface and
unrelated to each other or to tokens: `CHROME_FADE = 320 ms` (`idle_fade.rs:13`, longer than the
`fast` token's 130 ms), `BAR_HOLD_MS = 900` (`scrollbar.rs:83`),
`OVERLAY_CHROME_HOLD = 1_050 ms` (`floating_window.rs:50`), `HANDLE_HOLD = 3 s`
(`block_editor_handle_reveal.rs:17`).

**Impact.** An overlay scrollbar's fade is the most-repeated animation in any long document; a
linear ramp next to spring-everything reads as unfinished, and chrome lingers different amounts
per surface with no design reason. `tools/policy/check_motion_library.py` cannot see these because
`HOLD`/`DWELL`/`GRACE` are not in its motion-word list.

**Fix.** Express the fade as `MotionValue` on `CROSS_FADE::Exit`; put `hold` in
`tokens::motion` beside `Durations` so the three chrome call sites agree.

### MOTION-16 — minor — the catalogue's `exit` doc contradicts two specs

`motion/spec.rs:48` says "Exits are shorter", but `Transient::READOUT` is `enter: Fast, exit: Base`
(`named.rs:819-824`) and `CONFIRMATION` is `enter: Instant, exit: Base` (`:827-831`). Both are
intentional. The doc is what is wrong: *"Exits are usually shorter; a transient's own dismissal is
paced by what it says."*

## C6. Loading

### MOTION-17 — minor — operation spinners bypass the arrival grace period, so 60 ms jobs flash

`arriving.rs:41-46` defines `GRACE 150 ms` + `MIN_SHOWN 300 ms` and consumption is correct for
*content* (`arriving_view.rs:102-137`). Operation-level spinners do not use it:
`store/src/app/view.rs:167-177` shows `Spinner::new()` plus a `Banner` the frame `worker_busy`
flips, and `shortcuts/src/view.rs:651-658` does the same for a draft.

**Fix.** Reuse `arriving::GRACE` as a show-delay for job spinners.

## C7. App-level motion measurements

Raw count of motion API references in each app's `src/` (excluding tests), per 1 000 lines.
**Read it carefully:** near-zero is *usually correct* — those apps delegate all transitions to
shared widgets (`Button`, `ListRow`, `NavShell`, `ModalDialog`), which is exactly the
architecture this repo wants. It becomes a finding only where the app also hand-paints the surface,
because then nothing animates it.

| App | kLOC | motion refs | per 1k |
|---|---|---|---|
| activity, agent, browser, browser-parts, calculator, chip-design, clocks, contacts, containers, db-client, diffmerge, discord, disk-usage, feeds, firewall, git-client, logs, messages, monitor, osk, poker, remote, settings-core, shortcuts, snippets, store, tasks, themes, totp, tube, video-player, weather, windows, word-processor | 0–43 | **0** | 0.0 |
| file-explorer, settings, sheets, video-editor | 25–49 | 1–2 | 0.0 |
| calendar, camera, canvas, code-editor, mail, maps, pdf-viewer, typst-editor | 8–37 | 1–5 | 0.1 |
| backups, hex-editor, passwords, print-centre, terminal | 5–15 | 1–3 | 0.2 |
| slides | 16 | 5 | 0.3 |
| chat, disks | 6–32 | 2–13 | 0.4 |

**Where this is a genuine finding** (hand-painted surface + zero motion, verified by reading the
painting code):

- `MOTION-05/06` **calendar** — the month grid is hand-painted and has no hover, no transition.
- `MOTION-07` **logs** — the table is shared but the follow behaviour never reveals or animates.
- **sheets** — the grid is app-owned (`grid.rs`, 3 920 lines) with one motion reference in the
  whole app; cell edits, row insert/delete and selection changes are all hard cuts next to a
  shared `DataTable` that animates in every other app. Worth a follow-up audit of `grid.rs` against
  `data_table.rs`.
- **monitor** — sparklines redraw at the cadence with a hard jump; there is no interpolation
  between samples and no hover readout (see `VIS-02`).
- **git-client** — history rows and the branch graph are painted per frame with no transitions
  (`graph.rs`, `history.rs`), and the diff pane swaps commits with no cross-fade.

**Where it is correct and should be left alone:** activity, agent, browser, contacts, containers,
db-client, diffmerge, discord, feeds, firewall, messages, osk, poker, remote, settings-core,
shortcuts, snippets, tasks, themes, totp, tube, video-player, weather, windows, word-processor,
store — all delegate, none hand-paint a transition-bearing surface.

---

# Part D — Structural: duplicated chrome and toolkit bypass

Two structural questions were asked explicitly: *does the shell use normal UI widgets?* and *do
apps draw a top bar that should be the system bar?* Both have measurable answers.

## D1. App-drawn headers vs the shared window bar

The mechanism (`crates/sdk/ui-core/src/window_toolbar.rs:1` — *"An app's toolbar is slots in the
window's bar, never a row it draws"*) has four sanctioned shapes:

| path | mechanism |
|---|---|
| declare slots | `window_toolbar::declare(Some(WindowToolbar{…}))` (`:1077`) + `is_hosted()` (`:1258`) hides the app's own row |
| let `NavShell` do it | `with_window_toolbar()`; `nav_shell.rs:1148` computes `needs_header = !inputs.slots_in_bar() && …`, and `nav_shell/window_toolbar.rs:541` publishes the header title as the bar's location and its buttons as the bar's actions |
| header lives *in* the bar | `with_header_in_window_bar()` (`nav_shell.rs:665`) |
| legitimately keep a painted band | implement `ui::widget::WindowHeader` (`widget/window_header.rs:1-10`) — for full-bleed apps, "the bar *is* that row" |

**Result across all 70 app dirs:** 53 use the shared mechanism, 3 draw their own header, 9 are not
app windows, and 5 declare nothing so the bar carries only a title. The mechanical table is in
Appendix A; the findings follow.

### CHROME-01 — notable — Books re-implements the entire window bar twice, in-body, and declares no slots

`apps/ebook/src/app.rs:6851-6856` (library band) and `:8487-8500` (reader band) each build a band
with `ui::widget::toolbar::paint_band`, and the app contains **zero** `window_toolbar::declare`
calls. What those bands carry: Back, book title + author, Open book, Settings, shelf sort,
"Hide finished", bookmark, notes and the "Aa" type popover (`:6886-6941`, `:8513-8566`,
`shelf_controls.rs:151-192`).

**Impact.** Back is at y≈56 in Books and in the bar in Files, Code, Mail, Terminal, Browser,
Slides, Sheets. The bar's own search is not wired to the shelf filter that Books draws instead.
And 96 px of chrome (40 px bar + 56 px header) sit above any cover or page while the system bar
shows only the window title.

**Fix.** Declare `navigation.back` for the reader's Back, `location` = book title (author becomes
the elided detail the bar already handles), `actions` = Open book + Settings, shelf sort/hide as
views/menu, bookmark/notes/Aa as actions. Keep the body's page-title block and the reading-type
popover.

### CHROME-02 — notable — the Typst editor draws a full command bar under an otherwise empty system bar

`apps/typst-editor/src/app.rs:1049-1053` reserves `bar_rect` and `:1333` paints it;
`chrome.rs:276-352` fills it with the document name (`TextRole::Heading`, `chrome.rs:166-170`), a
hand-rolled `ViewTabs ["Edit","Preview"]` (`:173-178`) and a `toolbar::Toolbar` holding Compile,
Problems (n), Outline, Zoom out/in, Fit width, Save, Open, Export PDF — plus the window's own
History control (`:291-300`). The module's only `window_toolbar` use in the whole app is a notice
(`app.rs:1253`).

**Impact.** Two bars, one empty: 40 px of shell bar carrying Full screen/Restore/Close and the tab
title, then 40 px of app bar repeating "where am I" for a desktop that, by the app's own comment
(`chrome.rs:6`), owns document tabs. Every one of those controls is already a `ToolbarAction` or a
`ToolbarViews`, and History is already the bar's.

**Numbers (the app-specific hack):** `app.rs:139 bar_h = control_h + 2·s1` = **40 px with 4 px
insets**, against the shared row's **56 px with 12 px insets** (`chrome/header_row.rs:66-71`). So
this bar is 16 px shorter and its controls sit 8 px closer to the edge than any other app's.
`chrome.rs:280` also takes a `_documents: usize` parameter that is dead.

### CHROME-03 — notable — Canvas silently opts out of `NavShell`'s bar hand-over with a leading element

`apps/canvas/src/lib.rs:130-137` sets `.content_header_leading(panels::header::title(…))`, and
`nav_shell/window_toolbar.rs:445-462` declines the hand-over in exactly that case:

```rust
/// … `None` while a header holds something the
/// bar cannot show: a leading fill, or a control that is not a button.
if leading { return None; }
```

so `shell_bar` stays off (`:427-431`) and the row is painted in the body
(`panels/header.rs:17-105`: editable artwork title + zoom readout, then menu, Undo, Redo, Add
picture, Export, Zoom out/in/fit).

**Impact.** Every other `NavShell` app gets title→bar-location and buttons→bar-actions for free.
Canvas is the only `NavShell` app with two rows, and its commands are the only ones not reachable
from the window bar.

**Fix.** Drop `content_header_leading` for a declared editable `ToolbarLocation` (`Artwork::SET_TITLE`
already exists) and let the buttons go through `ShellBar`.

### CHROME-04 — medium — five apps declare nothing, so their window bar carries only a title

`discord` (`resource.rs:105`), `maps` (`app.rs:5356-5361` implements `WindowHeader`, declares no
slots), `publisher` (`lib.rs:572-580`), `scratchpad` (`lib.rs:67-84`), `speed-test` (`view.rs:1-3`).
No location, no actions, no search wiring, no view switcher.

**Impact.** These are the windows where the bar is a decoration. For Maps specifically the whole
command surface is a floating control column, so nothing about the window is discoverable from
the bar — which is the one place a person looks first.

### CHROME-05 — minor — Tasks draws an in-body Back next to a title the bar already provides

`apps/tasks/src/issue.rs:44-50` and `:59-80` build a `ChevronLeft` button plus number/title inside
the scrolling content. The bar already has `ToolbarNavigation{back}` and `ToolbarLocation`
(`window_toolbar.rs:8-11,19-20`). Because the row scrolls it is not a second *bar*, but a narrow
window now shows two Backs with different meanings.

### CHROME-06 — minor — the OSK puts window-level actions in its key toolbar

`apps/osk/src/toolbar.rs:1-3` is legitimately keyboard content (candidates, selection, clipboard,
emoji, voice) **but** also carries `Dismiss` and settings (`:31-38`). The keyboard declares no
slots, so the shell bar above it is title-only while its close and settings live inside the
instrument panel.

### CHROME-07 — minor — the mail composer floats a hand-drawn title bar, with two divergences

`apps/mail/src/compose.rs:1695-1703` lays out and `:2459-2500` paints a panel title bar with Close.
Height is fine (`title_h` = 40 = `TITLE_BAR_HEIGHT`), but the heading is `TextRole::Subheading`
(`:2465`) where every header row and the bar use `Heading`, and the doc comment promises "the
title, the pop-out and close buttons" (`:2459`) while the loop paints only Close (`:2481`). It also
hand-rolls what `ui::widget::FloatingWindow` provides — and no app uses that widget.

## D2. Where the shell bypasses its own toolkit

The short answer: **the shell is the toolkit's biggest adopter**, so "the shell doesn't use normal
UI widgets" is mostly false. Window menu (`overlay_host/tab_menu.rs:16`), workspace menu
(`desktop/src/workspace_menu.rs:17`), quick settings root and sections
(`quick_settings/root.rs:33`, `sections.rs:18`), the wifi password prompt
(`overlay_host/wifi_password.rs:165`), notification banners and centre
(`notifications/stack.rs:20`, `center.rs:18`), power menu (`power_menu.rs:20`), file chooser
(`file_picker_overlay.rs:605`) and the whole onboarding flow (`onboarding_view.rs:49`) are on
shared widgets with real hover, focus, keyboard and a11y trees.

The exceptions are the findings already written up in Part A, collected here for the structural
read:

| Surface | What it bypasses | Finding |
|---|---|---|
| Lock screen credential field | `TextInput`/`SecretPrompt` — hand-rolled `String` + 5-key map + dot painter | `SHELL-01` |
| App tray icons | `BarControlFace` — raw `ImageBrush`, not in `control_targets()` | `SHELL-03` |
| Desk widgets + pin | `PaintedTargets`/`ButtonFace` — `AccessNode`-free, `MouseInput`-only | `SHELL-04` |
| Six chrome text fields | `TextInput`'s pointer path — painted by the widget, edited by `EditCore`, but no caret/drag/context menu | `SHELL-09` |
| Projection/cast sheet | `ListRowFace` hover | `SHELL-12` |
| Shortcut guide rebind rows | per-row `AccessNode` (`tab_menu.rs` shows the shape) | `SHELL-10` |
| Lock-screen media transport | focusable `AccessNode` | `SHELL-11` |
| `desktop-surface` palette scrim | `ui::overlay::scrim::modal` | `SHELL-14` |

Also worth recording because it is the *opposite* of the premise: shell colour hygiene is better
than the apps'. Non-test `Color::from_rgb8`/`from_rgba8` in `shell/src`, `desktop/src` and
`desktop-surface/src` reduce to presence hues, night-light warmth maths, the theme-aware brand
mark (`topbar.rs:522-528`) and one duplicated `FALLBACK_BG` literal across three files
(`backend/headless.rs:30`, `backend/winit.rs:82`, `render/paint/retained_target.rs:155`) — which is
worth hoisting to one constant but is not a control.

---

# Part E — Cross-app duplication

The pattern here is consistent and it is *not* "apps hand-roll everything". The shared
implementation usually exists, is tested, and is better than the copy — and then two to nine apps
re-invent it, each with a slightly different answer to the same user gesture. That divergence, not
the duplicated code, is what a person feels.

Ordered by person impact.

## DUP-01 — notable — find *matching* is re-implemented five times, with five different word-boundary and case rules

The find **bar** is genuinely shared and adopted everywhere (`widget/window_search.rs` +
`widget/find_bar.rs:540` + the `Findable` capability; zero private find bars in production — see
DUP-00). What is duplicated is the matcher:

| Where | Whole-word rule | Case rule |
|---|---|---|
| `ui-core/widget/block_editor.rs:824-832` (`is_whole_word`) | `!c.is_alphanumeric()` → **`_` is a boundary** | lowercase compare |
| `apps/code-editor/src/search.rs:383-385` (`is_word_char`) | `is_alphanumeric() \|\| '_'` → **`_` is part of the word** | lowercase, and **falls back to case-sensitive** when folding changes length (`:346-354`) |
| `apps/hex-editor/src/search.rs:69-71` (`is_word_byte`) | **ASCII bytes**, `b'_'` included | `eq_ignore_ascii_case` only (`:90-92`) |
| `apps/terminal/src/main_body.rs:1480` | `'_'` in word | per-char `to_lowercase` (`:1477-1479`) |
| `apps/word-processor/src/document_find.rs:229-234` | `'_'` in word | `regex` with `case_insensitive` (`:190-192`) |

**Impact on the same gesture** (Ctrl+F, "Whole word" ticked):

- `foo` against `foo_bar`: **no match** in the code editor, **match** in Notes/Mail/Chat bodies.
- `STRASSE` against `straße`: code editor returns **nothing** (it silently abandons folding);
  word-processor finds it; hex-editor compares bytes.
- `café` in a byte view: `é` is not `is_ascii_alphanumeric`, so boundaries differ again.

A person who learns in Notes that `id` matches `user_id` gets the opposite answer one window over,
and the match count in the bar changes meaning between surfaces.

**Fix.** Keep the shared bar; move matching into `ui-core` as one `FindMatcher` (needle +
`FindOptions` → spans) with one `is_word_boundary` and one case-fold-with-offset-map (the naive
fold problem is already solved correctly at `code-editor/src/search.rs:343-354`). Browser and mail
keep their engine-owned find, which is the one genuine exception.

## DUP-02 — notable — nine zoom clusters, eight scale models, six meanings for Ctrl+0

| App | Cluster | Model |
|---|---|---|
| browser | two clusters: `window_bar.rs:252-257` (menu) + `toolbar.rs:152-165` | `ZOOM_STEPS -5..=9` ×1.2 (`daily_controls.rs:18-20`) |
| pdf-viewer | `window_bar.rs:119-126` (in/out/Fit page/Fit width) | 0.25–16, ×1.15 (`app.rs:74,79`) |
| typst-editor | `chrome.rs:126-133` | ×1.25 (`preview.rs:443,449`) |
| canvas | `panels/header.rs:99-101` | ×1.25 (`surface.rs:58`) |
| photos | `chrome.rs:352-360` (map only) | 1.0–18.0, +1.0 steps (`places.rs:16-17`) |
| word-processor | `widget.rs:489-497` `NumberInput` + own kernel `zoom_control.rs` | 0.4–3.0, step 0.1 |
| maps | `map_controls.rs:60-101` | 3–19, ±1.0 |
| file-explorer | keyboard only (`config.rs:17-25`) | levels −2..=6 at 15 % |

**Ctrl+0 means six different things:** 100 % (word-processor `input.rs:1121-1126`), fit width
(typst `app.rs:1704-1707`), fit page (pdf `app_events.rs:1131-1135`), zoom to fit (canvas
`surface.rs:1718`), default icon scale (files `files_app.rs:9096-9099`), actual size (browser
`daily_controls.rs:326-345`). Ctrl+= diverges too, including the modifier test: canvas accepts any
modifiers (`surface.rs:1716-1717`), Photos binds **bare** `+`/`-` (`app/widget.rs:1113-1114`), and
word-processor's own handler shadows the shared window zoom entirely (`input.rs:1114-1129`).

The shared mechanism exists — `content_zoom.rs:45-140` (`ZoomStep`, `LEVELS 0.5…3.0`,
`ContentZoom`) — and exactly **one** app implements it (`browser_widget.rs:4375`).

**Impact.** Muscle memory for Ctrl+= / Ctrl+0 is wrong in five of six windows; "100 %" is
unreachable by keyboard in a PDF or a Typst preview; and the window-level text zoom does nothing
in exactly the apps with the most content to read.

**Fix.** A `ZoomCluster` widget (out / editable percent / in / fit) over a `ZoomModel` from
`ui-core`, with the *rules* in `content_zoom` and doc-specific bounds passed as constructor
arguments. Nobody keeps `ZOOM_MIN`/`ZOOM_STEP` of their own.

## DUP-03 — notable — six shortcut sheets, four of them printing chords that are wrong on the person's keyboard

| # | Implementation | Rows live in | Chord rendering |
|---|---|---|---|
| 1 | `apps/notes/src/shortcuts_overlay.rs` (292 lines, `ModalDialog`+`PropertyList`) | `GROUPS` with `{cmd:N}` markup (`:23-40`) | platform-aware |
| 2 | `apps/mail/src/shortcuts_overlay.rs` (282 lines, hand-rolled scrim/scroll/hit-test) | `const ROWS` (`:17-38`) | **literal `&["Ctrl","Enter"]`** (`:26`) |
| 3 | `apps/slides/src/help_overlay.rs` (283 lines, own scroll/layout) | own table | own |
| 4 | `apps/video-editor/src/app.rs:7532-7600` + `:8305-8420`, own `HelpMetrics` | raw `("Ctrl+O", …)` (`:7532-7541`) | **string split, no ⌘** |
| 5 | `apps/canvas/src/panels/shortcuts.rs` | raw `key("Ctrl+O", …)` (`:34-76`) | hand-rolled cap splitting with a `"+"` special case (`:108-112`) |
| 6 | `apps/file-explorer/src/files_app.rs:3326-3390` | `[(&str, Vec<Shortcut>); 14]` | platform-aware (`:3329-3335`) |

Provable divergences, not stylistic ones:

- **Same chord, different spelling.** On macOS, Files and Notes render ⌘O/⌘S; video-editor prints
  `Ctrl+O`/`Ctrl+S`/`Ctrl+Shift+S`; mail prints `Ctrl`/`Enter`; canvas prints `Ctrl+O`.
- **Same action, present in one sheet and absent in another.** `Ctrl+Shift+Z` redo appears in
  canvas (`shortcuts.rs:39`) but not in Files' 14 rows; `Alt+←/→` appears in Files
  (`files_app.rs:3345-3346`) and Notes (`:31`) but not in canvas.
- **Different dismissal contract:** video-editor and slides swallow every key while open
  (`app/interaction.rs:94-103`, `help_overlay.rs:58-71`); Files' sheet has no scrim of its own.
- **Different openers:** Notes `?` *and* F1; Files F1 *and* `?`; mail `?`; video-editor `?`; slides
  `?` only when no editor holds focus; canvas `?`/Shift+`.

**Impact.** The keyboard reference is the one place a person goes to learn a chord, and two of six
omit rows the others show. The sheet teaches a key that does nothing.

**Fix.** A `ShortcutSheet` widget in `ui-core/widget/` — `ModalSpec` shell + `Vec<ShortcutGroup>`
where each row is `{ keys: Shortcut, label: Fluent key }`, rendered through the existing
`widget/input_hint.rs` / `ui::keycaps`. Better: generate the rows from the `ActionBinding`s the apps
already declare, so the sheet cannot drift from the handler.

## DUP-04 — notable — save state: the good shared pieces exist and are nearly unadopted; no document app shows unsaved work except two

**Shared, written, tested, barely adopted:**

- Unsaved marker in the window bar — `window_toolbar.rs:308 pub edited: bool`, painted and
  announced (`window_toolbar_row/access.rs:95-96`). **Only two apps pass a live value** —
  `edited: buffer.dirty` (`code-editor/src/app/window_bar.rs:27`) and
  `edited: self.project_is_dirty()` (`video-editor/src/app/window_bar.rs:46`). Two more pass a
  hardcoded `false` (`browser_widget/window_bar.rs:357`, `files_app/window_bar.rs:154`), and
  word-processor passes `..ToolbarLocation::default()` (`widget/window_bar.rs:232-235`), as do the
  other document apps.
- Close-with-unsaved admission — the framework owns Save / Discard / Keep editing and a
  **save-failure dialog with Retry** (`app/close_admission.rs:47-60`, `:64-79`). An app reaches it
  by implementing `on_window_close_requested -> WindowCloseAdmission`; **two do**
  (`browser_window.rs:700-721`, `imager/src/app.rs:3264-3278`), which is how browser and imager
  end up being the only apps that can retry a failed save on close.

**Per-app inventions:**

| App | What it does | Citation |
|---|---|---|
| code-editor | own `ClosePrompt`/`CloseChoice`, own wording | `app.rs:401-417`, `app/dialogs.rs:117-129` |
| code-editor | own "Unsaved changes" string | `status_bar.rs:94` |
| notes | own `SaveStatus {Saved, Saving, Failed, SessionOnly}` + bubble | `status_bar.rs:156-158`, `:245` |
| contacts | own "Save changes?" with **no Cancel** | `app/dialogs.rs:245-253` |
| browser-parts | own leave-page sheet | `page_dialogs.rs:261,292` |
| hex-editor | failures as a toast string | `view.rs:428,519,535` |
| camera | toast severity guessed **from the message prefix** | `app/notice.rs:93-108` |
| typst-editor | no dirty flag at all; saving is deliberately silent | `app.rs:632`, `chrome.rs:229` |

Divergences on the same situation: the shared path offers Save / **Discard** / Keep editing; the
code editor offers Save / **"Don't save"** / Cancel; Contacts offers Save / "Don't save" and no way
back; body copy differs in all three. Camera decides a toast is an error if its text starts with
`"Save failed"`/`"Couldn't"` (`app/notice.rs:93-100`), so rewording silently downgrades a failure
to a success pill.

**Impact.** Whether a person can see they have unsaved work — and whether they can avoid losing it
— depends on which app they are in, and two of ten can retry a failed save.

**Fix.** Turn on `ToolbarLocation::edited` everywhere (one bool per app); delete `ClosePrompt` and
the Contacts dialog in favour of `close_admission`; one `SaveStatus` readout as a
`chrome/status_bar.rs:37-63 StatusReadout`; toast severity from `ToastKind`, never from text.

## DUP-05 — medium — eight private "transient message" types, three severities that cannot express Warning, and TTLs of 3/4/4.2/8 seconds

Shared and used by 15 apps: `widget/toast.rs` (`ToastKind` `:38`, `NOTICE_LIFETIME = 4 s` `:56`,
actionable cards `:535-578`) and `NavShell::toast`/`toast_with_action`
(`nav_shell/notices.rs:24-51`).

Private re-implementations: file-explorer (`Shown`, **8 s** for undo, `files_app/notice.rs:20`),
code-editor (`Notice{text,is_error}`), windows (`Notice{text,error}`), firewall
(`Tone{Info,Success,Error}` — **no Warning**, `app.rs:50-54`), video-editor
(`NoticeLevel{Ok,Warning,Failed}`), publisher (`BannerKind`), weather (`ForecastNotice`), browser
PiP (own **4 200 ms**, `browser_widget.rs:84`), plus containers passing a bare `error: bool`
(`app.rs:659-666`) that can only ever say success or error.

**Impact.** A failure can appear as a success, messages vanish at four different speeds, and
destructive actions that could be undone are not — e.g. Calendar's "Discarded empty draft" with no
undo (`app.rs:1685`), Contacts' delete toasts (`app.rs:484`).

**Fix.** Delete the eight types. Kill boolean severities. One `NOTICE_LIFETIME` plus a separate,
documented `UNDO_LIFETIME` that Files consumes instead of its own 8 s. Make undo the default for
destructive toasts.

## DUP-06 — medium — four copies of Cut/Copy/Paste/Select All, and the same rows show key hints in one pane and not the next

The context-menu *container* is shared (`widget/context_menu.rs:313`, `MenuItem` `:96-230`) and used
by 18 apps. There is **no shared "standard edit menu"** — the only two helpers are private:
`text_input.rs:5058-5082` and `block_editor_text_menu.rs:289-315`.

| # | Copy | Key hints? |
|---|---|---|
| 1 | `text_input.rs:5058-5082` | **no** |
| 2 | `block_editor_text_menu.rs:292-313` | yes (`.with_shortcut(Shortcut::command("X"))`) |
| 3 | `word-processor/src/widget/controls.rs:902-938` (clone of #1, plus a third spelling menu) | no |
| 4 | `video-editor/src/app.rs:4748-4760` | no, and **no selection check** — offers Cut with nothing selected |

The parallel-`Vec<Option<Action>>` dispatch pattern is re-implemented four times too
(`text_input.rs` `context_actions`, `block_editor_text_menu.rs:19-28`,
`word-processor/widget.rs:117-140` + `controls.rs:929-970`, plus index dispatch in music/weather).

**Impact.** Right-clicking a word gives different rows, different shortcuts and different ordering
depending on which pane has focus.

**Fix.** `widget/edit_menu.rs` exposing `standard_edit_items(abilities) -> (Vec<MenuItem>,
Vec<EditAction>)` with one ordering, one enabled-rule set, one shortcut table and one spelling
submenu; `TextInput`, `BlockEditor`, word-processor and video-editor all render it and implement
`apply(EditAction)`.

## DUP-07 — medium — two byte-identical 1 400-line query views, and four ranking entry points

- `apps/ebook/src/app/query_view.rs` (1 360 lines) and `apps/music/src/app/render/query_view.rs`
  (1 483 lines) are the same hand-written painter over the shared `QueryEngine`, down to the
  mojibake-safe apostrophe (`ebook:1156` / `music:1471`) and the "No matches in {}." string
  (`ebook:1163` / `music:1482`). They already differ in whether the retry button is drawn
  (`ebook:1160` draws `query_retry`; music has no equivalent).
- Four ranking entry points: `search/rank.rs:42 result_relevance`, `search/registry.rs:90 rank_key`,
  `action_palette.rs:3202 rank_and_group`, `search/system.rs:493 rank_fields` — plus a bespoke
  scorer that bypasses all of them (`apps/agent/src/surface.rs:529-552`, `:734-737`).
- The same field-weight mapping written twice inside one app:
  `store/src/model.rs:637-646` and `store/src/catalogue/mod.rs:345-360`.

**Impact.** The same word typed in two apps gives different ordering and different "no matches"
wording; a hit inside prose is a passage in one app and nothing in another; the two painters will
drift independently.

**Fix.** A `QueryView` painter in `tilo_app::framework::views` (which already owns `QueryEngine`,
`RemoteSection`, `facet_bar`, `groups`) so ebook/music contribute a row painter. Collapse ranking to
`rank::result_relevance`.

## DUP-08 — medium — "this pane is being read" has four different presentations and three different sentences

Shared vocabulary exists: `widget/skeleton.rs`, `widget/spinner.rs`, `widget/arriving_view.rs`,
`ui::arriving` (`GRACE`/`MIN_SHOWN`).

| Presentation | App | Citation |
|---|---|---|
| skeleton rows | disks | `view.rs:289-311` (`SkeletonShape::Row`, `Text{lines:4}`) |
| skeleton text | chat | `image_arrival.rs:29` |
| spinner | containers | `view.rs:1012` |
| frozen sentence | monitor | `view.rs:415` `EmptyState::new("Reading processes")` |
| frozen sentence | logs | `view.rs:412` `EmptyState::new("Reading the system journal")` |
| static copy | photos | `app.rs:1746` |
| indeterminate bar + label | print-centre | `surface/views.rs:208` |
| status-bar text | code-editor | `app.rs:451-455` |

**Impact.** A 2-second disk read animates while the same read in Monitor looks like a finished, empty
list. The person cannot tell "still working" from "empty" from "broken".

**Fix.** One `ArrivingPane` chosen by data shape; Monitor/logs/photos/maps/pdf adopt
`ArrivingView::placeholder_since`, whose "when do I stop lying about the skeleton" policy is already
shared (`arriving_view.rs:604-620`).

## DUP-09 — minor — three treatments for the same view-mode switcher

file-explorer uses the shared segmented `ToggleGroup` with labels (`toolbar.rs:294-310`); photos uses
icon-only `Command::icon(...).toggled(...)` (`app/chrome.rs:352-354`); tube and store use `BarItem`
menu entries (`tube/src/app.rs:1140`, `store/screens.rs:463`). State stays per-app, so this is
visual divergence only. Also inside Files: the sort menu is a plain button
(`toolbar.rs:319-324`) while the same app shows the shared sort indicator in the list header
(`file_list.rs:313-315`) — two sort affordances with different visuals.

## DUP-00 — no action — things that are already shared and should be left alone

Recorded so nobody "fixes" them twice: **empty states** (`widget/empty_state.rs`, 168 call sites
across 47 apps — the apps that look hand-rolled are wrappers that still delegate:
`chat/render_chat.rs:590-618`, `diffmerge/diff_view.rs:1106-1114`, `hex-editor/view/paint.rs:489-522`);
**find bar UI** (`find_bar.rs` + `window_search.rs` + `Findable`, zero private bars);
**sort indicator and column resize** (`data_table.rs:1477`, `:5932` — the only implementation);
**banners** (`widget/banner.rs`, 24 apps); **modal shell and focus trap** (`modal_dialog.rs`);
**clipboard and secret-clipboard policy** (`ui::try_clipboard_copy_secret`);
**search index, facets, paging** (`search/provider.rs`, `widget/filter_menu.rs`,
`tilo_app/framework/views/*`, 19 apps).

---

---

# Part F — Visual review of rendered frames

## F0. Method, and what a frame can and cannot prove

Frames in this part were **rendered from this exact commit** with the app's own offscreen
capture path, not taken from a mocked-up design or an older artifact:

```sh
cargo build -p calculator --locked
./target/debug/calculator --shots-dir /tmp/uxr/calc --theme dark
# → 11 frames: 01-calculator, 02/03-graph2d/graph3d, 04-empty, 05-narrow,
#   06-narrow-graph2d, 07-narrow-functions, 08-narrow-graph3d, 09-long-formula,
#   10-error, 11-remote-cursor
```

The capture flags worth using for UX review, all on `LaunchArgs`
(`crates/sdk/ui-core/src/launch.rs:78-125`): `--shots-dir DIR` (every declared scenario),
`--scenario NAME`, `--theme light|dark|high-contrast`, `--window-bar` (the app drawn under the
real window bar with its slots), `--responsive-matrix DIR`, `--text-scale N`, `--device-scale N`
(HiDPI), `--search QUERY`, `--history`, `--workspace-hue`, `--your-colour`, `--pseudo-locale`.

**A frame cannot show:** motion, hover, focus, press, latency, scroll behaviour, or a state the
scenario does not stage. Every motion finding in Part C came from source, not from a picture, and
nothing below claims otherwise.

**Coverage honesty.** Two apps are re-rendered and verified at this commit: Calculator (F1, 11
frames) and Settings (F3, 74 frames). The other screenshot sets (`monitor`, `passwords`, `maps`,
`git-client`, and the shell's `tests/snapshots/e2e/*`) were **not** re-rendered, and the observations
I first made from older artifacts mostly did not survive inspection — `F2` records all seven and what
killed six of them. `F4` has the commands to finish the job.

## F1. Calculator — verified at `3203631d96`

### VIS-01 — notable — at a narrow width the scientific keys silently disappear, with no indicator and no way back

`apps/calculator/src/keypad.rs:202-226`:

```rust
fn ensure_layout(&mut self, keypad_w: f64) {
    self.adopt_layout(KeypadLayout::for_width(keypad_w));
}

fn ensure_layout_within(&mut self, keypad_w: f64, keypad_h: f64) {
    self.ensure_layout(keypad_w);
    for narrower in [KeypadLayout::Standard, KeypadLayout::Compact] {
        if self.grid_min_h() <= keypad_h { break; }
        if narrower.cols() < self.cols { self.adopt_layout(narrower); }
    }
}
```

The layout is chosen **purely by available width**, only ever moving to a *narrower* pad, and
`KeypadLayout::Scientific` (7 columns, set once as the default at `:130`) is never restored by any
user action. The 480×800 capture (`05-narrow.png`) shows a 4-column pad: `AC ⌫ ( )`, `sin cos xʸ √`,
digits, `. = + − × ÷`. Gone are **tan, π, e, %, |x|, asin, acos, atan, sinh, cosh, tanh, ln, log, eˣ,
1/x, x², ±, Γ, 00, ans, mod, !** — twenty-two keys — and the frame contains no overflow key, no
"more" affordance, no disabled state and no hint that they exist.

**Impact.** The person narrows the window (or the desktop gives the app a strip of a tiled
workspace, which is the normal case in this shell) and trigonometry, logs and constants silently
vanish. Nothing tells them the capability is still there.

**Fix.** Either keep the scientific pad and scroll/wrap it, or add an explicit `2nd`/`fx` key that
pages the function block, or show a "Scientific" toggle in the window bar's views. Any of the three
is one affordance; the current answer is zero.

### VIS-02 — minor — operators and functions share the digit face except for one elevation step, so `÷` reads like `7`

`apps/calculator/src/keypad.rs:663-669`:

```rust
fn key_theme(role: KeyRole, theme: &Theme) -> Theme {
    if role == KeyRole::Digit {
        theme.on_ground(theme.tokens.colors.surfaces.raised)
    } else {
        theme.clone()
    }
}
```

Only `Digit` gets a different ground. The captured `01-calculator.png` confirms the consequence: the
`÷ × − +` column is visually indistinguishable from the digit keys beside it, and the whole pad reads
as one undifferentiated 7×7 field of buttons. The single accented key in the pad is `=`.

**Impact.** A calculator's keypad is read by muscle memory, and operator keys are found by their
tint. Here the eye has to read every label.

**Fix.** Give `KeyRole::Operator` (and arguably `Function`) its own tint through the existing
`key_button_role`/`ButtonRole` path; the theme already carries an accent and a selection tier.

### VIS-03 — minor — two display strings are hardcoded English outside the shipped catalogue

`apps/calculator/src/keypad.rs:480` `format!("Ans = {}", …)` and `:580`
`format!("Memory {}", …)`, while the app ships `apps/calculator/locales/en-US.ftl`. Neither string is
a key, so no second locale can translate them — and the same pattern is a real gap elsewhere
(`APP-browser-04`, `APP-maps-04`, `APP-db-client-05`, `APP-windows-01`).

### VIS-04 — positive — the same narrow frame shows responsive behaviour done right, which is the useful contrast for `VIS-07`

In `05-narrow.png` the calculator keeps its hierarchy: the display keeps its two lines with the
result still right-aligned, the memory chips reflow to one row, the pad drops columns instead of
clipping, and vertical rhythm holds. Compare the Settings frame in `F2`, where content is clipped
mid-control. Calculator is what a narrow window should look like; the finding above is about
*capability* loss, not layout.

### Impressions from the frames that did **not** survive checking

Recorded so this section is not read as "the author only saw what they expected":

- *"Keypad labels are left-aligned while `=` is centred."* **False.** Every key label is
  `Align::Center` (`keypad.rs:625`); the glyphs are simply small in a wide key.
- *"The result is dimmer than the expression, which inverts the hierarchy."* **Intentional.**
  `keypad.rs:531-533`: `let ink = if model.after_equals { c.text_primary } else { c.text_dim };` — a
  live preview is dim and committing with `=` promotes it. The frame shows the pre-`=` state, exactly
  as designed.
- *"`narrow-functions` proves the functions stay reachable."* **False.** `07-narrow-functions.png` is
  the *graph* screen's function-list panel, not the keypad; it says nothing about keypad keys. This
  is what turned into `VIS-01` after checking the code.

## F2. Leads from older artifacts — triaged, and six of seven were stale

The first pass of this audit looked at app screenshot sets that were left in a previous build's
output tree. They came from a different commit, and when each lead was checked against
`3203631d96` — by source, and for Settings by re-rendering — **six of the seven did not survive**.
Recorded in full, because "the artifact looked wrong" is exactly the reasoning that produces
phantom work:

| Lead | Outcome | What settled it |
|---|---|---|
| `VIS-05` Settings content clipped at narrow width | **Dead** | Settings implements a proper narrow mode: `settings_app.rs:10-20` documents list-screen → content-screen navigation with a back arrow. The re-rendered `26-narrow.png` (438×800) shows the Region & formats pane full-width, nothing clipped, with its own back chevron and reset action. The old artifact predates that work. |
| `VIS-06` internal config path printed in Settings | **Dead** | `grep -rn "qos/mail\|accounts.json"` over the whole tree returns **nothing** — the string no longer exists. The re-rendered `20-external_accounts.png` has no path line at all; the account page has been redesigned (icon + title + status chip + overflow menu). |
| `VIS-06b` "Connected" status shown by colour alone | **Dead** | Current page uses a **text** chip ("Not checked yet"), not coloured text. Nothing colour-only remains on that page. |
| `VIS-07` Monitor: summary `8.6 GiB` vs process `10.4 GiB` contradicts itself | **Dead — synthetic fixture** | The card reads `metrics.mem_used` (`view.rs:255-257`) and the row reads per-process RSS (`model.rs:550`), so they are different measures — but the capture was a *sample* snapshot whose process memory is fabricated (`model.rs:540-552 build_sample_process`). The apparent contradiction is sample data, not a defect. |
| `VIS-07b` Monitor `Kill` is one-click with no confirm and no graceful alternative | **Dead** | The action carries a confirmation policy (asserted at `app.rs:1935-1941`, "the confirmed kill executes") and the app publishes **`end-process` alongside `kill`** (`resource.rs:336-350`). Both halves of the lead were wrong. |
| `VIS-10` Git-client window bar action clipped to `Syn…` | **Dead** | The toolbar row folds rather than clips: it has a dedicated `Overflow` slot — "the button whose menu holds the actions the row has no room for" — plus a `ViewsMenu` (`window_toolbar_row/layout.rs:17-45`). |
| `VIS-11` Shell overview shows workspace position twice | **Unresolved** | `topbar.rs:833` documents "three position dots at the large-text setting", so the dots are deliberate; whether the `Home 1/3` pill is the same fact in a second vocabulary needs a render with `--window-bar` and a large `--text-scale`, which is not done here. Not counted as a finding. |
| `VIS-08` Passwords: delete not reachable outside the editor | **Transformed** | Delete *is* published on the door (`actions.rs:227`: `["open","edit","copy","reveal","delete"]`) and current Settings-style pages reach such actions through a trailing overflow control. Whether Passwords does the same needs its own render — dropped as a finding, kept as a check. |

**The lesson this section is evidence for:** a picture of an older build is a hypothesis, not a
finding. Every claim in Parts A–E is a source claim at this commit and survives that test; the
visual claims are only those in `F1` and `F3` below, which were rendered here.

## F3. Settings — verified at `3203631d96`

Rendered with `./target/debug/settings --shots-dir /tmp/uxr/settings --theme dark` → 74 frames
(25 pane scenarios, narrow and search variants, plus 49 interaction states).

### VIS-05 — minor — a status that cannot exist, repeated under every account

`20-external_accounts.png` shows four accounts (Work email, Personal email, Team calendar, Home
Nextcloud), and every one carries the same two facts:

- the chip **"Not checked yet"**, and
- the tertiary line **"No synchronization status is available yet."**

The chip and the sentence say the same thing twice, in two vocabularies, four times down the page.
The one useful question — "is this account working?" — is therefore the least scannable thing on the
page, and nothing in the frame offers a way to *make* a status exist (no "Check now"), so the state
is permanent by construction.

**Fix.** One statement, not two: keep the chip and drop the sentence (or use the sentence only where
a real state exists). Add the action that the chip implies — "Check now" — or, if checking is not
implemented, do not present a status surface at all until it is.

### VIS-06 — minor — existing accounts are inert rows while the "Add account" rows below them are clickable

The four account rows are icon + title + chip + subtitle + a trailing `…` overflow; they have no
chevron and no visible interactive surface. Directly beneath, the "Add account" section renders
rows **with chevrons** (`Add Email ›`), which reads as "these open something; those above do not".

**Impact.** The page teaches two row vocabularies four rows apart. Opening or editing an existing
account is only reachable through a control whose label is `…`.

**Fix.** Give account rows the same chevron-and-open behaviour (opening the account's editor), and
leave the overflow for the destructive/rare verbs.

### VIS-07 — positive — the narrow mode is the thing `VIS-01` should copy

`26-narrow.png` (438×800) is a well-formed single-pane screen: back chevron with the category name
as a heading, a reset action in the top-right, full-width content, one scrollbar, and vertical rhythm
preserved. This is the counter-example to Calculator's `VIS-01`: narrowing the window changes the
*navigation model* instead of silently removing capability. Worth pointing at whenever a narrow
regression is reported elsewhere.

## F4. Re-render checklist

Each is one build plus one command; the frames land in `/tmp/uxr/<app>`.

```sh
for a in settings monitor passwords maps git-client; do cargo build -p $a --locked; done
./target/debug/settings     --shots-dir /tmp/uxr/settings  --theme dark --responsive-matrix /tmp/uxr/settings-matrix
./target/debug/monitor      --shots-dir /tmp/uxr/monitor   --theme dark
./target/debug/passwords    --shots-dir /tmp/uxr/passwords --theme dark
./target/debug/maps         --shots-dir /tmp/uxr/maps      --theme dark
./target/debug/git-client   --shots-dir /tmp/uxr/git       --theme dark
```

For the chrome findings (`CHROME-01`, `CHROME-02`, `CHROME-03`, `CHROME-04`) the decisive render is
the same app **with and without** `--window-bar`: with it, the shared bar carries the app's slots;
without it, you see the row the app draws for itself. Two frames side by side settle each one.

For the motion findings, frames cannot help — `motion::testing::settled` deliberately renders the
resting pose, so a capture is the one thing that cannot show a transition. Part C stands on source.



# Part G — Individual apps

One subsection per app, grouped by domain. Findings are only things that would change what a
person can do or how they read the screen; per-app versions of the cross-cutting problems are in
Part E and are referenced rather than repeated. Each app ends with the parts that are genuinely
good, because several of them are the reference implementation for the rest.

## G1. Document and editing apps

### notes

- **`APP-notes-01` — medium — no heading outline for a long note.** Headings are first-class in
  a note's markdown (`src/markdown.rs:7`), but the app has no outline pane
  (`grep -rn "OutlinePane\|outline_pane" apps/notes/src` → nothing) and `BlockEditor` exposes no
  heading list. The word processor ships one (`apps/word-processor/src/outline_pane.rs:19`), so the
  pattern exists. Long notes are navigable only by Ctrl+F or scrolling.
- **`APP-notes-02` — minor — switching notes always resets the scroll to the top.**
  `src/main_panel.rs:700`: `if same_page { self.scroll_layout_dirty = true } else { self.scroll_y =
  0.0; self.glide.stop(); }`, and `scroll_y` is a single field (`:450`) — there is no per-page
  offset store. Reading note B, flipping back to A for a reference and returning to B lands at the
  top of B every time. (The same page *re-bound* after an edit is handled, so the intent was "no
  stale offset", not "no memory".) Fix: `HashMap<PageId, f64>` clamped by the extent, exactly as
  the `same_page` branch already does.
- **`APP-notes-03` — minor — no unsaved-work marker.** Autosave is silent by design and failures are
  loud and retryable (`src/status_bar.rs:74-77`, `src/save_notice.rs:98-120` — genuinely good), but
  `ToolbarLocation::edited` is never set, so the window bar never says so either (see `DUP-04`).
- Good: autosave-failure handling is the fleet's best (`needs_attention` sticky status + a
  notification that survives until dismissed), the empty state has a real focused CTA
  (`main_panel.rs:487-495`), and the shortcut sheet is the platform-aware reference the others
  should copy (`shortcuts_overlay.rs:23-40`).

### word-processor

- **`APP-word-processor-01` — medium — PageUp/PageDown do nothing.**
  `src/widget/input.rs:855` (`handle_navigation_key`) matches Left/Right/Up/Down/Home/End and
  returns `false` otherwise; `grep "PageUp\|PageDown" apps/word-processor/` has **no matches at
  all**. In the one app in the tree that actually paginates, you cannot page by keyboard — you hold
  an arrow key (dragging the caret) or grab the scrollbar. Every sibling handles it
  (`notes/main_panel.rs:1108`, `ebook/app.rs:1515`, `sheets/grid.rs:1900`,
  `pdf-viewer/app_events.rs:1268`, `code-editor/editor_view.rs:2561`).
- **`APP-word-processor-02` — medium — no "page N of M" readout, and the only page label is
  suppressed by any footer.** The status band reports words and characters only
  (`widget/controls.rs:1114-1131`); the one page label is drawn in the inter-page gap under
  `if !drew_footer && !foot_editing` (`widget/paint.rs:382`), i.e. it disappears exactly on the
  documents that have footer page numbers. There is no "Go to page" action either. Every peer gives
  a position readout (`code-editor` "Ln 12, Col 5", `notes` "4 notes, 142 words").
- **`APP-word-processor-03` — minor — `ToolbarLocation::edited` not set** (`widget/window_bar.rs:232-235`
  passes `..ToolbarLocation::default()`), so no unsaved marker. See `DUP-04`.
- **`APP-word-processor-04` — minor — its own zoom handler shadows the shared window zoom**
  (`widget/input.rs:1114-1129`), so window-level text zoom is unreachable in the one window where
  reading tiny text is most likely. See `DUP-02`.
- Good: the find/replace implementation is the model the shared matcher should be extracted from —
  regex, match case, whole word, replace-all applied as *one* action so a single undo reverts it
  (`widget/find.rs:27-60`, test `:403-460`), and replace correctly disabled for read-only
  (`:56-58`). Table Tab/Shift+Tab cell walking with append-row on the last cell, as one undoable
  action (`input.rs:818-837`). Formatting toolbar state derived from the selection's marks, not
  local state (`widget/window_bar.rs:60-110`).

### sheets

- **`APP-sheets-01` — notable — right-clicking a cell does nothing.** The entire right-press
  handler is a sheet-tab menu:
  ```rust
  // apps/sheets/src/app.rs:3799
  fn handle_right_press(&mut self, handled: &mut bool) {
      let p = self.last_mouse;
      // Right-click on a sheet tab opens its context menu.
      if self.tabs_rect.contains(p) && let Some(idx) = … { self.open_tab_menu(idx); … }
  }
  ```
  `:2911` routes `MouseButton::Right` only here, and the crate contains no `context_menu` at all
  (`grep -rn context_menu apps/sheets/src` → nothing) while every peer has one
  (`word-processor/widget/controls.rs:1102`, `slides/app.rs:2626`, `canvas/menu.rs:117`,
  `notes/app.rs:3378`, `pdf-viewer/app_events.rs:494`). Insert/delete row, clear contents and
  format — the most-used right-click operations in a spreadsheet — are only reachable from the
  command bar.
- **`APP-sheets-02` — notable — no find/replace over a workbook.** The Ctrl-chord block
  (`app.rs:3894`) handles `g`, `Shift+F` and `k` but not `f`; `grid.key` has no Ctrl branch
  (`grid.rs:1857`); the effect list has no find (`actions.rs:92-106`). The global palette does list
  cells (`resource.rs:423`), but that is a different gesture with no in-grid highlight and no
  replace.
- **`APP-sheets-03` — medium — cell edits and row/column structural changes are hard cuts.** The
  grid is app-owned (`grid.rs`, 3 920 lines) and the app has one motion reference in total, while
  the shared `DataTable` animates arrivals and reorders in every other list app (see Part C7).
  Worth a dedicated pass of `grid.rs` against `data_table.rs`.
- Good: `NULL` vs empty distinguished in db-client style; formula view; the name box and formula bar
  are correctly document-level rather than window chrome (`app/chrome.rs:1-9`, `:233`).

### slides

- **`APP-slides-01` — notable — the slide canvas cannot be zoomed.** The scale is unconditionally
  fit-to-window: `app/editor_chrome.rs:465` (`let (slide_dest, scale) = self.fit_slide(…)`) and
  `:569-579` (`let s = (avail_w / SLIDE_W).min(avail_h / SLIDE_H)`). There is no zoom state, no
  control, and the wheel over the canvas does nothing (`app.rs:4140-4152` handles only the rail and
  inspector). The word "zoom" appears once in the crate, in a comment (`app.rs:3498`). On a 13-inch
  panel the slide draws at roughly 30 % of its 960×540 design size — nudging a text box by 1 px or
  reading 9 pt body copy is impossible.
- **`APP-slides-02` — medium — Ctrl+letter runs the plain-letter shortcut.** After three guarded
  cases (`if ctrl && lc == "s"`, `"o"`, `"d"` at `app.rs:4402-4413`) the code falls into
  `match lc.as_str() { … }` **with no `ctrl` check**, so `"p" => self.enter_present()`,
  `"n" => self.new_slide()`, `"g" => overview`, `"t"/"r"/"o"/"l" => insert_element(…)` all fire
  under Ctrl. Ctrl+P (print, the chord every app shares) starts a full-screen slideshow.
- **`APP-slides-03` — notable — no clipboard: elements and slides cannot be copied, cut or pasted.**
  No `ui::clipboard_*` call anywhere in the crate; the shortcut table (`app.rs:4414-4449`) has no
  `c`/`x`/`v` arms (only `d` for duplicate). The verbs exist but are unreachable
  (`deck.rs:128`, `:454`, `:591`), and peer apps all wire it (`canvas/gesture/edit.rs:231-255`
  including SVG paste). You cannot copy a shape between slides, and cannot paste a screenshot into a
  deck — the only image path is the file picker (`app.rs:68`).
- Good: thumbnail drag-reorder with edge auto-scroll, and Escape aborting a pending drag without
  mutating the deck (`app.rs:3272-3278`, `:4177-4183`). The rail's "SLIDES" header is correctly
  panel chrome (`:1039-1043`).

### canvas

- **`CHROME-03` above** — the header row that `NavShell` would otherwise put in the bar.
- **`APP-canvas-01` — minor — Ctrl+= accepts any modifier combination**
  (`surface.rs:1716-1717` `("+" | "=", _, _)`), so the chord fires in cases the rest of the system
  reserves. See `DUP-02`.
- Good: typed clipboard format carrying layer info, plus SVG-markup paste from other apps, exposed
  in both the context menu and the shortcut sheet (`gesture/edit.rs:231-255`, `menu.rs:117-135`,
  `panels/shortcuts.rs:65`). Selection strip is correctly separate from window chrome.

### typst-editor

- **`CHROME-02` above** — the full in-body command bar.
- **`APP-typst-01` — notable — no find or replace anywhere.** The Ctrl table at `app.rs:1665` is
  `"s"`, `"o"`, `"e"`, `"="`, `"-"`, `"0"`, `"j"`, `"c"` — no `"f"`/`"h"`, and the crate implements
  no `Findable`; the source pane is the shared `CodeEditor` (`editor.rs:64`), which contains zero
  `Findable` occurrences in `ui-core`. So Ctrl+F opens the platform search narrowed to the app,
  which cannot find text in the open file, step between matches, or replace.
- **`APP-typst-02` — medium — the editor never says the file on disk is stale.** The window bar's
  location is the bare file name (`chrome.rs:229`) with no modified marker; the save reporter carries
  only `Saving`/`Saved`/`Failed` (`tilo-app/src/framework/saves.rs:47-61`), never dirty; saving is
  deliberately silent (`app.rs:632`); and no "unsaved"/"modified" string exists in the locales.
  Compare `code-editor/src/status_bar.rs:95`. Fix: track `saved_source` per file as
  `code-editor/model.rs:1091` does, and mark the name.
- Good: the preview's failure states are honest and specific — Compiling / Blank / Failed with an
  explanation (`preview.rs:1261-1279`).

### code-editor

- **`APP-code-editor-01` — medium — no zoom or font-size control.** `settings.rs:8` holds only
  `language_servers` and `format_on_save`; "zoom" does not occur in the crate; the only wheel handler
  is for the completion list (`editor_view.rs:2462-2481`); text size comes only from the OS theme
  (`ui-core/widget/code_editor.rs:549`). Ctrl+scroll and Ctrl+= are muscle memory in every code
  editor, and every sibling text surface has a zoom (`word-processor/zoom_control.rs`,
  `pdf-viewer/app.rs:1791`, `canvas/panels/header.rs:787`, `ebook/reading_settings.rs:576`).
  `Theme::with_text_scale` already exists (`code_editor.rs:1413`).
- Good: the reference implementation for unsaved work — Save / Don't save / Cancel on file switch and
  on close, plus a distinct "changed on disk" Reload / Overwrite / Cancel conflict question, both
  non-dismissable modals (`app/dialogs.rs:96-131`, raised at `app.rs:1403-1415`); one of only two
  apps setting a live `ToolbarLocation::edited` (`app/window_bar.rs:27`); and a full `Findable`
  (`app/find_workflow.rs:17`).

### pdf-viewer

- **`APP-pdf-viewer-01` — medium — a PDF always reopens at page 1.** `src/markup.rs:293` — the
  persisted `PdfMarkup` carries source, pages, annotations, comments, signatures and form values,
  but no reading position; `current_page` is app-local (`app.rs:2132`) and the scroll lives in the
  in-memory viewport (`:1798`). The ebook proves the architecture supports it
  (`ebook/app.rs:1619 persist_progress`). Fix: `reading_position` on the record (page + fraction +
  zoom) written on page change / scroll settle and restored after `open`.
- Good: annotation and markup tooling is the deepest in the tree — text/underline/strikeout, five
  marker colours, ink/note/text/sign, a shared selection HUD with Copy + swatches + Note, a real
  outline resource with stable ids and a `CopySelection` effect
  (`actions.rs:88-230`, `app.rs:722-730 sel_hud`).

### ebook

- **`CHROME-01` above** — the whole window bar re-implemented twice in-body.

### hex-editor

- **`APP-hex-editor-01` — medium — no copy/cut/paste of bytes.** `handle_global_chord`
  (`view.rs:1253`) accepts `"o"|"s"|"f"|"h"|"g"|"i"|"a"` and nothing else; `grep clipboard` in the
  crate returns **no matches**. The view has a real byte selection (`select_all` on Ctrl+A,
  shift-arrow extension at `:1282-1305`), but "copy these 16 bytes as hex" and "paste a hex string
  at the cursor" have no path — the only way to move bytes is typing them. Fix: Ctrl+C copies the
  selection in the current focus mode via `ui::clipboard_copy`; Ctrl+V parses with the existing
  `crate::actions::parse_bytes` and inserts through `model.replace_spans`, sharing the one-undo-step
  discipline already in `find.rs:submit_replacement`.
- Good: find/replace works across both modes — hex and text/UTF-8 queries, replace-all over every
  match (not just the painted set), one undo step per batch, and a refusal that names the fix when
  an overwrite-mode replacement changes length (`view/find.rs:27-130`, `:155-172`).

## G2. Communication and scheduling

### chat

- **`APP-chat-01` — notable — the timeline has one date divider, for the oldest loaded message, and
  rows carry a clock but never a date.** `render_chat.rs:831-846` draws the divider once at the top
  of the loaded window; each group header paints only `clock_label(m.ts)` (`:1201`). Scroll a screen
  and the divider is gone, so a message from three days ago reads "14:32" with no day boundary.
  Fix: emit a divider whenever `day_label` differs from the previous message's day (the measure pass
  already walks groups), and carry the day in the group header when it changes.
- **`APP-chat-02` — notable — shared images and files can never be opened, enlarged or saved.**
  `draw_attachment` (`render_chat.rs:1605-1676`) paints the media card and returns nothing; it never
  calls `push_hit`, and `enum Hit` has no attachment variant. The message menu (`app.rs:170-190`)
  offers Reply-in-thread, Edit, Pin, Delete, Report. The only way to see a screenshot is to squint at
  the inline thumbnail, and a shared PDF cannot be opened at all.
- **`APP-chat-03` — medium — the attach picker silently hides everything past six rows.**
  `render_chat.rs:2642` `.take(max_rows)`, `:2655` computes `more`, `:2761` paints
  `format!("{more} more in the workspace")` as **plain text with no hit rect**, and the panel has no
  scroll (`attach_picker_max_rows` caps at 6, `:2777-2784`) while the source list holds up to 60
  entries (`picker.rs:80`). The picker looks broken rather than truncated.
- **`APP-chat-04` — medium — uploading an attachment reports no progress and cannot be cancelled.**
  `events.rs:1890-1900` sets `attachment_status = format!("Uploading {name}…")` and
  `AttachmentImportPending` carries only `token` and `channel`; no cancel exists anywhere. Dropping a
  200 MB video shows a static sentence for a long time with no way out.
- **`APP-chat-05` — medium — mention notifications cannot be acted on.** `mention_alerts.rs:94-100`
  builds a notification with no `.with_activation(...)`, and chat never calls
  `set_activation_handler` — while browser-parts, calendar, clocks, mail and notes all do (e.g.
  `mail/app.rs:517`). Clicking "Ada mentioned you in #design" raises Chat but leaves whatever channel
  was open. Fix: a `chat:message/<id>` activation handled by the existing `reveal` path.
- **`APP-chat-06` — minor — a saved draft is invisible in the channel list.** Drafts are kept per
  channel (`composer.rs:32-40`) but `sidebar.rs:146-155` never derives a "Draft: …" secondary line.
- Good: send refusals restore the text and the reason beside the composer
  (`events.rs:1701-1707`); drafts persist in the person's record; live draft presence;
  mention autocomplete (`composer.rs:768-793`); mention alerting correctly excludes your own
  mentions and the channel you are reading (`mention_alerts.rs:44-80`).

### mail

- **`APP-mail-01` — notable — the "Undo" on a sent message disappears 6 s before the message is
  actually sent.** `app.rs:92 SEND_UNDO_SECS = 10` holds the message for ten seconds
  (`:2614`), but the only undo affordance is the bar notice (`:2211`), and every notice is dropped at
  `NOTICE_LIFETIME = 4 s` (`toast.rs:227`). Nothing re-shows it, so anyone reaching for Undo a beat
  late finds nothing and the mail goes out.
- **`APP-mail-02` — notable — a message the server refused has no outbox view and no Retry.**
  `app.rs:2505-2510` reopens the composer only if it is closed; otherwise the failure is a transient
  toast. Nothing renders `surface.toml`'s declared `outbox` resource — `list.rs`, `reader.rs` and
  `state.rs` never mention it, and `Folder` has no Outbox row. After a restart the failed message is
  recoverable only through an agent-level action.
- **`APP-mail-03` — medium — "Select all → Mark read" only covers the pages already fetched.**
  `state.rs:781-789 select_all_visible` walks `list_rows` → `mailbox.threads(folder)`, i.e. loaded
  messages; loading is paged at 50 (`sync.rs:114`, `engine.rs:1274`). On a 400-message inbox,
  Ctrl+A then Mark read reports "Marked 50 messages read" and leaves a wall of unread mail.
- **`APP-mail-04` — medium — search results carry no facets.** `search.rs:111-126` builds
  `SearchEntry`s with fields and a subtitle but no `.facet(...)` anywhere in the crate, while the
  framework renders facet chips (`tilo-app/framework/views/chips.rs:68`) and Tasks uses them
  (`tasks/model/search.rs:24`). "invoice" returns everything in every folder with no way to narrow
  to unread, one sender, one folder or a date.
- Good: draft durability (`app.rs:1651-1656` saves a touched composer on dismiss and materialises a
  real Drafts row at `:1970-1985`); the actionable new-mail notification (`:517`, `:559`); quoted-text
  folding (`reader.rs:748`, `:1084-1086`); recipient completion from the address book
  (`compose.rs:1110-1146`); a real shortcut sheet bound to `?` (`shortcuts_overlay.rs`, `app.rs:5293`);
  per-folder empty states (`list.rs:36-56`); swipe triage (`list.rs:88`).

### contacts

- **`APP-contacts-01` — notable — a contact page offers only Copy and Edit; you cannot call, text or
  email from it.** `detail.rs:176-205` builds phone and email properties with `.copyable()` and
  `Request::EditField`; `Request` (`view.rs:52-85`) has `OpenWebsite` but no Compose/Call/Message,
  and `ContactsEffect` (`actions.rs:23-70`) likewise. The most common reason to open a contact is
  absent — you copy the address and switch apps. Mail already has `mail.contact-draft` rows
  (`mail/src/contact_recipients.rs`).
- **`APP-contacts-02` — medium — its "Save changes?" dialog has no Cancel** (`app/dialogs.rs:245-253`),
  unlike the shared `close_admission` path which offers Save / Discard / Keep editing. See `DUP-04`.

### calendar

- **`MOTION-05`, `MOTION-06` above** — no hover on the month grid; no transition on period nav or
  swipe.
- **`APP-calendar-01` — medium — there is no quick-add; every new event starts at 12:00–13:00 and
  needs the full form.** `app.rs:1612-1633` seeds `NaiveTime::from_hms_opt(12, 0, 0)` and
  `title: String::new()`. No text parser exists anywhere in the app
  (`grep -i "quick\|parse_when\|natural"` → only "natural" sizing in `modal_dialog::layout`), and even
  the Tasks entry field takes only a name (`tasks.rs:180-185`). "Lunch with Sam Friday 1pm" costs a
  title, two date pickers, two time spinners and a time-zone row.
- **`APP-calendar-02` — medium — the app's fastest features are undiscoverable.** Single-key
  shortcuts exist — `"m"/"w"/"d"/"a"/"t"/"n"/"["/"]"` (`app.rs:3742-3750`) — but Calendar ships no
  shortcut sheet (`grep -rln shortcuts apps/calendar/src` → nothing) while Mail and Notes do. Same
  for feeds and chat (`DUP-03`).
- Good: recurring-event editing asks only-this / all-future (`app/repeat_scope.rs:1-20`); drag-create
  and resize snap to 15 minutes (`timeline_interaction.rs:29-31`); all-day keeps RFC 5545 exclusive
  ends (`app.rs:2502-2530`); time zones are authored correctly per event and folded
  (`shown.rs:1-10`, `timezone.rs:44-78`, "Time zone" row at `app/editor.rs:329-343`); a pristine new
  draft is discarded rather than left behind (`app.rs:1676-1696`).

### messages

- **`APP-messages-01` — notable — the thread shows no timestamps at all.** `history.rs:236-259` builds
  rows from text, direction and delivery only; `tilo_messaging::Message` does carry
  `timestamp_ms` (`crates/platform/tilo-messaging/src/history.rs:38`), and the group panel keeps it in
  a JSON answer but never paints it (`groups/operations.rs:236-244`). "see you at 8" is
  unverifiable — no time, no day separators.
- **`APP-messages-02` — medium — the composer has no keyboard path: no Enter-to-send, no shortcut at
  all.** `grep -rn "NamedKey\|Key::Character" apps/messages/src` returns nothing; the composer is a
  bare `TextInput` plus a `Button` (`view.rs:749-789`). Return inserts a newline, as in every other
  messenger.
- **`APP-messages-03` — medium — the draft being typed is lost when the window closes.** The draft is
  a widget-local in-memory `LocalState` (`sending.rs:14,22`, `ui-core/local_state.rs:27-48`, no
  persistence) and `surface.toml` has no draft field, unlike chat's `chat.drafts`.
- **`APP-messages-04` — medium — threads beyond 128 messages dead-end on a caption.**
  `history.rs:272-273` pushes `Text::new("messages-more-history")` ("Showing recent messages") with no
  action, and the client has no cursor to page with (`tilo-messaging/src/history.rs:118` always
  asks for `MAX_HISTORY_MESSAGES = 128`, `:12`). Older SMS is permanently unreadable from Tilo.
- Good: per-message delivery state is exposed and mapped honestly including `Uncertain`
  (`history.rs:259-271`), and a refused send is attributed (`sending.rs:120-131`).

### feeds

- **`APP-feeds-01` — medium — a failed refresh of a specific subscription discards both the feed and
  the reason.** `app.rs:1320-1326` keeps only `refresh.failed_feeds += 1`; the app then reports an
  aggregate "N of M subscriptions failed to refresh" (`:1341-1346`) and no sidebar row is marked. With
  20 subscriptions you are told two failed and cannot find out which two or why; the failing feed
  keeps looking healthy while silently never updating.
- Good: j/k/m/s/t/r/a/n/e/i and 1/2/3 triage (`app.rs:1516-1531`), filter-scoped mark-all-read gated
  on `unread_count_for(filter)` (`:180-181`), per-filter empty copy (`list_pane.rs:190-207`).

### discord

- **`APP-discord-01` — medium — the page shows counts only: no channel list, no linked account, no
  unlink.** The module doc promises "People see … who linked their account" (`resource.rs:2-3`) but
  `Show` holds only `channels: usize, links: usize` (`:20-30`); the Link section offers
  `discord-link-account` unconditionally, even after linking, and `DiscordEffect::UnlinkMine`
  (`effects.rs:44`, answered at `:192`) is rendered by `Reading.unlink` (`:148`) — nowhere.
  After linking, the page still says "Link my Discord account" with a stale `/link` code.
- Good: the bridge reports refusal, unreachable and unreadable distinctly
  (`locales/en-US.ftl:28-34`) and states why calls do not reach Discord (`resource.rs:57-66`).

### tasks

- **`APP-tasks-01` — medium — there is no filtering or sorting at all.** The sidebar is exactly a list
  place and a board place (`lib.rs:119-127`); `grep -rni "filter\|sort" apps/tasks` only matches
  iterator calls. There is no "mine", no label filter, no text search outside the platform palette.
  With a few dozen issues the list is unusable.
- **`APP-tasks-02` — medium — an issue has no date and no priority.** `model/issue.rs:139-180` —
  number, title, body, status, assignee, model, subject, labels, parent. You cannot ask "what's due
  this week" or sort by urgency.
- **`CHROME-05` above** — in-body Back beside the bar's own navigation.
- Good: facets in the platform search (`model/search.rs:22-26`); board drag is one undoable `place`
  action (`board.rs:1-6`); a real list empty state (`list.rs:64-71`).


## G3. Media, maps and browsing

### browser

Tabs, the new-tab page and per-tab mute are the **desktop's** responsibility
(`crates/sdk/ui-core/src/shell_tabs.rs:1`, asserted in
`apps/browser/src/browser_window.rs:1041`), which is why the browser-side gaps below are about the
page, the library and the prompt rather than about tabs.

- **`APP-browser-01` — medium — a tab can never be muted.** The window tracks the state
  (`browser_widget.rs:3096 WebViewEvent::AudioMutedChanged`) but never renders it and never sends the
  command the engine already implements (`ui-core/webview/mod.rs:565 SetAudioMuted(bool)`, wired at
  `apps/browser/chromium/src/host.cc:1113`); `grep -rn SetAudioMuted apps/` → nothing. There is not
  even a "this tab is playing audio" indicator. Landing on an autoplaying video leaves no way to
  silence that page short of changing system volume or closing it.
- **`APP-browser-02` — medium — bookmarks never appear in the address bar's suggestions.**
  `omnibox::build_rows` (`omnibox.rs:400`) merges typed intent, history and engine suggestions;
  `grep -i bookmark omnibox.rs address_overlay.rs` → nothing, and the index is rebuilt from history
  only (`address_rows.rs:143`). Bookmark "Annual report", later type "annual", get nothing — the only
  path is Library → Bookmarks → scroll (`library.rs:23 ROW_LIMIT = 50`).
- **`APP-browser-03` — medium — a history entry cannot be removed individually.** Library history
  rows are `ListRow::new(format!("{id}.open"), title)` (`library.rs:98`); the only deletion is the
  all-or-nothing Settings row (`browser_widget.rs:6843`), although the resource declares a `forget`
  verb. Removing one visit means wiping all history. Bookmark rows already show the per-row trailing
  button pattern (`library.rs:141`).
- **`APP-browser-04` — medium — large parts of the UI are hardcoded English while a German catalogue
  ships.** `library.rs:207 "Show in Files"`, `:212 "Retry"`, `:258 "No downloads yet"`,
  `:345 "Library"`/`"Done"`, `browser_widget.rs:300 "Waiting for a save destination…"`,
  `browser-parts/src/capabilities.rs:385 format!("{origin} requests {rights}. Expires in 60
  seconds.")`, `site_controls.rs:411 "Block permissions"`/`"Ask again"`/`"Clear site data"` — while
  `browser-parts/src/lib.rs:115` declares `embedded_catalog!(… ["en-US", "de-DE"])` and
  `de-DE.ftl` (14 KB) translates the menu and toolbar. A German user gets a translated toolbar over
  an English Library, download list, permission prompt and site panel — the surfaces where a decision
  has consequences.
- **`APP-browser-05` — minor — a finished download leaves a permanent toolbar chip.**
  `capabilities.rs:214` exposes `downloads.terminal_notice`, which is only reassigned when *another*
  download ends (`:331`) and is rendered as an always-present icon whose accessible name is that
  stale text (`toolbar.rs:322`). The toolbar keeps claiming "Download complete" about a download
  from an hour ago.
- Good: back/forward long-press with a bounded history menu (`browser_widget.rs:4828`,
  `browser-parts/src/back_hold.rs:24`); find-in-page with "N of M" via the shared bar
  (`:3789-3830`); per-origin zoom held by the engine (`chromium/src/host.cc:1215`); crashed-tab
  recovery that names what happened and offers Retry (`:3129-3134`, `:2734-2750`); a full download
  lifecycle (progress, "12 MB of 40 MB", Cancel, Retry, Show in Files, portal save picker, and a
  leave/keep choice before exit — `library.rs:176-220`, `browser_widget.rs:1127`); permission prompts
  with expiry and a per-site panel (`capabilities.rs:385`, `site_controls.rs:400-425`); reader view
  reporting its real outcome (`:3851`, `:7708`); tiered frecency omnibox with inline completion,
  debounced engine suggestions and a 10k-page perf test (`omnibox.rs:158-660`).

### tube

- **`APP-tube-01` — notable — watching never resumes.** `app.rs:1568` resets `self.model.playhead =
  0.0` on open; watch history stores only an id and a timestamp (`history.rs:21
  watched: BTreeMap<String, u64>`), and `grep -rni "resume\|continue watching" apps/tube/src` finds
  nothing. A 90-minute video closed at minute 70 restarts at 0:00, and the History screen shows no
  progress bar, so half-watched is indistinguishable from watched. This is the most-felt gap in any
  video app.
- **`APP-tube-02` — notable — the Resolution menu is a preference that never reaches the stream.**
  Choosing an option lands at `app.rs:1370` → `change_preferences`, which writes only
  `settings.preferred_quality` (`:820-833`); stream selection ignores it (`api.rs:547` picks
  `[22, 18]`, `backend/proxy.rs:12 KNOWN_MUXED_ITAGS`). Pick 480p on a metered connection and the
  video keeps streaming 720p, silently, costing data.
- **`APP-tube-03` — medium — no playback speed.** `model.rs:405` (`Settings`) has autoplay,
  show_comments, sponsorblock_enabled and preferred_quality; `grep -rni "speed\|playback_rate"` over
  `apps/tube` and its locales → nothing, and the shared player exposes no rate control either.
- **`APP-tube-04` — medium — no captions/subtitles at all.** `grep -rni "captionTracks\|timedtext\|
  transcript" apps/tube crates/` → no caption parsing, and `prepare_player` (`screens.rs:803`) offers
  only resolution and a SponsorBlock skip. Deaf and hard-of-hearing users, non-native speakers, and
  anyone watching a foreign-language video have no path.
- Good: player keyboard control when the surface is focused (Space/←/→/↑/↓/Home/End/F/M —
  `ui-core/widget/video_player/input.rs:20-62`); distinct honest failure sentences for
  unplayable/offline/no-source/playback-limit; Download refuses explicitly rather than failing
  silently.

### maps

- **`APP-maps-01` — medium — one route, no alternatives.** `net.rs:368` requests
  `?overview=full&geometries=geojson&steps=true` and `:391` reads `.get(0)`; `alternatives=true` is
  never sent and `grep -rni alternative apps/maps/src` only matches transit-itinerary comments.
  Transit offers several journeys (`Hit::Journey(usize)`, `app.rs:132`); car/bike/foot do not. When
  the single route is bad, the person must retype the destination or pan by hand.
- **`APP-maps-02` — medium — raw transport errors are shown to the person.** `app.rs:1992
  format!("Search failed: {e}")` where `e` comes straight from the network layer
  (`net.rs:105 .map_err(|e| e.to_string())`, `:107 format!("HTTP {}", resp.status())`), published into
  the window bar (`:2805 publish_status`, `:2813`). Same shape for routing, transit and location
  (`:2048`, `:2069`, `:2162`). People see "error sending request for url
  (https://router.project-osrm.org/…): connection refused". The browser translates engine errors
  (`browser_widget/load_failure.rs:41`); maps should too, keeping the raw text in the log.
- **`APP-maps-03` — medium — an offline map download cannot be stopped.**
  `OfflineAction` (`offline_panel.rs:46`) has `Download`, `DeleteRegion`, `ClearCache` and no Cancel;
  `app.rs:2323` starts a multi-zoom tile fetch that reports progress (`offline_panel.rs:764`) and
  `grep -rn cancel apps/maps/src/{tiles.rs,app.rs}` → nothing. Pick the wrong city and you must wait
  out hundreds of tiles, then delete the region.
- **`APP-maps-04` — medium — the whole map UI is untranslated, status line included.**
  `lib.rs:67` declares `embedded_catalog!(… ["en-US"])` and `locales/en-US.ftl` holds only 27 lines of
  collaboration-history verbs; every user-facing string is inline English
  (`app.rs:3795 "Use my location"`, `:1435 "Locating…"`, `:1989 "{} results for “{}”"`,
  `:2090 "Nearby places couldn't be loaded"`, `:2321 OFFLINE_AREAS_REFUSED`) — in a tree where a
  sibling app already ships `de-DE`.
- **`CHROME-04` above** — declares no bar slots.
- Good: arrow pan, `+/-` zoom, `d/t/f/s/1/2/m` shortcuts and Escape unwinding in priority order
  (`interaction.rs:63-160`); a labelled Swap button (`endpoint_editor.rs:496`); "Use my location" with
  an honest "Location unavailable — reason" when the portal refuses (`app.rs:2162`).

### weather

- **`APP-weather-01` — minor — precipitation stays in millimetres under Imperial.** `units.rs`
  converts temperature and wind only; `detail.rs:856` formats `"{:.1}"` `.unit("mm")` and
  `grep -rn '"mm"' apps/weather/src` matches that one line. A person who chose Imperial (°F/mph,
  `region.rs:16`) still reads "2.4 mm" of rain. Fix: a `PrecipUnit` in `Units`, `.in(2)` under
  imperial.
- **`APP-weather-02` — medium — the hourly and daily forecasts have no accessible equivalent.**
  `detail_accessibility` (`core/detail.rs:47`) exposes only the Retry button, "Current conditions"
  and "Air quality" groups (`:70-91`); the hourly strip (`chart.rs:193`) and the 7-day list are
  painted only. A screen-reader user hears that 24 hourly and 24 daily readings are available and can
  never read one — and the visual scroll keys have no semantic target.
- **`APP-weather-03` — minor — the app never says how old the forecast is.** `model.rs:279
  fetched_at` is read only by the desktop summary (`presentation.rs:134`) and the probe
  (`main.rs:92`); no locale key states an observation time. After a restart a restored snapshot can
  be days old with no age indicator, because only an *observed* failure raises a banner
  (`core.rs:741`), which a cached snapshot never does. Fix: "Updated 14:05", restyled once stale.
- Good: hourly scrolling with wheel, drag bar, Arrow/Home/End paging, edge fades and clamping
  (`core/detail.rs:350-441`); failures raise a distinct `UpdateFailed`/`Offline` banner with Retry
  (`core.rs:74`, `detail.rs:569`); a failed place search resolves to "Couldn't search for places.
  Check your connection." (`core.rs:744`).

### photos

- **`APP-photos-01` — medium — no full-screen or slideshow presentation.** The header `Hit` enum
  (`app.rs:208-230`) is the complete command set and contains no play/fullscreen entry;
  `set_fullscreen` is used by `remote`, `slides` and the video player but nowhere in Photos. There is
  no way to show photos to someone across the room, and no set can advance while you talk.
- **`APP-photos-02` — medium — a batch thumbnail failure discards the whole batch and names the wrong
  thing.** `thumbnail/src/batch.rs:11` documents "A batch is all or nothing"; `:96` fails on the first
  undecodable input with `"input {index} is not an image"` where `index` is a position in the
  *deduplicated* vector, surfaced verbatim (`photos/src/actions.rs:408 .map_err(|e| e.to_string())`).
  Import 500 photos with one corrupt file and you get no thumbnails and an ordinal you cannot map to
  a file. Fix: per-input outcomes, and name the offending content.
- Good: grid density in 5 steps with pinch/wheel/AT (`navigation.rs:527`); rotate/crop/reset as real
  actions (`actions.rs:190-198`); EXIF-aware details with a metadata editor (`app.rs:247-266`).

### camera

- **`APP-camera-01` — minor — toast severity is guessed from the English text.** `app/notice.rs:93-108`
  classifies an error by `starts_with("Save failed")`/`"Couldn't"`, so rewording silently downgrades a
  failure to a success pill. Use `ToastKind`.
- **`APP-camera-02` — minor — no zoom/night/timer discoverability audit** — the header is
  `WindowHeader`-style full-bleed (`app.rs:2934`) with all controls in-body, which is defensible for a
  viewfinder, but it means nothing about the camera is discoverable from the window bar.

### music

- Good, and worth using as the reference for transport: scrubber, volume, mute, shuffle, repeat,
  previous/next and like are all bound to real effects rather than callbacks
  (`app/shell.rs:131-181`), volume is persisted (`model.rs:217`, `storage.rs:400`), and resume-on-open
  works (`app/local_files.rs:315 seeks position_ms`). The query view is a 1 400-line duplicate — see
  `DUP-07`.

### video-player

- **`APP-video-player-01` — medium (notable for the resume half) — the player forgets volume and speed
  and never resumes.** `preferences.rs:18-26 PlayerSettings` holds only autoplay, loop and recent
  history; volume is a widget default (`ui-core/widget/video_player.rs:392 volume: 0.8`); rate is
  re-derived from the engine on each open (`app.rs:216`); opening goes straight to `open`/`play` with
  no seek (`:1328-1333`). Every launch the volume jumps back to 80 %, 1.5× is forgotten, and a
  2-hour lecture paused at 1:40 restarts at 0:00 *even though it is in Recent* — while Music does
  resume. Fix: persist volume, rate and a per-item `position_ms`, and seek on open.
- Good: no persistent chrome bar (`app.rs:5-8`), which is right for a player.

### video-editor

- **`APP-video-editor-01` — minor — its shortcut sheet prints `Ctrl+O`/`Ctrl+S`/`Ctrl+Shift+S` as
  literal strings** (`app.rs:7532-7541`), so macOS users are taught the wrong modifier, and it is one
  of six sheets (`DUP-03`).
- Good: export has real progress and a working cancel (`export.rs:215-225`, `:598-601`).

### web-search (service — no window)

- **`APP-web-search-01` — medium — safe search is hard-coded off with no setting.**
  `crates/platform/tilo-web-search/src/engines/google.rs:79 ("safe", "off")`, and
  `apps/web-search/src/actions.rs:92 search_params()` exposes only `query`, `count`, `engines`. The
  other engines send no safe parameter at all. This service is invoked by agents and shortcuts on a
  person's behalf (`main.rs:12`), so a household or work profile has no way to ask for filtered
  results.
- **`APP-web-search-02` — minor — results cannot be paged.** `actions.rs:43 MAX_COUNT = 20` and no
  `page`/`offset` is sent to any engine, while `fetch` does page correctly via `next_offset`
  (`:130`) — the inconsistency is visible inside one contract.
- Good: cross-engine merge with URL dedup and score ranking (`merge.rs:29`), and per-engine status
  (`ok`/`blocked`/`rate_limited`/`failed`/`skipped`) in the schema.

### thumbnails (compute package — no window)

- Covered by `APP-photos-02`; the all-or-nothing batch is the whole finding.


## G4. System, storage and administration

### file-explorer

- **`APP-file-explorer-01` — notable — there is no search of the folder you are in.** The app's own
  `search_provider.rs:8` states the scope: *"Only stable, in-memory sets are searchable — never the
  live directory contents"*. The keyboard map (`files_app.rs:8954-9132`) has Ctrl+L, Ctrl+H,
  Ctrl+0/+/-, F2, F5, F1 and no Ctrl+F; the only lookup is prefix type-ahead
  (`files_app.rs:9110-9118`), which jumps one name at a time. In a 5 000-file folder, finding
  `report-q3.pdf` is scrolling or guessing the first letter.
- **`APP-file-explorer-02` — medium — resized columns are lost on relaunch.** `resource.rs:864-875
  FilesPreferences` persists view, sort, order, zoom and hidden files but no widths; the widths live
  in an in-memory slot (`views/file_view.rs:285-286`, `files_app.rs:947-949 list_slot`). Drag the Size
  column wide, quit, and it is back to default every session.
- **`APP-file-explorer-03` — minor — two sort affordances with different visuals.** A plain button in
  the toolbar (`toolbar.rs:319-324`) plus the shared sort indicator in the list header
  (`file_list.rs:313-315`). See `DUP-09`.
- Good: conflict resolution is a real choice, not a silent overwrite (`files_app.rs:87-89` maps
  keep-both/skip/replace/ask onto `Conflict::*`, surface in `files_app/clash.rs`); transfer progress
  and cancel (`files_app.rs:1307`); delete goes to Trash first and is undoable (`file_ops.rs:96`);
  published selection actions with whole-batch validation (`select-entries`, `copy-selection`,
  capped at 256 ids); sort preserves selection, cursor, range anchor and an active inline rename.

### terminal

- **`APP-terminal-01` — medium — copy-on-select and OSC 52 clipboard are per-window flags that reset
  every launch.** `main_body.rs:872 select_to_copy: false` and `allow_osc52_clipboard: false` are
  widget fields whose only production mutator is the context menu toggling them in place
  (`:4892-4896`); nothing in `resource.rs` or any prefs section stores them
  (`set_select_to_copy` is called only from tests, `src/tests.rs:3829`). Enable copy-on-select on day
  one and re-enable it every time you open a terminal, with no explanation.
- **`APP-terminal-02` — medium — the bell is a no-op.** `main_body.rs:5359-5361`:
  ```rust
  // Everything else, including 7 (BEL), is a no-op — the toolkit has no audible bell yet
  _ => {}
  ```
  and there is no visual fallback either (`grep urgency|visual_bell apps/terminal` → nothing).
  `printf '\a'`, `beep` and shells configured to beep on completion produce nothing — exactly when
  you have tabbed away. Fix: treat BEL as an effect (flash the border / set a window-bar attention
  hint) until an audio path exists; pairs with `SHELL-02`.
- Good: find-in-scrollback with match cycling (`main_body.rs:584-595`, `:1466`); reflow on resize
  (`:2156`); OSC 8 links opened through the native opener and refused for non-web targets
  (`:177-195`).

### settings

- **`APP-settings-01` — medium — internal file paths are printed in the UI.** The External Accounts
  page renders `/home/user/.config/qos/mail/accounts.json` in monospace directly under the section
  description (confirmed in the committed capture, `apps/settings/shots/settings.png`). Implementation
  leakage in a settings page; it belongs in a "details" disclosure at most.
- **`APP-settings-02` — medium — connection status is conveyed by colour alone.** "Connected" is green
  text with no icon and no text alternative (`settings_app` External Accounts rows). Colour-only
  status fails for colour-blind users and in high contrast.
- **`APP-settings-03` — see `VIS-03`** — at narrow width the content column is clipped rather than
  collapsed.
- Good: search reaches individual settings and renders them as their real rows
  (`settings-core/settings_app.rs:557-580`); failed writes are announced with the reason
  (`:932-953`); the sidebar draws no search field of its own (asserted at `:2084`).

### store

- **`APP-store-01` — notable — an install can be neither cancelled nor watched.**
  `app.rs:2283` refuses every other action with "a package operation is already in progress";
  `StoreEffect` (`resource.rs:61-360`) has no cancel verb; the worker's install path is one blocking
  call with no progress channel (`worker.rs:1012-1014`), while *runtime repair* does stream bytes
  (`worker.rs:793`). A 400 MB Flatpak shows "Installing…" indefinitely with no size, no percentage and
  no way out, and the whole Store refuses to do anything else meanwhile.
- **`APP-store-02` — medium — installed size appears nowhere in lists.** Per-row meta is only
  `store-device-system` / `-kept` / `-cached` (`view.rs:1592-1610`); size exists only in the detail
  facts (`:966-974`), and there is no total anywhere. "On this device" answers what is installed, never
  what is eating the disk.
- **`APP-store-03` — minor — removing five apps means opening five pages.** `action_row`
  (`view.rs:262-272`) gives each row a single trailing control; the destructive remove button exists
  only on the app page (`:820-830`).
- Good: permission disclosure is complete including the "we don't know" case (`view.rs:1666-1691`);
  search has a real empty state with a way out (`:565-570`) and splits results by source; permissions
  sorted broadest-first with a warning icon.

### logs

- **`MOTION-07` above** — Follow never follows. This is the app's headline feature.
- **`APP-logs-01` — minor — a log entry cannot be copied.** `JournalEffect` (`actions.rs:20-45`) is
  Refresh, SetLiveTail, ExportFilteredJsonl, SelectEntry; `grep clipboard apps/logs/src` → nothing.
  The inspector renders the message as read-only rows, so getting two lines of a failed unit into a bug
  report means exporting a `.jsonl` and digging. Fix: Copy message / Copy entry / Copy all filtered.
- Good: per-filter export, selection-driven reveal (`view.rs:549`), and the "Reading the system
  journal" state is at least honest about work in progress (see `DUP-08` for why it should be a
  skeleton).

### monitor

- **`APP-monitor-01` — medium — the sparkline cards have no time range, no scale and no hover
  readout.** The four cards (CPU, Memory, Network, Disk) draw a series with a value label but no
  axis, no window duration and no way to read a past point. `committed capture confirms` the cards are
  value-only. You cannot answer "when did that spike happen" or "what is the scale".
- **`APP-monitor-02` — medium — the summary and the process list contradict each other without
  explanation.** The Memory card reads `8.6 GiB` while the top process row reads `10.4 GiB` for the
  same instant (committed capture `apps/monitor/shots/monitor.png`) — the summary is used memory and
  the row is RSS/virtual, but nothing on screen distinguishes them, so the app appears to claim a
  process uses more than the total in use.
- **`APP-monitor-03` — medium — "Kill" is a one-click destructive primary with no confirmation and no
  alternative signal.** The selected-process card offers a single filled red `Kill`
  (`app.rs` process card; capture confirms) — not Term/Interrupt/Kill, and no confirm. Killing the
  wrong PID is one click. Fix: a small menu (Interrupt, Terminate, Kill) and a confirmation naming the
  process.
- **`APP-monitor-04` — minor — the window bar shows a live clock that duplicates the desktop clock**
  (`19:38:39` plus uptime in the app bar, while the system top bar also shows the time). Uptime is
  content; the clock is duplicated system chrome.
- Good: sample cadence including Paused, persisted per device (`view.rs:89-112`, `prefs.rs:54-60`),
  manual refresh still available; "19 of 38 shown" with a real filter; kernel-threads toggle.

### disks

- **`APP-disks-01` — minor — after mounting a filesystem you cannot go to it.** `DiskEffect`
  (`resource.rs:88-135`) is Rescan, Select, ShowTab, Mount, Unmount, Format, Benchmark, Eject; a
  mounted row only prints where it lives (`view.rs:465 format!("{fstype} at {mount}")`). Plug in a USB
  stick, mount it, then hand-copy `/media/<user>/<LABEL>` into Files. Fix: "Open in Files" on the row
  and the drive page.
- Good: destructive format is gated by a typed phrase naming the partitions and the files lost
  (`resource.rs:203`); a skeleton arrival rather than a spinner (`view.rs:289-311`).

### disk-usage

- **`APP-disk-usage-01` — medium — a scan shows an indeterminate spinner with no progress and no
  visible Cancel.** `app.rs:557-560` renders `EmptyState::new("Calculating folder sizes").busy()`;
  cancellation exists (`:454-458 cancel_scan`) but is reachable only from `go_home`/`go_back`
  (`:459-461`), i.e. via the window bar's Back, which reads as navigation rather than "stop", and
  `scan.rs:145` reports nothing until it returns. Point it at `/` or a network home and you get
  minutes of a bare spinner with no way to tell working from hung.

### activity (History)

- **`APP-activity-01` — notable — History has no text search and publishes no search provider.**
  `history.rs:39` gives the header no commands, `:110-175` narrows only by scope toggle plus three
  dropdowns (period, person, lens), `grep -rn search apps/activity/src` → nothing, and
  `grep -rn "SearchEntry\|SearchProvider" apps/activity` → nothing while 35 other apps publish.
  Finding "the change where the budget note was edited" means guessing period + lens and paging an
  unbounded feed (`limit` grows by `wire::FEED_PAGE`).
- **`APP-activity-02` — notable — Undo and Restore fire on one click with no confirmation.** The
  inspector's button calls `provider.ask(action, &range)` directly (`detail.rs:96-118`); the contract
  is `ActionKind::Undoable` with `ConfirmationPolicy::None`
  (`crates/collab/actions/src/contract.rs:333-336`), and `wire::RESTORE` is documented as restoring
  the room to before a range of changes (`workspace_activity.rs:59`). One click can roll the shared
  room back past everyone's changes with no preview. The app's own rule-deletion path elsewhere in the
  tree does ask.
- **`APP-activity-03` — medium — "compare before and after" never shows a before or after value.**
  `detail.rs:75-88` renders the host's change *labels* as uniform `ListRow`s
  (`activity-change-updated-field = Updated { $field }`, `desktop-surface/src/activity/pane.rs:2875-2900`).
  The Compare affordance tells you a field changed and never what it changed to.
- Good: filters only offer "clear" when something is narrowed (`history.rs:167-175`) with a
  `clear_filters` verb agents can call too; the inspector shows authors, resources, agent instruction,
  decision and affected records (`detail.rs:46-96`, `:120-160`).

### clocks

- **`APP-clocks-01` — notable — a ringing alarm can only be dismissed; there is no snooze.** The due
  card has exactly one control, `Button::new("clocks.notice.dismiss", "Dismiss")` (`app.rs:1027-1052`),
  and `ClocksEffect` (`resource.rs:21-46`) has no snooze verb. Someone in a meeting must dismiss and
  then recreate a timer.
- **`APP-clocks-02` — medium — an alarm cannot be edited.** The verbs are `add_alarm`,
  `set_alarm_enabled`, `remove_alarm` (`model.rs:344-393`). Moving 7:30 to 8:00 means delete + re-add,
  which resets `enabled_at_ms` and loses the label and repeat days rather than adjusting them.
- **`APP-clocks-03` — minor — an alarm row shows no next occurrence.** `app.rs:442-455` renders
  `{time}` / `{repeat}, currently {On|Off}`. With several alarms you cannot tell which fires next, or
  that a repeat mask means "tomorrow". `ArmedAlarm::next_due_ms` already exists.
- Good: rings exactly once per device across restarts (a persisted per-device delivery ledger,
  `schedule.rs:1-12`); timers get +1 min, pause/resume, float and remove (`app.rs:626-692`).

### calculator

- **`APP-calculator-01` — medium — the result cannot be copied.** The key router handles Ctrl+1/2/3
  and Ctrl+V paste (`app.rs:619-632`, `:686-700`); Ctrl+C is unhandled, `clipboard` appears once in the
  file, and no copy control exists on the display or keypad (`keypad.rs:24 MEM_KEYS` = MC/MR/M+/M−).
  The answer you just computed can only be copied from the separate History surface
  (`history_face.rs:64-70`).
- **`APP-calculator-02` — minor — no unit/currency conversion**; `keypad.rs:1-5` documents the app as
  display + memory bar + scientific keypad, and the only "unit" is the angle strip.
- Good: full keyboard entry (Enter/Backspace/Escape, characters, Ctrl+V — `app.rs:657-712`) and a real
  memory bar.

### passwords

- **`APP-passwords-01` — minor — "Edit" is a bare text link while lesser actions get buttons.** In the
  item detail the primary action is plain dim text (see `VIS-05`), which reads as a label rather than a
  control.
- Good: timed clipboard and reveal (copy clears after 30 s, reveal after 10 s —
  `actions.rs:51-62`), secret clipboard path used for copies, and a full export warning modal.

### totp

- **`APP-totp-01` — medium — codes are always on screen; there is no lock or reveal gate.**
  `list.rs:597-601` paints every code on every row. Second factors are readable by anyone walking past
  or watching a screen share, which is why phone authenticators hide or gate them. Fix: an optional
  app lock / privacy veil held in local (never synced) state.
- **`APP-totp-02` — medium — no clock-drift warning.** `totp.rs:55-60 unix_now()` is the only clock and
  `grep -i drift\|skew apps/totp` → nothing outside test staging. A device clock 60 s off silently
  generates codes every server rejects, with no explanation on screen.
- **`APP-totp-03` — minor — the clipboard timer is a fixed 30 s while periods are 5–300 s.**
  `app.rs:26-28 CODE_ON_CLIPBOARD_FOR = 30` with a doc comment claiming "one code period"; for a
  60-second account the code is wiped while still valid. Fix: clear at `account.period`.
- Good: hover turns the countdown ring into a copy icon and the copy goes through
  `try_clipboard_copy_secret` (`list.rs:620-628`, `app.rs:343-357`); the ring warns under 5 s and
  degrades correctly under reduced motion (`:634-644`).

### backups

- **`APP-backups-01` — notable — the live catalog hardcodes every "derived" fact, so schedule, type,
  retention, failure and health surfaces can never reflect reality.** `model.rs:468-475` and
  `:504-506`, `:516-518` set `kind: Manual`, `retention: Daily`, `schedule: OnDemand`, `health: Ok`
  as constants in the only live builder (`Catalog::from_local`), and the UI prints them as facts
  (`view.rs:688`, `:750-751`, `:553`, `:586`, `app.rs:733 publish_health` always "All backups
  healthy"). Every live snapshot is labelled Manual/Daily, a destination can never show "Last run
  failed", and the health toast can only ever say healthy.
- **`APP-backups-02` — notable — the app advertises schedules it does not have.** `schedule` is
  always `OnDemand`; the document holds only the retention policy (`resource.rs:29-31`); and
  `create_snapshot` is called from exactly one place, the manual start (`app.rs:567`). No scheduler
  exists. A "Next run" tile that says "On demand" forever is a silent-data-loss class gap: either wire
  a scheduler and schedule editing, or remove the surfaces.
- **`APP-backups-03` — notable — a running backup, verify or restore cannot be cancelled, and the app
  is fully blocked meanwhile.** `app.rs:325-331` refuses everything with "A backup operation is already
  running; wait for it to finish." (`:370`, `:393`), and the progress section is a label, a line and an
  indeterminate bar with no action. A backup started on the wrong folder runs to completion.
- **`APP-backups-04` — medium — verification reports no progress at all.** `app.rs:611-618` ignores the
  progress handle and `engine::verify_snapshot` (`engine.rs:399`) has no `on_progress`, unlike create
  and restore. On a large snapshot "Checking every file…" sits frozen for minutes.
- **`APP-backups-05` — medium — pruning deletes snapshots without saying what was deleted.**
  `app.rs:591-600` echoes `pruned` only into the effect's JSON; the toast says "Backup complete: N
  files totalling X". History silently shrinks.
- **`APP-backups-06` — medium — the retention page is four bare counts with no consequence preview.**
  `view.rs:704-734` renders "{} snapshots to keep" inputs; nothing lists which snapshots the policy
  would drop, and nothing warns that lowering a count deletes snapshots at the next run (`app.rs:576`).
  Tune 7→3 blind, and find out afterwards (with no message, per `APP-backups-05`).
- **`APP-backups-07` — medium — a failed run carries no reason.** `view.rs:670-676` says "Couldn't
  copy to {dest}" and `Snapshot` has no error field (`model.rs:246-274`) — so nothing can say whether
  it was permission denied, a full drive, or an unplugged one.
- Good: restore is previewed before it happens — what will happen, a read-only file tree with sizes,
  and a staging-dir verify before publishing (`view.rs:813-817`, `engine.rs:433-460`).

### imager

- **`APP-imager-01` — minor — drives that are too small are disabled with no stated reason.**
  `app.rs:1176-1181 .with_enabled(self.required_capacity().is_some_and(|required| drive.fits(required)))`
  — the row says nothing; only the page's top line names the required size.
- **`APP-imager-02` — minor — "You can safely remove the drive" with no eject affordance on Linux.**
  `app.rs:1448-1451` gives the instruction; the Done footer offers only "Start over"/"Review and
  retry" (`:977-990`), and ejection is armed only on macOS (`write.rs:925 macos_should_eject`).
- Good, and the reference for destructive flows: typed device-name confirmation
  (`app.rs:1355-1361`), a louder warning for a system or internal disk (`:1270-1290`), native host
  authentication for the privileged writer (`:27-29`), an honest "Stopping…" cancel that downgrades a
  cancelled-but-succeeded race rather than claiming a usable medium (`:962-975`, `:2406-2413`), and an
  automatic read-back that reports the first mismatching offset.

### containers

- **`APP-containers-01` — notable — the Logs tab is a frozen snapshot.** Logs are fetched once per
  selection (`app.rs:769-780`) and carried over wholesale on every 3-second re-probe
  (`if newc.logs.is_empty() { newc.logs = old.logs; }`), because `podman::probe()` never fetches logs
  (`podman.rs:247 logs: Vec::new()`). A container that logs an error after you select it never shows
  it, the tab reads as live, and there is no refresh control.
- **`APP-containers-02` — medium — lifecycle verbs have no in-flight state and the confirmation names
  the container by raw id with an uninflected verb.** `app.rs:1995-2034` keeps buttons enabled
  (`view.rs:513 .with_enabled(model.controls)`) and toasts `format!("{verb} accepted for {target}")`,
  i.e. "stop accepted for 9f3c1a7be4d2" — although `past_tense(action)` exists (`app.rs:1502`). After
  clicking Stop on a slow container nothing changes for seconds, so people click again.
- **`APP-containers-03` — medium — an image pull cannot be cancelled, only hidden.**
  `app.rs:1168-1188`: "Closing while the pull runs only hides it"; `Intent::ClosePull` clears the dialog
  (`:1381-1384`) and the worker keeps streaming. A mistaken multi-GB pull keeps consuming bandwidth and
  disk.
- **`APP-containers-04` — minor — pull output is capped at the last 8 lines in a non-scrolling well.**
  `view.rs:1010-1044` (`skip = pull.lines.len().saturating_sub(8)`), so the reason a pull stalled (an
  auth prompt, a layer retry) scrolls out of existence, and no byte totals are shown.
- Good: an absent engine is explained with the raw error behind "Details" (`view.rs:363-380`), and
  missing stats/logs read as "Usage is unavailable. Podman did not return a telemetry sample." rather
  than 0 %/empty, with a regression test pinning it (`view.rs:653-676`, `:1320-1340`); the
  fail-closed live audit example.

### firewall

- **`APP-firewall-01` — notable — "Apply the rules to the system" cannot apply anything unless the
  launcher arms it, and the UI does not say so up front.** `backend.rs:77-84`:
  ```rust
  let armed = std::env::var("TILO_FIREWALL_APPLY").as_deref() == Ok("1");
  let mode = if has_cmd && armed { Mode::Live } else { Mode::DryRun };
  ```
  and a repo-wide grep finds `TILO_FIREWALL_APPLY` only in that file, its comments, its own test
  (which explicitly clears it, `:610-613`) and the manifest comment (`tillo-app.toml:4`) — no
  systemd unit, desktop entry or launcher under `os/` sets it, so a packaged app runs `Mode::DryRun`. The toolbar action is a plain
  `ToolbarAction::new(APPLY, …, "Apply the rules to the system")` with no state; the dry-run fact
  appears only *afterwards* as a toast (`resource.rs:230-236`). The same gate silently makes the
  on/off switch and panic mode inert (`backend.rs:341-345`). The most safety-relevant finding in the
  set: the person closes the app believing their rules and panic mode are live.
  Fix: show the backend mode persistently ("Preview only: this build does not apply rules"), relabel or
  disable Apply, and carry the same state on the enable/panic switches.
- **`APP-firewall-02` — notable — the Source field asks for a numeric keyboard, so an IPv4 range cannot
  be typed on a touch device.** `view.rs:668-674 .with_kind(TextInputKind::Number)`, which maps to
  `InputPurpose::Number` (`text_input.rs:254`) and selects the OSK's numeric layout — a digit pad with
  `-` and `.` only (`tillo-keyboard-map/src/touch.rs:332-334`), no `/`. The field's own placeholder
  example is `192.168.1.0/24`, which is untypable without a hardware keyboard. Fix:
  `TextInputKind::Identifier`.
- **`APP-firewall-03` — notable — there is no view of blocked traffic.** `grep log` over
  `model.rs`/`backend.rs`/`view.rs` returns only catalog/login hits; there is no log-level setting, no
  `--get-log-denied` read, and nowhere a drop is reported. The primary diagnostic loop of a firewall UI
  is missing, so the person cannot tell whether their deny rules work or what is hammering the machine.
- **`APP-firewall-04` — medium — a rule can only be added, toggled or deleted; there is no edit.**
  `Intent` (`view.rs:37-49`) has `OpenAddRule`/`AskDelete`/`SetMatchService`/`SetProto`/`SetAction` and
  no `EditRule`; the table's per-row controls are the enable switch and the trash button
  (`view.rs:522-533`). Fixing a typo means deleting the rule and re-entering it.
- **`APP-firewall-05` — medium — turning the firewall off and entering panic mode are single clicks with
  no confirmation.** `app.rs:730-742` toggles both switches directly; `!cfg.enabled` plans `systemctl
  stop firewalld`, and panic mode plans `--panic-on` (all traffic dropped). Neither goes through a
  dialog, unlike the app's own rule deletion (`view.rs:698-722`) — and panic mode can cut the very
  remote session being used to fix the machine.
- **`APP-firewall-06` — minor — the `applying` state never reaches the view.** `app.rs:143`, `:400`,
  `:410` set it; nothing in `ViewKey`/`declare_window_toolbar` reads it and Apply is never disabled.
- Good: persistent banners for panic mode and for a stopped firewall, plus a clear "Firewall service
  unavailable / Start firewalld, then try again. No rules were changed." empty state with one retry
  (`view.rs:346-362`, `:249-263`).

### remote

- **`APP-remote-01` — medium — a connection attempt cannot be cancelled and nothing bounds how long it
  hangs.** `app.rs:1849-1862` offers `action: None` while `Connecting` — and the Tilo case waits
  indefinitely for the owner's consent — with no connect timeout anywhere (the only timeouts are
  `inactivity_timeout` at `ssh.rs:460` and VNC read timeouts at `vnc.rs:202-221`). A mistyped host
  leaves "Connecting to X" with no way out from the page.
- **`APP-remote-02` — minor — disconnecting a live session is immediate and unconfirmed.** `app.rs:973
  Action::Disconnect => self.disconnect_session()`, while every other session-shaped action goes
  through `PendingConfirmation` (`:311-328`). One click ends an SSH session and its scrollback.
- **`APP-remote-03` — medium — there is no file transfer.** `grep sftp|scp|transfer` finds one
  unrelated comment (`ssh.rs:34`). Moving a file to a machine you are already connected to requires
  falling back to a terminal; for a VNC/Tilo desktop there is no path at all.
- Good: `Failed(reason)` carries the transport's real message, `Unavailable` never masquerades as an
  auth failure (`session.rs:132-145`), each status page supplies a targeted action (`app.rs:1840-1912`),
  and SSH host-key decisions are confirmed with the observed key (`:311-328`). Clipboard sync is
  deliberately withheld and documented (`lib.rs:28-30`), which is the right call.

### osk

- **`APP-osk-01` — medium — there is no keyboard size preference.** `widget.rs:130-133
  key_row_height(metrics) = metrics.header_height`, and `OskPreferences` (`resource.rs:26-42`) carries
  layout, layouts, suggestions, personalization and emoji recents only. A 13-inch tablet and a phone get
  the same key height, and enlarging keys means changing the whole desktop's density.
- **`APP-osk-02` — medium — autocorrect cannot be separated from the suggestion bar.**
  `widget.rs:1546-1548` gates `autocorrect_allowed` on `suggestions_enabled`. Someone who wants
  predictions but not word rewriting cannot have it: turning off the thing that mangles their typing
  also removes the candidates they use.
- **`APP-osk-03` — medium — the typing language is the layout, so QWERTY can only be corrected against
  English.** `dict.rs:28-33 enum Language { English, French }` with `Language::for_layout` mapping
  QWERTY→English, AZERTY→French. Anyone typing German, Spanish or Portuguese on QWERTY is *corrected*
  against an English dictionary. (Long-press accents cover characters, not words:
  `touch.rs:192-204`.)
- **`APP-osk-04` — minor — the keyboard cannot be moved or parked.** `toolbar.rs:216-245 default_set()`
  is paste, selection-left, selection-right, dismiss; the action enum (`:20-40`) has no move/drag
  variant, and the surface is compositor-floated (`lib.rs:42-44`) with no window bar.
- Good: shift vs caps are visually distinct with a filled icon and an `Active` tone, tested
  (`presentation.rs:339-343`); long-press accents and their ranking (`:553-573`); a real hide affordance
  (`widget.rs:1162-1164`); and a test pinning that a touch-down previews its character *except* in a
  password field (`widget.rs:5436`).

### themes

- **`APP-themes-01` — medium — themes can be exported but never imported.** The commands are APPLY,
  EXPORT, DUPLICATE, RESET, DELETE (`view/window_bar.rs:65-72`) and `grep -rn import apps/themes` →
  nothing. Two people in a workspace cannot share a theme, and a theme restored from backup cannot come
  back. Fix: "Import theme…" validating through `ThemeDocument::validate`.
- **`APP-themes-02` — minor — the preview shows only a desktop and the presence colours.**
  `preview.rs:18-26 enum Show { Desktop, Presence }` and `view.rs:671-694` draws those two, so density,
  type scale, motion and list-surface roles can be edited without ever being seen in context. Fix: a
  switcher (Desktop / List / Dialog / Form / Presence).
- Good: refuses to ship an illegible theme — Apply and Export disabled unless `legible`, with a
  findings line explaining every missed contrast floor (`view/window_bar.rs:70-71`, `view.rs:688-702`).

### print-centre

- **`APP-print-centre-01` — notable — the print form offers three settings, so multi-page documents can
  only be printed whole.** `surface/views.rs:937-974` offers "Send to", "Copies" (`NumberInput`
  1–99) and "Print on both sides"; the model is `PrintSettings { copies, two_sided }`
  (`os-actions/src/printing.rs:77-80`). No page range, no paper size, no orientation, no colour mode.
  Printing pages 3–5 of a 40-page PDF is impossible.
- **`APP-print-centre-02` — minor — the document card shows one page with no pager.**
  `surface/views.rs:909-935` renders a single `Image`, so a wasteful or wrong-page print cannot be
  caught before it happens, while the scan side already has `PreviousScanPage`/`NextScanPage`.
- Good: cancel is confirmed (`surface.rs:263-264`), printer faults are explained in words
  (`friendly_reason`, `:496-500`), and pause/resume/retry all exist (`resource.rs:170-192`).

### publisher (App Publisher)

- **`APP-publisher-01` — medium — the one warning about the only copy of the signing key is dead copy,
  and it points at a file the app never writes.** `locales/en-US.ftl:25` declares
  `publisher-key-created-detail = Back up publisher-key.json: it is the only copy.` but `lib.rs:217`
  renders `Notice::info("publisher-key-created").detailed(key::public_hex(&key))` instead; grep finds
  `publisher-key.json` in no code at all (the key lives only in the secret store via `key::create` →
  `put_blocking`), and the recovery text at `:33` names the same non-existent file. There is also no
  "export key to a file" verb (`KeyEffect`: CreateKey, CopyProof, CopySecret, ApplyForNamespaces,
  CheckStatus). Fix: render the backup warning, say the key is in this device's keychain, add
  "Export key…" mirroring `tilo-app keys export`.
- **`APP-publisher-02` — medium — the namespace proof flow never says where to publish the proof.**
  `lib.rs:307-361` shows the proof line with the help "Publish this line where only you can write, to
  prove a namespace" — but not the DNS record name or the `.well-known` URL, although
  `registry_client::proof_places` exists (`crates/packaging/registry-client/src/lib.rs:71-80`) and the
  CLI prints them (`tilo-app-cli/src/publishing.rs:131`). GUI users are sent to the CLI docs.

### shortcuts (automation)

- **`APP-shortcuts-01` — notable — the action picker has no search.** `view.rs:1535-1618` renders every
  action of every app in the workspace as `ListRow`s with no query field
  (`grep -rni "search\|Filterable" apps/shortcuts/src` → nothing). With the catalogue loaded, adding
  "Create note" is dozens of screens of scrolling — for the app's core repeated gesture. Fix: a filter
  field (`Filterable`, as Clocks' `CityChoices` does) matching action label, app and resource.
- **`APP-shortcuts-02` — minor — a shortcut cannot be duplicated.** The model publishes add/move/
  remove/rename; `grep -rn duplicate` finds only `CheckErrorKind::DuplicateStep`. Making a variant means
  rebuilding it step by step.
- Good: shows the permission grant and the checker's problems before saving (`view.rs:1926-1970`),
  with least-privilege computation (`grant::least_privilege`) and a blocking banner when steps do not
  check out.
- Note: `apps/shortcuts` is the automation builder, not the key-binding editor; key bindings live in
  Settings.

### snippets

- **`APP-snippets-01` — notable — folder, tags and description can never be set after creation.** The
  only field edits the window submits are favorite (`app.rs:512`), language (`:540`), title (`:550`) and
  body (`:352`); a new snippet inherits its folder from the current selection (`:452`) and tags only ever
  arrive from `seed.rs`. So the library's whole organising model is write-once: you cannot file a
  snippet, tag it, or write the description the list uses as its secondary line
  (`list_pane.rs:30-36`). The verbs `rename_folder`/`rename_tag`/`remove_tag` (`resource.rs:86-125`) are
  only reachable once something is already in that folder.
- **`APP-snippets-02` — medium — the only way out of the library is the clipboard.** `Copy(SnippetId)`
  is the single export; the manifest requests only `optional = ["clipboard"]`; no other app references
  the library. "Insert snippet into other apps" — the advertised text-expander behaviour — does not
  exist: copy, switch app, paste. Fix: a system-level "Insert snippet…" action available in any text
  field, or document copy-only as the contract.
- **`APP-snippets-03` — minor — copying uses the non-expiring, unchecked path.**
  `app.rs:492 ui::clipboard_copy(&body)` discards the outcome and never expires, unlike
  `try_clipboard_copy_secret` used by TOTP and Passwords — so a snippet holding an API key stays on the
  clipboard indefinitely, and the "Copied" toast fires even when the host refused.
- Good: `record_use` plus the fill-in dialog with live preview and per-variable fields
  (`resource.rs:64-69`, `expand.rs:1-35`).

### agent, model-providers and the model-only crates

- **`APP-agent-01` — notable — with no provider configured, a model run silently degrades to keyword
  matching and then blames permissions.** `session.rs:319-325` falls back to `Chooser::new(false, …)`;
  `:503-505` skips the model loop when `loop_model()` is not Ok and `!self.managed`; the rules-only
  planner then fails with "No permitted live action can complete this task" (`:543-547`). The person
  goes looking at capabilities when the real problem is that no model was ever selected, and nothing in
  the run's status says which path planned it.
- **`APP-agent-02` — medium — planning failures are reported in internal shorthand with no route to a
  fix.** `session/agent_loop.rs:900-906` and `model.rs:64-70` render "Managed model planning
  unavailable: no api key" / ": switched off" — lower-case fragments, no provider named, no mention of
  Model Providers. "no api key" is also wrong for sign-in providers and for a profile with no model
  chosen (`providers.rs:23-27` returns `None` for a profile without a default model).
- **`APP-agent-03` — medium — token usage is parsed and then dropped; nothing shows usage or cost.**
  `Usage { input_tokens, output_tokens, … }` is deserialized from every response
  (`provider-config/src/protocol.rs:206-221`) but never read outside tests; `TaskRunUpdate` carries only
  message and presentation (`runtime-collab/src/lib.rs:3020-3043`). A person running agents cannot see
  what a task costs — in the run view or in provider settings.
- **`APP-agent-04` — minor — provider settings talk about "the backend".**
  `provider-settings-ui/locales/en-US.ftl:98-99` ("Provider change saved by the backend." / "Provider
  settings loaded from the backend."), shown as the panel's notice after any change (`lib.rs:634-641`);
  the German catalogue mirrors it. Reads like an error about an internal component.
- **`APP-agent-05` — minor — one unlabelled notice line carries results for every provider.**
  `lib.rs:506-510`, `:634-641` push the notice into a generic "Agent service" section, and "Test
  connection" reports "Connected: 14 models" with no provider named, while a listing failure is also
  echoed inline per profile (`:896-899`).
- **`APP-agent-06` — informational — `moderation-model`, `staging-handoff-model` and `discord-model`
  have no app surface to audit.** Each ships `Cargo.toml` + `src/lib.rs` (+ record/migration/event
  modules) with no `tilo-app.toml` and no `surface.toml`, and `model-providers` is a shim
  (`pub use provider_settings_ui::ProviderSettingsApp`). Anything a person sees about moderation,
  Discord or staging handoff comes from another app's surface — worth stating explicitly so these are
  not mistaken for audited-but-fine.
- Good: model output streams through a coalesced `TaskPresentation` on the run's channel instead of
  blocking (`agent_loop.rs:1014-1050`), cancellation is honoured between turns and between actions
  (`session.rs:540-566`, `:601-606`), provider credentials are handled carefully — secret input over an
  external zeroized buffer (`provider-settings-ui/lib.rs:1138-1160`, `:614-620`), key status as
  stored/needed/none, legacy migration requiring the original endpoint to be confirmed (`:1064-1105`),
  and each `ErrorKind` mapped to a distinct actionable sentence on "Test connection" (`:1414-1423`).

## G5. Developer tools

### git-client

- **`APP-git-client-01` — notable — switching branches with uncommitted work has no confirmation or
  parking.** `app.rs:1080 BranchAction::Checkout(name) => repo::checkout_branch(&path, &name)` runs
  straight through (`repo.rs:813-815 co.safe()`), and `enum Confirm` (`app.rs:117-127`) has only
  DiscardFile/DiscardHunk/DeleteBranch. Non-conflicting edits are silently carried across; conflicting
  ones surface as a raw `checkout tree for X` error afterwards, and there is no "stash and switch".
- **`APP-git-client-02` — notable — commit changed-file lists are never loaded, so History has no diff
  at all.** `repo.rs:334 read_commits` pushes `files: vec![]` for every commit and the detail pane draws
  them only when non-empty (`history.rs:642`); only the bundled sample populates them (`sample.rs:303`).
  For real repositories History shows summary/body/refs and never "Changed files", and there is no
  per-commit diff (`grep commit_diff|load_commit` → nothing).
- **`APP-git-client-03` — notable — the app is pointer-only apart from list arrows.**
  `navigate_list` accepts only ArrowDown/ArrowUp (`app.rs:2803-2806`); the rest of `handle_key` is
  Escape, 1/2/3, `d` and text-field forwarding (`:2708-2795`), and the app hand-hit-tests its own rects
  (`:1110-1117`) instead of using `ui::widget::activation::PaintedTargets` — which is what gives other
  painted surfaces Tab/Enter traversal (`activation.rs:660-665`). No keyboard path to stage/unstage a
  file or hunk, focus the message field, commit, or reach a branch's Check out/Delete.
- **`APP-git-client-04` — notable — conflicts are labelled but not resolvable.** `repo.rs:94` classifies
  `Status::CONFLICTED` and that is the only conflict handling in the crate (`grep conflict` → badge,
  label, classify, i18n); no ours/theirs, no reference to diffmerge. The only way out of a merge conflict
  is a terminal. Fix: Take ours / Take theirs / Open in Diff & Merge on conflicted rows.
- **`APP-git-client-05` — medium — no commit-message history and no amend.** `commit_input: TextInput`
  and `GitClientEffect::Commit { message }` (`actions.rs:120`) are the whole story
  (`grep amend|COMMIT_EDITMSG|reflog` → nothing). Every message is retyped, and a typo can only be fixed
  by another commit.
- **`APP-git-client-06` — medium — push/pull/fetch give no success feedback and no progress.**
  `app.rs:652` stores `last_error` only, so success shows nothing; the banner label is hardcoded English
  (`:616-619`); progress is reported indeterminate at both entry points (`:3333-3334`, `:3300`).
- **`APP-git-client-07` — minor — two of four diff preferences are palette-only.**
  `app/window_bar.rs:61` offers Unified/Split/Syntax, while Show whitespace and Wrap lines exist only as
  effects (`actions.rs:203`, `:209`) — the two controls that fix an unreadable diff are hidden behind
  Ctrl+K.
- **`APP-git-client-08` — minor — one published row label is untranslated**: `actions.rs:409
  "Stashes".to_owned()` where the key `git-section-stashes` exists and `panels.rs:295` uses it.
- Good: hunk-level stage/discard with the staged/worktree direction derived from the selected area
  (`diffview.rs:36-50`, `app.rs:1000-1015`) and discard behind a confirm; a real lane-sweep commit graph
  (`graph.rs:29-33`, `history.rs:191-230`); diff preferences persisted per device (`settings.rs:24-31`);
  remote work off the UI thread with re-entrancy guards (`app.rs:597-637`).

### diffmerge

- **`APP-diffmerge-01` — notable — the merged result cannot be written anywhere.** The manifest states
  the app "Never writes" (`tillo-app.toml:9`) and `DiffmergeEffect` (`actions.rs:26-58`) has no save; the
  composed result lives in the record (`doc.rs:167 merged: String`). The tool walks a person through a
  three-way merge and then cannot save it — every resolution is transcribed by hand.
- **`APP-diffmerge-02` — medium — there is no keyboard path to accept a side.** `handle_keyboard_shortcut`
  (`app.rs:739`) binds `n`/`N`, `p`/`P`, `1`, `2`, `F8`, `F7` only; taking ours/theirs/both requires the
  pointer or the palette (`TAKE_OURS`/`TAKE_THEIRS`/`TAKE_BOTH`, `resource.rs:180-190`), and there is no
  key to open files. A 40-conflict merge is 40 mouse round trips.
- **`APP-diffmerge-03` — medium — no folder comparison.** `read_text` treats every input as one file
  (`app.rs:1136`), the pickers filter to text extensions (`:52`), and `grep read_dir|recursive|directory`
  → nothing. Comparing two trees is impossible.
- Good: the two panes cannot drift — one aligned row model and one `scroll_y`
  (`diff_view.rs:200-212`, `:423`) with change navigation scrolling it into view (`:399-408`);
  accept-both and hand edits (`merge_view.rs:494-496`, `doc.rs:196-200`); content-addressed change
  navigation so palette addresses survive recomputation (`actions.rs:78-165`).

### db-client

- **`APP-db-client-01` — notable — a long query cannot be cancelled from the UI.** Cancellation happens
  only when the *caller* of the effect cancels (`app.rs:1311 if reply.is_cancelled() { … }`), while the
  only query control is disabled during the run — `ToolbarAction::new(RUN_QUERY, …).enabled(can_run)`
  with `can_run = model.connected && !model.busy` (`:967-981`). `cancel_in_flight_query`
  (`session.rs:557`) is called from nowhere else, and engine-level cancel tokens exist but no UI path
  invokes them (`engine.rs:41-74`). `SELECT *` on a big table leaves a disabled Run button and no way
  out but killing the app.
- **`APP-db-client-02` — medium — truncated results are reported by a bare "+".** `model.rs:372
  format!("{} rows{} in {:.1} ms", …, if self.truncated { "+" } …)` with the cap at
  `MAX_QUERY_ROWS = 1_000` (`engine.rs:27`) and a second cap from bytes/cells (`:373-378`). "1000 rows+
  in 12.4 ms" gives no reason, no remedy, and no way to fetch the remainder (only table browsing is
  paged).
- **`APP-db-client-03` — medium — CSV export exists only in the palette.**
  `DbEffect::ExportCsv` is attached to a resource (`search.rs:198`) and handled at `app.rs:1230`, but no
  control in the results pane, query pane, navigator or toolbar mentions it.
- **`APP-db-client-04` — medium — result cells cannot be selected or copied.** The table is
  `DataTable::new(columns).ruled()` with no selection model (`results.rs:203`) and `grep clipboard` in
  the crate → nothing. Copying one id out of a result requires retyping it.
- **`APP-db-client-05` — medium — the whole UI is hardcoded English** while the manifest advertises a
  German name: `locales/en-US.ftl` holds only 23 History strings, and user-facing text is literal
  (`query.rs:188-196 "Ready"/"The query returned no columns."`, `:47`, `:115`,
  `connect_modal.rs:248`, `navigator.rs:105`).
- Good: `NULL` rendered distinctly from empty text (`results.rs:190-200`, `model.rs:274-323`), and the
  schema tree is the shared keyboard-capable `NavList` (`navigator.rs:6-10`).

### api-tester

- **`APP-api-tester-01` — notable — a hung request cannot be cancelled and has no timeout.**
  `actions.rs:48 #[effect(job, returns, untrusted)] Send` is not `cancellable`, `grep timeout http.rs` →
  nothing, and the in-flight pane is actionless (`response.rs:35-42`). One unreachable host blocks the
  app's only primary action indefinitely.
- **`APP-api-tester-02` — notable — history replay discards the request.**
  `app.rs:876 replay_history` builds a blank request and copies only method and url — the history record
  keeps only method/url/status/duration (`resource.rs:268-272`). Re-opening "the request that failed at
  14:02" loses its headers, body, params and auth.
- **`APP-api-tester-03` — medium — the response body and headers cannot be copied.**
  `grep clipboard|Copy` in the crate → only `#[derive(Clone, Copy)]`. Pasting a response into a bug
  report, or grabbing a token out of a header, is impossible. Fix: Copy body / Copy as cURL / copy a
  header row.
- **`APP-api-tester-04` — minor — history rows offer no actions** (`sidebar.rs:137 .without_actions()`)
  while saved requests get rename/delete (`:190-200`); history is capped at 50 (`record.rs:347`), so the
  only cleanup is implicit eviction.
- Good: bounded large responses are explained rather than silently cut ("More than 2 MB", "First 64 KiB
  shown as hex" — `response.rs:94-118`).

### chip-design

- **`APP-chip-design-01` — medium — fixed-height canvas with no zoom and no fit.**
  `operator_graph_view.rs:163` pins a 540 px `Container` around a both-axis `Scrollable` with nodes at
  authored coordinates (`:285`); `grep zoom|fit|scale` in the crate finds only a fixed 900 px scaling and
  a text-scale test. A graph wider than the window must be scrolled in two axes inside a 540 px band
  regardless of window size.
- **`APP-chip-design-02` — medium — there is no operator list, so a node is only reachable by clicking
  it.** Publishes the graph, reports and receipts (`resource.rs:311`) but not nodes; later operators land
  off-screen (`new_node` places at `y: (n / 4) * 320`, `:82-92`), so inspecting or removing one means
  hunting the canvas. `ProjectState::of` already has `state.nodes`.
- Good: undo works through ordinary attributed history with real `app.undo()` tests
  (`surface.toml` kind = undoable; `matrix_graph_tests.rs:78-114`).

### poker

- **`APP-poker-01` — medium — the "fold for the slow player" button is always enabled but refuses for 30
  seconds, with no countdown.** `view.rs:465` pushes `cx.button("poker-action-skip", Table::SKIP.invoke())`
  unconditionally; `game.rs:825-827` returns `Err("they still have time")` while
  `now - hand.since_ms < TURN_MS` (`= 30_000`, `:38`). Nothing renders the remaining time
  (`grep TURN_MS` → `game.rs` only). The one control for "someone walked away" looks available and does
  nothing.

### windows (Windows appliance)

- **`APP-windows-01` — minor — user-facing sentences bypass the app's own Fluent catalogue.**
  `appliance_owner.rs:66 "Expired; refreshing status…"`, `:69 format!("Expires in {}m {}s", …)`,
  `:467`, `:639`, `:25`, `:585` are literal English while `strings.rs` provides `t()`/`format()` and
  even asserts key coverage (`:18-36`). The app shows translated controls with English status and error
  sentences, including during a restart.
- Good: every power action maps to a starting notice, actions are gated on
  `status.power_state.allows(*action)` and `!busy` (`app.rs:877-889`, `:1825-1848`), and status
  auto-refreshes (`appliance_owner.rs:431-448`).

### speed-test

- Good: a run can be stopped and results are kept — `run_binding` swaps to `SpeedEffect::Stop`
  (`app.rs:155-162`), `stop()` sets the worker flag and drops the result (`:203-213`), measurements
  persist with a bounded 20-entry history (`resource.rs:17-21`).

### scratchpad

- Good: save state is owned by the framework, and the view adds only the one fact the framework cannot
  know (`RecordSaveStatus::MemoryOnly => "Not saved on this device"`, `lib.rs:96-100`), with failures
  reported by the shared save indicator (`tilo-app/src/framework/saves.rs:342`).

---

# Part H — Prioritised backlog

Effort: **S** < half a day · **M** 1–2 days · **L** multi-day (usually because a shared widget must
grow first). "Lands in" names the number of surfaces a fix reaches, because that is what decides
order: a toolkit fix that touches forty apps outranks an app fix of the same size.

## Tier 1 — correctness, safety, or a dead end

| Rank | Finding | Effort | Lands in | Why it is tier 1 |
|---|---|---|---|---|
| 1 | `SHELL-01` lock screen credential field | M | 1 surface, every session | A passphrase typed through an IME cannot be entered. Hard lock-out. Needs the clipboard-off edit policy in `TextInput` first. |
| 2 | `APP-firewall-01` dry-run is invisible | S | 1 app | The person closes the app believing rules and panic mode are live. Either show the mode or remove the claim. |
| 3 | `APP-backups-01` + `APP-backups-02` fabricated facts, phantom schedule | M | 1 app | The UI prints constants as measurements and implies automation that does not exist. Silent-data-loss class. |
| 4 | `APP-db-client-01` no query cancel | S | 1 app | The app is fully blocked with no way out but killing the window; the cancel machinery already exists. |
| 5 | `APP-api-tester-01` no timeout, no cancel | S | 1 app | One unreachable host permanently blocks the app's only primary action. |
| 6 | `APP-diffmerge-01` merged result cannot be saved | M | 1 app | The tool completes a merge and cannot write it; every resolution is transcribed by hand. |
| 7 | `APP-activity-02` undo/restore with no confirmation | S | 1 app | One click rolls a shared room back past everyone's changes. `ConfirmationPolicy` is one field. |
| 8 | `APP-monitor-03` one-click Kill | S | 1 app | Destructive, unconfirmed, no Term/Interrupt alternatives. |
| 9 | `APP-mail-02` refused sends vanish | M | 1 app | The message is unrecoverable after a restart except through an agent-level action. |
| 10 | `APP-firewall-02` numeric keyboard for CIDR | S | 1 app | The field's own example (`192.168.1.0/24`) is untypable on the OSK. One enum change. |
| 11 | `APP-firewall-05` panic mode and "turn off" are one click | S | 1 app | Can cut the remote session being used to fix the machine. |
| 12 | `APP-remote-01` connection cannot be cancelled or timed out | S | 1 app | "Connecting…" forever, with the only escape in another part of the window bar. |

## Tier 2 — adopt what already exists (highest leverage per hour)

| Rank | Finding | Effort | Lands in | Note |
|---|---|---|---|---|
| 13 | `DUP-04` turn on `ToolbarLocation::edited` | S | 7+ document apps | One boolean per app; the marker is already painted and announced (`window_toolbar.rs:308`). |
| 14 | `DUP-04` implement `WindowCloseAdmission` (framework shows Save/Discard/Keep editing, with Retry) | M | 8 apps | Collapses three divergent prompts into one, and is the only path that can retry a failed save. |
| 15 | `DUP-02` one `ZoomCluster` + `ContentZoom` | M | 9 clusters | Six meanings for Ctrl+0 become one; doc-specific bounds become constructor args. |
| 16 | `DUP-03` one `ShortcutSheet` from `ActionBinding`s | M | 6 sheets | Fixes four sheets that currently print chords that are wrong on the person's keyboard. |
| 17 | `DUP-08` one `ArrivingPane` | S | 8 apps | Kills four presentations of "reading now"; the policy already exists in `arriving.rs`. |
| 18 | `DUP-05` delete the eight private notice types | S–M | 15+ apps | Fixes severities that cannot express Warning and TTLs of 3/4/4.2/8 s. |
| 19 | `DUP-01` one `FindMatcher` | M | 5 surfaces | Whole-word and case-fold rules currently disagree between panes on the same gesture. |
| 20 | `DUP-06` `standard_edit_items()` | M | 4 copies | One ordering, one shortcut table, one spelling submenu. |
| 21 | `DUP-07` one `QueryView` painter | M | 2×1 400 lines | Byte-identical surfaces that already differ in whether retry is drawn. |
| 22 | `CORE-01`/`CORE-02`/`CORE-03` keyboard holes | S each | every app with a picker / date / autocomplete | Home/End/PageUp/Ctrl+A; PageUp/Down that move the selection; Escape that falls through. |

## Tier 3 — high-frequency friction a person feels weekly

| Rank | Finding | Effort | Lands in |
|---|---|---|---|
| 23 | `APP-file-explorer-01` search the current folder | M | Files |
| 24 | `MOTION-07` logs Follow actually follows | S | Logs |
| 25 | `APP-tube-01` resume playback | S | Tube |
| 26 | `APP-store-01` install progress + cancel | L | Store |
| 27 | `APP-chat-02` open/save attachments | M | Chat |
| 28 | `APP-git-client-04` conflict resolution entry | M | Git |
| 29 | `APP-git-client-02` per-commit file list and diff | M | Git |
| 30 | `APP-git-client-01` dirty branch switch | S | Git |
| 31 | `APP-git-client-03` keyboard reach for painted targets | M | Git |
| 32 | `APP-terminal-01` persist copy-on-select / OSC 52 | S | Terminal |
| 33 | `APP-terminal-02` bell as a visual effect | S | Terminal |
| 34 | `APP-clocks-01` snooze | S | Clocks |
| 35 | `APP-print-centre-01` page range / paper / orientation | M | Print |
| 36 | `APP-activity-01` History text search + provider | M | Activity |
| 37 | `VIS-01` calculator scientific keys at narrow width | S–M | Calculator |
| 38 | `APP-tasks-01` + `APP-tasks-02` filters, due date | M | Tasks |
| 39 | `APP-notes-01` heading outline | M | Notes |
| 40 | `APP-snippets-01` edit folder/tags/description | S | Snippets |
| 41 | `APP-contacts-01` email/message/call from a contact | S | Contacts |
| 42 | `APP-sheets-01` + `APP-sheets-02` cell context menu, find | M | Sheets |
| 43 | `APP-slides-01` + `APP-slides-03` zoom, clipboard | M | Slides |
| 44 | `APP-store-02` installed size per row and total | S | Store |
| 45 | `APP-backups-03` cancel a running backup | M | Backups |
| 46 | `APP-containers-01` live logs | S | Containers |
| 47 | `APP-remote-03` file transfer | L | Remote |
| 48 | `APP-osk-02` split autocorrect from suggestions | S | OSK |
| 49 | `APP-totp-01` privacy veil | S | TOTP |
| 50 | `APP-shortcuts-01` filter the action picker | S | Shortcuts |

## Tier 4 — motion: the named motions that were never wired

| Rank | Finding | Effort | Lands in |
|---|---|---|---|
| 51 | `MOTION-02` autocomplete arrives with `Appearing` | S | 7 call sites |
| 52 | `MOTION-03` display-confirm card arrives with `Presence` | S | shell |
| 53 | `MOTION-01` modal confirmations leave with motion | M | every app's palette confirm |
| 54 | `MOTION-08` wire `motion::Feedback` for hover release | M | all chrome, all apps |
| 55 | `MOTION-09` chevron rotation in the three that snap | S | Settings sidebar, trees, tables |
| 56 | `MOTION-04` notice strip height follows its fade | S | every compact window |
| 57 | `MOTION-05` + `MOTION-06` calendar hover and period transition | M | Calendar |
| 58 | `MOTION-10` press scale as a motion | S | tabs, selectors, toggle groups |
| 59 | `MOTION-13` remove the reduced-motion bypass | S | Capture Studio |
| 60 | `MOTION-12` decide `motion.character`: wire it or delete it | S | every theme |
| 61 | `MOTION-15` idle fade as a spring + one hold token | S | scrollbars, editor handles |
| 62 | `MOTION-11`, `MOTION-14`, `MOTION-16`, `MOTION-17` | S each | small |

## Tier 5 — chrome and shell structure

| Rank | Finding | Effort | Lands in |
|---|---|---|---|
| 63 | `SHELL-05` undo toast on closing a window | S | every window |
| 64 | `SHELL-15`/`SHELL-16` notification identity | M | every third-party notification |
| 65 | `SHELL-02` attention marks for refused focus | M | shell |
| 66 | `SHELL-09` pointer in the six chrome fields | M | shell |
| 67 | `SHELL-03`/`SHELL-04` tray and desk a11y + keyboard | M | top bar, desktop |
| 68 | `SHELL-06` width presets and equalise | S | window menu |
| 69 | `SHELL-17` per-app badge counts rendered | S | top bar |
| 70 | `CHROME-01` Books declares slots | M | Books |
| 71 | `CHROME-02` Typst declares slots | M | Typst |
| 72 | `CHROME-03` Canvas drops `content_header_leading` | S | Canvas |
| 73 | `CHROME-04` five apps declare nothing | S each | Discord, Maps, Publisher, Scratchpad, Speed test |
| 74 | `SHELL-12`/`SHELL-13` hover on the projection sheet, directory, guide, display card | S | shell |
| 75 | `SHELL-10`/`SHELL-11` a11y for the guide rows and lock transport | S | shell |

## Tier 6 — polish

`CORE-04`…`CORE-15`, `SHELL-07`, `SHELL-08`, `SHELL-14`, `CHROME-05`…`CHROME-07`, `DUP-09`,
`VIS-02`, `VIS-03`, and the per-app `minor` items in Part G. Individually small; collectively they
are the difference between "feels finished" and "feels like a beta".

## Sequencing note

Do Tier 2 before Tier 3. Every Tier 2 item is a shared implementation that already exists, is
tested, and is currently adopted by one or two apps — so the work is deletion plus adoption, and it
removes whole classes of Tier 3–6 findings at once. `DUP-04` in particular is roughly one boolean
per app plus one dialog swap, and it is the finding a person is most likely to be harmed by.


---

# Appendix A — Coverage

**Chrome class** is from Part D1: `A` uses the shared window bar/row correctly, `B` draws its own
header, `—` is not an app window, `T` declares nothing so the bar carries only a title.
**Depth**: `deep` = root widget + resources/effects + the feature code the finding names;
`medium` = targeted greps plus the key files; `—` = no user-facing surface to audit.

| App | Chrome | Depth | Findings |
|---|---|---|---|
| activity | A | deep | `APP-activity-01`, `-02`, `-03` |
| agent | — | deep | `APP-agent-01`…`-05` |
| api-tester | A | deep | `APP-api-tester-01`…`-04` |
| backups | A | deep | `APP-backups-01`…`-07` |
| browser | A | deep | `APP-browser-01`…`-05` |
| browser-parts | — | deep | `APP-browser-04`, `-05` |
| calculator | A | deep + **rendered** | `APP-calculator-01`, `-02`, `VIS-01`…`VIS-04` |
| calendar | A | deep | `APP-calendar-01`, `-02`, `MOTION-05`, `MOTION-06` |
| camera | A | medium | `APP-camera-01`, `-02` |
| canvas | **B** | deep | `CHROME-03`, `APP-canvas-01` |
| chat | A | deep | `APP-chat-01`…`-06`, `MOTION-02` |
| chip-design | A | medium | `APP-chip-design-01`, `-02` |
| clocks | A | deep | `APP-clocks-01`, `-02`, `-03` |
| code-editor | A | deep | `APP-code-editor-01`, `DUP-04` |
| contacts | A | medium | `APP-contacts-01`, `-02` |
| containers | A | deep | `APP-containers-01`…`-04` |
| db-client | A | deep | `APP-db-client-01`…`-05` |
| diffmerge | A | deep | `APP-diffmerge-01`…`-03` |
| discord | T | medium | `APP-discord-01` |
| discord-model | — | — | `APP-agent-06` (no surface) |
| disks | A | medium | `APP-disks-01` |
| disk-usage | A | medium | `APP-disk-usage-01` |
| ebook | **B** | deep | `CHROME-01`, `DUP-07` |
| feeds | A | deep | `APP-feeds-01` |
| file-explorer | A | deep | `APP-file-explorer-01`…`-03`, `DUP-03` |
| firewall | A | deep | `APP-firewall-01`…`-06` |
| git-client | A | deep | `APP-git-client-01`…`-08` |
| hex-editor | A | medium | `APP-hex-editor-01` |
| imager | A | deep | `APP-imager-01`, `-02` |
| logs | A | deep | `APP-logs-01`, `MOTION-07`, `DUP-08` |
| mail | A | deep | `APP-mail-01`…`-04`, `CHROME-07` |
| maps | T | deep | `APP-maps-01`…`-04` |
| messages | A | deep | `APP-messages-01`…`-04` |
| model-providers | — | medium | `APP-agent-06` (shim) |
| moderation-model | — | — | `APP-agent-06` |
| monitor | A | medium | `APP-monitor-01`…`-04` |
| music | A | medium | `DUP-07` |
| notes | A | deep | `APP-notes-01`…`-03` |
| osk | A | deep | `APP-osk-01`…`-04`, `CHROME-06` |
| passwords | A | medium | `APP-passwords-01` |
| pdf-viewer | A | deep | `APP-pdf-viewer-01` |
| photos | A | medium | `APP-photos-01`, `-02` |
| poker | A | medium | `APP-poker-01` |
| print-centre | A | medium | `APP-print-centre-01`, `-02` |
| publisher | T | medium | `APP-publisher-01`, `-02` |
| remote | A | deep | `APP-remote-01`…`-03` |
| scratchpad | T | medium | — (verified good) |
| settings | A | deep | `APP-settings-01`, `-02`, `VIS-05`, `VIS-06` |
| settings-core | A | medium | `APP-settings-01`, `-02` |
| settings-rare | — | — | pane library |
| sheets | A | deep | `APP-sheets-01`…`-03`, `C7` |
| shortcuts | A | medium | `APP-shortcuts-01`, `-02` |
| slides | A | deep | `APP-slides-01`…`-03` |
| snippets | A | deep | `APP-snippets-01`…`-03` |
| speed-test | T | medium | — (verified good) |
| staging-handoff-model | — | — | `APP-agent-06` |
| store | A | deep | `APP-store-01`…`-03` |
| tasks | A | medium | `APP-tasks-01`, `-02`, `CHROME-05` |
| terminal | A | deep | `APP-terminal-01`, `-02` |
| themes | A | medium | `APP-themes-01`, `-02` |
| thumbnails | — | medium | `APP-photos-02` |
| totp | A | medium | `APP-totp-01`…`-03` |
| tube | A | deep | `APP-tube-01`…`-04` |
| typst-editor | **B** | deep | `CHROME-02`, `APP-typst-01`, `-02` |
| video-editor | A | medium | `APP-video-editor-01`, `DUP-03` |
| video-player | A | medium | `APP-video-player-01` |
| weather | A | deep | `APP-weather-01`…`-03` |
| web-search | — | medium | `APP-web-search-01`, `-02` |
| windows | A | medium | `APP-windows-01` |
| word-processor | A | deep | `APP-word-processor-01`…`-04` |

Plus: `crates/sdk/ui-core` (Part B, 15 findings), `crates/session/shell` + `desktop` +
`desktop-surface` (Part A, 17 findings), motion across all of the above (Part C, 17 findings),
cross-app duplication (Part E, 9 findings), chrome structure (Part D, 7 findings), and one app
re-rendered and verified frame-by-frame (Part F).

# Appendix B — What is genuinely well handled

Recorded because a list of 120 problems misrepresents a codebase that gets a lot right. These were
checked specifically and need no work.

**Toolkit**
- Focus-visible vs mouse focus is tracked per widget and cleared on blur (`button.rs:154`, `:1975-1980`).
- Pointer-target minimums for the main controls, including a taller face on a coarse pointer
  (`button.rs:1499-1540`, `interaction.rs:216-221`).
- Slider keyboard model: arrows, PageUp/Down, Home/End, fine-drag (`slider.rs:699-721`).
- Text editing: double/triple click, word-granularity drag, Ctrl+A/C/X/V, Ctrl+Backspace, and
  primary selection deliberately suppressed for secrets (`text_input.rs:6740-6802`, `:6855-6872`).
- `DataTable`: arrows, Page/Home/End, Enter, Space on check cells, Ctrl+A, double-click with drag
  slop, header-click sorting (`data_table.rs:3394-3416`, `:3540-3611`, `:3279-3294`).
- Context menus: disabled rows visible but uninvokable, keyboard hover walks past them, submenus and
  ArrowLeft/Escape unwinding (`context_menu.rs:1200-1222`, `:1744-1757`).
- `Dropdown` opens highlighting and scrolling to the current value; long lists get a real filter
  (`dropdown.rs:1040-1043`, `:1272-1295`).
- Tooltips reach disabled controls (`widget/mod.rs:1796-1808`).
- Tab chips: a compile-time assert keeps the destructive close target off the maximise glyph, and
  chips drop out in a fixed order as a pill narrows (`tabs.rs:3544`, `:3527-3536`).
- Checkbox mixed state with correct `Toggled::Mixed` and proper narration (`checkbox.rs:817-825`).
- Empty states: one widget, 168 call sites, 47 apps.

**Motion**
- Nineteen named motions, every pace from tokens, every reduced variant declared, gate-enforced.
- Interruptible closed-form springs; a settled motion costs no frames.
- Dialog scrim fades with the sheet and is cut out under it — no double dim (`modal_dialog.rs:1015-1025`).
- Window close: width spring plus tile fade, reduced-motion guarded (`window_group.rs:859-866`, `:883-890`).
- Sidebar/drawer/inspector on `PanelSlide` with content reflowing once at the wide end (`sidebar.rs:785-799`).
- `DisclosureRow` height follows `Disclose::reveal` with the chevron rotating with it (`primitives/disclosure_row.rs:520-528`).
- Tab drag on `Snap` with velocity hand-off, lift and disk detents (`bottom_sheet.rs:239-241`).
- Loading: grace + minimum-shown + source naming, every step a deadline (`arriving.rs:1-46`).
- `Attention` (shake/flash) and `Speaking` have correct reduced variants (`named.rs:1120-1230`).
- Nothing moves under a pointer aiming at it (`held_by_aim`, `motion/mod.rs:66-68`).
- Contrast-safe hover/selected fills via `lift_on`'s step flip (`colors.rs:1005-1018`).

**Shell**
- A client's refusal to close is honoured: the surface, tab and focus stay live until the real
  destroy (`window_actions.rs:418-421`, `overlay_host/tree_harvest.rs:658-661`).
- Reopen-closed-tab covers every close door, per workspace, 25 deep, including whole-group closes
  (`closed_tabs.rs:1-21`, `chrome_state.rs:3011-3018`).
- Pointer resize stops at the client's declared width limits (`foreign.rs:3813-3820`, `:5230-5237`).
- Tab-drag cancellation with Escape and a published `tab.drag.cancel` (`chrome_state.rs:1683-1690`,
  `overlay_host/keyboard.rs:610-617`).
- Unresponsive apps dim and say "Not responding", with Wait and Close (`app_liveness.rs:7-17`,
  `state.rs:7657-7710`).
- Notifications are fully keyboard-operable, with per-row/group dismiss, clear-all and undo
  (`overlay_host/keyboard.rs:769-802`, `shell/notification.rs:600-625`).
- Every tray item and menu entry is published as a palette/agent/screen-reader action, with toggles
  rendered as "… (on)" (`shell/search/providers/tray_items.rs:120-160`).
- Multi-monitor is designed, not accidental: pointer crossing focuses the target display's last
  window, Super+arrows cross the seam, groups reflow on unplug (`displays.rs:531-660`,
  `hotplug.rs:12-17`).
- An empty workspace shows a launchpad instead of a void (`shell/mod.rs:1088-1090`), and a window
  launched off-screen is panned into view.
- The window menu is one model for every door, with shortcuts beside rows and semantic keys for
  automation (`window_menu.rs:256-360`, `tab_menu_host.rs:300-345`).

**Apps (reference implementations worth copying)**
- Imager's destructive flow: typed device name, louder system-disk warning, native host auth, honest
  cancel-vs-success race, read-back verify.
- Code-editor's unsaved-work handling and disk-conflict question.
- Word-processor's find/replace as one undoable action, and read-only replace refusal.
- Hex-editor's find/replace across both modes with one undo step per batch.
- Notes' save-failure attention (sticky, explains why, retries from a notification).
- Music's transport, and volume/resume persistence.
- Files' conflict resolution, transfer cancel, trash-first delete, sort-preserving selection.
- Passwords' timed clipboard/reveal, and TOTP's secret-copy path.
- Themes' refusal to apply or export an illegible theme, with the contrast findings explained.
- Store's permission disclosure including the unknown case.
- Containers' refusal to fake telemetry, with a test pinning it.
- Speed-test's stop-and-keep-results.
- Chart/Plot, Ebook and Store for `Arriving` adoption.

# Appendix C — What this audit did not check

Stated plainly so no one reads a pass here as a pass there:

1. **Live backends.** Whether a passed effect reaches a real server is
   `docs/reviews/live-path-audit.md`'s subject, not this one. I read effect and UI plumbing; I did
   not run a live mail, git, Podman, Discord or registry backend.
2. **Motion as motion.** Part C is a source audit. No frame can show a transition
   (`motion::testing::settled` renders the resting pose by design), and I did not capture video or
   instrument frame times. "This hard-cuts" means "no motion is applied here in source", which is
   what decides it, but the *perceived* result on slow hardware is unmeasured.
3. **Hover, focus, press and latency**, for the same reason: a capture cannot show them.
4. **Real assistive technology.** Accessibility findings are from the a11y-tree code
   (`AccessNode`/`Role`/`AccessAction`), not from Orca/NVDA/TalkBack sessions.
5. **Light, high-contrast and forced-colour themes.** Calculator renders were dark only. The
   contrast claims in `F2` leads are from a dark capture and are explicitly unverified.
6. **Locales other than en-US**, RTL, and pseudo-localisation. The i18n findings are "this string is
   not in the catalogue" (checkable, done) rather than "this layout breaks in German" (not done).
7. **Touch and pen**, other than the code-level pointer-precision findings (`CORE-08`, `CORE-09`,
   `APP-firewall-02`).
8. **The six apps whose frames were not re-rendered at this commit** — `F2` lists them as leads.
9. **Two apps I could only audit at the contract level** because they ship no window:
   `web-search` and `thumbnails`; and three model crates with no surface at all
   (`APP-agent-06`).
10. **Performance under load.** `docs/measurements/` covers the frame budget; nothing here does.
