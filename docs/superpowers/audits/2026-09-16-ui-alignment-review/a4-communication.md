# UI alignment review — A4 Communication (feeds, chat, mail, totp)

Branch `ui-review/a4-communication`, cut from `codex/macos-shell` @ `087d5a62c` (see
Limits for the base correction). Worker A4 of the 18-way review; scope is exactly
`apps/feeds/`, `apps/chat/`, `apps/mail/`, `apps/totp/`.

## Method

1. Read `ui/HIG.md` §1/§7 (+§7a and `ui/MANUAL_LAYOUT.md`, `ui/FORM_COMPOSITION.md`
   from the live checkout, read-only), `docs/hand-rolled-ui-audit.md`, the prior
   evidence in `docs/superpowers/audits/2026-09-15-all-app-ui-review/{review-1,review-2,
   review-3,corrected-review-1,closure-register}.md` and plan
   `2026-09-15-app-composition.md` tasks 5/7/8.
2. Source pass over every hand-painted layout file: private spacing consts, literals,
   text centred on font size, fixed row bands, hit rect vs painted rect.
3. Visual pass: BEFORE captures for every app — `--responsive-matrix` dark + light,
   `--text-scale 2` at 800×900, 480×900, 800×700, `--pseudo-locale`, and every listed
   scenario (mail's narrow scenarios additionally at their native viewports). Every PNG
   was opened with the Read tool.
4. Fixes use `Theme` tokens, measured text (`TextRenderer::measure*` with the paint
   style; vertical centring from the measured line box, never the font size), and one
   geometry description per surface consumed by paint, hit-testing and accessibility.
   Each fix has a unit regression on the pure geometry it introduced.
5. AFTER captures with the same matrix; compared side by side.

Capture roots (all under the worktree): `target/ui-review/<app>/{before,after}/`.
Helper scripts: `target/ui-review/cap.sh` (full matrix) and `target/ui-review/native.sh`
(scenario at its own default viewport).

## Conventions observed (shared across the four apps)

- Text is positioned by the top of its line box (`size × 1.25`); the correct centring is
  `cy − line_h / 2` with `line_h` **measured**. Chat and totp had `cy − 7`, `cy − 8`,
  `y + 34` style constants tuned for 1× that drift at any other scale.
- Spacing tokens `s1..s16` map cleanly onto most authored gaps (4/8/12/16/24/32); the
  off-scale values that remain (52 px sidebar header, 26 px reaction capsule, 38 px thread
  bar, 76/108 px totp rows, 760 px readable column) are now **named** constants with a
  comment saying why.
- Fixed pixel bands (`y0 + 12 / 32 / 52`) are the single biggest source of 200% overlap;
  the durable fix is a small measured-metrics struct computed once per paint and cached
  for the hit list (mail `ListRowMetrics`, totp `RowMetrics`, chat `RowM.header_h`).
- None of the four apps localises through fluent keys — `--pseudo-locale` produced no
  change in any of them. Recorded as open (F2/convention), not fixed here.
- The `ui` crate on this base already exposes `Form`/`Field`/`FormPage`/`Section`,
  `Actions`, `Manual`, `ContentCard`; mail's account sheet and totp's add panel still
  hand-roll labels, pills, toggles and buttons around shared `TextInput`s.
- **Open — needs F2:** the "centre a measured line box on `cy`" rule and the
  "row = max(measured lines + pads, 1× minimum)" rule are now written four times
  (chat `geometry::line_top`/`centered_band`, mail `ListRowMetrics::from_line_boxes`,
  feeds `article_row_layout`, totp `RowMetrics::measure`). `ui` has no such helper next
  to `TextRenderer::measure`; one shared `line_top(cy, line_h)` / measured-row helper
  would let all four apps drop their copies (raised by the code review of this diff).

---

## chat

**Scope:** `apps/chat/src/render_chat.rs` (sidebar, header, timeline, composer, thread),
`render_call.rs`, `paint.rs`, `app.rs` geometry. New pure module `apps/chat/src/geometry.rs`.

| id | surface | defect | viewport / theme | file:line (before) | before → after | status |
|---|---|---|---|---|---|---|
| C1 | sidebar header | Workspace title truncated to `side.width − 60` *before* the presence pill was measured, so "Acme Design channe…" ran under "3 people online" at every populated size (plan task 5). | 1200×800, 768×1024, 390×844; dark+light | `render_chat.rs:553-616` | `chat/before/people.png` → `chat/after/people.png` | **fixed** — pill measured first and reserved (dot + s1 + text + 2·s2 pads), title elided into the remainder, both on one measured centre line; stacks the pill under the title when < 72 px would remain (`geometry::sidebar_header_layout`, 4 tests) |
| C2 | composer | Caret / placeholder hugged the box's top edge while + / emoji / send were centred; attach button was a 28 px square beside 32 px buttons. | all populated shots, `starter.png`, `edit.png` | `render_chat.rs:2820-2910` | `chat/before/starter.png`, `edit.png` → `chat/after/starter.png`, `edit.png` | **fixed** — all three buttons `spacing.s8` tall on the box centre line; the `TextInput` band is sized from the measured draft (one line when empty) and centred (`geometry::centered_band`, 2 tests); multi-line drafts still fill the growing box |
| C3 | message header / reactions / thread bar | Header advanced a fixed 22 px, chips 26 px, bar 38 px — at 200% the body painted over the author line and the chip clipped its label (corrected-review-1 P1). | 800×900 @2× | `render_chat.rs:1966, 2200, 2277, 3759` | `chat/before/large-text.png` → `chat/after/large-text.png` | **fixed** — `header_row_h`, `reaction_chip_h`, `thread_bar_h` measured once and shared by `measure_row`, `RowM::height` and the painters; timestamp bottom-aligned to the name's line box (baselines meet at any scale) |
| C4 | channel / voice rows | 32 px / 24 px fixed rows with `y + 8` text: rows stacked over each other at 200%. | 800×900 @2× | `render_chat.rs:1034-1065, 1247-1304` | `chat/before/large-text.png` → `chat/after/large-text.png` | **fixed** — `channel_row_h` / `voice_row_h` measured (≥ 1× row), glyph, name, badge, live dot and bar centred on the row's measured centre; hit rect is the same row rect |
| C5 | header action pills | "📹 Call" / "👥 6" pills fixed 84/52 px with `cy − 8` labels. | 800×900 @2× | `render_chat.rs:1360-1415` | `chat/before/large-text.png` → `chat/after/large-text.png` | **fixed** — widths from measured labels + s3/s2 pads, labels centred on measured boxes |
| C6 | hamburger toggle | "☰" drawn at `x0 + 8, cy − 9` regardless of glyph size. | 480×900, @2× | `render_chat.rs:1502-1511` | `chat/before/narrow.png` → `chat/after/narrow.png` | **fixed** — centred by measurement |
| C7 | "(edited) · view history" | Fixed 18 px band under an edited body: at 200% the 29 px label ran into the reaction chips below (seen in the first AFTER capture). | 800×900 @2× | `render_chat.rs` (`edited_h`, footer hit rect) | `chat/before/large-text.png` → `chat/after/large-text.png` | **fixed** — `edited_row_h` measured (≥ 18 px), hit rect is the measured line box |
| C10 | thread composer | 48 px box vs 76 px main composer for the same control. | `thread-narrow.png` | `render_chat.rs:2945-2950` | — | open — authored; buttons now match (32 px) so it reads as a compact variant; unify heights if design wants one composer height |
| C11 | category header rows / identity bar | Category rows ("CHANNELS", "VOICE") are a fixed 24 px and the bottom "You · Available" bar has fixed offsets; both tight/overlapping at 200%. | 800×900 @2× | `render_chat.rs` `draw_sidebar_tree` (`hrow` 24 px), `draw_sidebar_identity_bar` | `chat/after/large-text.png` | open — same measured-row treatment as C4; not reached in this pass |
| C8 | expanded call controls | `overlay_button_width()` estimates `chars × 8 + 26` instead of measuring the label. | `call.png`, `share.png` | `render_call.rs:1525` | — | open — fine at 1×; needs measured widths for 200% (own file, not reached in this pass) |
| C9 | strings | Every label is a literal (`"Category"`, `"replies"`, …); `--pseudo-locale` is a no-op. | `pseudo.png` | throughout | — | open — convention (fluent keys), not an alignment defect |

Tests: `cargo test -p chat --lib geometry` → 9 passed (6 new sidebar-header/centring
tests + existing). `cargo check -p chat --tests` clean.

Code review (`/code-review medium`) on the diff surfaced two real points that were
fixed before commit: totp's pre-paint `RowMetrics::fallback` hard-coded a `code_top`
that disagreed with the measured rule by 4 px (now derived from the same formula), and
the chat composer cloned the whole draft `String` every frame (now borrows).

## feeds

**Scope:** `apps/feeds/src/app.rs` (`build_list_rows`, `draw_list`) — kept minimal and
local because `app.rs` carries uncommitted WIP in the user's checkout; the new geometry
lives in `apps/feeds/src/list_layout.rs`.

| id | surface | defect | viewport / theme | file:line (before) | before → after | status |
|---|---|---|---|---|---|---|
| F1 | article list rows | Title pinned to the row top, source·summary to the bottom: a one-line title sat ~30 px above its own meta line and visually closer to the *next* article (plan task 5 / review-1 P2). Star hit rect bottom-right, time top-right, marker at a fixed `y0 + 22`. | 1200×800 dark+light, 480×900, 800×700, 390×844 | `app.rs:2016-2021, 3206-3293` | `feeds/before/detail.png` → `feeds/after/detail.png` | **fixed** — title + meta measured and laid out as one block (gap `s1`) centred in the row; time and unread marker on the title's first line; star square (`s8`) vertically centred and shared by paint, pointer and a11y (`list_layout::{article_row_layout, star_rect}`, 5 tests) |
| F2 | article list @200% | Second title line paints over the meta line; sidebar rows overlap; phone header count collides with the toolbar. | 800×900 @2×, 390×844 | `app.rs:78 (LIST_ROW_H), 77 (SIDE_ROW_H), 3100-3125` | `feeds/before/large-text.png`, `dark/dev.qos.feeds-phone.png` | open — the row height itself is still the fixed 88 px; my layout clamps the block to `pad_min` so 200% is no worse than before. The scale-aware `list_row_h` / sidebar rows / bounded header are in the user's uncommitted `app.rs` WIP (observed read-only); duplicating them here would conflict |
| F3 | sidebar rows | `baseline = cy − size × 0.62` — happens to equal the measured half line box at 1×. | — | `app.rs:2999` | — | open (cosmetic) — replace with measured height when the WIP lands |
| F4 | strings | literals; `--pseudo-locale` no-op | `pseudo.png` | — | — | open — convention |

Tests: `cargo test -p feeds --lib list_layout` → 5 passed. `cargo check -p feeds --tests` clean.

## mail

**Scope:** `apps/mail/src/pane.rs` (folder rail, list rows, reader card header).

| id | surface | defect | viewport / theme | file:line (before) | before → after | status |
|---|---|---|---|---|---|---|
| M1 | reader card header | Date right-aligned but the "to …" line left-aligned from a fixed `card_x1 − 150` column, so the right column had two ragged edges; name/email block sat ~4 px below the avatar's centre; sender budget ignored the real date width. | 1200×800, 1920×1080, 1366×768 dark+light | `pane.rs:1803-1835` | `mail/before/wide.png` → `mail/after/wide.png` | **fixed** — `card_header_layout`: both right-column lines share the card's right edge, name/email centred on the avatar, sender budget = start of the right column − s3, header grows with measured text instead of overlapping the body (1 test) |
| M2 | folder rail rows | Folder title drawn with `Some(120.0)` max width → wraps into the next row instead of eliding; label at `(36 − 14)/2` (font size, not line box). | latent (English titles fit) | `pane.rs:673-699` | `mail/before/wide.png` → `mail/after/wide.png` | **fixed** — badge measured first, title elided into the remaining width, label and badge text centred on measured line boxes |
| M3 | message list rows | Fixed bands (`y0 + 12 / 32 / 52`, 78 px row): at 200% sender, subject and snippet overlap (review-2 P1). **Star hit rect at `y0 + 8..32` (under the date) while the star is painted at `y0 + 48..64`** — pointer toggled star from the date and selected the row from the star. | 800×900 @2×; hit≠paint at every size | `pane.rs:456-460, 1116-1161, 1186-1252` | `mail/before/large-text.png` → `mail/after/large-text.png` | **fixed** — `ListRowMetrics` measured per paint (before `rebuild_hits`), 1× row stays 79 px (78.875 rounded to a whole pixel); star hit is a 24 px target centred on the painted glyph; date bottom-aligned to the sender line; thread badge and icons centred on their lines (1 test) |
| M4 | compact inbox header | Near-empty 60 px hamburger strip above the "Inbox · Search · refresh" strip (plan task 8). | 480×900, 768×1024, 390×844 | `app.rs` NavShell hosting | `mail/before/narrow.png` | open — needs **F1/F6** (`NavShell` overview-header ownership option); app-side change waits on it |
| M5 | account sheet / compose | Hand-rolled labels, sign-in pills, TLS toggles, footer buttons around shared `TextInput`s; label at `y0 + 9` for 12.5 px text. Alignment itself is clean at 1×. | `setup*.png`, `compose*.png` | `app.rs:1951-2200`, `compose.rs:811-967` | — | open — needs **F3** guidance/host: migrate to `Form`/`Section`/`Field` + `Actions` (present on this base) — a retained subtree inside `MailApp`'s overlay with the existing tab order/a11y is a sizeable change, not done in this pass |
| M6 | strings | literals; `--pseudo-locale` no-op | `pseudo.png` | — | — | open — convention |
| M7 | list header / avatars @200% | The 32 px search box with its hand-drawn 13 px "Search" placeholder and the refresh button are fixed geometry; avatar initials outgrow the fixed 17 px disc (`draw.rs::draw_avatar` scales the text, not the disc). | 800×900 @2× | `pane.rs:347-366, 877-886`, `draw.rs:128` | `mail/after/large-text.png` | open — search should be the shared `SearchField`/`TextInput` with its own placeholder; avatar radius should follow the measured initials (shared `Avatar` concern, needs **F2**) |

Tests: `cargo test -p mail --lib pane::tests` → 18 passed (2 new geometry tests).
`cargo check -p mail --tests` clean (one pre-existing `dead_code` warning in
`engine.rs`, untouched).

## totp

**Scope:** `apps/totp/src/{app.rs,list.rs,add_panel.rs}`.

| id | surface | defect | viewport / theme | file:line (before) | before → after | status |
|---|---|---|---|---|---|---|
| T1 | header | "5 accounts" caption drawn at `y0 + 34` — ~6 px below the centre line of the Add / Edit buttons (`y0 + 18..58`). | all | `app.rs:896` | `totp/before/dark/dev.qos.totp-desktop.png` → `totp/after/dark/dev.qos.totp-desktop.png` | **fixed** — caption centred on the action row from its measured line box |
| T2 | readable width | Header actions, search and list spanned the whole window: at 1920 px identity and code were ~1400 px apart (plan task 7). | 1920×1080 dark+light | `app.rs:1105-1164` | `totp/before/dark/dev.qos.totp-desktop.png` → `totp/after/dark/dev.qos.totp-desktop.png` | **fixed** — one centred column ≤ `READABLE_CONTENT_W` (760, a named app composition choice) shared by actions, search, banner and list; below the cap the column is the window (2 tests, incl. the reflow test updated to the new contract) |
| T3 | header buttons @200% | Fixed 168/94 px buttons: "Edit selecte" ran under "+ Add account". | 800×900 @2× | `app.rs:48-51, 1111-1132` | `totp/before/large-text.png` → `totp/after/large-text.png` | **fixed** — `layout_header_buttons` sizes from the measured labels (cached from the draw pass; authored widths before first paint), used by both `apply_layout` and `draw_header` so hit == paint |
| T4 | account rows @200% | Fixed 76/108 px rows and `center ± 20 / + 4` bands: issuer/label collided, 60 px code ran into the ring, the ring's seconds label spilled past a fixed 22 px ring. | 800×900 @2× | `list.rs:36-46, 201-224, 408-470` | `totp/before/large-text.png` → `totp/after/large-text.png` | **fixed** — `RowMetrics` measured per paint (issuer/label/code line boxes, ring radius scaled with `theme.text_scale`, copy zone follows the measured code); a row stacks when the measured identity + code + ring no longer fit, not only below 480 px; hit-testing reads the same metrics; 1× geometry unchanged (3 tests) |
| T5 | placeholders | Search and add-panel fields painted their own 16 px placeholder over the shared `TextInput`. | `add.png`, all | `app.rs:995-1008`, `add_panel.rs:967-978, 1404-1421` | `totp/before/add.png` → `totp/after/add.png` | **fixed** — `TextInput::with_placeholder` on every field; the hand-rolled painter is gone, so hint and typed text share one font, gutter and centre |
| T6 | add panel | Hand-rolled labels/divider/method cards/buttons around shared inputs; alignment clean at 1× (label 1 px above its field, 14 px between fields). | `add*.png` | `add_panel.rs:575-716` | — | open — needs **F3**: migrate to `Form`/`Field` + `Actions` |
| T7 | strings | literals; `--pseudo-locale` no-op (the search placeholder now does pseudo-localise, because the shared `TextInput` routes it through `i18n!`) | `pseudo.png` | — | — | open — convention |
| T8 | avatars @200% | Initials outgrow the fixed 24 px avatar disc (shared `Avatar::draw_at` scales the monogram, the app fixes the radius). | 800×900 @2× | `list.rs` `AVATAR_R`, `ui` `Avatar` | `totp/after/large-text.png` | open — needs **F2** (avatar radius from measured initials) |

Tests: `cargo test -p totp --lib` → see "Tests run". `cargo check -p totp --tests` clean.

---

## Tests run

| command | result |
|---|---|
| `cargo check --locked --no-default-features -p chat --tests` | ok |
| `cargo check --locked --no-default-features -p feeds -p mail -p totp --tests` | ok (1 pre-existing `dead_code` warning: `mail::engine::report_engine_error`) |
| `cargo test --locked --no-default-features -p chat --lib geometry` | 9 passed, 0 failed |
| `cargo test --locked --no-default-features -p feeds --lib list_layout` | 5 passed, 0 failed |
| `cargo test --locked --no-default-features -p mail --lib pane::tests` | 18 passed, 0 failed |
| `cargo test --locked --no-default-features -p totp --lib` | 103 passed, 0 failed |

Pre-existing failures: none observed in the scoped runs.

## Limits

- **Base correction.** The worktree was first cut from `origin/main` (`24ee08c66`) by
  mistake; the coordinator had it rebased onto `codex/macos-shell` (`087d5a62c`) before
  any app edit. All BEFORE captures and findings above were re-taken and re-verified on
  the corrected base; the wrong-base captures were discarded. Note that `codex/macos-shell`
  carries a newer `ui/` but *older* chat/mail sources than main (no text-scale plumbing,
  no large-text mail reader), which is why C3/C4/M3 exist here.
- The build needed `git cherry-pick ui-review/lockfix` (Cargo.lock `libc` for browser).
  Because the worktree sits under `/Users/xaver/dev/qos/`, cargo merges that checkout's
  `.cargo/config.toml`, which appends `-C target-cpu=generic` for aarch64 macOS and makes
  `ring` 0.17's static feature assertion fail. No config was edited (a temporary local
  copy of the macOS block was reverted before commit); every cargo command in this report
  ran as `RUSTFLAGS="-C force-frame-pointers=yes --remap-path-prefix=/build=."
  RUSTC_WRAPPER=sccache SCCACHE_CACHE_SIZE=200G CARGO_BUILD_JOBS=1 cargo …`, which replaces
  the merged rustflags with the repo's exact macOS set. All unit tests listed under
  "Tests run" were executed, not only type-checked.
- `--pseudo-locale` cannot exercise these apps (no fluent keys), so long-string behaviour
  in list rows and forms was judged from the `long-titles` scenario and 200% text only.
- `--headless-drive` target dumps were not used: none of the four apps exposes
  `test_id`s for the rows in question; the numeric hit == paint proofs are the unit tests
  on the shared geometry structs instead.
- Feeds' scale-aware row height, sidebar rows and bounded header are deliberately left to
  the user's in-flight `apps/feeds/src/app.rs` WIP; only the intra-row grouping was changed.
