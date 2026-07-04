# file-explorer — Port-Parity Audit

Source: `src/main.rs` (iced 0.13, single file, ~553 lines) as of 2026-04-18.
This document is the authoritative feature list the `ui` port must match.
Anything not listed here is out of scope for v1 port parity (track separately
if desired).

## Convention
Each feature is tagged with a category, a one-line behavioural summary taken
from the current Iced code, and a port-parity test expectation that must exist
in `apps/file-explorer/tests/parity.rs` before `main.rs` is replaced.

## Navigation
- [N1] **Initial path resolution.** On startup, `/workspace` if it exists, else
  `dirs_next::home_dir()`, else `std::env::current_dir()`, else `/`.
  *Test:* `initial_path_prefers_workspace_then_home_then_cwd_then_root`.
- [N2] **Directory navigation on double-click.** Clicking an entry twice within
  500 ms opens it: dirs trigger navigation, files call `open::that`.
  *Test:* `double_click_dir_navigates_into_it`.
- [N3] **Back button.** Navigates to `current_path.parent()` when present;
  no-op at filesystem root.
  *Test:* `back_button_moves_to_parent_and_noops_at_root`.
- [N4] **Refresh.** Re-reads the current directory.
  *Test:* `refresh_rereads_same_directory`.
- [N5] **Breadcrumb buttons.** Each path component is a button that navigates
  to that component; home prefix collapses to `󰋜 ~`.
  *Test:* `breadcrumb_component_click_navigates_to_that_component`.

## Sidebar
- [S1] **Fixed shortcut list.** Home, Documents, Downloads, Pictures, Music,
  Videos (only when `home_path` is Some) plus Root (`/`). No user-added
  shortcuts, no drag-reorder, no right-click remove — the current Iced app
  has none of these. The spec's richer sidebar is deferred.
  *Test:* `sidebar_shows_fixed_seven_shortcuts_when_home_present`.
- [S2] **Active-shortcut highlight.** The shortcut whose path equals
  `current_path` is rendered with the primary button style; others with text
  style.
  *Test:* `sidebar_highlights_active_shortcut`.
- [S3] **Fixed width.** 180 px, full height, own background color.
  *Test:* `sidebar_has_fixed_width_180`.

## Toolbar
- [T1] **Back button (`󰁍`).**  See N3.
- [T2] **Refresh button (`󰑐`).** See N4.
- [T3] **Search input (200 px wide).** Live-filters the entry list by
  lowercase-contains match on `entry.name`.
  *Test:* `search_filters_entries_by_lowercase_contains`.
- [T4] **View-mode toggle (`󰕰` list / `󰕲` grid).** Active mode uses primary
  button style.
  *Test:* `view_mode_toggle_switches_between_list_and_grid`.
- [T5] **Sort-by buttons (Name/Size/Date/Type).** Active sort key uses primary
  button style. Clicking applies `sort_entries` immediately.
  *Test:* `sort_by_name_places_alpha_order_with_dirs_first`,
  `sort_by_size_orders_by_file_size`,
  `sort_by_modified_orders_by_mtime`,
  `sort_by_type_orders_by_mime`.
- [T6] **Sort-order toggle (`󰁝` asc / `󰁅` desc).** Flips ordering; directories
  always remain grouped first regardless of direction.
  *Test:* `sort_order_toggle_reverses_non_dir_group_but_keeps_dirs_first`.
- [T7] **Open Terminal button (`󰆍 Open Terminal`).** Spawns `terminal`
  with `current_dir(current_path)` as a *separate process*. Port preserves
  this: the file-explorer does not embed a terminal itself.
  *Test:* `open_terminal_button_spawns_quick_terminal_subprocess` (uses a mock
  `command-runner` trait so the test does not actually spawn).

## Main view
- [M1] **List mode.** Each row shows icon + name + mime-subtitle + size +
  modified (columns). Icon color: blue for dirs, grey for files. Selected
  row has a dark-blue background tint.
  *Test:* `list_mode_row_has_icon_name_size_modified_columns`.
- [M2] **Grid mode.** Items wrap 6 per row, each a 120×100 card with a 48-pt
  icon above the name (center-aligned). Selection tints the card background.
  *Test:* `grid_mode_wraps_six_per_row`, `grid_mode_card_is_120x100`.
