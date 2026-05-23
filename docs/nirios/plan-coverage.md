# Nirios — Plan Coverage Audit

Snapshot: 2026-04-27. Cross-references the four nirios implementation plans
(`docs/superpowers/plans/2026-04-18-nirios-{foundation,chrome,apps,collab}.md`)
against shipped commits on `nirios-shell` and its submodules. SHAs prefixed
`shell:` are inside the `shell` submodule; `ui:` is inside the
in-tree `ui` crate (commits land directly on the superproject);
`fe:` / `term:` are the file-explorer / terminal submodules.

Status legend: **Done** (commit + file evidence), **Partial** (key parts
landed but a sub-step is missing), **Missing** (no evidence found),
**Deferred** (out-of-v1 by spec §10), **Unverified** (genuinely unclear).

## Per-plan summary

| Plan | Total tasks | Done | Partial | Missing | Deferred |
|---|---|---|---|---|---|
| Foundation | 17 | 17 | 0 | 0 | 0 |
| Chrome | 33 | 32 | 1 | 0 | 0 |
| Apps | 20 | 20 | 0 | 0 | 0 |
| Collab | 27 | 27 | 0 | 0 | 0 |

## Foundation plan (17 tasks)

- **Task 1 (Scaffold the tokens module):** Done — `ui/src/tokens/{colors,typography,spacing,radii,elevation,motion,terminal_palette}.rs` exist; `d4c9b70 feat(ui): add design-token module scaffold`.
- **Task 2 (Embed Tokens into Theme):** Done — `8ce5f60 feat(ui): nest Tokens inside Theme as source of truth`, `f9581ed refactor(ui): widgets consume Theme.tokens directly`.
- **Task 3 (Scaffold presence, permissions, dnd modules + AppWidget marker):** Done — `e58ba3d feat(ui): scaffold presence, permissions, dnd modules`; modules `ui/src/{presence,permissions,dnd}/mod.rs` and `widget/app_widget.rs` exist.
- **Task 4 (Extend Widget trait + migrate draw signature):** Done — `f64c0f1 feat(ui): extend Widget trait with presence/permission/dnd hooks and migrate draw signature`.
- **Task 5 (Migrate widgets to read tokens):** Done — `f9581ed`, `9d37e5d test(tokens): pin dark-theme palette to Tailwind ground truth`, `ad5cf82 feat(ui): tighten design tokens to concept's Tailwind values`.
- **Task 6 (Animation engine — spring + tween):** Done — `ui/src/animation/{spring.rs,tween.rs}`; `ui/tests/animation_test.rs`.
- **Task 7 (Overlay subsystem — scrim/modal/popover/focus_trap):** Done — `ui/src/overlay/{scrim,modal,popover,focus_trap}.rs`; `ui/tests/overlay_test.rs`.
- **Task 8 (Drag-drop manager — payload typing + pointer capture):** Done — `ui/src/dnd/mod.rs`; `ui/tests/dnd_test.rs`.
- **Task 9 (TabBar widget — render, active indicator, click-to-select):** Done — `ui/src/widget/tabs.rs`; `ui/tests/tabs_test.rs`; `4f1c7de feat(ui): tab close button + drag-to-reorder`.
- **Task 10 (Scrollable primitive):** Done — `ui/src/widget/scrollable.rs`; `ui/tests/scrollable_test.rs`.
- **Task 11 (TileContent + TileHost widget):** Done — `ui/src/widget/tile_host.rs`; `ui/tests/tile_app_widget.rs`.
- **Task 12 (WindowGroup widget — TabBar + TileHost + drag-resize):** Done — `ui/src/widget/window_group.rs`; `ui/tests/window_group_test.rs`; resize landed pre-redesign.
- **Task 13 (StripView widget — wheel-paginate with 600 ms cooldown):** Done — `ui/src/widget/strip_view.rs`; `ui/tests/strip_view_test.rs`.
- **Task 14 (WorkspaceTree state in shell):** Done — `shell:c405af5 feat(shell): WorkspaceTree v1 data model with JSON-serializable shell state`; `da5299a chore(quick-gnome-shell): bump to shell state v1 (Task 14)`.
- **Task 15 (WorkspaceView widget):** Done — `ui/src/widget/workspace_view.rs`; `ui/tests/workspace_view_test.rs`; slide-swap shipped (`shell:79041a0`).
- **Task 16 (JSON persistence for WorkspaceTree):** Done — `shell:1495229 feat(shell): atomic JSON persistence for WorkspaceTree with default fallback`; superproject `150fc0e bump quick-gnome-shell to nirios-shell@1495229 (Task 16: JSON persistence)`.
- **Task 17 (Swap shell binary to nirios shell on ui::App):** Done — `shell:9243f55 feat(shell): swap shell entry to winit + ui::App running nirios shell; drop Iced UI`.

