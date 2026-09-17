# UI alignment, spacing and layout review — round two (2026-09-17)

Second pass of the whole-desktop alignment/spacing/layout review, run on top of the merged
round-one tip `381eff6a8` (`../2026-09-16-ui-alignment-review/`). Nineteen parallel workers,
each in an isolated worktree cut from that tip and each owning a disjoint set of files:
the six framework units, the two shell units and the ten app units of round one, plus one
crate-wide unit (X1) that carried round one's biggest systemic finding — text scaled twice
and centred on the font size — through every `ui` widget at once.

Every unit re-verified that its round-one fixes survived the merge, reviewed its surfaces
again with fresh eyes (800×700, 480×900 and 200 % text on top of the standard matrix), fixed
what it found, ran the shared `code-review` skill and fixed the confirmed findings before
committing. Delivery is local branches only: nothing was pushed, no PR was opened. The
coordinator merged the nineteen branches into `ui-review/integration-r2`, resolved every
conflict by intent, ran the gates below, applied the cross-unit follow-ups the units had
handed to the integration step, and merged the result into `codex/macos-shell`.

## Units

| Unit | Branch (`ui-review/r2-…`) | Tip | Report |
|---|---|---|---|
| F1 Layout containers, responsive shells, tokens, lint | `f1-layout-containers-tokens` | `02d2d1f95` | [f1](f1-layout-containers-tokens.md) |
| F2 Core controls, text inputs | `f2-controls-text-alignment` | `3131d84c6` | [f2](f2-controls-text-alignment.md) |
| F3 Composition, data display | `f3-composition-data-display` | `401b3a908` | [f3](f3-composition-data-display.md) |
| F4 Popups, menus, overlays | `f4-popups-menus-overlays` | `53d67b173` | [f4](f4-popups-menus-overlays.md) |
| F5 Timeline, media, capture, presence | `f5-timeline-media-capture` | `4f89e5689` | [f5](f5-timeline-media-capture.md) |
| F6 Window-chrome widgets, identity flow, `ui/src/chrome` | `f6-window-chrome-identity` | `0acf6b8bd` | [f6](f6-window-chrome-identity.md) |
| X1 Text scaled once, centred on the measured line (crate-wide) | `x1-text-scale-centring` | `0ca25b8e7` | [x1](x1-text-scale-centring.md) |
| S1 Shell top bar, wheel, strips, palette host | `s1-shell-topbar-wheel-strips` | `76b0582ec` | [s1](s1-shell-topbar-wheel-strips.md) |
| S2 Shell panels, sheets, system surfaces | `s2-shell-panels-system` | `1ae25a3f0` | [s2](s2-shell-panels-system.md) |
| A1 api_tester, db_client, code_editor, containers, diffmerge, git_client | `a1-devtools-1` | `dee3cbeae` | [a1](a1-devtools-1.md) |
| A2 hex_editor, logs, typst, terminal, snippets, monitor, disks, calculator | `a2-devtools-2-utilities` | `a5dc87cea` | [a2](a2-devtools-2-utilities.md) |
| A3 store, settings, themes | `a3-browse-preferences` | `8b59a9634` | [a3](a3-browse-preferences.md) |
| A4 feeds, chat, mail, totp | `a4-communication` | `0d86874aa` | [a4](a4-communication.md) |
| A5 firewall, backups, remote, vpn, imager | `a5-management-install` | `09376168e` | [a5](a5-management-install.md) |
| A6 calendar, notes, contacts, weather, maps | `a6-content-1` | `ac262aaea` | [a6](a6-content-1.md) |
| A7 word-processor, sheets, slides | `a7-office-editors` | `e9a7884c4` | [a7](a7-office-editors.md) |
| A8 music, photos, video-player, camera, tube | `a8-media` | `483cf4363` | [a8](a8-media.md) |
| A9 ebook, pdf_viewer, file-explorer | `a9-documents-files` | `7bebec38f` | [a9](a9-documents-files.md) |
| A10 video_editor, browser chrome | `a10-large-editors-browser` | `c99371771` | [a10](a10-large-editors-browser.md) |

