//! Concept-parity spec tests — a verifiable ledger of remaining work.
//!
//! ## Why this file exists
//!
//! `docs/nirios/concept-coverage.md` (commit `ce66421`) audits every
//! component shipped on the `nirios-shell` branch against the source-of-
//! truth TSX in `nirios-concept/`. The audit catalogues *visual* and
//! *functional* gaps (rendered-but-wrong colour, drawn-but-missing
//! affordance, state-but-no-render). The audit's verdict is in prose;
//! this file lifts those prose findings into executable Rust assertions
//! so each remaining gap has a single failing test pinning it.
//!
//! Each `#[test]` here:
//! 1. Sets up the minimum chrome state to exercise the gap.
//! 2. Renders into a `vello::Scene` (no GPU required — we walk the
//!    encoded `draw_data` to look for colour brushes / draw counts).
//! 3. Asserts the *behaviour* the concept demands, with tolerance.
//!
//! ## Anti-watering-down covenant — DO NOT BREAK
//!
//! These tests are intentionally failing. They are the spec ledger; a
//! green test means a gap closed, a red test means work remains. The
//! list below is what *removing* a failing test must not look like:
//!
//! - **DO NOT** add `#[ignore]`. The whole point is that they run and
//!   fail — `#[ignore]`'d tests show as "ok" in `cargo test --quiet`,
//!   which silently fakes spec coverage. Use `#[ignore = "<reason>"]`
//!   *only* if a gap was closed by a different test elsewhere AND that
//!   coverage is documented in this file's header above the affected
//!   test.
//! - **DO NOT** weaken the assertion to make it pass against current
//!   behaviour. Replacing `assert_eq!(rendered_count, expected_count)`
//!   with `assert!(rendered_count >= 0)` is a watering-down; replacing
//!   `assert_color_close(c, target, 4)` with a 64-tolerance is a
//!   watering-down; flipping a `>` into a `>=` to absorb the off-by-
//!   one is a watering-down. Don't do it.
//! - **DO NOT** delete the test "because it always fails". A failing
//!   spec test is the *specified* behaviour; deleting it deletes the
//!   spec. If you genuinely think the audit was wrong, push the
//!   correction *into the audit* (with a commit message that says so)
//!   then update the test in the same commit.
//! - **DO NOT** swap `assert_eq!` for `assert!(_ < some_loose_value)`
//!   where the original assertion was strict. Strict equality on a
//!   countable shape (number of action-row tiles, presence of an
//!   icon glyph) is the contract; "approximately equal" smuggles bugs
//!   in.
//! - **DO NOT** comment-out individual `assert*` calls. If the spec
//!   row has multiple sub-assertions, all of them are part of the
//!   contract. Comment-skip individual asserts is the same anti-pattern
//!   as `#[ignore]` for whole tests.
//!
//! Each test header below quotes the audit row + the concept TSX
//! `path:line` so the gap is traceable from a single grep.
//!
//! ## Methodology: scene-encoding inspection
//!
//! Vello's `Scene::encoding()` exposes `draw_tags` + `draw_data`. Solid
//! colour brushes encode as `DrawTag::COLOR` (= `0x44`) with 4 bytes
//! of premultiplied RGBA u32 in `draw_data`. By scanning the byte
//! stream we can detect "the renderer at some point fill'd a region
//! with colour C" without round-tripping pixels through wgpu (which
//! requires llvmpipe / a real adapter). Tests that need *positional*
//! information (e.g. selected-row colour) either rely on rect counts
//! or fall back to `OffscreenRenderer` pixel sampling — clearly
//! marked when they do.

#![allow(clippy::bool_assert_comparison)]

use std::collections::HashMap;

use taffy::{AvailableSpace, Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;

use quick_desktop::shell::build_nirios_overlay_host_with_chrome;
use quick_desktop::shell::chrome_state::Chrome;
use quick_desktop::shell::quick_settings::{QsLevel, QuickSettings};
use quick_desktop::shell::state::WorkspaceTree;
use quick_desktop::shell::topbar_chrome::{CenterWidget, TopBarChrome};

use quick_ui::presence::PresenceCtx;
use quick_ui::text_render::TextRenderer;
use quick_ui::theme::Theme;
use quick_ui::widget::Widget;
use quick_ui::widget::command_palette::{CommandEntry, CommandPalette};
use quick_ui::widget::{
    AppKind, TabDescriptor, TileContent, WindowGroup, WindowWidth,
};

mod support;

// ===========================================================================
// Shared helpers
// ===========================================================================

const W: f32 = 1600.0;
const H: f32 = 900.0;

/// Build, lay out, and render the full `ShellOverlayHost` widget tree at
/// (W, H). Returns the produced `Scene`. Mirrors the harness in
/// `e2e_shell_flows.rs` minus the PNG snapshot — these spec tests inspect
/// the encoded scene directly, no GPU required.
fn render_overlay_host(tree: &WorkspaceTree) -> (Scene, Chrome) {
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(tree);
    let theme = Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let child = host.build_layout(&mut taffy, &theme, &mut node_map);
    let root_node = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(W),
                    height: Dimension::length(H),
                },
                ..Default::default()
            },
            &[child],
        )
        .unwrap();
    let mut tr = TextRenderer::new();
    let measure_map = node_map.clone();
    taffy
        .compute_layout_with_measure(
            root_node,
            Size {
                width: AvailableSpace::Definite(W),
                height: AvailableSpace::Definite(H),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = measure_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, &mut tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )
        .expect("compute_layout");
    host.apply_layout(&taffy, child, Point::ORIGIN);

    let mut scene = Scene::new();
    let presence = PresenceCtx::default();
    host.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    (scene, chrome)
}

/// Walk a `vello::Scene`'s encoding and return every solid-colour brush
/// that was emitted into the draw stream, as a tight `(r, g, b, a)`
/// tuple for ergonomic comparison. Premultiplied → unpremultiplied is
/// done losslessly when alpha == 0xff, which covers every spec-relevant
/// fill in the shell (we never pre-blend the indigo accent or the
/// emerald monitor colour against an alpha < 255 — those compositions
/// happen at the layer level).
fn collect_solid_colors(scene: &Scene) -> Vec<(u8, u8, u8, u8)> {
    use vello_encoding::DrawTag;
    let enc = scene.encoding();
    let mut out = Vec::new();
    let mut data_cursor = 0usize;
    for tag in enc.draw_tags.iter() {
        if *tag == DrawTag::COLOR {
            // 4 bytes = u32 RGBA (LE), low byte = alpha.
            if data_cursor + 4 <= enc.draw_data.len() {
                let bytes = &enc.draw_data[data_cursor..data_cursor + 4];
                // u32 = (r<<24)|(g<<16)|(b<<8)|a — see DrawColor doc.
                // LE memory order: [a, b, g, r].
                let a = bytes[0];
                let b = bytes[1];
                let g = bytes[2];
                let r = bytes[3];
                out.push((r, g, b, a));
            }
            data_cursor += 4;
        } else {
            // info_size is in u32 words.
            data_cursor += (tag.info_size() as usize) * 4;
        }
    }
    out
}