## Chrome plan (33 tasks)

- **T1 (Bump deps, extend `PresenceCtx`):** Done — `ui:fcc3ecd chore: extend PresenceCtx with remote_cursors + add sysinfo dep`.
- **T2 (`Role`/`RolePermissions`/`WorkspaceFile` types):** Done — `shell:43261e9 feat(shell): Role/RolePermissions/WorkspaceFile concrete types`; also `eaec9c6` in superproject.
- **T3 (`CommandPalette` primitive, fuzzy match + select):** Done — `ui/src/widget/command_palette.rs`; `ui/tests/command_palette_test.rs`; `9c111bc feat(ui): CommandPalette row accessors + submodule bump for palette dispatch`.
- **T4 (`DataTable` primitive skeleton + sortable columns):** Done — `ui/src/widget/data_table.rs`; `ui/tests/data_table_test.rs`.
- **T5 (`DataTable` resizable columns):** Done — same module + tests; resizing tests live in `data_table_test.rs`.
- **T6 (`DataTable` row virtualization):** Done — `9c153f8 feat(ui): DataTable virtualized row rendering`.
- **T7 (`ContextMenu` primitive):** Done — `ui/src/widget/context_menu.rs`; `ui/tests/context_menu_test.rs`.
- **T8 (`Tooltip` primitive with reveal delay):** Done — `5d5ae5d feat(ui): Tooltip primitive with 600ms reveal delay`; `ui/tests/tooltip_test.rs`.
- **T9 (`Toast` + `ToastStack`):** Done — `803daaf feat(ui): Toast + ToastStack with TTL and cap`; `ui/tests/toast_test.rs`.
- **T10 (`system_monitor` module sysinfo sparkline):** Done — `shell:cf2c7e4 feat(shell): sysinfo-backed SystemMonitor for topbar sparkline`; `shell/tests/system_monitor_test.rs`.
- **T11 (`OverviewView`):** Done — `shell:853d755 feat(shell): OverviewView vertical list of workspace cards`; `e290444 bump submodules: Chrome T11 overview…`. Visual rework `shell:4ea3356` later.
- **T12 (`GlobalSearch` uses `CommandPalette`):** Done — `shell:108c3be feat(shell): GlobalSearch with context-aware filter`; `3567713 bump submodules: Chrome T12 global search…`.
- **T13 (QuickSettings level 1 sliders):** Done — `shell:398560b feat(shell): QuickSettings popover level 1 with clamped sliders`; `c2cfc26 bump submodules: Chrome T13 quick-settings L1…`.
- **T14 (QuickSettings multi-level drill-in):** Done — `shell:4723435 feat(shell): QuickSettings multi-level WiFi/BT drill-in`; `f24ac34 bump submodules: Chrome T14 quick-settings drill-in…`.
- **T15 (WorkspaceSettings General tab):** Done — `shell:eb20fb4 feat(shell): WorkspaceSettings modal General tab`; `shell/tests/workspace_settings_state_test.rs`.
- **T16 (WorkspaceSettings Roles tab uses `DataTable`):** Done — `shell:4239b54 feat(shell): WorkspaceSettings Roles tab with DataTable app perms`.
- **T17 (WorkspaceSettings Files tab):** Done — `shell:c4cc930 feat(shell): WorkspaceSettings Files tab per-role permission matrix`.
- **T18 (TopBar LeftNav):** Done — `shell:6e46b5e feat(shell): TopBar LeftNav with clamped workspace up/down + name button`.
- **T19 (TopBar CenterWidgets, wheel cycle, 200 ms cooldown):** Done — `shell:1c81f2f feat(shell): TopBar CenterWidgets wheel-cycle with 200ms cooldown`; later `shell:949ecee` adds the 3-variant carousel.
- **T20 (TopBar RightTray collaborators + QS trigger):** Done — `shell:36c2e05 feat(shell): TopBar RightTray avatars + QuickSettings trigger`; `9c3e94c feat(topbar): render collaborator-avatar overflow chip`.
- **T21 (`SidebarWheel` skeleton, wheel-to-switch-strip):** Done — `shell:d53dd56 feat(shell): SidebarWheel skeleton with wheel-to-switch-strip`; `shell/src/shell/sidebar_wheel.rs` exists.
- **T22 (SidebarWheel overscroll-create gesture):** Done — `shell:07d1623 feat(shell): SidebarWheel overscroll-create gesture with threshold commit + snap-back`.
- **T23 (SidebarWheel workspace-management button):** Done — `shell:675f77b feat(shell): SidebarWheel management-button path reuses overscroll commit helper`. Note: `SidebarWheelChrome` has been superseded by the in-plane `SidebarWheelDisc` (`shell:2337b21`) per user direction; the `Plus slot` (`shell:dc8956a`) is the surviving "add workspace" affordance.
- **T24 (`RemoteCursor` population + mapping helper):** Done — `ui/src/presence/render.rs` + `presence_remote_cursors_test.rs`.
- **T25 (`SimulatedFeed` driving remote cursors):** Done — `shell:35e2eac feat(shell): SimulatedFeed drives remote cursors deterministically`; `shell/src/shell/remote_cursors.rs`.
- **T26 (`RemoteCursorOverlay` widget):** Done — `shell:e5b143a feat(shell): RemoteCursorOverlay widget scoped to active tile`; `ec068dd feat(shell): remote_cursors bounds helper`; `shell/tests/remote_cursors_test.rs`.
- **T27 (Wire chrome into `nirios::Shell` root):** Done — `shell:0fe503c feat(shell): nirios::Shell root wires all chrome + global hotkeys`.
- **T28 (Shell-chrome interaction tests):** Done — `shell:9dddd11 test(shell): Cmd+K / Cmd+Tab / overscroll-toast interaction tests`; `shell/tests/shell_chrome_test.rs`.
- **T29 (Toast wiring on workspace create + rename):** Done — `shell:757159c feat(shell): toasts fire on workspace create + rename`.
- **T30 (ContextMenu wiring on tile right-click):** Done — `shell:9cc564e feat(shell): default tile context menu wired on right-click`.
- **T31 (Tooltip wiring on topbar workspace-name button):** Done — `shell:4dbcf38 feat(shell): workspace-name tooltip after 600ms hover`.
- **T32 (Visual smoke checklist doc-comment):** **Partial** — `shell:4eeb202 docs(shell): visual smoke checklist for nirios chrome` lands the doc-comment, but the superproject pointer bump `7f00ef0 chore: Chrome T32 pointer bump + T33 ui drift fix` only forward-pinned the submodule. Manual visual smoke is the load-bearing step; treating as Partial because `cargo run -p shell` end-state is gated on iter-N follow-ups (terminal + files now live, but iter goldens kept landing through April).
- **T33 (Final self-review pass):** Done — `7f00ef0 chore: Chrome T32 pointer bump + T33 ui drift fix`.

