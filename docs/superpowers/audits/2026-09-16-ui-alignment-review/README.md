# UI alignment, spacing and layout review — 2026-09-16/17

Batch review of the whole desktop for element alignment, spacing and layout correctness,
starting at the `ui` toolkit and working up through the shell chrome and all 47 windowed apps.
Eighteen parallel workers, each in an isolated worktree cut from `codex/macos-shell`, each
owning a disjoint set of files. Delivery is local branches only: nothing was pushed.

Every unit's report in this directory carries a findings table (id, surface, defect,
viewport/theme, file:line, before/after PNG paths, status fixed/open, owner for cross-unit
items), the exact tests run with counts, and the limits of what could be observed on macOS.
The 2026-09-15 all-app review (`../2026-09-15-all-app-ui-review/`) was reference evidence; its
open register items were fixed only where they were alignment/spacing defects inside a unit's files.

## Result

| Unit | Branch (all `ui-review/…`) | Tip | Findings | Fixed | Open | Report |
|---|---|---|---:|---:|---:|---|
| F1 Layout containers, tokens, lint | `f1-layout-containers-tokens` | `fe9a32e5e` | 26 | 14 | 12 | [f1](f1-layout-containers-tokens.md) |
| F2 Core controls, text inputs | `f2-controls-text-alignment` | `3f575a07c` | 17 | 13 | 4 | [f2](f2-controls-text-alignment.md) |
| F3 Composition, data display | `f3-composition-data-display` | `0c262f8ef` | 21 | 11 | 5 | [f3](f3-composition-data-display.md) |
| F4 Popups, menus, overlays | `f4-popups-menus-overlays` | `eec34e170` | 28 | 16 | 12 | [f4](f4-popups-menus-overlays.md) |
| F5 Timeline, media, capture | `f5-timeline-media-capture` | `ec20fa345` | 24 | 14 | 8 | [f5](f5-timeline-media-capture.md) |
| F6 Window-chrome widgets, identity flow | `f6-window-chrome-identity` | `6d817ae3b` | 33 | 25 | 5 | [f6](f6-window-chrome-identity.md) |
| S1 Shell top bar, wheel, palette host | `s1-shell-topbar-wheel-strips` | `1f32e3179` | 31 | 20 | 11 | [s1](s1-shell-topbar-wheel-strips.md) |
| S2 Shell panels, sheets, system | `s2-shell-panels-system` | `0dd427244` | 39 | 26 | 13 | [s2](s2-shell-panels-system.md) |
| A1 api_tester, db_client, code_editor, containers, diffmerge, git_client | `a1-devtools-1` | `20ec84aeb` | 39 | 30 | 9 | [a1](a1-devtools-1.md) |
| A2 hex_editor, logs, typst, terminal, snippets, monitor, disks, calculator | `a2-devtools-2-utilities` | `11709c917` | 26 | 12 | 10 | [a2](a2-devtools-2-utilities.md) |
| A3 store, settings, themes | `a3-browse-preferences` | `a985d03e5` | 27 | 18 | 9 | [a3](a3-browse-preferences.md) |
| A4 feeds, chat, mail, totp | `a4-communication` | `d3d2ac03f` | 30 | 16 | 14 | [a4](a4-communication.md) |
| A5 firewall, backups, remote, vpn, imager (+ windows, osk source-only) | `a5-management-install` | `61cb4d220` | 44 | 26 | 16 | [a5](a5-management-install.md) |
| A6 calendar, notes, contacts, weather, maps | `a6-content-1` | `591e469bf` | 47 | 30 | 14 | [a6](a6-content-1.md) |
| A7 word-processor (findings only), sheets, slides | `a7-office-editors` | `8c983b31b` | 36 | 24 | 12 | [a7](a7-office-editors.md) |
| A8 music, photos, video-player, camera, tube | `a8-media` | `b4701f794` | 31 | 18 | 3 | [a8](a8-media.md) |
| A9 ebook, pdf_viewer, file-explorer | `a9-documents-files` | `d7d60eab4` | 33 | 22 | 11 | [a9](a9-documents-files.md) |
| A10 video_editor, browser chrome | `a10-large-editors-browser` | `662c73faa` | 26 | 14 | 12 | [a10](a10-large-editors-browser.md) |
| **Total** | | | **558** | **349** | **180** | |