/// Return true if `colors` contains any tuple within `tol` per channel
/// of `target` (rgb only — alpha tolerated independently of nominal).
fn contains_color_close(
    colors: &[(u8, u8, u8, u8)],
    target: (u8, u8, u8),
    tol: u8,
) -> bool {
    colors.iter().any(|&(r, g, b, _a)| {
        r.abs_diff(target.0) <= tol
            && g.abs_diff(target.1) <= tol
            && b.abs_diff(target.2) <= tol
    })
}

/// Open the quick-settings popover. Wraps the chrome-state mutation so
/// each test reads the same setup line.
fn open_quick_settings(chrome: &Chrome) {
    chrome.borrow_mut().overlays.quick_settings = true;
}

// ===========================================================================
// 1. WorkspaceSettings — General tab must render an Invite Link affordance
// ===========================================================================

/// Audit row: `WorkspaceSettingsModal.tsx → workspace_settings.rs /
/// draw_ws_settings`, "🟥 🟥 — General tab is **just a Name input**.
/// Invite link API exists (`workspace_settings.rs:147 invite_link`) but
/// is never drawn." (`docs/nirios/concept-coverage.md:24,182-186`)
///
/// Concept ref: `nirios-concept/src/components/WorkspaceSettingsModal.tsx:95-149`
/// — the General tab paints a labelled input row containing the invite
/// link (a URL like `https://nirios.app/invite/abc123`) plus a "Copy"
/// affordance.
///
/// What this test asserts: when the modal is open, *something* in the
/// rendered scene contains the substring "invite" (case-insensitive) so
/// users get a visible cue that an invite link exists. We assert via the
/// scene's encoded draw_data — text is rasterised through `cosmic-text`
/// into glyph runs that are not introspectable as ASCII strings, so we
/// instead assert a stronger structural property: when the modal is open,
/// a *substantially* greater number of solid-colour fills are emitted
/// than when only the name input is drawn (the invite-link row + copy
/// pill should add at least one fill on top of the existing surface +
/// border + name-input rect + 4 tab pills + caret).
///
/// Forbidden modifications (re: the file header covenant):
/// - Don't add `#[ignore]`.
/// - Don't lower the `+ at_least_one_extra_fill` minimum to absorb the
///   gap. "+0" means there's no invite-link surface in the modal — the
///   audit's exact finding.
/// - Don't refactor the assertion to "the modal is at least visible"
///   (e.g. `total > 0`). The whole modal is *already* visible; we're
///   pinning the *additional content* the General tab is supposed to
///   carry.
#[test]
fn workspace_settings_renders_general_tab_with_invite_link() {
    let tree = WorkspaceTree::bootstrap_default();

    // First: render with the modal closed so we know the baseline number
    // of indigo-500 fills the rest of the chrome emits (topbar dots,
    // sparkline bars, accent borders, etc).
    let (baseline_scene, _baseline_chrome) = render_overlay_host(&tree);
    let baseline_indigo = collect_solid_colors(&baseline_scene)
        .iter()
        .filter(|&&(r, g, b, _)| {
            r.abs_diff(0x63) <= 4 && g.abs_diff(0x66) <= 4 && b.abs_diff(0xf1) <= 4
        })
        .count();

    // Build a fresh host with the modal pre-opened. The chrome state
    // must be set before the build/layout/draw cycle so layout-time
    // decisions observe it.
    let (mut host2, chrome2) = build_nirios_overlay_host_with_chrome(&tree);
    chrome2.borrow_mut().overlays.ws_settings = true;
    let theme = Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let child = host2.build_layout(&mut taffy, &theme, &mut node_map);
    let root_node = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(W),
                    height: Dimension::length(H),
                },
                ..Default::default()
            },
            &[child],
        )
        .unwrap();
    let mut tr = TextRenderer::new();
    let measure_map = node_map.clone();
    taffy
        .compute_layout_with_measure(
            root_node,
            Size {
                width: AvailableSpace::Definite(W),
                height: AvailableSpace::Definite(H),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = measure_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, &mut tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )
        .expect("compute_layout");
    host2.apply_layout(&taffy, child, Point::ORIGIN);
    let mut open_scene = Scene::new();
    let presence = PresenceCtx::default();
    host2.draw(&mut open_scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    let open_indigo = collect_solid_colors(&open_scene)
        .iter()
        .filter(|&&(r, g, b, _)| {
            r.abs_diff(0x63) <= 4 && g.abs_diff(0x66) <= 4 && b.abs_diff(0xf1) <= 4
        })
        .count();

    // Concept's General tab paints a prominent indigo-500 "Copy" button
    // *inside* the modal (`WorkspaceSettingsModal.tsx:108`,
    // `bg-indigo-500 ... text-white px-6 py-3 rounded-xl font-bold`)
    // alongside a `bg-indigo-500/20 text-indigo-400` "You" badge per
    // collaborator. Even one collaborator + the Copy button add ≥ 2
    // indigo-500 fills *above the closed-modal baseline*. Today the
    // only indigo the modal contributes is the caret while editing
    // (off by default) and the active-tab pill highlight (which is
    // surfaces.overlay, not indigo at all).
    let added_indigo = open_indigo.saturating_sub(baseline_indigo);
    assert!(
        added_indigo >= 2,
        "WorkspaceSettings General tab is supposed to paint an indigo-500
         Copy button next to the Invite Link plus an indigo You-badge per
         collaborator (concept TSX WorkspaceSettingsModal.tsx:95-149,
         in particular `bg-indigo-500` on the Copy button at line 108). \
         Opening the modal added only {added_indigo} extra indigo-500 \
         fills over the closed-modal baseline (open_indigo={open_indigo}, \
         baseline_indigo={baseline_indigo}). \
         Audit row `concept-coverage.md:24,182-186`: 'General tab is just a \
         Name input'. Fix `draw_ws_settings` to draw the invite link \
         + Copy button + collaborator grid."
    );
}

// ===========================================================================
// 2. WorkspaceSettings — Roles tab must render the per-role app grid
// ===========================================================================

/// Audit row: `concept-coverage.md:24,187-191` — "Roles tab not drawn.
/// `apps_table` + `files_table` exist as `DataTable` state but
/// `draw_ws_settings` ignores them."
///
/// Concept ref: `nirios-concept/src/components/WorkspaceSettingsModal.tsx:152-254`
/// — Roles tab paints a role list with coloured swatches plus a per-role
/// app-permissions toggle grid (5 apps × on/off).
///
/// What this test asserts: switching the chrome's modal-tab indicator to
/// Roles must change what is drawn. Because the current stub hard-codes
/// `active = i == 0` in the tab strip and ignores any external "active
/// tab" signal, the rendered scene is identical regardless of what tab
/// the user "selected". We pin this by snapshotting the encoded fill-
/// count for the General-tab open render and the Roles-tab open render
/// and asserting they differ — the per-role app grid alone should add
/// dozens of cell rects.
///
/// Forbidden: don't change this to "they may be equal — both draw the
/// same modal". The point of tabs is per-tab content.
#[test]
fn workspace_settings_renders_roles_tab_with_per_role_app_grid() {
    let tree = WorkspaceTree::bootstrap_default();

    // Render with modal open + (default) General tab.
    let (general_fills, _) = render_with_ws_settings_tab(&tree, /* roles */ false);
    // Render with modal open + Roles tab.
    let (roles_fills, _) = render_with_ws_settings_tab(&tree, /* roles */ true);

    // Concept's Roles tab adds at minimum: a role list (≥1 row × swatch +
    // label) + a 5-app permission grid (5 rows × 3 columns of toggle
    // pills) → ≥ 5+15 = 20 extra solid-colour fills over the General tab.
    let diff = roles_fills.abs_diff(general_fills);
    assert!(
        diff >= 20,
        "Switching to Roles tab must change the rendered output by
         at least 20 solid-colour fills (role swatches + per-app
         permission grid per concept TSX WorkspaceSettingsModal.tsx:152-254). \
         Observed |roles_fills - general_fills| = {diff} (general={general_fills}, \
         roles={roles_fills}). Audit row `concept-coverage.md:24,187-191`: \
         `apps_table`/`files_table` are dead state; no per-tab branch in \
         `draw_ws_settings`. Fix by per-tab branching the modal body."
    );
}

// ===========================================================================
// 3. WorkspaceSettings — Files tab must render the file grid
// ===========================================================================

/// Audit row: `concept-coverage.md:24,192-196`.
/// Concept ref: `WorkspaceSettingsModal.tsx:257-303` — Files tab is a
/// grid of files (icon + name + type badge + assigned-roles avatar pile
/// + Remove button).
///
/// Same shape as the Roles test: switching to Files must change the
/// rendered fill count by a substantial amount. Currently it doesn't.
#[test]
fn workspace_settings_renders_files_tab_with_role_assignments() {
    let tree = WorkspaceTree::bootstrap_default();

    let (general_fills, _) = render_with_ws_settings_tab(&tree, /* roles */ false);
    // We don't have a `files` toggle on the chrome — both Files and
    // Roles are part of the same "tab not General" behaviour; we encode
    // Files via a side-channel below. For now: assert that *some* tab
    // distinct from General produces a different render, since the
    // current implementation can't distinguish General from Files at
    // all.
    let (roles_fills, _) = render_with_ws_settings_tab(&tree, /* roles */ true);
    // Concept's Files tab adds a grid of file rows (icon + name + type
    // badge + Remove pill per row) + section header — ≥ 8 extra fills
    // for even a single-file workspace, ≥ 20 for a populated one. We
    // pin against the Roles render so the test isolates "Files-specific
    // content" from "any non-General content".
    //
    // CURRENTLY this assertion fails because *neither* tab renders
    // anything beyond General — the modal hardcodes `active = i == 0`.
    let diff_g = roles_fills.abs_diff(general_fills);
    assert!(
        diff_g > 0,
        "(precondition) Roles and General tabs must produce different
         renders before we can compare Files; if this assertion fires,
         the Roles spec test (#2) is the canonical place to fix it."
    );

    // The actual Files-tab assertion: building a workspace with N files
    // should make the Files tab render strictly more than the empty-
    // workspace case. Concept paints one row per file.
    let mut tree_with_files = WorkspaceTree::bootstrap_default();
    if let Some(active) = tree_with_files.active_workspace_mut() {
        for i in 0..3 {
            active
                .workspace_files
                .push(quick_desktop::shell::state::WorkspaceFile {
                    id: format!("f{i}"),
                    name: format!("doc-{i}.md"),
                    mime: "text/markdown".into(),
                });
        }
    }
    let (files_filled, _) = render_with_ws_settings_tab(&tree_with_files, /* roles */ true);
    let added = files_filled.saturating_sub(roles_fills);
    assert!(
        added >= 6,
        "Files tab must paint per-file rows (concept TSX
         WorkspaceSettingsModal.tsx:257-303). Adding 3 files to the
         workspace must add ≥6 extra solid-colour fills (icon + name
         badge + Remove pill at minimum, per file). Observed +{added}
         (files_with_3={files_filled}, roles_empty={roles_fills}). \
         Audit row `concept-coverage.md:24,192-196`: Files tab not drawn."
    );
}

/// Helper for tests #2 and #3: render the host with the modal open and
/// (optionally) try to drive it to a non-General tab. The "roles" bool
/// is currently *advisory* — the implementation has no chrome-level
/// switch for it, so this helper does the best it can: it sets
/// `editing_workspace_name = Some(...)` to put the modal in a
/// "non-default" inner state, which is the closest thing we can do
/// today. When the WorkspaceSettingsModal tab-state is wired into
/// chrome (audit fix #1), update this helper to use the real switch.
fn render_with_ws_settings_tab(tree: &WorkspaceTree, roles: bool) -> (usize, Chrome) {
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(tree);
    {
        let mut st = chrome.borrow_mut();
        st.overlays.ws_settings = true;
        if roles {
            // Best-effort signal that "we're not on General". Today this
            // is a no-op for the renderer — the Roles spec test fails
            // because of that.
            st.editing_workspace_name = Some(String::from("__roles_tab__"));
        }
    }
    let theme = Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let child = host.build_layout(&mut taffy, &theme, &mut node_map);
    let root_node = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(W),
                    height: Dimension::length(H),
                },
                ..Default::default()
            },
            &[child],
        )
        .unwrap();
    let mut tr = TextRenderer::new();
    let measure_map = node_map.clone();
    taffy
        .compute_layout_with_measure(
            root_node,
            Size {
                width: AvailableSpace::Definite(W),
                height: AvailableSpace::Definite(H),
            },
            |known, avail, node_id, _, _| {
                if let Some(&ptr) = measure_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, avail, &mut tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )
        .expect("compute_layout");
    host.apply_layout(&taffy, child, Point::ORIGIN);
    let mut scene = Scene::new();
    let presence = PresenceCtx::default();
    host.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    (collect_solid_colors(&scene).len(), chrome)
}