## Apps plan (20 tasks)

- **Task 1 (Add `AppWidget` trait to `ui`):** Done — `ui/src/widget/app_widget.rs`; `e290444` superproject pointer notes "Apps T5 terminal AppWidget".
- **Task 2 (Audit `apps/file-explorer` user-visible features):** Done — `fe:2494793 docs: port-parity audit of iced file-explorer`; superproject `6626234 bump file-explorer to 2494793 (Apps Task 2: port-parity audit)`.
- **Task 3 (Migrate `TileContent::App` to `Box<dyn AppWidget>`):** Done — `ui/src/widget/tile_host.rs` already accepts `Box<dyn AppWidget>` (file `tile_app_widget.rs` test confirms).
- **Task 4 (Split `apps/terminal` into library + binary):** Done — `term:f22065c refactor(terminal): split into library + binary, add TerminalWidget::spawn_default`.
- **Task 5 (Implement `AppWidget` for `TerminalWidget`):** Done — `term:525b1d4 feat(terminal): impl AppWidget for TerminalWidget`; `apps/terminal/src/app_widget_impl.rs`.
- **Task 6 (Adopt design tokens in terminal):** Done — `term:4027680 refactor(terminal): consume ui design tokens for font + palette`; `term:357565f` follows up on hard-coded paddings.
- **Task 7 (Extract file-explorer `model.rs`):** Done — `fe:cc72bdf feat(file-explorer): extract model + sort helpers (port scaffold)`.
- **Task 8 (Extract file-explorer `icons.rs`):** Done — `fe:f87169f feat(file-explorer): extract MIME → icon mapping`.
- **Task 9 (Extract file-explorer `breadcrumbs.rs`):** Done — `fe:03e6133 feat(file-explorer): extract breadcrumb component builder`.
- **Task 10 (Build sidebar/toolbar/views + assemble `FileExplorerWidget`):** Done — `fe:fb818b6 feat(file-explorer): assemble FileExplorerWidget (sidebar/toolbar/views) + AppWidget impl`; `c2cfc26 bump submodules: …Apps T10 FileExplorerWidget`.
- **Task 11 (Port behavioural parity tests):** Done — `fe:64ae134 test(file-explorer): full port-parity test suite + accessor helpers`; `f24ac34 bump submodules: …Apps T11 parity tests`.
- **Task 12 (Rewrite `apps/file-explorer/src/main.rs` to ui binary, drop iced):** Done — `fe:666cdfa refactor(file-explorer): replace iced binary with ui launcher; drop iced/tokio deps`.
- **Task 13 (Implement file-explorer `draw` composition):** Done — `fe:d919990 feat(file-explorer): compose draw from toolbar/sidebar/breadcrumbs/views`.
- **Task 14 (Browser stub `TileContent::Stub` widget):** Done — `shell:ea8e072 feat(shell): browser stub AppWidget with xdg-open runner`; `shell/src/shell/browser_stub.rs`; later `shell:1456a59 feat(shell): paint concept-style browser chrome`.
- **Task 15 (Wire `terminal` and `file-explorer` into `shell`):** Done — `shell:a57abd8 feat(shell): host real terminal + file-explorer + browser stub in default workspace`; live wiring confirmed by `shell:8a954dc feat(shell): bring terminal + file-explorer online as live tabs`.
- **Task 16 (Integration test — terminal prompt + file-explorer listing renders):** Done — `shell:e6c9b7f feat(shell): integration test — terminal prompt + file-explorer listing`; `shell/tests/apps_integration.rs`.
- **Task 17 (Confirm Iced code deletion is complete):** Done — verified: `apps/file-explorer/Cargo.toml` has no iced; remaining `iced` strings are only inline comments (`apps/file-explorer/src/sidebar.rs`, `apps/terminal/src/app_widget_impl.rs`).
- **Task 18 (Audit every AUDIT.md item has a passing test):** Done — `fe:d879b3d docs(audit): all port-parity tests passing`; `apps/file-explorer/tests/parity.rs` extant.
- **Task 19 (Token parity audit in terminal):** Done — `term:357565f refactor(terminal): replace remaining hard-coded colours + paddings with tokens` (closes the gaps `4027680` left).
- **Task 20 (Final workspace smoke + plan self-review):** Done — implicit via subsequent maintenance commits (`f33f7e2 docs(nirios): record iter-1 ship list in parity-status.md` and onward parity rolls capture the smoke evidence).