- [M3] **MIME icon mapping.** `image/*` → 󰋩; `video/*` → 󰿎; `audio/*` → 󰝚;
  `*pdf*` → 󰈦; dir → 󰉋; otherwise 󰈔.
  *Test:* `mime_icon_mapping_matches_current`.
- [M4] **Selection.** Single-click selects (single selection only — the Iced
  app has no Ctrl/Shift multiselect). Clicking empty space does not
  deselect in the current app.
  *Test:* `single_click_selects_single_entry`.

## Status / errors
- [E1] **Loading state.** Shows centered "Loading..." text while a navigation
  is in flight.
  *Test:* `loading_state_shown_during_navigation`.
- [E2] **Error state.** If `read_dir_async` errors, shows a centered "Error
  Accessing Directory" heading + message + "Try Again" button (which
  refreshes).
  *Test:* `error_state_shows_message_and_retry_button`.
- [E3] **Footer.** Bottom bar shows `{n} items` + filler + formatted path
  (`~` substitution for home).
  *Test:* `footer_shows_item_count_and_formatted_path`.

## Sort logic (unit-testable)
- [U1] `sort_entries` groups directories first (`b.is_dir.cmp(&a.is_dir)`)
  then by the selected key; descending reverses only the inner key
  comparison, not the dir-first rule.
  *Test:* `sort_entries_always_places_directories_first`.

## Explicitly out of scope (not in current Iced code — DO NOT add in port)
The following features are in the design spec §7 expected list but **do not
exist** in the current Iced file-explorer. The port preserves parity, so they
are not implemented in this plan; each is tracked as a follow-on. The port
must *not* introduce them without separate scope.
- Drag-resize of the sidebar.
- Collapse/expand of the sidebar.
- Drag-reorder of sidebar shortcuts.
- Drop-new-shortcut by dragging grid items onto the sidebar.
- Right-click-remove of a shortcut.
- Context menu on a file (Open/Reveal/Rename/Delete/Copy/Move).
- Drag items from grid to sidebar/folders.
- Multi-select with Ctrl/Shift.

Follow-ups are tracked in `docs/superpowers/deferred/nirios-shell.md` under
"file-explorer parity beyond iced baseline".

## Coverage

As of Task 18, every AUDIT.md item above has a corresponding parity test in
`tests/parity.rs` and is currently passing.

- [x] N1 — `initial_path_prefers_workspace_then_home_then_cwd_then_root`
- [x] N2 — `double_click_dir_navigates_into_it`
- [x] N3 — `back_button_moves_to_parent_and_noops_at_root`
- [x] N4 — `refresh_rereads_same_directory`
- [x] N5 — `breadcrumb_component_click_navigates_to_that_component`
- [x] S1 — `sidebar_shows_fixed_seven_shortcuts_when_home_present`
- [x] S2 — `sidebar_highlights_active_shortcut`
- [x] S3 — `sidebar_has_fixed_width_180`
- [x] T1 — covered by N3 (`back_button_moves_to_parent_and_noops_at_root`)
- [x] T2 — covered by N4 (`refresh_rereads_same_directory`)
- [x] T3 — `search_filters_entries_by_lowercase_contains`
- [x] T4 — `view_mode_toggle_switches_between_list_and_grid`
- [x] T5 — `sort_by_name_places_alpha_order_with_dirs_first`,
      `sort_by_size_orders_by_file_size`,
      `sort_by_modified_orders_by_mtime`,
      `sort_by_type_orders_by_mime`
- [x] T6 — `sort_order_toggle_reverses_non_dir_group_but_keeps_dirs_first`
- [x] T7 — `open_terminal_button_spawns_quick_terminal_subprocess`
- [x] M1 — `list_mode_row_has_icon_name_size_modified_columns`
- [x] M2 — `grid_mode_wraps_six_per_row`, `grid_mode_card_is_120x100`
- [x] M3 — `mime_icon_mapping_matches_current`
- [x] M4 — `single_click_selects_single_entry`
- [x] E1 — `loading_state_shown_during_navigation`
- [x] E2 — `error_state_shows_message_and_retry_button`
- [x] E3 — `footer_shows_item_count_and_formatted_path`
- [x] U1 — `sort_entries_always_places_directories_first`

Run `cargo test -p file-explorer --test parity` to re-verify.
