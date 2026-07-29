# Hand-rolled UI primitives audit

**What this is.** An evidence-based sweep of every qOS app that hand-rolls an
interactive UI primitive instead of using the shared `ui` toolkit widget. These
are the things that MUST be unified: a painted rect with a manual hit-test, or a
`String` field you `push`/`pop` on raw key events, silently drops text selection,
clipboard (Ctrl+C/V/X), keyboard activation and caret nav, the focus ring, the
a11y tree, and `Theme`-driven colors. The rule is codified in `ui/HIG.md` §8.

**Scope.** All 45 UI apps under `apps/` (excludes `collab-broker`, which has no
UI). Read-only audit — no refactors here. Method: for each category, the
canonical widget lives in `ui/src/widget/`; app code was graded as hand-rolled
only when it reimplements that widget rather than legitimately composing it.

**Risk key.** `HIGH` = breaks clipboard / selection / IME / a11y (every text
input), `MED` = misses hover/press/focus/keyboard/a11y-role/theme (buttons,
controls, modals), `LOW` = pure color / cosmetic. **Effort.** `S` < ½ day,
`M` ~1–2 days, `L` multi-day (usually because a `ui` widget must be extended
first — see [ui gaps](#ui-gaps)).

---

## Executive summary

| Category | Canonical widget | Apps affected | Worst offenders |
|---|---|---|---|
| 1. Text input (HIGH) | `TextInput` / `SearchField` / `NumberInput` / `EditableLabel` | **23** | db_client, imager, pdf_viewer, chat, maps, mail |
| 2. Buttons (MED) | `Button` / `Toolbar` | **~28** (painted-rect + hit-test) | ebook, software_store, slides, camera, monitor |
| 3. Modals (MED) | `ModalDialog` | **14** | containers (×3), vpn (×2), db_client, monitor |
| 4. Other controls (MED) | `Dropdown`/`Toggle`/`Slider`/`TabBar`/`ContextMenu`/`Popover`/`Chart` | **~22** | tube, music, monitor, logs, camera |
| 5. Hard-coded colors (LOW) | `Theme` tokens | **35** (≥1 literal) | ebook 62, word-processor 44, tube 32 |

**Structural finding.** Only three apps import `ModalDialog` (db_client — and it
*declines* to use it — mail, notes) and only nine import `Button`. `Slider` is
used by two apps, `SearchField` by one. Toggle adoption is good. So the debt is
overwhelmingly concentrated in **text input, buttons, and modals**, and a large
share of the buttons/modals is the *same* anti-pattern copied across apps: a
private `Vec<(Action, Rect)>` hit-test table painted by hand.

**Model apps (adopt these as references; no material hand-rolling):**
`settings` (Button/Dropdown/Toggle/Slider/TextInput/TreeView), `store`
(TextInput/Button/ToggleGroup), `themes` (ColorPicker/Slider/Dropdown),
`file-explorer` (TextInput/Button/Toggle/TabBar/ContextMenu), `notes`
(BlockEditor/TextInput/ContextMenu), `calendar` and `contacts`
(TextInput/BlockEditor/Popover), `totp`, `terminal` (grid legitimately app-owned;
reuses ContextMenu), `video-player` (thin `VideoPlayer` shell).

> **CLAUDE.md correction:** `browser` no longer uses `iced` — it was migrated to
> the `ui` toolkit (`ui::webview::WebView` for content; `TextInput`/`Button`/
> `Toggle` for chrome). Only `address_overlay.rs` remains custom. Update the note.

---

## Category 1 — Text input (HIGH risk)

The worst class: a per-app `String` (or `chars`/`caret`) that receives characters
directly from `Key::Character`/`Backspace`. No real selection, no clipboard, no
IME, no a11y, no caret-by-click. Several offenders also rewrite the whole CRDT
field every keystroke (O(n²) tombstone churn — see `ui/HIG.md` §6). The
calibration exemplar is feeds' `Modal::AddSubscription`.

### Pure non-adoption — a fitting widget already exists (do these first)

| file:symbol | app | evidence | ui alt | effort | risk |
|---|---|---|---|---|---|
| `apps/feeds/src/app.rs:3040-3048` `Modal::AddSubscription/RenameFeed{input}` | feeds | `input.push_str(s)`/`.pop()`/`.push(' ')` on raw keys; fake caret `vline` at fixed end-x (`:2726`) — **the exemplar** | `TextInput` | M | HIGH |
| `apps/maps/src/app.rs:3491` search + `:3461` route endpoints | maps | `query.push_str(s)`, `endpoint_query_mut(o).push_str(s)`; `draw_search` fake blinking caret (`:1918`); zero `TextInput` | `SearchField`/`TextInput` | M | HIGH |
| `apps/weather/src/core.rs:1146` `search_query` | weather | `push_str`/`pop`/`clear`, append-only, no caret; zero `TextInput` | `SearchField` | S | HIGH |
| `apps/logs/src/app.rs:1414` search box | logs | `LogsDoc::search_push`/`search_backspace` on raw keys | `SearchField` | M | HIGH |
| `apps/monitor/src/app.rs:257` `filter` + `:2582` | monitor | `filter.push(ch)`/`.pop()`, own `search_focused` | `SearchField` | M | HIGH |
| `apps/snippets/src/app.rs:990` search + `expand.rs:337` var fields | snippets | `search.push_str`/`pop`; `f.value.pop()/push_str` per variable | `SearchField`/`TextInput` | M | HIGH |
| `apps/hex_editor/src/view.rs:1188` search + `:1226` goto | hex_editor | `search_query.pop()/push_str`, `goto_query…` | `SearchField`/`NumberInput` | S | HIGH |
| `apps/word-processor/src/widget.rs:2533` `find_query`/`replace_query` | word-processor | `buf.pop()`/`push_str`, drawn via `draw_field_box` | `SearchField` | M | HIGH |
| `apps/sheets/src/app.rs:136` `NameBoxEdit`, `:125` `TabRename` | sheets | `nb.chars.insert/remove`, `r.chars.insert/remove` (go-to + tab rename) | `TextInput`/`EditableLabel` | M | HIGH |
| `apps/music/src/app.rs:384` `name_input` | music | new-playlist/rename prompt; `render.rs:654` `.pop()`, `:670` `push_str` | `TextInput` | M | HIGH |
| `apps/ebook/src/app.rs:541` `num_edit`, `:2344` note composer | ebook | digit-filtered click-to-type; `a.note.push_str`/`.pop()` | `NumberInput`/`TextInput` | S/M | HIGH |
| `apps/pdf_viewer/src/app.rs:349` `editing`, `:364` `query` | pdf_viewer | inline comment/reply/signer editor **self-admits "no selection"** + hand-rolled clipboard (`app_events.rs:789`); find query | `TextInput`/`SearchField` | L | HIGH |
| `apps/disks/src/app.rs:2099` `confirm_input` | disks | `push(ch)`/`.pop()` on the format-confirm field | `TextInput` | S | HIGH |
| `apps/vpn/src/app.rs:1469` `Modal::Add` | vpn | `add_name`/`add_endpoint` Strings, Tab-cycled focus, `f.push_str(s)`/`.pop()` | `TextInput` | M | HIGH |
| `apps/remote/src/app.rs:649` `type_char`/`:664` backspace | remote | splices into `draft.{name,host,port,user,group}` + `search`; port hand-filters `is_ascii_digit` | `TextInput`/`NumberInput`/`SearchField` | L | HIGH |
| `apps/software_store/src/app.rs:2965` `search_query` | software_store | `.push(ch)`/`.pop()` on Backspace/Character | `SearchField` | M | HIGH |
| `apps/chat/src/events.rs:714` inline rename | chat | `text.push_str(s)`/`.pop()` | `EditableLabel` | M | HIGH |
| `apps/containers/src/panels.rs:1893` `prompt_key` | containers | `prompt.value.push_str(ch)`/`.pop()` | `TextInput` | S | HIGH |

### Forced by a missing widget capability (extend `ui` first — see [ui gaps](#ui-gaps))

| file:symbol | app | evidence | blocked on | effort | risk |
|---|---|---|---|---|---|
| `apps/db_client/src/connect_modal.rs:36-42` `ConnectModal` | db_client | **7** hand-rolled Strings (name/host/port/db/user/**password unmasked**/file) `field_mut(f).push_str(t)` (`:224`) | multiline not needed, but wants secret mode + dynamic field set | L | HIGH |
| `apps/imager/src/app.rs:1367` `type_into_field` | imager | one splicer feeding **7** fields incl. **masked password** (`secret=true` `:957`); `set_profile` rewrites whole CRDT each keystroke | secret `TextInput` mode | L | HIGH |
| `apps/mail/src/compose.rs:874` `plain_body`, `wysiwyg.rs:740` | mail | comment (`:177`): *"a single-line `TextInput` can't hold a multi-line body"*; `Wysiwyg` is a full forked rich editor (own `caret`, insert/backspace/split/select) | multiline `TextInput` / reusable rich editor | M/L | HIGH |
| `apps/chat/src/events.rs:676` composer, `:737` inline edit | chat | `insert_text` splices into Automerge + tracks `caret: usize` (`app.rs:242`) by hand; zero `TextInput` | multiline CRDT-bound composer | L | HIGH |
| `apps/slides/src/app.rs:2310` `insert_text`, `:5742` backspace | slides | canvas text elements + speaker-notes box, hand-rolled caret/selection/splice | multiline canvas text editor | M | HIGH |
| `apps/sheets/src/grid.rs:48` `EditState{chars,caret,sel}` | sheets | cell/formula body editor fully reimplements caret+selection; formula bar is a 2nd view | formula-aware single-line editor | L | HIGH |
| `apps/code_editor/src/editor_view.rs` (1999 LOC), `command_palette.rs:215` | code_editor | full multiline editor (caret/selection/completion); its own palette `query` String | shared code editor + reuse `ui` command_palette | L | HIGH |
| `apps/db_client/src/editor.rs:310` SQL editor | db_client | own `caret`+`buffer`, `EditorDoc::insert`, pixel-approx column math | shared code editor | L | HIGH |
| `apps/typst_editor/src/editor.rs` (1198 LOC) | typst_editor | hand-rolls caret/anchor/selection (correctly splices into Automerge `Text`) | shared code editor | L | HIGH |
| `apps/calculator/src/model.rs:467` `expr`, `mathfield/mod.rs` | calculator | expression `String` spliced by keypad; `mathfield` is a legit **2D structured** editor | shared `MathField` (mathfield is CRDT-clean already) | M | MED |

**Legitimately app-owned text surfaces (not flagged):** the terminal PTY grid
(`terminal/src/main_body.rs`), the hex byte-grid cursor. These *are* the app; but
even they must route **side** fields through shared widgets (terminal currently
has none; hex_editor's search/goto are flagged above).

---

## Category 2 — Buttons (MED risk)

Nearly universal anti-pattern: a painted rect (often collected into a private
`Vec<(Action, Rect)>` "hit list") tested with `.contains()`. Misses hover/press/
focus states, Space/Enter activation, a11y role, and `Theme` colors. **Zero
`ui::Button` in db_client, api_tester, git_client, diffmerge, logs, monitor,
ebook, tube, music, camera, video_editor, sheets, typst_editor, snippets, disks,
containers, firewall, vpn, remote, backups, imager, software_store, calculator,
slides.** Representative helpers:

| file:symbol | app | evidence | ui alt | effort | risk |
|---|---|---|---|---|---|
| `apps/ebook/src/app.rs:122` `struct Hit{rect,action}` + `:2361` `push_hit` | ebook | entire app is one painted rect + hit-test table | `Button` | L | MED |
| `apps/music/src/app/render.rs:581` `self.hits` | music | play/pause, transport, queue, filter tabs, genre chips all painted | `Button`/`TabBar` | L | MED |
| `apps/camera/src/app.rs:319` `push_region` | camera | shutter, gallery items, action bar (Favorite/Share/Delete), settings rows | `Button` | L | MED |
| `apps/pdf_viewer/src/app_draw.rs:76` `toolbar_layout`, `:1454` `SidebarHit::*` | pdf_viewer | toolbar tools + comment/signature action buttons as hit-rect table | `Toolbar`/`Button` | L | MED |
| `apps/video_editor/src/app.rs:1004` `Regions` | video_editor | transitions/colors/resolution/container/fps toolbar rects | `Button` | L | MED |
| `apps/tube/src/widgets/flex.rs:318` `Flex::on_click` | tube | app-wide clickable container (~20 sites); hover+press but no keyboard/focus/a11y | `Button` | L | MED |
| `apps/software_store/src/app.rs:1362` `fn pill_button` + `BtnStyle` enum | software_store | every install/open/update/back action | `Button` | L | MED |
| `apps/slides/src/app.rs:271` `struct UiButton{rect,kind,hit}` | slides | every toolbar/inspector button painted + hit-listed | `Button`/`Toolbar` | L | MED |
| `apps/monitor/src/app.rs:1986` `draw_action_button` | monitor | kill/stop actions | `Button` | M | MED |
| `apps/db_client/src/connect_modal.rs:50` `cancel_btn`/`save_btn` | db_client | painted rects + manual hit-test | `Button` | M | MED |
| `apps/git_client/src/diffview.rs:354` `draw_hunk_button` | git_client | stage/unstage/discard hunk buttons | `Button` | M | MED |
| `apps/diffmerge/src/merge_view.rs:340` take-ours/theirs/both, `app.rs:293` toolbar | diffmerge | painted colored rects, hit-tested `:482` | `Button`/`Toolbar` | M | MED |
| `apps/calculator/src/keypad.rs:245` `key_rects` | calculator | painted keypad grid + manual hit-test | `Button` grid (+ keypad widget gap) | L | MED |
| `apps/sheets/src/app.rs:83` `MenuButton`, `:201` `fx_rect` | sheets | File/Format/Σ menu-bar + fx affordance | `Button`/`Toolbar` | M | MED |
| `apps/disks/src/app.rs:2047` whole-disk + action pills | disks | painted pills hit-tested | `Button` | M | MED |
| `apps/vpn/src/app.rs:554` add/connect/footer, `apps/remote/src/app.rs:1438` rail | vpn, remote | `LayoutRects`/`push_hit` painted button rects | `Button` | M | MED |
| `apps/imager/src/app.rs:421` wizard back/primary | imager | painted rects hit-tested | `Button` | M | MED |
| `apps/api_tester/src/app.rs:808` `send_label` | api_tester | hand-painted Send button | `Button` | S | MED |
| `apps/logs/src/app.rs:873` `draw_back_button` | logs | narrow-mode nav back | `Button` | S | MED |
| `apps/word-processor/src/widget.rs:2463` `find_buttons`, `:5685` `recovery_buttons` | word-processor | find/replace + recovery-banner painted rects | `Button` | M | MED |
| `apps/snippets/src/app.rs:1545` `toolbar_button`, `expand.rs:249` copy/cancel | snippets | painted rect + hover-only hit-test | `Button` | S | MED |
| `apps/backups/src/app.rs:384` `Hit::BackupNow` | backups | "Backup now" painted button | `Button` | S | MED |
| `apps/notes/src/sidebar.rs:672` button chip | notes | one hand-drawn hover chip (otherwise a model app) | `Button` | S | MED |
| `apps/containers/src/panels.rs:1280` `draw_confirm` action rects | containers | create/confirm/prompt buttons hit-tested | `Button` | M | MED |
| `apps/video-player/src/app.rs:375` `open_button_rect` | video-player | floating "Open" pill (only leftover) | `Button` | S | MED |

---

## Category 3 — Modals / dialogs (MED risk)

App-drawn confirm/prompt/sheet + scrim instead of `ModalDialog` — misses focus
capture, Esc, scrim semantics, a11y, theming. Only `mail` and `notes` use the
real widget; `db_client` explicitly declines it.

| file:symbol | app | evidence | ui alt | effort | risk |
|---|---|---|---|---|---|
| `apps/containers/src/panels.rs:1163/1280/1310` | containers | `draw_create_modal` + `draw_confirm` + `draw_prompt`, own scrim (`:1509`) | `ModalDialog` | M | MED |
| `apps/vpn/src/app.rs:1099/1163` | vpn | `draw_modal` (add) + `draw_confirm_modal` + `modal_button_rects` | `ModalDialog` | M | MED |
| `apps/db_client/src/connect_modal.rs:256` | db_client | hand-drawn scrim `from_rgba8(0,0,0,150)` + panel; header comment declines `ModalDialog` for dynamic fields | `ModalDialog` | L | MED |
| `apps/monitor/src/app.rs:2093` `draw_modal` | monitor | kill/terminate confirm overlay + `confirm_stop` | `ModalDialog` | M | MED |
| `apps/disks/src/app.rs:1275` `draw_modal` | disks | format/erase confirm + scrim (`:246`) | `ModalDialog` | M | MED |
| `apps/feeds/src/app.rs:2669` `draw_input_modal` | feeds | scrim + card + title/helper/footer + field | `ModalDialog` | M | MED |
| `apps/software_store/src/app.rs:2497` `draw_ref_sheet` | software_store | flatpakref install-confirm sheet + scrim (`:2509`) | `ModalDialog` | M | MED |
| `apps/music/src/app/render.rs:3216` `draw_name_prompt` | music | name-input overlay + painted OK/Save | `ModalDialog` | M | MED |
| `apps/video_editor/src/app.rs:2292` `draw_export_modal` | video_editor | export dialog toggled by `show_export` | `ModalDialog` | M | MED |
| `apps/imager/src/app.rs:1589` `export_text` overlay | imager | Esc-dismiss config sheet | `ModalDialog` | S | MED |
| `apps/snippets/src/expand.rs` `ExpandModal` | snippets | own scrim (`:87`) + panel + focus + text fields + Esc/Enter/Tab | `ModalDialog` | M | HIGH |
| `apps/git_client/src/app.rs:149` `armed_discard` | git_client | destructive discard is a two-step "arm then click again" inline confirm, not a dialog | `ModalDialog` | M | MED |
| `apps/sheets/src/app.rs:2582` own scrim | sheets | paints its own scrim behind the (shared) command palette | `ModalDialog` scrim | S | LOW |
| `apps/notes/src/shortcuts_overlay.rs:172` | notes | dim backdrop + card + Esc; comment says it "mirrors `ModalDialog`'s 70% black" | `ModalDialog` | M | LOW |

---

## Category 4 — Other input controls (MED risk)

Dropdowns, toggles, sliders, tabs, segmented switches, menus, popovers, and
charts/tables reimplemented where a `ui` widget exists.

### Dropdowns / selects
| file:symbol | app | evidence | ui alt | effort |
|---|---|---|---|---|
| `apps/api_tester/src/app.rs:704` `draw_select` | api_tester | HTTP-method + ENV pickers | `Dropdown` | M |
| `apps/logs/src/app.rs:74/75` `unit_trigger`/`boot_trigger` | logs | unit-filter + boot-selection popups | `Dropdown` | M |
| `apps/ebook/src/app.rs:550/553/557` font/sort/font-picker menus | ebook | bool-gated overlay menus | `Dropdown`/`ContextMenu` | M |
| `apps/remote/src/app.rs:128` `SetKind` + AuthMethod | remote | SSH/VNC/RDP/qOS + auth pickers | `Dropdown`/`Segmented` | M |
| `apps/db_client/src/connect_modal.rs:48` `engine_btns` | db_client | Postgres/MySQL/SQLite segmented picker | `Segmented`/`Dropdown` | M |
| `apps/containers/src/combobox.rs` `ComboState` | containers | typeable image field — **documented `ui` gap** | `ComboBox` (new) | M |

### Toggles / switches
| file:symbol | app | evidence | ui alt | effort |
|---|---|---|---|---|
| `apps/tube/src/widgets/controls.rs:382` `struct Switch` | tube | speaks `ActionBus` — **`ui::Toggle` is CRDT-path-bound** (doc `:3`) | extend `Toggle` | M |
| `apps/camera/src/app.rs:1009` `toggle_switch` | camera | HDR/Grid/Level/Mirror/Geotag rows | `Toggle` | M |
| `apps/firewall/src/app.rs:884` `toggle_panic` | firewall | panic-mode switch | `Toggle` | S |
| `apps/vpn/src/app.rs:562` per-row on/off | vpn | connection switch | `Toggle` | M |
| `apps/remote/src/app.rs:1729` favorite/scale/fullscreen | remote | hand-drawn toggles | `Toggle` | M |
| `apps/backups/src/app.rs:867` source enable pill | backups | hand-drawn switch | `Toggle` | S |

### Sliders / scrubbers
| file:symbol | app | evidence | ui alt | effort |
|---|---|---|---|---|
| `apps/music/src/app/render.rs:414` scrubber, `:421` volume | music | bound to non-CRDT playhead/audio engine | extend `Slider` | M |
| `apps/tube/src/widgets/seek_bar.rs:32` `struct SeekBar` | tube | SponsorBlock scrubber with inline segment bands | extend `Slider` | M |
| `apps/video_editor/src/app.rs:113` filter drags, `:1442` timeline | video_editor | Brightness/Contrast/… + playhead scrub | `Slider` | M |
| `apps/ebook/src/app.rs:2` (see also) num_edit drag | ebook | drag-step numeric | `NumberInput` drag | S |

### Tabs / segmented
| file:symbol | app | evidence | ui alt | effort |
|---|---|---|---|---|
| `apps/disks/src/app.rs:876` `draw_tabs` | disks | Overview/SMART/Partitions | `TabBar` | M |
| `apps/vpn/src/app.rs:574` `DetailTab` | vpn | Status/Config/Logs | `TabBar` | M |
| `apps/pdf_viewer/src/app_draw.rs:1237` `SidebarHit::Tab` | pdf_viewer | sidebar tab strip | `TabBar` | M |
| `apps/software_store/src/app.rs:1132` nav-tab row | software_store | active/hover nav tabs | `TabBar`/`NavSplit` | M |
| `apps/code_editor/src/tab_bar.rs:37` `TabBar`, `bottom_panel.rs:205` | code_editor | file tabs + panel tabs | `ui` `TabBar` | M |
| `apps/sheets/src/app.rs:185` `tab_rects` | sheets | sheet-tab bar (per-tab color + rename) | `TabBar` (extend) | M |
| `apps/backups/src/app.rs:386` segmented tabs | backups | Timeline/Sources/Destinations/Schedules | `TabBar`/`Segmented` | M |
| `apps/camera/src/app.rs:992` `segmented` | camera | Photo/Video/Scan mode | `ToggleGroup`/`segmented` | M |
| `apps/video_editor/src/app.rs:920` resolution/container/fps | video_editor | painted segments | `ToggleGroup` | M |
| `apps/diffmerge/src/app.rs:293` mode selector | diffmerge | Diff / 3-way merge, width-estimated segments | `ToggleGroup` | M |
| `apps/logs/src/app.rs:625` priority threshold | logs | painted priority segments | `ToggleGroup` | M |
| `apps/firewall/src/app.rs:889` filter/zone cyclers | firewall | All/Allow/Deny + zone | `Segmented`/`ToggleGroup` | M |
| `apps/imager/src/app.rs:394` step chips | imager | wizard step selector | `Breadcrumb`/`Segmented` | M |
| `apps/word-processor/src/widget.rs:2468` find chips | word-processor | Aa / W / .* case/word/regex toggles | `Toggle`/`segmented` | S |
| `apps/hex_editor/src/view.rs:1043` search-kind chip | hex_editor | painted cycle | `segmented` | S |
| `apps/sheets/src/app.rs:95` `SettingChip` | sheets | settings option chips | `ToggleGroup` | M |

### Menus / popovers / charts / tables
| file:symbol | app | evidence | ui alt | effort |
|---|---|---|---|---|
| `apps/pdf_viewer/src/app.rs:226` `struct ContextMenu` | pdf_viewer | local struct, not `ui::ContextMenu` | `ContextMenu` | M |
| `apps/ebook/src/app.rs:488` `type_popover`, `:366` `ColorPick` | ebook | reading-settings popover + hand-rolled HSV picker | `Popover`/`ColorPicker` | M |
| `apps/music/src/app/render.rs:3096` `draw_volume_popover` | music | volume popover | `Popover` | M |
| `apps/hex_editor/src/view.rs:1018/1114` search/goto overlays | hex_editor | floating panels | `Popover` | M |
| `apps/typst_editor/src/app.rs:611` overflow menu | typst_editor | compact `menu_hits` | `ContextMenu` | M |
| `apps/browser/src/address_overlay.rs:22` | browser | ranked autocomplete overlay (reuses `Dropdown` geometry) | autocomplete `Popover` | M |
| `apps/slides/src/app.rs:285` fill-picker | slides | `ColorPicker` widget hand-positioned as popover | `Popover` | S |
| `apps/monitor/src/charts.rs:40/119/173/196` | monitor | line/area/gauge fully hand-rolled (`BezPath`/`Arc`) | `Chart`/`Sparkline`/`Gauge` | L |
| `apps/monitor/src/app.rs` process table, `apps/logs/src/app.rs:671` `draw_list` | monitor, logs | own column/row rects; `DataTable` only referenced in a comment | `DataTable` (virtualized) | L |
| `apps/notes/src/sidebar.rs:133` `NoteTreeView` | notes | custom page tree (needs inline rename) | extend `TreeView` | M |

---

## Category 5 — Hard-coded colors (LOW risk)

`Color::rgb*`/`from_rgb*`/`from_hex` literals instead of `Theme` tokens — breaks
dark-mode, high-contrast, and OS-accent. Total per app (production + tests):

| app | count | app | count | app | count |
|---|---|---|---|---|---|
| ebook | 62 | sheets | 13 | mail | 5 |
| word-processor | 44 | camera | 12 | calculator | 5 |
| tube | 32 | chat | 11 | weather | 4 |
| pdf_viewer | 18 | diffmerge | 10 | video_editor | 4 |
| feeds | 16 | slides | 7 | typst_editor | 3 |
| terminal | 14 (8 tests) | remote | 7 | containers | 3 |
| | | db_client | 7 | *(≤2: themes, monitor, logs, hex_editor, file-explorer, contacts, calendar, api_tester, software_store, maps, git_client, imager, firewall, disks, music, notes, video-player)* | |

**Not every literal is a violation** — triage before mass-replacing:

- **Genuine chrome violations (fix):** `word-processor/src/widget.rs:100-112`
  (`PAPER_DEFAULT`/`PAPER_INK`/`PAPER_SELECTION`/`PAPER_CARET` hard-coded paper
  palette — all 44 are in this one file); `diffmerge/src/merge_view.rs:340-342`
  (hard-coded green/blue/gray resolution colors — should read theme accents);
  the bulk of `ebook/src/app.rs` (34).
- **Defensible but still worth tokenizing:** drop-shadows written as
  `from_rgba8(0,0,0,α)` (chat, maps, weather, software_store scrims) → a
  `Theme` shadow/scrim token; domain palettes (feeds/ebook **reader themes**,
  mail **avatar** colors, tube **thumbnail** placeholders) → theme-derived seeds.
- **Not violations:** color-math helpers (lerp/alpha/hex-parse) in
  `calculator/src/draw.rs`, `slides/src/model.rs`, `maps/src/draw.rs`,
  `tube/src/contrast.rs`; `ColorPicker` seed defaults; test-only literals.

`code_editor`, `snippets`, `settings`, `backups`, `vpn`, `store`, `totp`, `osk`
are color-clean (0 production literals) — good references.

---

## ui gaps

Cases where an app hand-rolls because the shared widget is genuinely **missing a
capability**. Extend the one `ui` widget so every app inherits the accessible,
themed, clipboard-correct version — do **not** keep forking.

1. **Multiline `TextInput` / `TextArea`.** `TextInput` is single-line only. mail
   (`compose.rs:177` comment), chat composer, and slides speaker-notes each fork
   a multiline `String`/CRDT buffer. **Highest leverage** — unblocks the most
   HIGH-risk offenders. Add a multiline mode (soft-wrap, up/down caret, growable).

2. **Shared multiline code editor** (gutter + syntax-highlight hook, caret/
   selection). `code_editor/editor_view.rs` (1999 LOC), `typst_editor/editor.rs`
   (1198 LOC), and `db_client/editor.rs` all reimplement this; the first two even
   comment "same rule as `TextInput`". Biggest single body-editor gap.

3. **Reusable rich-text document editor.** `BlockEditor` exists (block-structured)
   but is not drop-in for a paged document with running headers
   (`word-processor/widget.rs`), a rich compose body (`mail/wysiwyg.rs`), or
   arbitrary-position canvas text (`slides`). Generalize `BlockEditor` /
   `text_input` so these stop forking full editors.

4. **Formula-aware single-line cell editor.** `sheets/grid.rs:48`
   `EditState{chars,caret,sel}` + point-mode is a second full editor; the formula
   bar is a third view. A `TextInput` variant with cell-reference point-mode.

5. **Masked / secret `TextInput` mode.** `imager` (`secret=true` `app.rs:957`) and
   `db_client` (unmasked password today) hand-roll password fields. Add a
   password/obscure mode with reveal toggle and clipboard suppression.

6. **Typeable `ComboBox`.** Free-text value + filtered suggestion dropdown.
   `containers/combobox.rs` documents this exact gap ("belongs in `ui` next to
   `Dropdown`") because `Dropdown` commits only fixed options.

7. **Non-CRDT value binding on `Toggle` / `Slider`.** Both bind to an Automerge
   path, so apps whose state lives in an audio engine / `ActionBus` can't use
   them: `tube/controls.rs:3` ("its `Toggle` is CRDT-path bound"), `tube` SeekBar,
   `music` scrubber+volume, `video_editor` filter drags. Add a plain-value /
   callback binding alongside the CRDT-path one.

8. **`Slider` with overlay segment bands.** `tube/seek_bar.rs` needs SponsorBlock
   category bands drawn over the track — extend `Slider` with a decoration hook.

9. **`NumberInput` with click-to-type + drag-step.** `ebook num_edit` forks
   digit-filtered entry; `NumberInput` exists — extend with drag-step and adopt.

10. **`TreeView` with inline rename + arbitrary hierarchy.** Forced `notes`'
    `NoteTreeView` fork (`sidebar.rs:133`). Add inline-rename + free hierarchy.

11. **Shared "find bar" (SearchField-in-popover).** word-processor, sheets,
    code_editor, hex_editor, snippets, monitor, logs each fork a mini single-line
    find/goto field. One `SearchField`-in-`Popover` pattern removes the whole
    class at once.

12. **`Chart` / `Sparkline` / `Gauge` adoption + virtualized/live `DataTable`.**
    The widgets exist but `monitor` (charts + process table) and `logs`
    (virtualized log table, live-tail) hand-roll them — extend `DataTable` for
    large append-only/live streams, then adopt.

13. **`MathField` (2D structured math).** `calculator/mathfield` is a legitimate
    GeoGebra-class editor `TextInput` can't express; graduate it into `ui` as a
    shared primitive rather than leaving it app-local.

14. **Keypad-grid widget.** `calculator/keypad.rs` paints its grid; a reusable
    keypad/button-grid would also serve `osk`'s toolbar strip.

---

## Remediation plan (waves)

Ordered by risk × reach. Each wave is independently shippable; waves 2/5b/7
require extending a `ui` widget *before* the app migrations.

### Wave 1 — Text inputs, pure non-adoption (HIGH, no ui changes needed)
Route existing single-line fields to `TextInput`/`SearchField`/`NumberInput`/
`EditableLabel`. **Apps:** feeds, maps, weather, logs, monitor, snippets,
hex_editor, word-processor (find), sheets (name box + tab rename), music
(name_input), ebook (num_edit + note), disks (confirm), vpn (add), containers
(prompt), remote (fields), software_store (search), chat (inline rename),
pdf_viewer (find query). *Start with feeds (the exemplar) and weather (smallest).*

### Wave 2 — Extend `TextInput`, then migrate the blocked text inputs (HIGH)
Land ui gaps **1** (multiline), **5** (secret mode), then migrate: mail
(plain_body), chat (composer), slides (notes), imager (7-field form incl.
password), db_client (connect_modal fields incl. password). Fold in the
per-keystroke CRDT rewrites (imager, containers) as diff-splice while touching
them (`ui/HIG.md` §6).

### Wave 3 — Modals (MED)
Migrate app-drawn confirm/prompt/sheet overlays to `ModalDialog`. **Apps:**
containers (×3), vpn (×2), db_client, monitor, disks, feeds, software_store,
music, video_editor, imager, snippets, git_client (replace arm-twice with a
dialog), sheets (scrim), notes (shortcuts overlay).

### Wave 4 — Buttons sweep (MED, broadest single win)
Replace painted-rect + hit-test tables with `Button`/`Toolbar`. **Apps (zero
`ui::Button` today):** db_client, api_tester, git_client, diffmerge, logs,
monitor, ebook, tube, music, camera, video_editor, sheets, typst_editor,
snippets, disks, containers, firewall, vpn, remote, backups, imager,
software_store, calculator, slides; plus leftovers in word-processor,
pdf_viewer, notes, video-player. Do the large hit-table apps (ebook, music,
camera, slides, software_store) last — they are `L` because the whole draw layer
is the table.

### Wave 5a — Controls with an existing widget (MED)
- **Dropdowns:** api_tester (method/env), logs (unit/boot), ebook (font/sort),
  remote (kind/auth), db_client (engine).
- **Toggles:** camera, firewall, vpn, remote, backups.
- **Tabs/segmented:** disks, vpn, pdf_viewer, software_store, code_editor,
  sheets, backups, camera, video_editor, diffmerge, logs, firewall, imager,
  word-processor, hex_editor.
- **Menus/popovers:** pdf_viewer (`ContextMenu`), ebook/music/hex_editor
  (`Popover`), typst_editor (overflow), browser (address overlay), slides (fill).

### Wave 5b — Controls blocked on a ui gap (MED)
Extend then migrate: gap **7** (non-CRDT Toggle/Slider binding) → tube Switch,
music/tube/video_editor sliders; gap **6** (typeable ComboBox) → containers;
gap **8** (Slider segment bands) → tube SeekBar; gap **9** (NumberInput
drag-step) → ebook.

### Wave 6 — Theme-color reads (LOW)
Replace genuine chrome literals with `Theme` tokens, worst-first: word-processor
(paper palette, `widget.rs:100-112`), diffmerge (`merge_view.rs:340-342`), ebook
(`app.rs`), pdf_viewer, feeds, camera, chat, sheets. Add `Theme` shadow/scrim
tokens for the `from_rgba8(0,0,0,α)` cluster. Leave color-math helpers and
test-only literals alone.

### Wave 7 — Larger widget builds, then adopt (MED/HIGH)
The multi-day widget work: gap **2/3** (code editor + rich-text document editor)
→ code_editor, typst_editor, db_client SQL, word-processor, mail, slides; gap
**4** (formula cell editor) → sheets; gap **12** (Chart/Gauge + virtualized
DataTable) → monitor, logs; gap **10** (TreeView inline-rename) → notes; gap
**13** (MathField) → calculator; gap **11** (shared find bar) collapses the
Wave-1 editor find fields into one primitive.

---

*Grade every migration on `shots`, not a green build (`ui/HIG.md` §7). When a
migration reveals a missing capability, extend the widget — never re-fork it.*