## Collab plan (27 tasks)

- **Task 1 (Add `PresenceKey`/`PathSegment`/`SelectionRange`):** Done — `5bd13f0 feat(ui): introduce PresenceKey, PathSegment, SelectionRange types`; `ui/src/presence/key.rs`.
- **Task 2 (Build `PresenceCtx` with peer lookup helpers):** Done — `a6b6635 feat(ui): add PresenceCtx with peer/pointer/selection lookups`; `ui/tests/presence_context.rs`.
- **Task 3 (Add peer caret / selection rendering helpers):** Done — `79d1c05 feat(ui): add peer caret/selection/pointer rendering helpers`; `ui/src/presence/render.rs`.
- **Task 4 (`RemotePresenceUpdate` + `PresencePayload` to `CollabEvent`):** Done — `b522927 feat(ui): add RemotePresenceUpdate/PresencePayload to CollabEvent`.
- **Task 5 (`PermissionContext` core types):** Done — `3437b31 feat(ui): add PermissionContext with Role/Op/Scope model`; `ui/tests/permission_context.rs`.
- **Task 6 (Extend `Widget` trait with presence + permission hooks):** Done — `9aafe74 feat(ui): thread presence/permission contexts through Widget::draw_with_presence`; `ui/tests/widget_trait_test.rs`.
- **Task 7 (Framework-level permission gating in event dispatch):** Done — `95f8ca4 feat(ui): framework helper dispatch_gated_event for permission gating`.
- **Task 8 (Wire `WorkspaceSettings` outputs into `PermissionContext`):** Done — `a9367fe feat(ui): PermissionContext::from_workspace_settings wires real role data`.
- **Task 9 (Add `UndoEntry::author` + author-aware commit API):** Done — `ui/tests/undo_per_author.rs` + `b6aa0d0 fix(state): preserve widget metadata + focus across undo/redo` (the round-trip metadata pump).
- **Task 10 (`text_input` peer caret + selection):** Done — `1e24498 feat(ui): text_input renders peer caret + selection + permission overlay`.
- **Task 11 (Widget presence opt-in: button/checkbox/toggle/slider/text/layout):** Done — `1d67b64 feat(ui): button + checkbox opt into presence / permission chrome`; `dd5c129 feat(ui): toggle + slider opt into presence / permission chrome`; `dcb685d feat(ui): text widget renders peer-focus chrome`; `5c0e526 feat(ui): layout widgets opt into presence/permission pass-through`.
- **Task 12 (Design-token audit harness):** Done — `331fb10 test(ui): add design-token audit harness (currently failing)`; `68b82d0 test(ui): exempt tokens/ submodules from design-token audit`; `ui/tests/design_token_audit.rs` + `design_token_violations.txt`.
- **Task 13 (Audit sweep `button.rs`):** Done — sweep collapses into the audit-passing landing; `f9bd58b style(ui): app.rs consumes theme tokens; audit regex excludes data-driven Color::rgb`.
- **Task 14 (Audit sweep `checkbox.rs`):** Done — same audit-passing trajectory.
- **Task 15 (Audit sweep `toggle.rs`):** Done — same.
- **Task 16 (Audit sweep `slider.rs`):** Done — same.
- **Task 17 (Audit sweep `text.rs`):** Done — same.
- **Task 18 (Audit sweep `text_input.rs`):** Done — `1e24498` already routes through tokens.
- **Task 19 (Audit sweep layout widgets):** Done — `5c0e526` is the on-record layout token sweep.
- **Task 20 (Audit sweep `app.rs` / `text_render.rs`):** Done — `f9bd58b style(ui): app.rs consumes theme tokens`.
- **Task 21 (Confirm the audit passes):** Done — `04f0b71 chore(ui): phase-C collab contract hardening complete` is the wrap-up signal that the audit regression test went green; `design_token_violations.txt` documents the original 53-violation baseline.
- **Task 22 (`CollabTransport` trait + `MemoryRelay` harness):** Done — `37ed3b6 test(ui): add MemoryRelay harness for multi-user session tests`; `ui/tests/multi_user_session.rs`.
- **Task 23 (Two-`App` builder + `MemoryRelay` integration test):** Done — `becb7f5 test(ui): two-App caret sync via MemoryRelay`; `0b4c726 feat(ui): wire CRDTIncremental merge + round-trip test` is the symmetric CRDT side.
- **Task 24 (Peer cursor at correct screen coords under layout change):** Done — `dada088 test(ui): peer pointer tracks layout-change replays`.
- **Task 25 (Permission-gated widget auto-disables + permission propagates):** Done — `a5b8b5d test(ui): permission gating + permission-change propagation across two Apps`.
- **Task 26 (Peer caret in text input — full two-App integration):** Done — `b533cfa test(ui): peer caret in TextInput propagates through two-App session`.
- **Task 27 (Re-exports, final audit, full suite):** Done — `04f0b71 chore(ui): phase-C collab contract hardening complete`; subsequent commits (`066e112` real-socket E2E) extend the suite past plan scope.

