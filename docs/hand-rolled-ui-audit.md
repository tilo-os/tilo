# Hand-rolled UI primitives audit

**What this is.** An evidence-based sweep of every qOS app that hand-rolls an
interactive UI primitive instead of using the shared `ui` toolkit widget. These
are the things that MUST be unified: a painted rect with a manual hit-test, or a
`String` field you `push`/`pop` on raw key events, silently drops text selection,
clipboard (Ctrl+C/V/X), keyboard activation and caret nav, the focus ring, the
a11y tree, and `Theme`-driven colors. The rule is codified in `ui/HIG.md` §8.

**Scope.** All UI apps under `apps/`. This is now a living migration ledger,
not the original read-only snapshot. A row is marked done only after the app
uses the shared primitive through its real event path; deliberate app-owned
surfaces keep their contextual caveat instead of being counted as failures.

**Risk key.** `HIGH` = breaks clipboard / selection / IME / a11y (every text
input), `MED` = misses hover/press/focus/keyboard/a11y-role/theme (buttons,
controls, modals), `LOW` = pure color / cosmetic. **Effort.** `S` < ½ day,
`M` ~1–2 days, `L` multi-day (usually because a `ui` widget must be extended
first — see [ui gaps](#ui-gaps)).

---

## Executive summary

| Category | Canonical widget | Current high-value remainder | Important caveat |
|---|---|---|---|
| 1. Text input (HIGH) | `TextInput` / `SearchField` / `NumberInput` / `EditableLabel` | mail rich text; Sheets cells; transformed slide-canvas text | these are domain-aware editors, not plain-input swaps |
| 2. Buttons (MED) | `Button` / `Toolbar` | exact create/restore footer faces; several retained-tree conversions | progress, ghost, keypad, canvas, and media surfaces are not ordinary buttons |
| 3. Modals (MED) | `ModalDialog` | dynamic multi-field and export bodies | use shared framing without flattening app-specific validation/content |
| 4. Other controls (MED) | `Dropdown`/`Toggle`/`Slider`/`ToggleGroup`/`ContextMenu`/`Popover`/`Chart` | targeted app-specific rows below | preserve semantic controls whose behavior exceeds the generic primitive |
| 5. Hard-coded colors (LOW) | `Theme` tokens | genuine chrome literals only | document, media-overlay, on-accent, and color-content literals are often intentional |

**Structural finding.** The straightforward text fields are mostly migrated.
The remaining debt is increasingly a domain editor (mail, slides, sheets) or
immediate-mode app chrome. The next useful work is therefore smaller and more
contextual: reuse the editor core while retaining app policy, and extract real
components that own state, drawing, hit-testing, and tests. Raw occurrence
counts were removed because they had become stale and encouraged mechanical
conversions.

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
field every keystroke (O(n²) tombstone churn — see `ui/HIG.md` §6).

**Status (search-field wave):** **feeds** and **weather** were already migrated
to shared `TextInput` / `Autocomplete` widgets (their audit entries were stale).
**hex_editor** now embeds shared
local `TextInput`s in its search and goto overlays; the app still owns byte
pattern parsing, hex/text mode, match navigation/counter and hex-or-decimal
offset parsing, while the fields gain caret navigation, selection, clipboard,
context menu and IME. **logs** now embeds a CRDT-bound shared `TextInput` in its
existing filter-bar chrome. Its collaborative `search` text remains the source
of truth and type-to-filter still transfers focus into the field; the copied
append/pop editor and its document helpers were deleted. **monitor** now
composes its responsive process-filter toolbar around a shared local
`TextInput`; process matching, `/` focus, and host-owned global-search chords
remain app policy, while selection, clipboard, caret navigation, IME and a11y
come from the toolkit. **snippets** now embeds the same local editor core in
both its search box and each dynamic expansion-variable row. Its compact
rectangular chrome, search grammar, clear button, live preview and
Tab/Enter/Escape modal flow remain app policy; the transient values still
create no CRDT changes, while selection, clipboard, Unicode-safe editing,
context menus and IME come from the toolkit.
**db_client** now binds the shared `CodeEditor` directly to its Automerge SQL
`Text` with `Language::Sql`; query execution, snippets, result/status coupling,
and global run shortcuts remain app policy, while selection, clipboard, undo,
IME, shaped hit-testing, scrolling, gutter and a11y come from the toolkit. Its
hand-rolled editor, approximate column geometry, SQL lexer, and editor-only
document splice helpers were deleted. The built-in SQL highlighter was extended
generically with SQL functions/types, operators and doubled-quote escaping so
the adoption does not regress highlighting quality.

**Current wave:** **maps** now uses a dedicated shared-input search component
plus shared route-endpoint inputs; **word-processor** has a shared-input find/
replace component; **sheets** uses `TextInput`/`EditableLabel` for its name box
and tab rename; **ebook** uses shared number inputs and multiline inputs for
both standalone notes and highlight comments; **pdf_viewer** uses shared
inputs for find and annotation text; **disks**, **vpn**, **remote**, and all six
**imager** customization fields are migrated. Imager's destructive confirmation
is now a peer-local shared input and no longer replicates consent. **db_client**'s
seven connection fields are shared inputs and its password is protected secret
input. **chat** composers, sent-message editing, and structural rename now use
shared inputs. The composers retain their distinct per-keystroke CRDT draft
binding; message edits and renames stay local until Save/Enter so peers never
observe an incomplete revision. The rows below record the narrowed boundaries.
**containers** now binds all six create-form fields to shared inputs without
flattening its image suggestions, field order, validation, or Podman policy.
**slides** now binds speaker notes directly to a shared multiline editor while
leaving transformed on-canvas text to its geometry-aware editing path.

### Pure non-adoption — a fitting widget already exists (do these first)

| file:symbol | app | evidence | ui alt | effort | risk |
|---|---|---|---|---|---|
| `apps/feeds/src/app.rs` add/rename modal | feeds ✅ | **DONE (prior)** — shared `ModalDialog` body hosts a real `TextInput` | `TextInput` | M | HIGH |
| `apps/maps/src/search_box.rs` search + `app.rs` route endpoints | maps ✅ | **DONE** — extracted `SearchBox` and two shared endpoint `TextInput`s; ranking/network/recenter policy remains in the app | `SearchField`/`TextInput` | M | HIGH |
| `apps/weather/src/core.rs` city search | weather ✅ | **DONE (prior)** — shared `Autocomplete`, including selection/clipboard/IME and async geocode suggestions | `Autocomplete` | S | HIGH |
| `apps/logs/src/app.rs` search box | logs ✅ | **DONE** — embedded CRDT-bound `TextInput`; shared editor with collaborative live-filter state | `TextInput` | M | HIGH |
| `apps/monitor/src/app.rs` process filter | monitor ✅ | **DONE** — shared local `TextInput` embedded in the responsive toolbar | `TextInput` | M | HIGH |
| `apps/snippets/src/app.rs` search + `expand.rs` var fields | snippets ✅ | **DONE** — shared local `TextInput`s embedded in the existing search and dynamic modal chrome; view state remains outside the CRDT | `TextInput` | M | HIGH |
| `apps/hex_editor/src/view.rs` search + goto | hex_editor ✅ | **DONE** — two embedded local `TextInput`s; byte-search and mixed-radix goto policy preserved | `TextInput` | S | HIGH |
| `apps/word-processor/src/find_bar.rs` | word-processor ✅ | **DONE** — component owns shared query/replacement inputs, layout, hits, and effects | `SearchField` | M | HIGH |
| `apps/sheets/src/app.rs` name box + tab rename | sheets ✅ | **DONE** — shared `TextInput` and `EditableLabel`; grid/formula editing remains a separate domain editor | `TextInput`/`EditableLabel` | M | HIGH |
| `apps/music/src/app/render.rs` playlist prompt | music ✅ | **DONE** — `ModalDialog<PromptBody>` with a real `TextInput` | `TextInput` | M | HIGH |
| `apps/ebook/src/input_controls.rs`, `comment_overlay.rs` | ebook ✅ | **DONE** — number fields plus both note surfaces use shared inputs; annotation persistence remains app policy | `NumberInput`/`TextInput` | S/M | HIGH |
| `apps/pdf_viewer/src/app.rs` annotation editors + find | pdf_viewer ✅ | **DONE** — shared inputs; PDF-specific find shell, toggles, signature pad, and match navigation remain app-owned | `TextInput`/`SearchField` | L | HIGH |
| `apps/disks/src/app.rs` format confirmation | disks ✅ | **DONE** — peer-local shared `TextInput` with exact-name gate | `TextInput` | S | HIGH |
| `apps/vpn/src/app.rs` add form | vpn ✅ | **DONE** — shared Name/Endpoint inputs with IME, selection, clipboard, and focus cycling | `TextInput` | M | HIGH |
| `apps/remote/src/app.rs` search + connection form | remote ✅ | **DONE** — six shared inputs; connection and validation policy stays local | `TextInput`/`NumberInput`/`SearchField` | L | HIGH |
| `apps/store/src/app.rs` search input | store | **DONE** — shared `TextInput`, bound to the store search document | `TextInput` | M | HIGH |
| `apps/chat/src/events.rs` inline message edit + rename | chat ✅ | **DONE** — shared local single/multiline `TextInput`s own caret, selection, clipboard, pointer placement, and IME; Chat retains ownership checks and explicit CRDT commit policy | `TextInput` | M | HIGH |
| `apps/containers/src/panels.rs` prompts + six-field create form | containers ✅ | **DONE** — shared `PromptBody` plus six CRDT-bound `TextInput`s; image suggestions, validation, form order, and Podman assembly remain app policy | `TextInput` | M | HIGH |

### Forced by a missing widget capability (extend `ui` first — see [ui gaps](#ui-gaps))

| file:symbol | app | evidence | blocked on | effort | risk |
|---|---|---|---|---|---|
| `apps/db_client/src/connect_modal.rs` `ConnectModal` | db_client ✅ | **DONE** — seven shared local inputs; password uses protected secret mode; dynamic engine field set and validation remain local | `TextInput` secret mode | M | HIGH |
| `apps/imager/src/customize.rs`, `app.rs` confirmation | imager ✅ | **DONE** — all six customization fields plus peer-local destructive confirmation use shared inputs; password is protected | `TextInput` secret mode | L | HIGH |
| `apps/mail/src/compose.rs` `plain_body`, `wysiwyg.rs:740` | mail | **plain body DONE** — the hand-rolled multiline `String` is now a multiline `TextInput`. `Wysiwyg` (HTML mode) is still a forked rich editor (own `caret`, insert/backspace/split/select) | multiline `TextInput` (gap 1) done; rich editor = gap 3 | M/L | HIGH |
| `apps/chat/src/app.rs` composers | chat ✅ | **DONE** — shared CRDT-aware multiline inputs; their live collaborative-draft semantics intentionally remain separate from local-until-save edit/rename inputs | multiline `TextInput` | L | HIGH |
| `apps/slides/src/app.rs` speaker notes + canvas text | slides ✅ (notes) | title rename and directly-bound multiline speaker notes are **DONE**; transformed on-canvas text remains a geometry-aware domain editor | multiline input / canvas editor | M/L | HIGH |
| `apps/sheets/src/grid.rs:48` `EditState{chars,caret,sel}` | sheets | cell/formula body editor fully reimplements caret+selection; formula bar is a 2nd view | formula-aware single-line editor | L | HIGH |
| `apps/code_editor/src/editor_view.rs` (1999 LOC) | code_editor | full multiline editor (caret/selection/completion). **Palette DONE** — the app-specific painter was replaced by a thin typed adapter over `ui::CommandPalette`, including shared IME/selection/clipboard and direct go-to-line arguments | `ui::CodeEditor` (gap 2, **built**) | L | HIGH |
| ~~`apps/db_client/src/editor.rs` SQL editor~~ | db_client ✅ | **DONE** — CRDT-bound `ui::CodeEditor` with `Language::Sql`; app-local editor + lexer deleted, while query/snippet/results policy stays in the app | `ui::CodeEditor` (gap 2, **built**) | L | HIGH |
| ~~`apps/typst_editor/src/editor.rs` (1198 LOC)~~ typst_editor | typst_editor ✅ | **DONE** — migrated onto `ui::CodeEditor` (`Language::Typst`); hand-rolled caret/anchor/selection/gutter/highlight + the per-line lexer deleted (~1100 LOC), thin shell keeps only the diagnostics overlay | `ui::CodeEditor` (gap 2, **built**) | L | HIGH |
| `apps/calculator/src/model.rs:467` `expr`, `mathfield/mod.rs` | calculator | expression `String` spliced by keypad; `mathfield` is a legit **2D structured** editor | shared `MathField` (mathfield is CRDT-clean already) | M | MED |

**Legitimately app-owned text surfaces (not flagged):** the terminal PTY grid
(`terminal/src/main_body.rs`), the hex byte-grid cursor. These *are* the app; but
even they must route **side** fields through shared widgets (terminal currently
has none; hex_editor's search/goto are flagged above).

---

## Category 2 — Buttons (MED risk)

The recurring anti-pattern is a painted rect (often collected into a private
`Vec<(Action, Rect)>` hit list) tested with `.contains()`. It misses some
combination of hover/press/focus, keyboard activation, a11y role, and shared
theming. The old zero-adoption list was removed because several apps now mix
shared ordinary buttons with legitimate custom surfaces. Representative
remainders follow.

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
(toolbar Open/Load/Next/Prev), **store** cards/actions, and ordinary **disks**/
**vpn** controls, plus the **backups** restore footer and **containers** create
footer. Keyboard/AT activation
and per-button a11y nodes
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
| `apps/store/src/app.rs` action controls | store ✅ | **DONE** — actions use retained `Button`; cards use shared button faces | `Button` | L | MED |
| `apps/slides/src/app.rs:271` `struct UiButton{rect,kind,hit}` | slides | every toolbar/inspector button painted + hit-listed | `Button`/`Toolbar` | L | MED |
| `apps/monitor/src/app.rs:1986` `draw_action_button` | monitor ✅ | kill/stop actions | `Button` | M | MED |
| `apps/db_client/src/connect_modal.rs:50` `cancel_btn`/`save_btn` | db_client ✅ | painted rects + manual hit-test | `Button` | M | MED |
| `apps/git_client/src/diffview.rs:354` `draw_hunk_button` | git_client | stage/unstage/discard hunk buttons | `Button` | M | MED |
| `apps/diffmerge/src/merge_view.rs:340` take-ours/theirs/both, `app.rs:293` toolbar | diffmerge ✅ (toolbar) | painted colored rects, hit-tested `:482` | `Button`/`Toolbar` | M | MED |
| `apps/calculator/src/keypad.rs:245` `key_rects` | calculator | painted keypad grid + manual hit-test | `Button` grid (+ keypad widget gap) | L | MED |
| `apps/sheets/src/app.rs:83` `MenuButton`, `:201` `fx_rect` | sheets | File/Format/Σ menu-bar + fx affordance | `Button`/`Toolbar` | M | MED |
| `apps/disks/src/app.rs` ordinary controls | disks ✅ | **DONE where semantically ordinary** — shared faces; partition/device surfaces remain custom | `Button` | M | MED |
| `apps/vpn/src/app.rs` ordinary controls, `apps/remote/src/app.rs` rail | vpn ✅, remote | VPN **DONE** with shared faces; Remote rail remains contextual | `Button` | M | MED |
| `apps/imager/src/app.rs:421` wizard back/primary | imager | painted rects hit-tested | `Button` | M | MED |
| `apps/api_tester/src/app.rs:808` `send_label` | api_tester ✅ | hand-painted Send button | `Button` | S | MED |
| `apps/logs/src/app.rs:873` `draw_back_button` | logs | narrow-mode nav back | `Button` | S | MED |
| `apps/word-processor/src/widget.rs:2463` `find_buttons`, `:5685` `recovery_buttons` | word-processor | find/replace + recovery-banner painted rects | `Button` | M | MED |
| `apps/snippets/src/app.rs:1545` `toolbar_button`, `expand.rs:249` copy/cancel | snippets ✅ | painted rect + hover-only hit-test | `Button` | S | MED |
| `apps/backups/src/app.rs` restore footer + `Hit::BackupNow` | backups ✅ | **DONE where ordinary** — Restore/Cancel use shared faces; Backup Now retains its progress sliver and custom compact glyph | `ButtonFace` | S | MED |
| `apps/notes/src/sidebar.rs:672` button chip | notes | one hand-drawn hover chip (otherwise a model app) | `Button` | S | MED |
| `apps/containers/src/panels.rs` create/confirm/prompt actions | containers ✅ | **DONE where ordinary** — Create/Cancel use shared faces; confirmation/prompt actions come from `ModalDialog` | `ButtonFace`/`ModalDialog` | M | MED |
| `apps/video-player/src/app.rs:375` `open_button_rect` | video-player ✅ | floating "Open" pill (only leftover) | `Button` | S | MED |

---

## Category 3 — Modals / dialogs (MED risk)

App-drawn confirm/prompt/sheet + scrim instead of `ModalDialog` — misses focus
capture, Esc, scrim semantics, a11y, theming.

**Status (this wave):** the confirm/prompt/sheet overlays in **monitor,
containers, vpn, store** are now routed through the shared
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
| `apps/store/src/app.rs` reference detail | store ✅ | **DONE** — retained shared controls replaced the retired store's scrim-only reference sheet |
| `apps/feeds/src/app.rs` (was `draw_input_modal`) | feeds | **DONE (prior)** — `ModalDialog<ModalBody>` with a real `TextInput` |
| `apps/maps/src/map_info_sheet.rs` attribution/licences | maps ✅ | **DONE** — extracted component owns state and event routing while shared `ModalDialog<Text>` supplies the focus trap, a11y dialog role, backdrop/Escape dismissal, and Close action |
| `apps/db_client/src/connect_modal.rs` | db_client | **DEFER framing only** — all fields and buttons are shared and the password is protected; the engine-dependent body/validation remains a custom modal surface |
| `apps/disks/src/app.rs:1275` `draw_modal` | disks | **DEFER** — a gated *type-the-device-name-to-confirm* dialog with a locked primary; needs a gated-primary `ModalDialog` capability first |
| `apps/music/src/app/render.rs` `draw_name_prompt` | music | **DONE** — create/rename playlist prompt → `ModalDialog<PromptBody>` (real `TextInput`, kills the hand-rolled `name_input` char-splice); `name_input` kept only as the submit value-carrier |
| `apps/video_editor/src/app.rs:2292` `draw_export_modal` | video_editor | **DEFER** — embeds interactive segmented controls (resolution/format/fps); belongs with the controls wave (5a), then wrap in `ModalDialog` |
| `apps/imager/src/export_dialog.rs` config preview | imager ✅ | **DONE** — extracted component preserves the responsive size and per-line syntax colours; shared `ModalDialog` owns framing, capture, focus trap, a11y role, and dismissal |
| `apps/snippets/src/expand.rs` `ExpandModal` | snippets | **DEFER** — multiple per-variable text fields; belongs with the text-input wave |
| `apps/git_client/src/app.rs:149` `armed_discard` | git_client | **DEFER** — an inline two-click "arm then confirm" affordance, not an overlay; converting to a dialog is a behaviour change, not a drop-in |
| `apps/sheets/src/app.rs:2582` own scrim | sheets | **LEAVE** — scrim-only behind the already-shared command palette; low value, and wrapping the palette would fight its own layout |
| `apps/notes/src/shortcuts_overlay.rs` | notes ✅ | **DONE** — the Notes-specific two-column shortcut body is preserved, while shared `ModalFrame` now owns the scrim, elevated card, title, visible close button and exact close/card/scrim hit geometry; the overlay also exposes modal a11y semantics and traps focus |

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
(Diff / 3-way merge switch), **store** (nav tabs, left-aligned + badge
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
| `apps/store/src/app.rs` nav/category controls | store ✅ | **DONE** — nav/category controls use `ToggleGroup` | `ToggleGroup` | M |
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

`Color::rgb*`/`from_rgb*`/`from_hex` in app chrome can break dark mode,
high-contrast, and OS accent. Raw per-app literal counts are intentionally no
longer tracked: they mixed production chrome with tests, document content,
syntax/category palettes, and over-media ink, producing a misleading target.

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
   `TextInput`. Chat's main/thread composers and Ebook's note/comment editors
   now use the same mode. Remaining straightforward adopter: Slides speaker
   notes; transformed canvas text is a separate geometry-aware problem.

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
   now for free. **Adopter:** `db_client` — its CRDT-bound SQL editor now uses
   `Language::Sql`; the app-local caret/buffer painter, approximate hit-testing,
   lexer, and editor-only splice helpers are deleted. Query execution, snippet
   loading, result/status interaction and Ctrl/Cmd+Enter remain host policy.
   **Remaining adopter to migrate:** `code_editor/editor_view.rs` (1999 LOC; its
   command palette already reuses `ui::CommandPalette`).

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
   value mirrors `profile.user.password`. `db_client` now uses the same
   protected mode for its connection password; no known unmasked plain-input
   password fork remains in this audit.

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

## Current remediation order

The broad mechanical waves are complete enough that a shorter, contextual
queue is safer than old app counts:

1. **Exact button faces:** audit the remaining immediate-mode apps for truly
   ordinary actions. Do not fold progress, ghost, keypad, canvas, or media
   controls into an ordinary button merely to lower a count.
2. **Component boundaries:** continue extracting cohesive surfaces from large
   app modules. Maps' information sheet, Imager's export dialog, and Word
   Processor's review overlay now own their state, layout/drawing, hit-testing,
   event routing, and focused tests while domain persistence stays in the host.
3. **Domain editors:** design formula-aware Sheets and transformed-canvas Slides
   adapters around the shared editor core. These are not plain `TextInput`
   migrations.

Theme literals are already triaged: diffmerge and Sheets' genuine chrome
offenders are fixed. Document colors, syntax/categorical palettes, over-media
ink, shadows/scrims, picker seeds, and test literals remain intentional.

---

*Grade every migration on `shots`, not a green build (`ui/HIG.md` §7). When a
migration reveals a missing capability, extend the widget — never re-fork it.*