// ===========================================================================
// 4. GlobalSearch — must render a leading lucide search-icon glyph
// ===========================================================================

/// Audit row: `concept-coverage.md:25,204-207` — "Concept input row uses
/// leading lucide `Search` glyph + input + trailing `Cmd-K` kbd chip;
/// ours: just placeholder \"Search…\" text in a neutral inset rect. **No
/// search icon, no kbd chip.**"
///
/// Concept ref: `nirios-concept/src/components/GlobalSearch.tsx:138-153`.
///
/// Search-glyph icons are filled with `theme.tokens.colors.text_dim` in
/// the icon path (lucide stroke colour). We assert the rendered palette
/// contains the text-dim slate-400 colour (#94a3b8) at least once
/// inside the input row — given the current stub, the only fills near
/// the input row are surface + stroke colours, none of which match
/// text-dim *as a glyph stroke* (text-dim is used for the placeholder
/// "Search…" string but glyph rendering goes through a separate path
/// that doesn't emit a `DrawTag::COLOR` for the body of each glyph).
///
/// To make this assertion specific to "an icon was drawn", we count the
/// number of solid-colour fills emitted by `CommandPalette::draw` *in
/// isolation* (no chrome) — concept's draw should add at least 1
/// extra fill for the icon-glyph path's stroke; ours adds 0.
#[test]
fn global_search_renders_leading_search_icon() {
    // Build a standalone CommandPalette with a couple of seeded entries
    // so the visible-list path runs and the "input row" is the dominant
    // surface.
    let mut palette = CommandPalette::new();
    palette.push(CommandEntry::new("ws.create", "Create Workspace"));
    palette.push(CommandEntry::new("file.open", "Open File"));
    let theme = Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let child = palette.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(640.0),
                    height: Dimension::length(400.0),
                },
                ..Default::default()
            },
            &[child],
        )
        .unwrap();
    let mut tr = TextRenderer::new();
    let measure_map = node_map.clone();
    taffy
        .compute_layout_with_measure(
            root,
            Size {
                width: AvailableSpace::Definite(640.0),
                height: AvailableSpace::Definite(400.0),
            },
            |known, avail, node_id, _, _| {
                if let Some(&ptr) = measure_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, avail, &mut tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )
        .expect("compute_layout");
    palette.apply_layout(&taffy, child, Point::ORIGIN);
    let mut scene = Scene::new();
    let presence = PresenceCtx::default();
    palette.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

    let fills = collect_solid_colors(&scene);

    // Concept paints a leading lucide `Search` icon — that's a stroked
    // circle + line (`CommandPalette::draw` should call `icons::draw_icon`
    // with `Icon::Search` and `text_dim` stroke). Our current draw path
    // emits exactly: card surface + card stroke + input rect surface +
    // input rect stroke + (per visible row) selected-row highlight +
    // (text glyphs which don't emit COLOR tags). That's at most ~6 fills
    // for 2 entries with cursor at row 0. Concept's icon adds at least
    // one more solid-colour fill for the icon body or stroke.
    //
    // Threshold: ≥ 8 means we got the icon; ≤ 7 means it isn't there.
    assert!(
        fills.len() >= 8,
        "GlobalSearch palette must paint a leading lucide Search icon
         (concept TSX GlobalSearch.tsx:138-153). Observed only {} solid-
         colour fills in the standalone palette render — the icon path
         would have added at least one. Audit row `concept-coverage.md:25,204-207`.",
        fills.len(),
    );
}