"Open" counts include items handed to another unit, items recorded as deliberate off-scale
values, and items that belong to the pending 2026-09-15 composition/text-geometry plans.
Unit A7 left word-processor untouched because its chrome files carry uncommitted work in the
live checkout; its six findings are recorded. Every worker ran the shared `code-review` skill
and fixed the confirmed findings before committing; the review regressions caught in F6 and A6
(rows painted over action buttons at 200 %, hit rects populated only during paint, month-cell
events becoming unreachable) were fixed, not recorded.

### The user-reported defect

"In the dark theme, right-click context menus and dropdowns show no highlighting" — root cause
found and fixed in F4: `ContextMenu` painted its armed row with a 10 % white veneer over the
near-black overlay surface (a lightness difference of ~12 that dim displays swallow) while every
other menu list used the accent; it now uses the accent fill with `on_accent` ink (ΔL* 56 dark /
52 light, ≥ 28 in all 14 presets), and the closed `Dropdown` control never painted its hover at
all. Regression: `ui/tests/f4_menu_highlight_render.rs`. S1 additionally established that the
shell itself has **no** working right-click menu at runtime (`Shell::ctx_menu` has no production
constructor; `WindowGroup::take_tab_context_menu` has no consumer) — see cross-unit item 4.

## Systemic findings that outlive any single unit

1. **Double text scaling.** Theme typography tokens are already text-scaled, and the legacy
   `TextRenderer` APIs scale again, so any widget or app that passes `tokens.typography.scale.*`
   into `draw_text`/`measure` renders 4× glyphs at 200 %. Fixed locally in F3's ten widgets,
   F2's `Button`, F4's menu rows, S2's Quick Settings/Room Activity, and most app units. Still
   present in ~32 other `ui` widget files (F3-21), `Chip::label_size_for_height`, `Avatar::draw_at`,
   the slash menu box, and the raw `--shots` host. Needs one shared helper on `TextRenderer`
   (F3 hosts a `renderer_type_size` twin in `data_table.rs`); `text_render.rs` carries user WIP.
2. **Measured-line vertical centring has no home.** The correct rule (centre on the measured
   line box, not the font size) now exists as `widget::text::centered_text_top` (F2), inside
   F3's `data_table.rs`, and as private copies in calendar, contacts, weather, maps and four A5
   apps. Consolidate onto `TextRenderer` and delete the copies.
3. **`NavSplit` lays out its panes with `TextRenderer::new()` and `Theme::dark()`**
   (`ui/src/widget/nav_split.rs` ~506–527, 625–640), so everything beneath a split is measured
   at 1× and painted at the real scale (A8, tube). Related: `NavShell`/`AppShell`/`SidebarDrawer`
   breakpoints ignore `theme.text_scale`, so apps re-implement breakpoint scaling with three
   different policies (A6).
4. **The shell has no runtime context menu** (S1, F4). Needs F6 (`WindowGroup` request) +
   shell wiring + F4 menu.
5. **Density tokens exist now but are not consumed.** F1 added `Tokens::metrics` (bar 40, bar
   control 28, panel row 44, selector 28, minimum target 32, panel widths 320/360/560, icons,
   sidebar/drawer/overview widths, compact breakpoint). Toolbar still sizes its minimum hit rect
   from `spacing.s8`; two `TITLE_BAR_HEIGHT`s (48 modal / 38 floating) remain; tab action chips
   are 14×24 px; S1/S2 kept several off-grid literals (28 px strip gutters, 26/30/78 px rows).
