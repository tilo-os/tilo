# Hand-rolled UI primitives audit

**What this is.** An evidence-based sweep of every qOS app that hand-rolls an
interactive UI primitive instead of using the shared `ui` toolkit widget. These
are the things that MUST be unified: a painted rect with a manual hit-test, or a
`String` field you `push`/`pop` on raw key events, silently drops text selection,
clipboard (Ctrl+C/V/X), keyboard activation and caret nav, the focus ring, the
a11y tree, and `Theme`-driven colors. The rule is codified in `ui/HIG.md` §8.

**Scope.** All 45 UI apps under `apps/`. Read-only audit — no refactors here. Method: for each category, the
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
| 2. Buttons (MED) | `Button` / `Toolbar` | **~28** (painted-rect + hit-test; 6 converted via `Button::paint`) | ebook, software_store, slides, camera |
| 3. Modals (MED) | `ModalDialog` | **14** (6 done: monitor, containers, vpn, software_store, music, feeds) | disks, db_client, snippets |
| 4. Other controls (MED) | `Dropdown`/`Toggle`/`Slider`/`ToggleGroup`/`ContextMenu`/`Popover`/`Chart` | **~22** (6 toggle/segmented/tab converts via `Toggle::paint` + `ToggleGroup::paint`) | tube, music, monitor, logs, camera |
| 5. Hard-coded colors (LOW) | `Theme` tokens | **35** (≥1 literal); **2 genuine chrome offenders fixed** (diffmerge, sheets) — the high-count apps are already themed, their literals are document/over-media/on-accent/color-math content | ebook 62, word-processor 44, tube 32 *(all defensible — see Category 5)* |

**Structural finding.** `ModalDialog` adoption is now broad after the modals
wave (monitor, containers, vpn, software_store, music, feeds, mail, notes) — it gained
`with_destructive` (danger primary) and a shared `PromptBody` text-entry body.
Nine apps mount the retained `Button`; the buttons wave added an immediate-mode
`Button::paint` face now shared by six more (see Category 2). `Slider` is
used by two apps, `SearchField` by one. Toggle adoption is good. So the debt is
overwhelmingly concentrated in **text input, buttons, and modals**, and a large
share of the buttons/modals is the *same* anti-pattern copied across apps: a
private `Vec<(Action, Rect)>` hit-test table painted by hand.

**2026-07-30 — Ctrl+K unified on the shared search widgets.** The app-level Ctrl+K
palette (`ui::ActionPalette`) previously bridged the resource/action model into
the older single-provider `CommandPalette`. It now renders through the shared
`SearchField` (input) + `SearchResultsView` (grouped results), driven by the
resource/action model, with `SearchProvider`s folded in via a new
`Widget::search_providers()` hook. So `SearchField`/`SearchResultsView` back
*every* app's Ctrl+K, and contacts' `ContactSearchProvider` (previously wired to
nothing) surfaces there. `CommandPalette` remains a general widget for the shell
global-search overlay, containers, and sheets — it is just no longer a second
Ctrl+K stack in `ui::App`.

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

### 2026-08-17 re-sweep — authoritative current status (supersedes the tables below)

A full fresh sweep of every app's text-entry surfaces (grepping
`push_str`/`pop`/`insert`/`splice` on `Key::Character`/`Backspace`, fake caret
strokes, and `TextInput`/`SearchField`/`EditableLabel`/`Autocomplete` adoption).
The pre-existing tables in this section are historical; this block is the current
truth. **Excluded this round (owned by other agents): `software_store`,
`snippets`, `totp` — untouched.**

The shared primitives all exist and fit: `TextInput::local()` (self-contained
field — owns caret/selection/clipboard, read `.value()`, `with_enter_submits`/
`take_submit`), `TextInput::new(path)` (CRDT-bound), `with_secret(true)` (masked),
`multiline(true)`, `SearchField` (magnifier pill wrapping a local `TextInput`),
`EditableLabel` (`EditOutcome::Committed`, blank-keeps-previous rename semantics),
`Autocomplete` (a `TextInput` + suggestion dropdown). The embedding pattern for
the monolithic immediate-mode apps is proven by `contacts`/`api_tester`: hold the
widget as a struct field, place with `apply_layout_external(rect)`, forward
`sync`/`draw`/`handle_event` (pass the app's own `node_id` + `taffy` + a throwaway
`focus_target` sink), and drive focus explicitly with `set_focus`.

**Already on the shared widget — nothing to do (verified this sweep):**

| app | surface | now uses |
|---|---|---|
| weather | city search | `Autocomplete` (wraps `TextInput`) — `core.rs:136` |
| feeds | add / rename / unsubscribe | `ModalBody` + real `TextInput` — `modal.rs:75` |
| maps | search / origin / dest / download | `TextInput` ×3 + `Autocomplete` — `app.rs:214-304` |
| music | playlist name prompt / library search | `ModalDialog<PromptBody>` + `TextInput` — `app.rs:512,228` |
| typst_editor | source editor | `ui::CodeEditor` (prior wave) |
| chat | composer (main + thread) | `TextInput` multiline — `app.rs:292-293` |
| ebook | inline renames | `EditableLabel` — `app.rs:116` |
| containers | single-field prompt (new-volume / tag / pull) | `ModalDialog<PromptBody>` — `app.rs:116` |
| imager | 6 profile fields incl. **secret** password | `TextInput` (+ `with_secret`) — `customize.rs:105-135` |
| pdf_viewer | inline comment / reply editor | `TextInput` — `app.rs:300` |
| slides | deck title | `TextInput` — `app.rs:527` |