// ===========================================================================
// 5. GlobalSearch — must render a trailing "Cmd+K" kbd chip
// ===========================================================================

/// Audit row: `concept-coverage.md:25,204-207` — same TSX line as #4 but
/// asserts the *trailing* kbd chip on the right edge of the input.
///
/// The kbd chip is a separate small rounded rect (`bg-white/5
/// border-white/10`) painted to the right of the input — that's at
/// minimum 1 surface fill + 1 border stroke = +2 solid-colour
/// emissions over the current stub. We assert by the same count-diff
/// shape as #4 but with a higher threshold.
#[test]
fn global_search_renders_cmd_k_kbd_chip() {
    let mut palette = CommandPalette::new();
    palette.push(CommandEntry::new("ws.create", "Create Workspace"));
    let theme = Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let child = palette.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(640.0),
                    height: Dimension::length(400.0),
                },
                ..Default::default()
            },
            &[child],
        )
        .unwrap();
    let mut tr = TextRenderer::new();
    let measure_map = node_map.clone();
    taffy
        .compute_layout_with_measure(
            root,
            Size {
                width: AvailableSpace::Definite(640.0),
                height: AvailableSpace::Definite(400.0),
            },
            |known, avail, node_id, _, _| {
                if let Some(&ptr) = measure_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, avail, &mut tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )
        .expect("compute_layout");
    palette.apply_layout(&taffy, child, Point::ORIGIN);
    let mut scene = Scene::new();
    let presence = PresenceCtx::default();
    palette.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

    let fills = collect_solid_colors(&scene);

    // Concept paints leading icon (≥ 1 fill) + input rect (≥ 2) +
    // trailing Cmd-K kbd chip (≥ 2 fills: pill body + border) +
    // visible row selection highlight (≥ 1) = ≥ 6, plus the outer card
    // surface + border (= 2) = ≥ 8 *minimum*. Asserting the kbd chip
    // requires going one step higher than test #4 to isolate this
    // affordance.
    assert!(
        fills.len() >= 9,
        "GlobalSearch palette must paint a trailing Cmd+K kbd chip
         (concept TSX GlobalSearch.tsx:138-153). Observed {} solid-
         colour fills — chip would have added at least 2 (body + border).
         Audit row `concept-coverage.md:25,204-207`.",
        fills.len(),
    );
}

// ===========================================================================
// 6. GlobalSearch — selected row must be indigo-500, not slate mid-grey
// ===========================================================================

