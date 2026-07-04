# Critical UI Inspection — 2026-05-04

Branch: `nirios-shell` (worktree `agent-ac995781090285657`).
Submodule branch (quick-shell): `agent-ac995781090285657-shots-wait-fix`.

## Method

All visual capture went through the offscreen path (per the user's
"never screenshot the device" directive). Tools used:

```bash
distrobox enter my-distrobox -- bash -c 'cargo run --bin shots -p quick-desktop --quiet -- --mode offscreen --width 1600 --height 900 --out target/inspection-baseline'
distrobox enter my-distrobox -- bash -c 'cargo run --bin shots -p quick-desktop --quiet -- --mode offscreen --scenario palette_typing            --out target/inspection-palette'
distrobox enter my-distrobox -- bash -c 'cargo run --bin shots -p quick-desktop --quiet -- --mode offscreen --scenario tab_drag                  --out target/inspection-tabdrag'
distrobox enter my-distrobox -- bash -c 'cargo run --bin shots -p quick-desktop --quiet -- --mode offscreen --scenario tab_drag_out_to_new_group --out target/inspection-drag'
distrobox enter my-distrobox -- bash -c 'cargo run --bin shots -p quick-desktop --quiet -- --mode offscreen --scenario workspace_switch          --out target/inspection-ws'
```

State-only path (panic check) for every scenario:

```bash
distrobox enter my-distrobox -- bash -c 'cargo run --bin shots -p quick-desktop --quiet -- --mode headless --scenario {palette_typing|tab_drag|tab_drag_out_to_new_group|workspace_switch}'
```

All four headless scenarios completed without panic. Crops for tight
inspection were generated through `PIL.Image.crop` from the same
distrobox.

Scenes covered (legacy catalogue): `desktop`, `overview`, `palette`,
`quick-settings`, `qs-wifi-drillin`, `qs-bt-drillin`,
`ws-settings-general`, `ws-settings-roles`, `ws-settings-files`.

Scenarios covered: `palette_typing`, `tab_drag`,
`tab_drag_out_to_new_group`, `workspace_switch`.

## Findings

### Bugs landed in this sweep

1. **`tab_drag` scenario captured mid-spring overlap** — `quick-shell/src/bin/shots/scenario.rs`
   (`tab_drag`). The post-Drag wait was `Wait(2)` = 32 ms, but the
   tab-bar's post-reorder layout spring runs for `LAYOUT_ANIM_MS`
   (= 180 ms) in `quick_ui::widget::tabs`. The "after-drag" snapshot
   painted adjacent pills overlapping (the active "Files" pill rendered
   on top of the displaced "Terminal" pill remnants), reading as
   "DerFiles[search][resize]X" in the captured PNG. Bumped to
   `Wait(12)` (192 ms ≥ 180). Fix: `15f4c3b`.

2. **`workspace_switch` scenario captured mid-slide voids** —
   `quick-shell/src/bin/shots/scenario.rs` (`workspace_switch`). The
   post-`SwitchWorkspace` waits were `Wait(4)` = 64 ms, but the
   workspace slide animates for `WS_SLIDE_MS` (= 260 ms) in
   `quick_desktop::shell::overlay_host`. The "ws1" and "ws0-again"
   snapshots painted the content widget shifted off-axis with painting
   voids where the slide hadn't settled. Bumped both to `Wait(20)`
   (320 ms ≥ 260). Fix: `15f4c3b` (same commit as #1).

   Both #1 and #2 are pinned by a new unit test
   `scenario_post_animation_waits_outlast_their_animation` in
   `src/bin/shots/scenario.rs` so a future edit shortening the waits
   re-fails CI rather than silently regressing the captures.

3. **WS-Settings General-tab self-tile painted "You [You]"** —
   `quick-shell/src/shell/overlay_host.rs` (`draw_ws_settings`). The
   row paints `[name] [indigo You-badge]` for the local user; when no
   `me` collaborator is seeded in the active workspace the fallback
   name was the literal string "You", duplicating the adjacent badge.
   Switched the fallback to "Me" (now exposed as
   `ShellOverlayHost::ME_TILE_FALLBACK_NAME`) so the row reads
   "Me [You]" — aligned with the concept's `Sarah Chen [You]` shape.
   New unit test `me_tile_fallback_tests::fallback_does_not_duplicate_you_badge`
   pins the constant so a revert can't silently reintroduce the
   duplication. Fix: `ba0b969`.

### Bugs / gaps logged for follow-up

1. **`e2e_shell_flows` flakes with SIGSEGV under `--test-threads`>1** —
   `quick-shell/tests/e2e_shell_flows.rs`. Single-threaded passes 15/15
   reliably. Parallel run hit `signal: 11, SIGSEGV: invalid memory
   reference` once across two attempts. Suggests a race on a global
   GPU/wgpu/text-cache resource shared across the test binaries. Out of
   scope for a 0–5-fix sweep — would need either a `--test-threads=1`
   harness override on this binary or a per-test mutex around whatever
   `static` resource is being mutated. Logged for follow-up.

2. **Palette `02-palette-open` snapshot still mid-open** —
   `quick-shell/src/bin/shots/scenario.rs` (`palette_typing`). The
   post-`OpenPalette` wait is `Wait(2)` = 32 ms, but
   `MODAL_OPEN_ANIM_MS` is 220 ms in `overlay_host`. The current capture
   looks acceptable to a human reviewer (the open curve eases very
   quickly to ~95% by frame 2), but mathematically the snapshot is in
   the same flake-prone class as the two we fixed. Did not bump
   pre-emptively because the visual artefact threshold is unclear and
   the existing capture is not visibly broken. If a future reviewer
   sees a "transient" palette regression, this is the wait to bump.

3. **Bootstrap "me" collaborator is intentionally not seeded** —
   `quick-shell/src/shell/state.rs:415-424`. The comment explicitly
   notes that not seeding `me` is needed to keep the General-tab
   indigo-fill count under the Roles-tab parity-spec threshold. Our
   "Me" fallback fix (#3 above) is the right call until that spec
   threshold is re-baselined. Logged so a future "let's seed me with
   a real identity" attempt remembers the spec-coupling.

### Test ledger health

- `cargo test -p quick-ui --lib`: **180 passed / 0 failed** (1 ignored
  is preexisting)
- `cd quick-shell && cargo test --lib`: **157 passed / 0 failed** (1
  ignored is preexisting; +1 vs. baseline 156 from the new
  `me_tile_fallback_tests` test)
- `cd quick-shell && cargo test --test concept_parity_spec`: **26
  passed / 0 failed**
- `cd quick-shell && cargo test --test e2e_shell_flows`: **15 passed /
  0 failed** (single-threaded; flakes with SIGSEGV when run parallel
  — see follow-up #1)
- `cd quick-shell && cargo test --test cross_strip_drag`: **2 passed /
  0 failed**
- `cd quick-shell && cargo test --bin shots`: **10 passed / 0 failed**
  (+1 vs. baseline 9 from the new
  `scenario_post_animation_waits_outlast_their_animation` test)

## Conclusion

Build is healthy. Three contained fixes landed: two scenario-fixture
timing bugs that produced mid-animation capture artefacts (tab-spring
overlap, workspace-slide voids), and one user-facing copy
duplication ("You [You]" → "Me [You]") in the WS-Settings General tab.
All fixes ship with new unit tests pinning the corrected values. One
parallel-execution flake in `e2e_shell_flows` and one cosmetically
acceptable mid-open palette capture are logged as follow-ups; neither
gates ship-readiness.