**Still hand-rolled — MIGRATE (this round's scope):**

| app | surface | state → key-mutation | verdict / target | effort |
|---|---|---|---|---|
| disks ✅ | format-confirm field | `app.rs:292 confirm_input` → `:2108 push(ch)` | **DONE** → `TextInput::local` (plain chrome; app keeps its success-border box) | S |
| pdf_viewer | find query | `app.rs:406 query` → `app_events.rs:918 push(ch)` | MIGRATE → `SearchField` | S |
| imager | "type device name to arm" confirm | `app.rs:104 confirm` → `:1421 push(c)` | MIGRATE → `TextInput::local` (paste-safety caveat) | S |
| vpn ✅ | Add-tunnel name / endpoint | `app.rs:270/271` → `:1719 push_str` | **DONE** → `TextInput::local` ×2 (plain chrome inside app boxes; Tab cycles focus) | M |
| monitor | process filter | `app.rs:265 filter:String` → `:2656 push(ch)` | MIGRATE → `SearchField` | M |
| logs | filter search | Automerge `Text` `"search"` → `app.rs:1424 search_push` | MIGRATE → `SearchField` (mirror to doc) | M |
| hex_editor | find search | `view.rs:69 search_query` → `:1285 push_str` | MIGRATE → `SearchField` (adds missing caret) | M |
| hex_editor | go-to offset | `view.rs:77 goto_query` → `:1316 push_str` | MIGRATE → `TextInput::local` (hex parse stays; **not** `NumberInput`) | S |
| sheets | name-box / go-to | `app.rs:139 NameBoxEdit` → `:3517 insert` | MIGRATE → `TextInput::local` (enter_submits) | M |
| sheets | sheet-tab rename | `app.rs:127 TabRename` → `:3403 insert` | MIGRATE → `EditableLabel` | S |
| word-processor | find / replace bar | `widget.rs:1595/1596` → `:2753 push_str` | MIGRATE → `TextInput::local` ×2 | M |
| chat | inline channel rename | `app.rs:330 rename` → `events.rs:925 push_str` | MIGRATE → `EditableLabel` | S |
| chat | edit-sent-message | `app.rs:317 editing` → `events.rs:950 insert_str` | MIGRATE → `TextInput` multiline | M |
| ebook | on-page note composer | `app.rs:587 composing` → `:2845 note.push_str` | MIGRATE → `TextInput` multiline (CRDT `Text`) | M/L |
| containers | Create-container (5 text fields) | `doc.rs:285 CreateField` → `panels.rs:1742` | MIGRATE → `TextInput::local` ×5 | M |
| db_client | connect-modal 7 fields (incl. **password**) | `connect_modal.rs:42` → `:224 field_mut().push_str` | MIGRATE → `TextInput::local` + `with_secret` for password | M/L |
| remote | draft name/host/user/group + search | `app.rs:179`/`model.rs:217` → `:654 push_str` | MIGRATE → `TextInput::local` (port stays numeric-filtered) | M/L |
| slides | speaker-notes box | `app.rs:351 editing_notes` → `:2503 splice_notes` | MIGRATE → `TextInput` multiline (CRDT `Text`) | M |

**Genuine KEEP-with-reason (document/structured engines whose buffer *is* the
app, or a non-plain-text field):**

| app | surface | why KEEP |
|---|---|---|
| terminal | PTY grid | the app *is* the grid; no side text field today |
| code_editor | multiline code editor | full editor; adopts `ui::CodeEditor` (gap 2) — not a plain field |
| calculator | mathfield + keypad | 2D structured math editor; keypad is a button grid |
| sheets | grid cell/formula editor (`grid.rs:51`) | formula point-mode (click a cell to insert its ref) |
| hex_editor | byte grid (`view.rs:698`) | the hex canvas cursor is the editor |
| word-processor | rich-text document editor | paged rich-text CRDT editor (gap 3) |
| slides | on-canvas free-position text | arbitrary position/rotation, not a rectangular inline field |
| containers | Create-container **Image** field (`combobox.rs`) | typeable free-text ComboBox (value + filtered suggestions) — documented `ui` gap 6 |
| ebook | `num_edit` page field (`app.rs:651`) | digit/`.`-filtered numeric entry — `NumberInput` shape, not plain text |

**Counts.** Text-input surfaces across the fleet (excl. the 3 owned elsewhere):
**11 apps already fully on the shared widget** (incl. the done halves of the 6
partially-migrated apps), **18 surfaces across 15 apps still hand-rolled and in
scope to MIGRATE**, **9 genuine KEEP-with-reason** engines/fields.

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
| `apps/imager/src/app.rs:1367` `type_into_field` | imager | ~~one splicer feeding **7** fields incl. **masked password**~~ **password field DONE** — migrated to secret `TextInput`; the other 6 fields + whole-CRDT-per-keystroke rewrite remain | secret `TextInput` mode (gap 5) | L | HIGH |
| `apps/mail/src/compose.rs` `plain_body`, `wysiwyg.rs:740` | mail | **plain body DONE** — the hand-rolled multiline `String` is now a multiline `TextInput`. `Wysiwyg` (HTML mode) is still a forked rich editor (own `caret`, insert/backspace/split/select) | multiline `TextInput` (gap 1) done; rich editor = gap 3 | M/L | HIGH |
| `apps/chat/src/events.rs:676` composer, `:737` inline edit | chat | `insert_text` splices into Automerge + tracks `caret: usize` (`app.rs:242`) by hand; zero `TextInput` | multiline CRDT-bound composer | L | HIGH |
| `apps/slides/src/app.rs:2310` `insert_text`, `:5742` backspace | slides | canvas text elements + speaker-notes box, hand-rolled caret/selection/splice | multiline canvas text editor | M | HIGH |
| `apps/sheets/src/grid.rs:48` `EditState{chars,caret,sel}` | sheets | cell/formula body editor fully reimplements caret+selection; formula bar is a 2nd view | formula-aware single-line editor | L | HIGH |
| `apps/code_editor/src/editor_view.rs` (1999 LOC), `command_palette.rs:215` | code_editor | full multiline editor (caret/selection/completion); its own palette `query` String | `ui::CodeEditor` (gap 2, **built**) + reuse `ui` command_palette | L | HIGH |
| `apps/db_client/src/editor.rs:310` SQL editor | db_client | own `caret`+`buffer`, `EditorDoc::insert`, pixel-approx column math | `ui::CodeEditor` (gap 2, **built**) w/ `Language::Sql` | L | HIGH |
| ~~`apps/typst_editor/src/editor.rs` (1198 LOC)~~ typst_editor | typst_editor ✅ | **DONE** — migrated onto `ui::CodeEditor` (`Language::Typst`); hand-rolled caret/anchor/selection/gutter/highlight + the per-line lexer deleted (~1100 LOC), thin shell keeps only the diagnostics overlay | `ui::CodeEditor` (gap 2, **built**) | L | HIGH |
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
`ui::Button` in git_client, logs, ebook, tube, music, camera, video_editor,
sheets, typst_editor, disks, containers, firewall, vpn, remote, backups, imager,
software_store, calculator, slides.** Representative helpers:

**Status (this wave):** the standard apps are monolithic *immediate-mode*
widgets — a single leaf `Widget` that paints its whole surface and hit-tests
its own rects — so they can't mount the retained `ui::Button` without a
tree rewrite. To convert them cleanly the toolkit gained an **immediate-mode
button face**, `ui::widget::Button::paint(&ButtonFace)` (extended, not forked:
`face_colors` is now the one source of truth for the fill/border/label palette
per interaction state, shared by the retained widget's look, the file-picker
footer, and every converted app). `ButtonFace` builds fluently —
neutral/primary/danger style, hover/press/focus/disabled state, radius, weight,
leading icon. Apps keep their existing rect hit-test (which already fires the
action) and now paint the themed, stateful face instead of a hand-rolled rect,
gaining real hover/press feedback and one consistent button look. Converted:
**api_tester** (Send), **snippets** (action toolbar + expand Copy/Cancel),
**db_client** (connect-modal Cancel/Save), **video-player** (floating Open
pill), **monitor** (detail Stop/Kill, incl. the danger style), **diffmerge**
(toolbar Open/Load/Next/Prev). Keyboard/AT activation and per-button a11y nodes
remain bounded by these apps' monolithic immediate-mode architecture (a separate
retained-tree refactor), not by the button primitive.