/// Audit row: `concept-coverage.md:25,217-221` — "Concept result row:
/// `${idx === selectedIndex ? 'bg-indigo-500 text-white' : ...}` —
/// selected row is **fully indigo**. Ours
/// (`command_palette.rs:308-316`) highlights with
/// `theme.tokens.colors.neutral[4]` (mid-grey). Wrong colour."
///
/// Concept ref: `nirios-concept/src/components/GlobalSearch.tsx:251`.
///
/// We assert by encoding inspection: the rendered palette must contain
/// indigo-500 (#6366f1) as one of the solid-colour fills.
#[test]
fn global_search_selected_row_is_indigo_500_not_grey() {
    let mut palette = CommandPalette::new();
    palette.push(CommandEntry::new("ws.create", "Create Workspace"));
    palette.push(CommandEntry::new("file.open", "Open File"));
    let theme = Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let child = palette.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(640.0),
                    height: Dimension::length(400.0),
                },
                ..Default::default()
            },
            &[child],
        )
        .unwrap();
    let mut tr = TextRenderer::new();
    let measure_map = node_map.clone();
    taffy
        .compute_layout_with_measure(
            root,
            Size {
                width: AvailableSpace::Definite(640.0),
                height: AvailableSpace::Definite(400.0),
            },
            |known, avail, node_id, _, _| {
                if let Some(&ptr) = measure_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, avail, &mut tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )
        .expect("compute_layout");
    palette.apply_layout(&taffy, child, Point::ORIGIN);
    let mut scene = Scene::new();
    let presence = PresenceCtx::default();
    palette.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

    let fills = collect_solid_colors(&scene);

    // Indigo-500 = #6366f1 → (99, 102, 241). slate-600 = (71, 85, 105).
    let has_indigo = contains_color_close(&fills, (0x63, 0x66, 0xf1), 4);
    let has_slate_600_fill = contains_color_close(&fills, (71, 85, 105), 4);

    assert!(
        has_indigo,
        "Selected row in the GlobalSearch palette must paint indigo-500
         #6366f1 (concept TSX GlobalSearch.tsx:251). Observed solid-
         colour fills: {fills:?}. \
         Audit row `concept-coverage.md:25,217-221`: ours highlights with \
         `neutral[4]` (mid-grey) instead. \
         has_slate_600_fill={has_slate_600_fill} (when present, that's \
         the stale grey highlight)."
    );
}

// ===========================================================================
// 7. QuickSettings — top action row of Settings/Camera/Power tiles
// ===========================================================================

/// Audit row: `concept-coverage.md:22,128-131,131` — "Concept top row:
/// three equal-width buttons `flex-1 bg-white/10` with **Settings /
/// Camera / red-tinted Power** glyphs. **Ours has none of these** — no
/// row of action buttons."
///
/// Concept ref: `nirios-concept/src/components/QuickSettings.tsx:181-201`.
///
/// We assert structurally: a 3-tile action row above the sliders is
/// 3 surface fills + 3 stroke pairs + 3 icon glyphs. The current
/// implementation paints title + 2 sliders + 2 toggles = ~9 fills.
/// With the action row we expect ≥ 12 fills.
#[test]
fn quick_settings_renders_action_row_with_settings_camera_power() {
    let tree = WorkspaceTree::bootstrap_default();
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);
    open_quick_settings(&chrome);
    let theme = Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let child = host.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(W),
                    height: Dimension::length(H),
                },
                ..Default::default()
            },
            &[child],
        )
        .unwrap();
    let mut tr = TextRenderer::new();
    let measure_map = node_map.clone();
    taffy
        .compute_layout_with_measure(
            root,
            Size {
                width: AvailableSpace::Definite(W),
                height: AvailableSpace::Definite(H),
            },
            |known, avail, node_id, _, _| {
                if let Some(&ptr) = measure_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, avail, &mut tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )
        .expect("compute_layout");
    host.apply_layout(&taffy, child, Point::ORIGIN);
    let mut scene = Scene::new();
    let presence = PresenceCtx::default();
    host.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

    let fills = collect_solid_colors(&scene);

    // The Power tile in the concept is `bg-red-500/20` — that's
    // semantic.danger (red-500 = #ef4444 = (239, 68, 68)). Pinning by
    // the red presence makes this test fail loudly and *specifically*
    // when the concept's red-tinted Power tile is missing, vs the
    // generic "fill count too low" shape used in other tests.
    let has_red_power = contains_color_close(&fills, (239, 68, 68), 4);
    assert!(
        has_red_power,
        "QuickSettings top action row must include a red-tinted Power
         tile (concept TSX QuickSettings.tsx:181-201, `bg-red-500/20`).
         Observed solid-colour fills: {fills:?}. \
         Audit row `concept-coverage.md:22,128-131`: 'no row of action \
         buttons' — Settings/Camera/Power tiles missing entirely."
    );
}

// ===========================================================================
// 8. QuickSettings — slider fill is white, not indigo
// ===========================================================================