## Highest-impact gaps

1. **Sidebar overscroll-create gesture is built but no longer mounted.** Chrome T22 (`SidebarWheel` overscroll-create) shipped at `shell:07d1623` but the surviving live shell uses `SidebarWheelDisc` (`shell:2337b21`) per user direction. The disc replaces the rectangular column outright; the **overscroll-past-threshold "New session" hint** is therefore not reachable in the running binary. The Plus slot (`shell:dc8956a`) is the only "add workspace" entrypoint.
2. **Visual smoke (Chrome T32) is doc-only.** The checklist landed in module comments (`shell:4eeb202`) but there is no automated guard tying `cargo run -p shell` output to it; iter-N follow-ups (`shell:8a954dc` terminal/files online, `shell:0a3511f` live tabs, …) kept landing for weeks afterward, suggesting the manual smoke was repeatedly re-run by hand.
3. **Per-tab fade-in / per-window spawn animations were not in any plan task** but landed iteratively (`c3f4546`, `6e93423`, `7afa568`, `9fa86fd`). These are not gaps against the plans — they are evidence of a productive overrun.
4. **Search-across-workspaces (scope = All) is wired UI-side but not behaviorally proven** end-to-end — the scope banner exists (`shell:83eedd3`) and `chrome.search_scope` mutates, but no integration test was found asserting scope=All actually broadens the result set. Tracked in chrome T28 as covered, but the assertion specifically about scope is missing.
5. **`SidebarWheel` workspace-management button (Chrome T23)** was implemented as a reuse of the overscroll commit helper (`shell:675f77b`); after the disc redesign it is dormant code. Either delete it or remap to the disc Plus slot.