Left for later, with the reason each isn't a clean face-swap:
- **git_client** hunk stage/unstage/discard — a *ghost* style (translucent at
  rest, accent/danger fill only on hover); needs a `Ghost`/tinted face variant.
- **notes** new-page chip — a borderless accent-tint icon chip; same ghost gap.
- **logs** narrow-mode Back — a bespoke translucent pill with a left-aligned
  chevron (not centred); ghost variant + leading-icon alignment.
- **backups** "Back up now" — carries a progress sliver + custom `+` glyph; a
  custom surface, not a plain button.
- **imager** footer primary — bespoke per-state styling (lightens on hover,
  muted-danger at rest, flat-grey disabled) that diverges from the shared face
  on several states; its neutral Back is convertible but the pair is left
  together.

Out of scope for the Buttons wave (custom interactive surfaces, not action
buttons): the merge take-ours/theirs/both regions in **diffmerge**
(`merge_view.rs`), the segmented mode control in diffmerge's toolbar (a Tabs/
segmented job — Wave 5a), and the painted **calculator** keypad grid (a keypad
widget, tracked separately).

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
| `apps/monitor/src/app.rs:1986` `draw_action_button` | monitor ✅ | kill/stop actions | `Button` | M | MED |
| `apps/db_client/src/connect_modal.rs:50` `cancel_btn`/`save_btn` | db_client ✅ | painted rects + manual hit-test | `Button` | M | MED |
| `apps/git_client/src/diffview.rs:354` `draw_hunk_button` | git_client | stage/unstage/discard hunk buttons | `Button` | M | MED |
| `apps/diffmerge/src/merge_view.rs:340` take-ours/theirs/both, `app.rs:293` toolbar | diffmerge ✅ (toolbar) | painted colored rects, hit-tested `:482` | `Button`/`Toolbar` | M | MED |
| `apps/calculator/src/keypad.rs:245` `key_rects` | calculator | painted keypad grid + manual hit-test | `Button` grid (+ keypad widget gap) | L | MED |
| `apps/sheets/src/app.rs:83` `MenuButton`, `:201` `fx_rect` | sheets | File/Format/Σ menu-bar + fx affordance | `Button`/`Toolbar` | M | MED |
| `apps/disks/src/app.rs:2047` whole-disk + action pills | disks | painted pills hit-tested | `Button` | M | MED |
| `apps/vpn/src/app.rs:554` add/connect/footer, `apps/remote/src/app.rs:1438` rail | vpn, remote | `LayoutRects`/`push_hit` painted button rects | `Button` | M | MED |
| `apps/imager/src/app.rs:421` wizard back/primary | imager | painted rects hit-tested | `Button` | M | MED |
| `apps/api_tester/src/app.rs:808` `send_label` | api_tester ✅ | hand-painted Send button | `Button` | S | MED |
| `apps/logs/src/app.rs:873` `draw_back_button` | logs | narrow-mode nav back | `Button` | S | MED |
| `apps/word-processor/src/widget.rs:2463` `find_buttons`, `:5685` `recovery_buttons` | word-processor | find/replace + recovery-banner painted rects | `Button` | M | MED |
| `apps/snippets/src/app.rs:1545` `toolbar_button`, `expand.rs:249` copy/cancel | snippets ✅ | painted rect + hover-only hit-test | `Button` | S | MED |
| `apps/backups/src/app.rs:384` `Hit::BackupNow` | backups | "Backup now" painted button | `Button` | S | MED |
| `apps/notes/src/sidebar.rs:672` button chip | notes | one hand-drawn hover chip (otherwise a model app) | `Button` | S | MED |
| `apps/containers/src/panels.rs:1280` `draw_confirm` action rects | containers | create/confirm/prompt buttons hit-tested | `Button` | M | MED |
| `apps/video-player/src/app.rs:375` `open_button_rect` | video-player ✅ | floating "Open" pill (only leftover) | `Button` | S | MED |