/// Audit row: `concept-coverage.md:22,132-139` — "Concept sliders:
/// `h-1.5 rounded-full` thin bar with `bg-white` fill ... Ours paints
/// two stand-alone progress bars 6 px tall ... no `bg-white` filled
/// ramp — just `accent[5]` (indigo) bars. Wrong colour."
///
/// Concept ref: `QuickSettings.tsx:204-215`.
///
/// We assert the rendered palette must contain pure white as a fill
/// AND must contain it at a count consistent with a bar fill (≥ 1 white
/// rounded-rect for each of brightness + volume). Currently the only
/// whites in the rendered palette are surfaces (window cards) outside
/// the popover and the toggle knobs, which we discount by counting
/// only fills emitted while the popover is open vs closed.
#[test]
fn quick_settings_slider_fill_is_white_not_indigo() {
    let tree = WorkspaceTree::bootstrap_default();

    // Closed-popover render — establishes the per-frame "white pixel"
    // baseline (window-card surfaces, knobs, etc.).
    let (closed_scene, _) = render_overlay_host(&tree);
    let closed_fills = collect_solid_colors(&closed_scene);
    let closed_indigo_count = closed_fills
        .iter()
        .filter(|&&(r, g, b, _)| {
            r.abs_diff(0x63) <= 4 && g.abs_diff(0x66) <= 4 && b.abs_diff(0xf1) <= 4
        })
        .count();

    // Open-popover render.
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);
    open_quick_settings(&chrome);
    let theme = Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let child = host.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(W),
                    height: Dimension::length(H),
                },
                ..Default::default()
            },
            &[child],
        )
        .unwrap();
    let mut tr = TextRenderer::new();
    let measure_map = node_map.clone();
    taffy
        .compute_layout_with_measure(
            root,
            Size {
                width: AvailableSpace::Definite(W),
                height: AvailableSpace::Definite(H),
            },
            |known, avail, node_id, _, _| {
                if let Some(&ptr) = measure_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, avail, &mut tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )
        .expect("compute_layout");
    host.apply_layout(&taffy, child, Point::ORIGIN);
    let mut scene = Scene::new();
    let presence = PresenceCtx::default();
    host.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

    let open_fills = collect_solid_colors(&scene);
    let open_indigo_count = open_fills
        .iter()
        .filter(|&&(r, g, b, _)| {
            r.abs_diff(0x63) <= 4 && g.abs_diff(0x66) <= 4 && b.abs_diff(0xf1) <= 4
        })
        .count();

    // Concept slider fills are `bg-white` — opening the popover should
    // not increase the indigo-500 fill count (the slider track is a
    // dim slate, the fill is white). Ours bumps the indigo count by 2
    // (two slider fills both painted in indigo-500). Pin against the
    // delta: opening the popover must NOT add new indigo fills.
    let added_indigo = open_indigo_count.saturating_sub(closed_indigo_count);
    assert_eq!(
        added_indigo, 0,
        "Opening QuickSettings must not add any indigo-500 fills — the
         slider fill colour per concept TSX QuickSettings.tsx:204-215
         is `bg-white`, not the accent. Observed: closed={closed_indigo_count} \
         indigo fills, open={open_indigo_count}, delta={added_indigo}. \
         Audit row `concept-coverage.md:22,132-139`: 'no `bg-white` filled \
         ramp — just `accent[5]` (indigo) bars'. Fix: swap \
         `colors.accent[5]` for `colors.text_primary` (white in dark theme) \
         in the slider-fill paint call in `draw_quick_settings`."
    );
}

// ===========================================================================
// 9. QuickSettings — must support drill-in render (Wifi level)
// ===========================================================================

/// Audit row: `concept-coverage.md:22,148-148` — "Concept WiFi/BT drill-in
/// lists (`:101-162`) — no entries; the `nav_stack` field on
/// `QuickSettings` is dead state."
///
/// Concept ref: `QuickSettings.tsx:108-128, 101-162`.
///
/// We assert that pushing a Wifi level onto a `QuickSettings` instance
/// and then rendering the popover produces a different scene encoding
/// than the Root-level render. Today both renders are identical because
/// `draw_quick_settings` ignores `QuickSettings::nav_stack` entirely
/// (the chrome state has only the *flat* `wifi: bool` toggle).
#[test]
fn quick_settings_supports_wifi_drill_in_render() {
    // Build two QuickSettings — one at Root, one drilled to Wifi.
    let mut qs_root = QuickSettings::new();
    let mut qs_wifi = QuickSettings::new();
    qs_wifi.push_level(QsLevel::Wifi);
    assert_eq!(qs_root.current_level(), &QsLevel::Root);
    assert_eq!(qs_wifi.current_level(), &QsLevel::Wifi);

    // Render the popover for each. The current overlay-host pulls only
    // `chrome.quick_settings` (a struct of flat bools/floats — no
    // nav_stack), so changing `qs_*` does not reach the renderer.
    // That's the gap. To compose this test we install both QS instances
    // through a chrome-state side-channel (today: just the brightness
    // delta, since that's the only state the renderer reads). When the
    // renderer learns to consume nav_stack, the `wifi` branch will
    // emit additional fills (back-chevron + per-network rows + signal
    // bars).
    let tree = WorkspaceTree::bootstrap_default();

    // Render with the popover open and "root level" (brightness 0.5).
    qs_root.set_brightness(50.0);
    let (root_fills, _) = render_quick_settings_open(&tree);

    // Now mutate qs to "wifi level". The point of this test is that the
    // renderer should *care* — pushing a WiFi level should change what
    // is drawn. Since chrome-state has no nav_stack field, this test
    // pushes the wifi level on a *side* QuickSettings instance to keep
    // the API explicit; when the renderer learns nav_stack the test's
    // setup line below changes from a no-op to a real injection.
    let _ = qs_wifi; // Silence unused-binding pending the renderer wire-up.
    let (wifi_fills, _) = render_quick_settings_open(&tree);

    // EXPECTED: root_fills ≠ wifi_fills (drill-in adds back-chevron +
    // network rows). CURRENT: equal — drill-in path is unwired.
    assert_ne!(
        root_fills, wifi_fills,
        "QuickSettings drill-in into Wifi level must produce a different
         render than Root level (concept TSX QuickSettings.tsx:108-128, \
         101-162). Currently both render identically because \
         `draw_quick_settings` ignores `QuickSettings::nav_stack` (the \
         chrome state only carries flat `wifi: bool`). Audit row \
         `concept-coverage.md:22,148`: 'nav_stack is dead state'."
    );
}

fn render_quick_settings_open(tree: &WorkspaceTree) -> (usize, Chrome) {
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(tree);
    chrome.borrow_mut().overlays.quick_settings = true;
    let theme = Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let child = host.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(W),
                    height: Dimension::length(H),
                },
                ..Default::default()
            },
            &[child],
        )
        .unwrap();
    let mut tr = TextRenderer::new();
    let measure_map = node_map.clone();
    taffy
        .compute_layout_with_measure(
            root,
            Size {
                width: AvailableSpace::Definite(W),
                height: AvailableSpace::Definite(H),
            },
            |known, avail, node_id, _, _| {
                if let Some(&ptr) = measure_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, avail, &mut tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )
        .expect("compute_layout");
    host.apply_layout(&taffy, child, Point::ORIGIN);
    let mut scene = Scene::new();
    let presence = PresenceCtx::default();
    host.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    (collect_solid_colors(&scene).len(), chrome)
}

// ===========================================================================
// 10. TopBar SystemMonitor — CPU label is emerald-400, not indigo
// ===========================================================================

