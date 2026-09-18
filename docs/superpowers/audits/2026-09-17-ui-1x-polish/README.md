# 1×-first UI polish — batch index (2026-09-17)

The user looked at Quick Settings at 100 % in dark and saw touching elements, a cramped tile,
uneven section gaps and no visible borders. Two earlier review rounds had kept 1× byte-identical
and exempted "authored" pixel values, so nothing changed where it mattered. This batch removed
those two constraints and reworked every shell surface, toolkit control and app screen against
one bar, in dark **and** light at 1×:

1. **Nothing touches.** Every neighbouring pair keeps a token-step gap (`s1`+).
2. **Every surface has a visible derived edge** (`ui::surface::{edge_on, card, card_on, panel,
   separator}`, `ui::chrome::{panel_border, chrome_hairline, selection_tier, selection_tier_on,
   glass_selector}`) or a clear contrast step — never `border_standard` / `border_strong` /
   `*_white_*` / `neutral[]` on a raised or overlay ground (dark `border_standard` `#2B2C33` on
   `raised` `#1F2025` moves relative luminance by 0.007; the hairline threshold is 0.02).
3. **One spacing rhythm per panel or screen**: siblings `s3`, related rows `s2`, sections `s4`,
   next group `s6`.
4. **Content centred in its container** on measured line boxes.
5. **No "authored pixel" exemption**; byte-identical 1× is not a success criterion.
6. **Screenshots are the acceptance evidence**: before/after PNGs at 1×, dark and light, opened
   and inspected, plus the geometry lint where an inspection exists.

Plan: `~/.claude/plans/enchanted-forging-pancake.md`. HIG: `ui/HIG.md` §7, §7b, §9.3, §9.5, §9.13.

## Base, branches, evidence

- Base `fd1f84391` (`ux1x/unit0`, cut from `codex/macos-shell` `ac8ee5209`).
- Worker branches `ux1x/<unit>`; integration `ux1x/integration`; delivery is local branches only.
- Reports: this directory, one file per unit. Captures: `target/ux1x/<unit>/{before,after}/{dark,light}/`
  inside each worker worktree (`~/dev/qos/.claude/worktrees/agent-<id>/`); they are not committed.
- The Mac rebooted mid-batch (2026-09-17 ~17:15). Every worktree survived; the eight unfinished
  units were resumed from their checkpoints by fresh agents. `/tmp` was wiped, so the integration
  worktree now lives at `~/dev/qos/.claude/worktrees/ux1x-integration`.

## Unit 0 (coordinator, in the base)

`ui::surface::{EDGE_ALPHA_STEPS, edge_delta, edge_on, edge_on_with, edge_outline, card, separator}`;
`ui::chrome` re-exports for `panel_border`, `chrome_hairline`, `panel_ground`, `panel_shadow`,
`glass_selector` and the alpha constants; the `declared_borders_in_chrome_do_not_grow` ratchet
(`ui/tests/chrome_declared_border_baseline.txt`, `UPDATE_CHROME_BORDER_BASELINE=1`);
`--headless-drive` honouring `QOS_SHOTS_THEME` / `QOS_SHOTS_TEXT_SCALE` / `--scenario`;
`usability.py` `touching_peers` + `light` variant; the `ui-shots gallery_harness` example
(dark + light PNG + inspection); HIG §7 / §7b / §9.3 / §9.5 / §9.13.

## Coordinator follow-ups on `ux1x/integration` (from worker code reviews)

| Commit | What | Closes |
|---|---|---|
| 035e6d81a | one compositor (`colors::composite`) and one threshold (`PANEL_MIN_DELTA = HAIRLINE_MIN_DELTA`) behind `edge_on` | F1 reuse review |
| e87ec9964 | `ui::launch::shot_theme_from_env()`; table/data_table/nav_shell examples use it | F3 review; F4, A-units record adoption |
| cd5bbc1a7 | `ui::surface::card_on(fill, ground)` — edge from the composited wash | A12 (locating button, transit journey), F4 wash lifts |
| 086ba4d47 | `separator_rect` + whole-pixel snapping in `card` / `card_on` / `separator` | A6 verify, F2 open item |
| 414b0f0f0 | `--headless-drive` opens state through the spec's own opener | A11 (`shot_seed` workaround), A9 |
| 77e6cce15 | `ui::chrome::selection_tier_on(theme, state, ground)` + `LIFT_ALPHA_STEPS` | F3 VirtualList on raised, A5 firewall rows, A6 contacts menu / notes toggle, A4 totp/mail rows, A9 |
| 81b5d35a1 | `edge_on` per-thread memo, lazy ladder | A5 / F4 / A4 efficiency reviews |
| 1541e2fd3 | `ui::surface::panel(theme)` — Panel-role fill + border in one call | F2, A4 (12 sites), A7 |
| 6d3c2d855 | HIG §9.5: one painter per edge row, pixel snapping, ground-aware lift | — |
| c45dd5443 | weather: drop fields only tests read (CI denies warnings) | A12 review |
| b79246627 | date picker focus ring additive over the rest edge; `Shadow::with_alpha_scaled` pub | F1 merge regression, F4 open item |

## Units