---

## Category 3 — Modals / dialogs (MED risk)

App-drawn confirm/prompt/sheet + scrim instead of `ModalDialog` — misses focus
capture, Esc, scrim semantics, a11y, theming.

**Status (this wave):** the confirm/prompt/sheet overlays in **monitor,
containers, vpn, software_store** are now routed through the shared
`ui::widget::ModalDialog` (themed scrim + centered box + title + content +
primary/secondary + Esc/backdrop dismiss + focus trap + a11y `Dialog` role). The
widget was extended additively: `ModalDialog::with_destructive(true)` paints the
primary in `semantic.danger` for delete/kill/erase, and a shared
`ui::widget::PromptBody` (label + real `TextInput`, with `multiline`/`secret`)
is the canonical text-entry body so prompts stop hand-rolling a field. `feeds`
was already converted (its local `ModalBody` + `ModalDialog`); `mail`/`notes`
also use the widget elsewhere.

| file:symbol | app | status |
|---|---|---|
| `apps/monitor/src/app.rs` kill/stop confirm | monitor | **DONE** — `ModalDialog<Text>`, destructive primary; `draw_modal` + `hit.modal_*` deleted |
| `apps/containers/src/panels.rs` `draw_confirm` + `draw_prompt` | containers | **DONE** — confirm → `ModalDialog<Text>` (destructive); prompt → `ModalDialog<PromptBody>` (also kills the hand-rolled `prompt.value` String). `draw_create_modal` (multi-field form) left for the text-input wave |
| `apps/vpn/src/app.rs:1163` `draw_confirm_modal` | vpn | **DONE** — `ModalDialog<ConfirmBody>` (keeps the command-preview box), destructive primary for disconnect. `draw_modal` (the Add multi-field form) left for the text-input wave |
| `apps/software_store/src/app.rs:2497` `draw_ref_sheet` | software_store | **DONE** — `ModalDialog<RefBody>` (icon tile + title + source + comment); adds real Esc/focus-trap the scrim-only sheet lacked |
| `apps/feeds/src/app.rs` (was `draw_input_modal`) | feeds | **DONE (prior)** — `ModalDialog<ModalBody>` with a real `TextInput` |
| `apps/db_client/src/connect_modal.rs:256` | db_client | **DEFER** — declines `ModalDialog` for a dynamic field set + an unmasked password; belongs to the text-input wave (secret `TextInput` + dynamic content) |
| `apps/disks/src/app.rs:1275` `draw_modal` | disks | **DEFER** — a gated *type-the-device-name-to-confirm* dialog with a locked primary; needs a gated-primary `ModalDialog` capability first |
| `apps/music/src/app/render.rs` `draw_name_prompt` | music | **DONE** — create/rename playlist prompt → `ModalDialog<PromptBody>` (real `TextInput`, kills the hand-rolled `name_input` char-splice); `name_input` kept only as the submit value-carrier |
| `apps/video_editor/src/app.rs:2292` `draw_export_modal` | video_editor | **DEFER** — embeds interactive segmented controls (resolution/format/fps); belongs with the controls wave (5a), then wrap in `ModalDialog` |
| `apps/imager/src/app.rs:1578` `export_text` overlay | imager | **REMAINING** — read-only config sheet with per-line syntax colouring; wrap in a no-footer `ModalDialog` + a small coloured-config body |
| `apps/snippets/src/expand.rs` `ExpandModal` | snippets | **DEFER** — multiple per-variable text fields; belongs with the text-input wave |
| `apps/git_client/src/app.rs:149` `armed_discard` | git_client | **DEFER** — an inline two-click "arm then confirm" affordance, not an overlay; converting to a dialog is a behaviour change, not a drop-in |
| `apps/sheets/src/app.rs:2582` own scrim | sheets | **LEAVE** — scrim-only behind the already-shared command palette; low value, and wrapping the palette would fight its own layout |
| `apps/notes/src/shortcuts_overlay.rs:172` | notes | **REMAINING (LOW)** — info card that already mirrors `ModalDialog`'s backdrop; structured shortcut-row content needs a small custom body |