/// Audit row: `concept-coverage.md:23,163-166` — "Concept SystemMonitor
/// labels: CPU `text-emerald-400`, RAM `text-blue-400`, TMP
/// `text-orange-400`. Ours: CPU + RAM `colors.accent[5]` (indigo), TMP
/// `text_dim`. **The 'different semantic colour per metric' cue is lost.**"
///
/// Concept ref: `nirios-concept/src/components/TopBarWidgets.tsx:120-124`.
///
/// emerald-400 = #4ade80 = (74, 222, 128).
///
/// We render the TopBarChrome at SystemMonitor center widget and inspect
/// the encoding for an emerald-400 fill. CPU label glyphs go through
/// the text renderer which DOES emit colour-paint commands per glyph
/// run, so this is detectable in the encoding.
#[test]
fn topbar_system_monitor_cpu_label_is_emerald_400() {
    let tree = WorkspaceTree::bootstrap_default();
    let mut chrome = TopBarChrome::from_tree(&tree);
    chrome.set_center_widget_for_tests(CenterWidget::SystemMonitor);

    let theme = Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let child = chrome.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(1280.0),
                    height: Dimension::length(48.0),
                },
                ..Default::default()
            },
            &[child],
        )
        .unwrap();
    let mut tr = TextRenderer::new();
    let measure_map = node_map.clone();
    taffy
        .compute_layout_with_measure(
            root,
            Size {
                width: AvailableSpace::Definite(1280.0),
                height: AvailableSpace::Definite(48.0),
            },
            |known, avail, node_id, _, _| {
                if let Some(&ptr) = measure_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, avail, &mut tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )
        .expect("compute_layout");
    chrome.apply_layout(&taffy, child, Point::ORIGIN);
    let mut scene = Scene::new();
    let presence = PresenceCtx::default();
    chrome.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

    let fills = collect_solid_colors(&scene);
    // emerald-400 #4ade80 = (74, 222, 128).
    let has_emerald_400 = contains_color_close(&fills, (74, 222, 128), 4);
    assert!(
        has_emerald_400,
        "TopBar SystemMonitor CPU label must render in emerald-400
         #4ade80 (concept TSX TopBarWidgets.tsx:120-124,
         `text-emerald-400`). Observed fills: {fills:?}. Audit row \
         `concept-coverage.md:23,163-166`: ours uses indigo (`accent[5]`/\
         `accent_primary`); the 'different semantic colour per metric' \
         cue is lost. Fix by mapping CPU label/sparkline to \
         `theme.tokens.colors.semantic.success` (or a dedicated \
         emerald-400 token) in `topbar_chrome.rs:632-672`."
    );
}

// ===========================================================================
// 11. TopBar SystemMonitor — RAM label is blue-400, not indigo
// ===========================================================================

/// Audit row: `concept-coverage.md:23,163-166` — same line as #10 but
/// asserts the RAM label colour. blue-400 = #60a5fa = (96, 165, 250).
#[test]
fn topbar_system_monitor_ram_label_is_blue_400() {
    let tree = WorkspaceTree::bootstrap_default();
    let mut chrome = TopBarChrome::from_tree(&tree);
    chrome.set_center_widget_for_tests(CenterWidget::SystemMonitor);

    let theme = Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let child = chrome.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(1280.0),
                    height: Dimension::length(48.0),
                },
                ..Default::default()
            },
            &[child],
        )
        .unwrap();
    let mut tr = TextRenderer::new();
    let measure_map = node_map.clone();
    taffy
        .compute_layout_with_measure(
            root,
            Size {
                width: AvailableSpace::Definite(1280.0),
                height: AvailableSpace::Definite(48.0),
            },
            |known, avail, node_id, _, _| {
                if let Some(&ptr) = measure_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, avail, &mut tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )
        .expect("compute_layout");
    chrome.apply_layout(&taffy, child, Point::ORIGIN);
    let mut scene = Scene::new();
    let presence = PresenceCtx::default();
    chrome.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

    let fills = collect_solid_colors(&scene);
    let has_blue_400 = contains_color_close(&fills, (96, 165, 250), 4);
    assert!(
        has_blue_400,
        "TopBar SystemMonitor RAM label must render in blue-400 #60a5fa
         (concept TSX TopBarWidgets.tsx:120-124, `text-blue-400`).
         Observed fills: {fills:?}. Audit row `concept-coverage.md:23,163-166`."
    );
}

// ===========================================================================
// 12. WindowGroup — maximized chrome must hide when cursor is below y=80
// ===========================================================================

/// Audit row: `concept-coverage.md:20,114-123,377-381` — "Concept
/// `WindowFrame.tsx:108-123` only shows the floating title pill +
/// restore/close buttons when `clientY ≤ 80`; otherwise the chrome is
/// hidden. Ours always draws them while maximized."
///
/// Concept ref: `nirios-concept/src/components/WindowFrame.tsx:114-123`.
///
/// We render a maximized `WindowGroup` *twice*: once with no cursor
/// signal, once after driving a CursorMoved event with `y > 80`. The
/// floating chrome (restore pill + close glyph + title pill) should
/// disappear on the second render — observable as a drop in the
/// solid-colour fill count.
///
/// Today the maximized branch in `WindowGroup::draw` (window_group.rs
/// :659-810) paints the chrome unconditionally — so the two renders
/// emit identical fill counts. That's the gap.
#[test]
fn maximized_chrome_hides_when_cursor_below_y_80() {
    let theme = Theme::dark();

    // Build a single-tab maximized window group.
    let tabs: Vec<(TabDescriptor, TileContent)> = vec![(
        TabDescriptor {
            id: 1,
            title: "Notes".into(),
            app_kind: Some(AppKind::Editor),
        },
        TileContent::Stub {
            kind: AppKind::Editor,
            label: "notes".into(),
        },
    )];
    let mut group = WindowGroup::new(100, tabs, /* active_id */ 1, WindowWidth::Normal);
    group.maximized = true;

    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let child = group.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(960.0),
                    height: Dimension::length(540.0),
                },
                ..Default::default()
            },
            &[child],
        )
        .unwrap();
    let mut tr = TextRenderer::new();
    let measure_map = node_map.clone();
    taffy
        .compute_layout_with_measure(
            root,
            Size {
                width: AvailableSpace::Definite(960.0),
                height: AvailableSpace::Definite(540.0),
            },
            |known, avail, node_id, _, _| {
                if let Some(&ptr) = measure_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, avail, &mut tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )
        .expect("compute_layout");
    group.apply_layout(&taffy, child, Point::ORIGIN);

    // First render: no cursor → concept "ambient hidden chrome".
    let mut scene_no_cursor = Scene::new();
    let presence = PresenceCtx::default();
    group.draw(
        &mut scene_no_cursor,
        Affine::IDENTITY,
        &mut tr,
        &theme,
        &presence,
    );
    let fills_no_cursor = collect_solid_colors(&scene_no_cursor).len();

    // Second render: drive the cursor to y=200 (well below the y≤80
    // reveal band). Even after observing the cursor, concept hides
    // chrome.
    use winit::event::{Modifiers, WindowEvent};
    use winit::dpi::PhysicalPosition;
    let cursor_event = WindowEvent::CursorMoved {
        device_id: unsafe { std::mem::zeroed() },
        position: PhysicalPosition::new(480.0, 200.0),
    };
    let mut handled = false;
    let mut focus: Option<NodeId> = None;
    let mods = Modifiers::default();
    let mut doc = automerge::AutoCommit::new();
    let doc_root = automerge::ROOT.clone();
    group.handle_event(
        &cursor_event,
        1.0,
        &mods,
        &mut doc,
        &doc_root,
        &mut handled,
        &mut focus,
        child,
        &taffy,
    );

    let mut scene_below = Scene::new();
    group.draw(&mut scene_below, Affine::IDENTITY, &mut tr, &theme, &presence);
    let fills_below = collect_solid_colors(&scene_below).len();

    // Expected concept behaviour: cursor below y=80 → chrome hidden →
    // strictly fewer fills than the (cursor-absent OR cursor-above)
    // case. Today these are equal because the chrome is unconditional.
    assert!(
        fills_below < fills_no_cursor,
        "Maximized WindowGroup must hide its floating chrome (title +
         Restore + close) when the cursor sits below y=80 (concept TSX
         WindowFrame.tsx:108-123). Observed identical fill counts:
         no_cursor={fills_no_cursor}, below={fills_below}. Audit rows \
         `concept-coverage.md:20` and `:377-381`: ours always draws the \
         maximized chrome. Fix in `quick-ui/src/widget/window_group.rs:659+` \
         by gating the maximized chrome on `last_cursor_y <= 80.0` (with \
         a fade-in/out for polish)."
    );
}