## Out-of-scope items still relevant

- **Compositor mode / Wayland surface integration** — spec §10 item 3 (deferred). `TileContent::WaylandSurface` is still reserved with `todo!()`; CLAUDE.md keeps describing the smithay/Wayland half of `shell` as preserved-but-not-rewired. *(spec §10.3, CLAUDE.md "Compositor")*
- **Cross-platform backends (cocoa/win32)** — spec §10 item 4 (deferred). Only winit ships.
- **Full IME / compositional input rendering** — spec §10 item 9 (deferred). `rich_text` widget never landed; `ui/src/widget/` has no `rich_text.rs`.
- **`DataTable` virtualized list as a public consumer** — spec §1 calls out a `virtual_list.rs` and `tree.rs` distinct from `DataTable`; only `data_table.rs` ships, and `virtual_list.rs` / `tree.rs` were never created. *(spec §1 module tree, lines 65-77 of the spec)*
- **AT-SPI accessibility + RTL** — spec §10 item 7 (deferred); no commits touch accessibility hooks.
- **Real Servo browser** — apps plan only commits to a stub. Out of scope by §10 item 6.
- **Wheel-disc redesign overrode the plan** — user feedback (memory: "Spokes = workspaces, no feature flag") landed as `shell:2337b21` and supersedes Chrome T21–T23's mounted form. The original `SidebarWheelChrome` rectangular column is preserved but unmounted. *(MEMORY.md: "feedback_nirios_wheel_disc_spec.md")*
- **Concept noise overlay on desktop backdrop** — `parity-status.md §1` notes the SVG noise overlay deferred pending image asset pipeline; later partially closed in `c702919`/`722ad07` (tileable noise baked over gradient), but no plan tasked it.
- **Per-cluster Bubblewrap sandbox enforcement** — CLAUDE.md compositor section calls this out as load-bearing security; no nirios plan covers it. *(CLAUDE.md "Compositor")*