6. **Identity flow hand-rolls text inputs** (pre-existing, consolidated in F6's `sheet.rs`);
   migration path `TextInput::apply_layout_external`, seven call sites listed in the F6 report.
7. **User-facing literals instead of fluent keys** in identity flow, shell accessibility names,
   and several apps (calendar, chat, feeds, mail, totp) — recorded, not fixed.
8. **Pre-existing failures reproduced at the base and left untouched**: `design_token_audit`
   ratchets (`text_render/renderer_regressions.rs`, `apps/browser/src/browser_tab_strip.rs`,
   `shell/src/shell/frame.rs`), `typography_visual_regression` golden, ebook typography
   baselines (Linux-generated pixels), file-explorer `fixture_io_guard` race and German-locale
   asserts, notes `canonical_*` (10), slides notes-caret test, vpn Linux-only `/sys/class/net/lo`
   test, ux-bench `semantic:Files` ambiguity (12 of 48 matrix runs).

## Environment facts discovered (fix these once)

- **`Cargo.lock` at HEAD does not build with `--locked`**: `5a30514e3` added `libc` to
  `apps/browser/Cargo.toml` without the lock entry. One-line fix on `ui-review/lockfix`
  (`f9a579c9c`), cherry-picked by every unit; apply it to `codex/macos-shell`.
- **Worktrees under `/Users/xaver/dev/qos/.claude/worktrees/` inherit
  `/Users/xaver/dev/qos/.cargo/config.toml`** (cargo merges parent configs). That `main`-branch
  config applies `-C target-cpu=generic` to non-Linux aarch64, which trips `ring`'s CPU-feature
  const-assert for anything pulling `rustls` (the `ui` default features and several app test
  targets). Workers used `RUSTFLAGS="-C force-frame-pointers=yes --remap-path-prefix=/build=."`
  inline; nothing in-repo was changed. Put agent worktrees outside that directory, or align
  `main`'s config with this branch's `cfg(target_os = "linux")` gating.
- **The worktree tooling cut nine of eighteen worktrees from `origin/main`** instead of the
  checked-out branch; each was rebased/reset onto `codex/macos-shell` before work and their
  BEFORE captures were retaken. `origin/main` and `codex/macos-shell` have diverged on app code
  in both directions (main carries chat/mail text-scale work absent here).
- macOS: the `shots` binary, `--backend headless`, `qos-control` and `scripts/headless-examples`
  are Linux-only; `shell --shots` is theme-blind at 1280×800; `cargo check -p shell --tests`
  pulls the Linux-only `it` target; the vello CPU rasteriser panics on code_editor at 1920×1080.
  `ux-bench` (48-run matrix, `--interactive` rect dumps, `usability.py` overlap lint) is the
  shell evidence path and worked throughout.
- Today's Files 200 % repairs (`2026-09-16-ux-findings-repairs.md`) exist only as uncommitted
  work in the live checkout, not on the branch; A9 did not touch them.

## Merging

- Branches are 12–18 commits behind the moving `codex/macos-shell` tip (another session commits
  continuously). Suggested order: F1 (tokens, `Metrics`, `Spacer::fixed`, `Row::with_gap`),
  then F2–F6, S1, S2, then A1–A10.
- Expected conflicts, all mechanical: `ui/tests/it.rs` (six framework branches each append a
  `#[path]` registration — keep all), `ui/tests/design_token_baseline.txt` (F5 and F6 both lower
  entries — take the lower value per line), F1's one-line re-export in `ui/src/widget/mod.rs`
  and its edit to `ui/tests/design_token_audit.rs` against the live checkout's WIP in both files.
- App units edited only their own `apps/<app>/` trees; framework units only `ui/`; shell units
  only `shell/` plus new `scripts/ux-bench/{s1,s2}/*.json`. No unit changed `Cargo.toml` or
  `.cargo/config.toml`. The lock-fix commit has the same patch id on every branch.
- Before/after PNG evidence lives in each worktree's `target/ui-review/` and `target/ux-bench/`
  (259 GB of `target/` across the eighteen worktrees). Copy the capture directories you want to
  keep, then delete the worktrees' `target/` — the shared sccache retains the compilations.