// ===========================================================================
// 13. TopBar SystemMonitor — sparkline bars are emerald-500, not indigo
// ===========================================================================

/// Audit row: `concept-coverage.md:23,160-162` — "Concept SystemMonitor
/// sparkline (`TopBarWidgets.tsx:120-124`): bars are `bg-emerald-500/30`.
/// **Ours uses `theme.accent_primary` (indigo-500)** — wrong colour
/// family entirely."
///
/// Concept ref: `nirios-concept/src/components/TopBarWidgets.tsx:120-124`.
///
/// emerald-500 = #10b981 = (16, 185, 129).
///
/// Sparkline bars are *fills* (not glyph runs) so they're easy to
/// detect in the encoding stream. Today the sparkline emits 20 indigo-
/// 500 fills; the concept paints them at 30%-alpha emerald-500. We
/// assert by colour-presence: the rendered scene must contain at least
/// one emerald-500-coloured fill (with or without the 30% alpha
/// premultiplied).
///
/// This test, paired with #10 (CPU label emerald-400) and #11 (RAM
/// label blue-400), pins the *whole* "different semantic colour per
/// metric" cue per the audit's row-5 fix-hint.
#[test]
fn topbar_system_monitor_sparkline_is_emerald_500_not_indigo() {
    let tree = WorkspaceTree::bootstrap_default();
    let mut chrome = TopBarChrome::from_tree(&tree);
    chrome.set_center_widget_for_tests(CenterWidget::SystemMonitor);

    let theme = Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let child = chrome.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(1280.0),
                    height: Dimension::length(48.0),
                },
                ..Default::default()
            },
            &[child],
        )
        .unwrap();
    let mut tr = TextRenderer::new();
    let measure_map = node_map.clone();
    taffy
        .compute_layout_with_measure(
            root,
            Size {
                width: AvailableSpace::Definite(1280.0),
                height: AvailableSpace::Definite(48.0),
            },
            |known, avail, node_id, _, _| {
                if let Some(&ptr) = measure_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, avail, &mut tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )
        .expect("compute_layout");
    chrome.apply_layout(&taffy, child, Point::ORIGIN);
    let mut scene = Scene::new();
    let presence = PresenceCtx::default();
    chrome.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

    let fills = collect_solid_colors(&scene);
    // emerald-500 = #10b981 = (16, 185, 129). The 30%-alpha variant
    // would have premul rgb of (5, 56, 39); we accept either nominal
    // or premul as evidence the concept colour was used.
    let has_emerald_500_full =
        contains_color_close(&fills, (16, 185, 129), 4);
    let has_emerald_500_30pct = contains_color_close(&fills, (5, 56, 39), 4);

    assert!(
        has_emerald_500_full || has_emerald_500_30pct,
        "TopBar SystemMonitor sparkline bars must render in emerald-500
         #10b981 (concept TSX TopBarWidgets.tsx:120-124,
         `bg-emerald-500/30`). Observed fills: {fills:?}. Audit row \
         `concept-coverage.md:23,160-162`: ours uses \
         `theme.accent_primary` (indigo-500) — wrong colour family. Fix \
         in `topbar_chrome.rs:662-672` by swapping \
         `theme.accent_primary` → `theme.tokens.colors.semantic.success` \
         (or a dedicated `emerald-500/30` fill colour) on the sparkline \
         bar paint loop."
    );
}

// ===========================================================================
// Sanity unit tests for the helpers above
// ===========================================================================

#[cfg(test)]
mod helper_tests {
    use super::*;
    use vello::kurbo::Rect;
    use vello::peniko::Fill;

    /// Sanity-check `collect_solid_colors`: a Scene with a single fill
    /// of indigo-500 must report exactly one (99, 102, 241, 255) entry.
    /// Without this the spec tests above would silently misreport
    /// colour presence.
    #[test]
    fn collect_solid_colors_reports_single_indigo_fill() {
        let mut scene = Scene::new();
        let rect = Rect::new(0.0, 0.0, 10.0, 10.0);
        scene.fill(
            Fill::NonZero,
            vello::kurbo::Affine::IDENTITY,
            Color::rgb8(99, 102, 241),
            None,
            &rect,
        );
        let colors = collect_solid_colors(&scene);
        assert_eq!(colors.len(), 1, "expected exactly one solid-colour fill");
        assert_eq!(colors[0], (99, 102, 241, 255));
    }

    /// The rgb-tolerance helper must accept ≤ tol per channel and
    /// reject above. Pinned so the colour-presence assertions can't
    /// silently widen tolerance via a regression in this helper.
    #[test]
    fn contains_color_close_respects_tolerance() {
        let palette = vec![(99, 102, 241, 255)];
        // Inside tolerance.
        assert!(contains_color_close(&palette, (99, 102, 241), 0));
        assert!(contains_color_close(&palette, (101, 100, 240), 4));
        // Outside tolerance.
        assert!(!contains_color_close(&palette, (74, 222, 128), 8));
    }
}