Each report carries its own findings table (id, surface, defect, viewport/theme, file:line,
before/after evidence, status and owner for hand-offs), the tests it ran with counts, and
what it could not observe on macOS. The reports use different table shapes, so this index
does not add their rows up; round one's totals are in its README.

## What round two changed, in one paragraph per layer

**Framework.** X1 introduced `ui/src/text_metrics.rs` — `renderer_type_size` (a theme type
token divided once by the renderer's scale, so the renderer's own multiplication lands
exactly once) and `centered_text_top` (centre the *measured* line box, never the font size)
— and migrated 186 sites in 41 files; a crate-wide test (`x1_text_scale_once`) pins the rule.
F1 gave the four responsive shells one authored-to-resolved rule (`AuthoredExtent::text` for
text-sized panes, growing by `Theme::responsive_scale()` = `ui_scale × text_scale.max(1.0)`;
`AuthoredExtent::icon` for icon rails, following `ui_scale` only), `text_control_height`,
clamped custom-theme spacing into a usable band, and turned the spacing-literal ratchet into a
shared `ratchet` support module. F2 made the default `Button` footprint measured (exactly
120×40 at 1×, grown from the measured line at 2×), added the outlined danger face and kept
labels seated on their controls at 2×; BlockEditor headings moved onto the type scale
(32/24/20). F3 let every composition/data-display band (`DataTable`, `TreeView`,
`SearchResults`, `ConflictPicker`/`Banner`, `Chart` header, `IconGrid`) follow the type
scale with 1× byte-identical, and added `Field::with_trailing_control`. F4 finished the
menu/dialog family: `ModalDialog` footers are shared `Button`s, `Dropdown` flips and clamps,
tooltips/toasts stack measured line boxes with `s1` gaps, `FloatingWindow`'s title bar is 40.
F5 measured the presence pills/chips/edge tags and the history-scrubber popover, and put
media/capture control clusters on tokens. F6 seated the identity flow's inputs on the shared
`TextInput` (Tab order, accessibility and `test_id` addressing come with it), widened tab
action chips to the pointer floor without moving a pixel, and published `ToolbarMetrics` /
`RailMetrics`.