---

## Category 4 — Other input controls (MED risk)

Dropdowns, toggles, sliders, tabs, segmented switches, menus, popovers, and
charts/tables reimplemented where a `ui` widget exists.

**Status (toggles / segmented / tabs wave).** The standard apps are monolithic
*immediate-mode* painters, so — as with the buttons wave — the toolkit gained
two immediate-mode paint faces rather than forcing a tree rewrite:
`ui::widget::Toggle::paint(&ToggleFace)` (themed switch track + knob + focus
ring + disabled dim) and `ui::widget::ToggleGroup::paint(&SegmentedFace)`
(a segmented control over caller-owned rects). Both route their per-state
palette through one source of truth (`toggle_face_colors` / `segment_colors`)
shared with the retained widgets' own draw — *extended, not forked*.
`SegmentedFace` carries three styles so one primitive absorbs the common looks:
**Solid** (unified strip + separators + border), **Pill** (discrete
accent/hover pills, tab-strip / mode-switch), **Underline** (accent underline
under the active tab), plus a left-aligned-label option for nav-bar tabs.
Apps keep their existing rect hit-test (which already fires the action) and now
paint the shared, stateful control. **Converted:** **firewall** (rule/zone
switches + filter/kind/proto/action segmenteds), **backups** (view tabs +
source-enable switch), **disks** (Overview/SMART/Partitions tabs), **diffmerge**
(Diff / 3-way merge switch), **software_store** (nav tabs, left-aligned + badge
kept app-owned), **pdf_viewer** (Comments/Contents/Signatures underline tabs).