| Unit | Scope | Branch @ sha | Report |
|---|---|---|---|
| F1 | toolkit controls | `ux1x/f1-controls` @ 8027e8e47 | [f1-controls.md](f1-controls.md) |
| F2 | containers and Panel-role popups | `ux1x/f2-containers-popups` @ f52d87214 | [f2-containers-popups.md](f2-containers-popups.md) |
| F3 | data, navigation, app chrome | `ux1x/f3-data-nav-chrome` @ bf90e5b95 | [f3-data-nav-chrome.md](f3-data-nav-chrome.md) |
| F4 | media, history, collaboration widgets, surface helpers | `ux1x/f4-media-history-surface` @ 64023e65d | [f4-media-history-surface.md](f4-media-history-surface.md) |
| S1 | host, Bar, wheel, Quick Settings, notifications | `ux1x/s1-host-bar-quick-settings` @ _pending_ | s1-host-bar-quick-settings.md |
| S2 | panels, sheets, system surfaces | `ux1x/s2-panels-sheets-system` @ _pending_ | [s2-panels-sheets-system.md](s2-panels-sheets-system.md) |
| A1 | api_tester, db_client, code_editor, containers, diffmerge, git_client, browser | `ux1x/a1-devtools-browser` @ 5049ef9d1 | [a1-devtools-browser.md](a1-devtools-browser.md) |
| A2 | hex_editor, logs, typst_editor, terminal, snippets, monitor, disks, calculator | `ux1x/a2-utilities` @ 03dae3373 | [a2-utilities.md](a2-utilities.md) |
| A3 | store, settings, themes | `ux1x/a3-store-settings-themes` @ e3f27e44f | [a3-store-settings-themes.md](a3-store-settings-themes.md) |
| A4 | feeds, chat, mail, totp | `ux1x/a4-communication` @ _pending_ | a4-communication.md |
| A5 | firewall, backups, remote, vpn | `ux1x/a5-management` @ _pending_ | a5-management.md |
| A6 | calendar, notes, contacts | `ux1x/a6-calendar-notes-contacts` @ _pending_ | a6-calendar-notes-contacts.md |
| A7 | word-processor, sheets, slides | `ux1x/a7-office` @ ffc6e35fc | [a7-office.md](a7-office.md) |
| A8 | music, photos, video-player, camera, tube | `ux1x/a8-media` @ 25a8abd94 | [a8-media.md](a8-media.md) |
| A9 | ebook, pdf_viewer, file-explorer | `ux1x/a9-documents` @ d63877d9e | [a9-documents.md](a9-documents.md) |
| A10 | video_editor | `ux1x/a10-video-editor` @ 569ec5879 | [a10-video-editor.md](a10-video-editor.md) |
| A11 | imager | `ux1x/a11-imager` @ _pending_ | a11-imager.md |
| A12 | weather, maps | `ux1x/a12-weather-maps` @ a69fc9fdb | [a12-weather-maps.md](a12-weather-maps.md) |

## Gate on `ux1x/integration`

_Filled in after the last unit merges: ui lib suite, `it` filtered set, scoped shell lib set,
shell bins check, app crates build + `--tests` check, the four re-baselines, integration
captures (gallery_harness dark+light, run-suite matrix over `s1/` and `s2/`, shots scenes)._

Known-red before and after the batch (verified at `fd1f84391` in a detached worktree):

- `ui --lib`: `app::framework_history_tests::native_queued_remote_cannot_retarget_same_id_new_legacy_store`.
- `ui --test it`: `app_and_shell_theme_escape_hatches_do_not_grow` grows on
  `shell/src/shell/frame.rs` (user WIP at base; `apps/browser/src/browser_tabs.rs` half is fixed by A1).
- shell lib: `collaborator_actions_are_absent_without_the_invite_capability` and four
  `search::action_api` / `search::visibility` tests; the whole shell suite cannot run
  (parallel trips `DenyAmbientStarts`, serial spins) — a scoped filter set is used.

## Cross-unit items and where they closed

- Gallery `touching_peers` Bold↔Italic / Link↔Code (F4's lint on the base) — Toolbar items, closed
  by F3's `s3` item gap.
- Neutral `Button` rest edge invisible on dark (A5, A7, A8, A10, A12 recorded it) — closed by F1.
- Selection lift invisible on `panel_ground` / raised rows (F3, A4, A5, A6, A9) — `selection_tier_on`.
- Translucent wash edges flipping polarity on light (A12, F4) — `card_on`.
- Hand-spelled Panel fill + border (F2, A4, A7) — `surface::panel`.
- `--headless-drive` could not see `--scenario` fixtures for custom-opener apps (A7, A10, A11,
  A3) — the opener fix; `--scenario` is a CLI flag beside `--headless-drive`, not a JSON field.
- S2's `scripts/ux-bench/s2/s2-quick-settings.json` asserted the old 4–12 px Night-Light→power gap
  that S1 widened to a `s4` section — _reconciled at integration (see gate)_.

## Still open after the batch (for the next pass)

- A shared wash ramp (`surface::wash_on(surface, tier)`) so history_scrubber / activity_timeline /
  channel_browser / video_player stop carrying private alpha ladders (F4).
- `Spacing::px()` accessor to delete the per-app `step()` copies (A8); `Column::section()/page()`
  constructors for the repeated `with_no_padding().with_intrinsic_height()` idiom (A11).
- `Image::paint_at` callers in apps/shell should move to `paint_at_on(.., &Theme)` (F4).
- `ColorPicker` declares 220 px and paints ≈200 (A3); `Dropdown` has no initial-selection API (A3).
- Compile-time `Spacing::BASE` rhythm consts do not follow `Theme::with_ui_scale` (A5, A10, A12,
  S1 review) — allowed by the plan for hit layouts without a `Theme`, recorded as a limit.