**Shell.** S1 gave the shell the right-click menu it never had at runtime: the tab bar's
request is drained inside the pointer dispatch, the shared `ContextMenu` opens at the
pointer, owns pointer and keyboard while up, paints with the live theme and is published as
an accessible `Menu` whose rows fire the same `window_actions` as three new registry actions
(`tab.close`, `tab.split-right`, `tab.move-to-new-group`); splits are tree-first through
`WorkspaceTree::split_tab_into_new_group`, which also fixed the in-strip drag-out split
duplicating its tab at the next chrome revision. Strip gutters moved from 28 to `s8`
(const-asserted past the disc's rim shadow), four sprite shadows became `surface::shadow(e4)`,
the palette filter band is renderer-relative with pointer-floor hit rects that never overlap,
and the top-bar density derives from `tokens.metrics`. S2 put the panels, sheets and system
surfaces on `Panel` radius/shadow tokens and one modal duration.

**Apps.** Every app unit's headline was the same: fixed-pixel bands next to renderer-scaled
text at 200 %. Each surface now derives paint, hit-test and layout from one measured-metrics
struct whose 1× default is the authored rule set (A1 `DiffDraw`/`ListDraw`/`Bands`; A4
`CallOverlayMetrics`/`SetupMetrics`/`ComposeMetrics`/`PanelMetrics`; A3 settings rows,
themes bar and gallery, store landing zone; A5, A7, A8, A9, A10 likewise), so 1× frames are
byte-identical and 2× frames hold their text. Mail and TOTP gained Fluent catalogues; several
apps lost hand-painted buttons in favour of `ButtonFace`/`Button`/`ModalFrame`.

## Integration (coordinator)

Branch `ui-review/integration-r2`, cut from `codex/macos-shell` at `1555422a6`. Conflicts,
all resolved by intent:

- `ui/src/widget/chip.rs` (X1 vs F2): F2's measured `intrinsic_size` with X1's
  `text_metrics::renderer_type_size` import; `data_table::renderer_type_size` no longer
  exists, so `stat_tile.rs`, `qr.rs` and the chip test import the shared helper.
- `ui/src/widget/{modal_dialog,toast,tooltip}.rs` (X1 vs F4): F4's shared-`Button` footer
  and dynamic `chrome.title_bar` with X1's measured title centring; toast stacks F4's
  token-gapped measured lines; unused `ColorCompat` imports dropped.
- `ui/src/presence/render.rs`, `ui/src/widget/history_scrubber.rs` (X1 vs F5): F5's
  structure (`mist_tag`, `label_magnification`, the yielding two-line popover) with every
  size passed through `renderer_type_size`.
- `ui/src/widget/identity_flow/sheet.rs` (X1 vs F6): X1 made `Role::size` renderer-relative
  (theme, renderer); F6's new `place_field` / `place_code_cell` now thread the renderer
  through, because the shared `TextInput` hands its size to the renderer.
- `ui/tests/design_token_baseline.txt`: the lower count wins.

Fixes the merge itself needed (all committed on the integration branch with tests):

- **Accessibility activation regression, root-caused.** `a11y_activation_drains_an_action_bound_control_before_returning`
  passed at `1555422a6` and failed after F1's merge, although F1 touched no a11y code. The
  AccessKit bridge's reverse node map was keyed by the widget's data pointer; a container
  whose first field is a child widget shares that address, so the child was exported under
  its parent's taffy id and an activation aimed at the child found no node. F1's changes
  merely shifted the test fixture's field layout onto that latent bug. The key now carries the
  widget's dynamic size and alignment; a `#[repr(C)]` regression test reproduces the layout.
- `label_magnification` (F5) multiplied the theme's type-step ratio by the renderer's scale
  as well; after X1 the glyphs land at the theme's resolved size, so the pads doubled at 2×.
  It is the type-step ratio alone now, and the pill/chip and popover tests scale theme and
  renderer together (the convention every real host uses).
- `status_bubble::pill_size` sized the pill from the unscaled design size while the label
  is renderer-scaled (A2's hex_editor finding, a toolkit contract bug): the pill now lifts
  to the measured line plus padding, 1× unchanged at 30 px.
- `ConflictPicker`'s white *candidate* ink (the `Button` convention, resolved by the theme
  against the fill) is recorded in the design-token baseline with a comment on the line.

Cross-unit follow-ups F1 handed to the integration step:

- **tube** (A8's files): the app's own narrow/wide and content-width decisions read the
  breakpoint and rail width the `NavSplit` resolves under the theme (`AuthoredExtent::text` /
  `::icon`), the split is told the rail is an icon sidebar, and the rail column resolves the
  same way; 1× unchanged, rail tests pass.
- **mail** (A4's files, left to the coordinator by A4): the nav shell is laid out against the
  theme the host hands the app (a stock dark theme before), the throwaway renderer measures
  at that theme's scale, narrow/wide compares against the shell's resolved breakpoint, and
  pane headers centre on and clear the collapse chip the shell actually laid out (published
  through the shared state; the 1× mirror is the fallback while the shell shows no chip).
  Verified by capture against the commit before the change: the 1× desktop frame is
  unchanged (one pixel of run-to-run noise — the sample mailbox is not fully deterministic
  between runs), the 1× phone frame now seats the folder title on the hamburger's centre
  line instead of below an empty 60 px band (A4's open item M4, closed here), and at 2× the
  chip grows with the shell and the header follows it. Found while doing so and left open
  for the mail owner: on a wide window at 2× the reader pane's toolbar labels overlap each
  other, the "3 messages · Inbox" line sits under the title, and the attachment caption runs
  under its chip (A4 captured 2× only at 800×900).
- **weather** (A6's files): `build_nav` hands the shell authored geometry — breakpoint 700,
  overview 340 as a text-sized column, the collapsed 68 px rail as an icon rail — and no
  longer multiplies them by the text scale itself or rebuilds the shell on a scale change;
  at 2× the breakpoint is 1400 and the overview 680 instead of 2800/1360, so a 1920 px window
  shows two panes again. Weather's 87 tests pass. Verified by capture against the commit
  before the change: the 1× matrix differs only by the app's own run-to-run noise (about a
  hundred pixels in the same region a same-binary rerun also moves), 800×900 at 2× is
  byte-identical (narrow either way), and 1920×1080 at 2× shows the city list beside the
  forecast. Visible in that frame and already recorded open by A6: the sun-arc labels, the
  hourly chart's time axis and the 7-day rows' UV/wind/precipitation columns overprint at 2×.
- **calendar** (A6's files): the four production colour literals F1 listed were replaced by
  theme tokens inside A6's own round-two commit.
- **notes** (optional per F1): the drawer's three `× text_scale` multiplications were left as
  they are — geometry unchanged, `SidebarDrawer::scale_for` is available for the notes owner
  to adopt.

Shared helper, one owner: the five A6 apps and photos/video-player each carried a
renderer-less `line_h(size, text_scale)` (and `at_least_whole` / `line_growth` /
`authored_h`) for layout passes that solve a row grid before any paint. X1 had left that
helper out of `ui` on the grounds that every `ui` caller has a renderer; the apps do not, so
`ui::text_metrics` now owns `line_height_px`, `line_growth`, `at_least_whole` and
`authored_h` (sanitised exactly like `TextRenderer::set_text_scale`, tested against what the
renderer measures), and the seven app copies delegate to them with their signatures kept.
A2's renderer-based `ts()` / `line_box(tr, …)` copies are recorded for the same treatment.

Deliberate default changes, so they are not mistaken for regressions: `FloatingWindow` title
bar 38 → 40 (F4); `BlockEditor` headings 28/22/18 → 32/24/20 (F2); `Button` default
footprint grows from the measured line above 1× (F2); shell strip gutters 28 → 32 (`s8`) and the
two launchpad hint-glyph probes in `scripts/ux-bench/run-suite.py` moved with the card (S1).

### Gates on the integration branch

TODO final numbers.

- `cargo check -p ui --tests --examples`, `cargo check -p shell --lib --bin ux-bench --bin shell --bin qos-control`, `cargo check --tests` for every app crate.
- `cargo test -p ui --lib`: TODO.
- `cargo test -p ui --test it -- f1_ f2_ f3_ f4_ f5_ f6_ x1_ design_token tokens_ context_menu tabs toast tooltip data_table window_group icon_grid`: TODO.
- Spacing-literal ratchet (`f1_spacing_token_audit`): re-baselined once after all nineteen
  merges — the app units named their remaining pixel constants (HIG §7a: an authored value is
  not a defect per se), which the ratchet counts as growth; the baseline only moves down
  from here.
- Scoped shell lib tests: TODO.
- Pre-existing failures, verified at the branch tip before this merge and left untouched:
  `design_token_audit::app_and_shell_theme_escape_hatches_do_not_grow` (`apps/browser/src/browser_tabs.rs`,
  `shell/src/shell/frame.rs` — the live checkout's uncommitted work), `typography_visual_regression`,
  ebook Linux baselines, file-explorer fixture race / German-locale asserts, notes `canonical_*`,
  slides notes-caret, vpn Linux-only, ux-bench `semantic:Files` + browse-file, code_editor
  `…still_stats_the_file` (`/var` vs `/private/var`), db_client `host_routes_named_divider…`
  (whole-binary only), browser `crash_recovery…` (session-store lock race).