Left for later, with the reason each isn't a clean face-swap:
- **tube** Switch/SeekBar, **music**/**video_editor** sliders — `Toggle`/`Slider`
  are CRDT-path-bound; need the non-CRDT value binding (gap 7, Wave 5b).
- **vpn** — the connection switch is `semantic.success` (green = connected, not
  accent) and the DetailTab active state is a subtle *raised* surface (not the
  accent); both want an additive on-tone / subtle-selected option first.
- **camera** — the mode segmented + HDR/Grid/… switches are deliberately
  over-media overlays (fixed dark-on-light per Category 5), so routing them
  through the theme-tracking shared control would regress that contract.
- **logs** severity chips — a *cumulative*, per-priority-**coloured** threshold
  (many active at once, coloured by priority not accent), not a single-select
  segmented; converting would lose the semantics + colours.
- **remote** favorite/scale/fullscreen — icon toggle-*buttons*, not track+knob
  switches (a buttons-wave shape).
- **imager** step chips — a 3-state wizard breadcrumb/stepper (done / current /
  upcoming), not a two-state segmented.
- **word-processor** find chips (Aa/W/.*) — *independent* boolean toggles, not
  an exclusive selector; **hex_editor** search-kind — a cycle button, not a
  visible-options segmented. Both are toggle-button/chip shapes.

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
| `apps/camera/src/app.rs:1009` `toggle_switch` | camera | HDR/Grid/Level/Mirror/Geotag rows — **over-media, left (see status)** | `Toggle` | M |
| `apps/firewall/src/app.rs` rule/zone switches | firewall ✅ | **DONE** — `draw_toggle` → `Toggle::paint` | `Toggle` | S |
| `apps/vpn/src/app.rs:984` `draw_switch` | vpn | connection switch — **`semantic.success` green, left (see status)** | `Toggle` | M |
| `apps/remote/src/app.rs:1729` favorite/scale/fullscreen | remote | icon toggle-*buttons*, not switches — buttons-wave shape | `Toggle` | M |
| `apps/backups/src/app.rs` source enable switch | backups ✅ | **DONE** — `draw_switch` pill → `Toggle::paint` | `Toggle` | S |

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
| `apps/disks/src/app.rs:876` `draw_tabs` | disks ✅ | **DONE** — Overview/SMART/Partitions → `ToggleGroup::paint` (Pill) | `ToggleGroup` | M |
| `apps/vpn/src/app.rs:1029` `DetailTab` | vpn | Status/Config/Logs — **raised-subtle active (see status)** | `ToggleGroup` | M |
| `apps/pdf_viewer/src/app_draw.rs:1215` sidebar tabs | pdf_viewer ✅ | **DONE** — Comments/Contents/Signatures → `ToggleGroup::paint` (Underline) | `ToggleGroup` | M |
| `apps/software_store/src/app.rs:1126` nav-tab row | software_store ✅ | **DONE** — nav tabs → `ToggleGroup::paint` (Pill, left labels; badge app-owned) | `ToggleGroup` | M |
| `apps/code_editor/src/tab_bar.rs:37` `TabBar`, `bottom_panel.rs:205` | code_editor | file tabs + panel tabs (closeable, editor-owned) | `ui` `TabBar` | M |
| `apps/sheets/src/app.rs:185` `tab_rects` | sheets | sheet-tab bar (per-tab color + rename) | `TabBar` (extend) | M |
| `apps/backups/src/app.rs` view tabs | backups ✅ | **DONE** — Timeline/Sources/Destinations/Schedules → `ToggleGroup::paint` (Pill) | `ToggleGroup` | M |
| `apps/camera/src/app.rs:992` `segmented` | camera | Photo/Video/Scan mode — **over-media, left (see status)** | `ToggleGroup` | M |
| `apps/video_editor/src/app.rs:920` resolution/container/fps | video_editor | painted segments (inside export modal) | `ToggleGroup` | M |
| `apps/diffmerge/src/app.rs:317` mode selector | diffmerge ✅ | **DONE** — Diff / 3-way merge → `ToggleGroup::paint` (Pill) | `ToggleGroup` | M |
| `apps/logs/src/app.rs:625` priority threshold | logs | cumulative, per-priority-**coloured** chips — **not single-select (see status)** | `ToggleGroup` | M |
| `apps/firewall/src/app.rs` filter/kind/proto/action | firewall ✅ | **DONE** — `draw_segmented` → `ToggleGroup::paint` (Solid) | `ToggleGroup` | M |
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

**Status (this wave).** The raw counts badly over-state the debt: on close
triage the overwhelming majority of these literals are *not* chrome. The
app chrome (bars, panels, buttons, borders, selection, accents) is already
theme-driven in every app checked — proven by rendering each in light **and**
dark and confirming the whole chrome flips while only content stays put
(`QOS_SHOTS_THEME=light|high-contrast` was added to the `--shots` renderers to
make this repeatable). What remains hard-coded is legitimately *not* theme-tracking:

- **Document / paper palettes** — a page authored for a light sheet with dark
  ink: `word-processor` `PAPER_*` (documented "paper is always light" + the ink
  is authored for it), `pdf_viewer/render.rs` `ink*()` on an always-white page
  (`paper()` = `surfaces.window_card`), `ebook`/`feeds` **reader themes**
  (`reader_theme.rs` — the user's Light/Sepia/Dark reading mode, deliberately
  independent of the OS theme). These are content; forcing them to the theme
  background would make the authored ink unreadable.
- **Over-media overlays** — controls painted over a video / thumbnail / camera
  frame, which must stay dark-on-light regardless of OS theme: `tube` (play /
  duration / progress / DeArrow badges, seed-gradient thumbnails), `camera`
  (viewfinder scrims + REC red), `chat` (video-call plates).
- **On-accent / on-color foregrounds** — the near-white label on an accent fill
  or the dark glyph on a saturated chip: `ebook` `248,248,252`,
  `word-processor` `on_accent: WHITE`, `diffmerge` selected-chip ink `10,14,22`,
  `db_client` glyph-on-type-color `20,20,20`. These stay fixed on purpose (they
  contrast the saturated fill in every theme; flipping them to a theme neutral
  would *invert* them and lose contrast).
- **Color-math helpers, picker seeds, categorical / syntax palettes, test
  literals** — `*/theme_ext.rs` + `tube/contrast.rs` + `slides/model.rs`
  lerp/darken/hex-parse; `ColorPicker::new(seed)` defaults; `sheets`
  formula-reference palette (documented syntax-style), `mail` avatar palette
  (already 6/8 theme tokens + 2 categorical hues); anything under `#[cfg(test)]`.
- **Drop-shadows / scrims** — `from_rgba8(0,0,0,α)` (ebook, sheets, chat, feeds,
  camera, db_client, remote). Dark scrim/shadow is conventional in *both*
  light and dark; there is no `Theme` shadow token yet, so these stay (a future
  `Theme` shadow/scrim token could fold them in — tracked, not a blocker).

**Converted this wave (genuine "frozen to one theme's token value" chrome):**

| app | converted | left (defensible) |
|---|---|---|
| `diffmerge/src/merge_view.rs` | **6** — resolution chips (`Take/is` ours/theirs/both) were frozen to the *dark* theme's `16,185,129`/`96,165,250`/`148,163,184`; now read `semantic.success`/`semantic.info`/`text_dim` (matches the app's own `ColorClass` mapping) → chips retint in light/high-contrast | 4 — selected-chip on-color ink `10,14,22` (must stay dark on the saturated chip); 3× `with_alpha` helper |
| `sheets/src/grid.rs` | **1** — cell-selection wash was frozen to indigo-500 `99,102,241`@α; now `colors.accent[5].with_alpha_factor(0.15)` (matches the name-box/tab accents) → selection tracks the OS accent | 12 — 4× shadow/scrim `0,0,0,α`; 7× formula-reference syntax palette (documented); 1× dynamic `from_rgb8(r,g,b)` |
| `ebook` | 0 | 62 — reader palettes (`reader_theme.rs`), annotation/highlight content, on-accent fg, book-cover shadows, HSV picker internals, luminance-picked cover ink. Chrome already themed (proven light+dark). |
| `word-processor` | 0 | 44 — chrome already themed via `ChromeColors::for_theme` (proven light+dark); rest is the deliberate light-paper palette + on-paper markup (comments/tracked-changes) + style palette + picker + presence + tests |
| `tube` | 0 | 32 — over-media overlays, DeArrow/SponsorBlock brand hues, `contrast.rs`/`util.rs` color-math, tests. Chrome already themed (proven light+dark). |
| `pdf_viewer` | 0 | 18 — document ink on the always-white page; `swatch_color` already reads theme tokens |
| `feeds` | 0 | 16 — reader themes + one scrim + `with_alpha` helper |
| `camera` | 0 | 12 — over-viewfinder scrims + REC red |
| `chat` | 0 | 11 — drop-shadows + over-video-call plates |
| `mail` / `db_client` / `slides` / `remote` | 0 | avatar palette (already tokenised) / on-color inks + scrims / color-math + picker seed / `theme_ext` color-math + tests |

`code_editor`, `snippets`, `settings`, `backups`, `vpn`, `store`, `totp`, `osk`
are color-clean (0 production literals) — good references.

---

## ui gaps

Cases where an app hand-rolls because the shared widget is genuinely **missing a
capability**. Extend the one `ui` widget so every app inherits the accessible,
themed, clipboard-correct version — do **not** keep forking.

1. **Multiline `TextInput` / `TextArea`. — DONE.** `TextInput` now has a
   `multiline(true)` mode on the same caret/`text_selection`/clipboard/CRDT
   core: soft-wrap at the field width, `Up`/`Down` caret motion by visual line
   with a preserved goal column, per-visual-line `Home`/`End`, real newline
   insertion on `Enter` (with `with_enter_submits(true)` for single-line and
   chat-style hosts where Enter submits and Shift+Enter splits), cross-line
   selection, newline-aware paste, vertical scroll following the caret, and a
   content-height `measure` for auto-grow. It reports
   `AccessState::multiline`. **Adopter:** mail's compose body
   (`compose.rs`) — the hand-rolled `plain_body: String` (with its
   `handle_plain_key`/`draw_plain_body`) is deleted and replaced by a multiline
   `TextInput`. Remaining forks to migrate onto it: chat composer, slides
   speaker-notes.

2. **Shared multiline code editor** (gutter + syntax-highlight hook, caret/
   selection). **— BUILT.** `ui::widget::CodeEditor` puts real code editing on the
   multiline `TextInput` caret/`text_selection`/clipboard/undo/IME/a11y core and
   adds a line-number gutter with a current-line highlight, a pluggable
   `Highlighter` trait (`fn spans(&self, line, carry) -> LineSpans` — per-line
   byte-range spans + cross-line carry for block comments / fenced code) whose
   `TokenKind`s map to theme colours via `token_color` (resolved from the accent
   / semantic ramps, so syntax tracks light/dark), soft-tab + Shift+Tab outdent,
   and wheel scroll. Ships built-in Rust, Typst, and SQL highlighters (plus a
   generic keyword one); apps supply their own language tokenizer and the widget
   owns rendering/caret/selection. **Adopter:** `typst_editor` — its 1198-LOC
   hand-rolled `SourceEditor` (own caret/anchor/selection, insert/delete/word-
   motion, clipboard, soft-tab/outdent, gutter, and syntax-highlight draw) and
   its 255-LOC per-line Typst lexer (`highlight.rs`) are deleted; `editor.rs` is
   now a thin shell that owns a `CodeEditor` (with the built-in `Language::Typst`
   highlighter) and keeps only the Typst-specific compiler-diagnostic overlay
   (gutter markers, squiggles, hover tooltip), positioned from the widget's
   public geometry (`caret_rect` / `selection_rects` / `gutter_width`). ~1100 LOC
   of duplicated editor/highlighter code removed; real selection/clipboard/undo
   now for free. **Remaining adopters to migrate:** `code_editor/editor_view.rs`
   (1999 LOC, + its command palette → reuse `ui` `command_palette`) and
   `db_client/editor.rs` (SQL editor — use the built-in `Language::Sql`
   highlighter).

3. **Reusable rich-text document editor.** `BlockEditor` exists (block-structured)
   but is not drop-in for a paged document with running headers
   (`word-processor/widget.rs`), a rich compose body (`mail/wysiwyg.rs`), or
   arbitrary-position canvas text (`slides`). Generalize `BlockEditor` /
   `text_input` so these stop forking full editors.

4. **Formula-aware single-line cell editor.** `sheets/grid.rs:48`
   `EditState{chars,caret,sel}` + point-mode is a second full editor; the formula
   bar is a third view. A `TextInput` variant with cell-reference point-mode.

5. **Masked / secret `TextInput` mode. — DONE.** `TextInput::with_secret(true)`
   renders the value as `•` bullets while the model keeps the real text; the
   plaintext is kept out of the a11y tree (`protected`, `None` value) and
   `text_review`, and copy/cut/caret still operate on the real bytes. A reveal
   toggle (`with_reveal`/`set_reveal`/`toggle_reveal`) shows the glyphs on
   screen without ever leaking to a11y. **Adopter:** imager's password field
   (`app.rs`) — the `Field::Password` char-splice and the manual bullet-mask +
   fake caret in `draw_field` are gone; it now uses a secret `TextInput` whose
   value mirrors `profile.user.password`. Remaining fork to migrate: db_client's
   connect-modal password (unmasked today).

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
Migrate app-drawn confirm/prompt/sheet overlays to `ModalDialog`. **Done:**
monitor, containers (confirm + prompt), vpn (confirm), software_store (ref
sheet), music (name prompt); feeds landed earlier. The widget was extended
additively — `with_destructive` (danger primary) and a shared `PromptBody`
text-entry body. **Remaining:** imager (config info sheet), notes (shortcuts
overlay); **deferred to other waves:** db_client +
snippets (text-input wave: dynamic/secret fields, per-variable fields), disks
(needs a gated-primary capability), video_editor (controls wave — segmented
controls inside), git_client (arm-twice → dialog is a behaviour change), sheets
(scrim-only behind the shared palette). The multi-field forms left hand-rolled
for now: `containers` create-container, `vpn` add-tunnel.

### Wave 4 — Buttons sweep (MED, broadest single win)
Paint painted-rect + hit-test tables through the immediate-mode
`ui::widget::Button::paint(&ButtonFace)` face (added this wave), or mount the
retained `Button`/`Toolbar` where an app already has a widget tree. **Done:**
api_tester, snippets, db_client, video-player, monitor, diffmerge (see the
Category 2 status note). **Remaining (still hand-rolled):** git_client, logs,
ebook, tube, music, camera, video_editor, sheets, typst_editor, disks,
containers, firewall, vpn, remote, backups, imager, software_store, calculator,
slides; plus leftovers in word-processor, pdf_viewer, notes. The ghost-styled
ones (git_client, notes, logs) want a `Ghost`/tinted `ButtonFace` variant first.
Do the large hit-table apps (ebook, music, camera, slides, software_store)
last — they are `L` because the whole draw layer is the table.

### Wave 5a — Controls with an existing widget (MED)
The toolkit gained immediate-mode paint faces `Toggle::paint(&ToggleFace)` and
`ToggleGroup::paint(&SegmentedFace)` (Solid / Pill / Underline styles, optional
left-aligned labels) so canvas apps adopt the shared switch + segmented control
without a tree rewrite.
- **Dropdowns:** api_tester (method/env), logs (unit/boot), ebook (font/sort),
  remote (kind/auth), db_client (engine). *(not started)*
- **Toggles:** firewall ✅, backups ✅ **done**; remaining: camera (over-media),
  vpn (semantic green), remote (icon toggle-buttons).
- **Tabs/segmented:** disks ✅, backups ✅, diffmerge ✅, software_store ✅,
  pdf_viewer ✅, firewall ✅ **done**; remaining: vpn (raised-subtle active),
  code_editor (closeable file tabs), sheets (per-tab colour + rename), camera
  (over-media), video_editor (in export modal), logs (per-priority cumulative
  chips), imager (stepper breadcrumb), word-processor / hex_editor (independent
  toggle-buttons / cycle). See Category 4 status for why each remainder isn't a
  clean drop-in.
- **Menus/popovers:** pdf_viewer (`ContextMenu`), ebook/music/hex_editor
  (`Popover`), typst_editor (overflow), browser (address overlay), slides (fill).
  *(not started)*

### Wave 5b — Controls blocked on a ui gap (MED)
Extend then migrate: gap **7** (non-CRDT Toggle/Slider binding) → tube Switch,
music/tube/video_editor sliders; gap **6** (typeable ComboBox) → containers;
gap **8** (Slider segment bands) → tube SeekBar; gap **9** (NumberInput
drag-step) → ebook.

### Wave 6 — Theme-color reads (LOW)
**Largely a no-op once triaged** (see Category 5 status): the app chrome is
already theme-driven everywhere checked (proven by light+dark shots). The only
genuine "frozen to one theme's token value" chrome offenders were **diffmerge**
(`merge_view.rs` resolution chips → `semantic.*`/`text_dim`) and **sheets**
(`grid.rs` selection wash → `accent[5]`) — both **DONE**. The high-count apps
(word-processor paper, ebook reader themes, tube over-media/brand) are document /
over-media / on-accent / color-math content that must *not* track the OS theme;
leave them. A future `Theme` shadow/scrim token could fold in the
`from_rgba8(0,0,0,α)` cluster, but a dark scrim is conventional in both modes so
it is not a violation. Leave color-math helpers, picker seeds, syntax/categorical
palettes, and test-only literals alone.

### Wave 7 — Larger widget builds, then adopt (MED/HIGH)
The multi-day widget work: gap **2** (code editor) **built** as `ui::CodeEditor`
and adopted by **typst_editor** (its editor + lexer deleted); remaining adopters
**code_editor** and **db_client** SQL. Gap **3** (rich-text document editor) →
word-processor, mail, slides; gap
**4** (formula cell editor) → sheets; gap **12** (Chart/Gauge + virtualized
DataTable) → monitor, logs; gap **10** (TreeView inline-rename) → notes; gap
**13** (MathField) → calculator; gap **11** (shared find bar) collapses the
Wave-1 editor find fields into one primitive.

---

*Grade every migration on `shots`, not a green build (`ui/HIG.md` §7). When a
migration reveals a missing capability, extend the widget — never re-fork it.*
