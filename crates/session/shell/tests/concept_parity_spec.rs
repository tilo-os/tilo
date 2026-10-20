//! Concept-parity spec tests: each test pins one gap from
//! `docs/tilo/concept-coverage.md` as a failing assertion (see per-test
//! headers for the audit row + concept TSX line). Tests render into a
//! `vello::Scene` and inspect the encoded `draw_data` for colour brushes /
//! draw counts (no GPU required); `OffscreenRenderer` pixel sampling is used
//! where noted.
//!
//! ## Anti-watering-down covenant — DO NOT BREAK
//! Never `#[ignore]` a failing test, weaken an assertion (loosen a
//! tolerance, `==` → `>=`), delete a test because it fails, or comment out
//! individual `assert*` calls. A failing test here is the specified
//! behaviour; closing the gap means fixing the code, not the test.

#![allow(
    clippy::bool_assert_comparison,
    reason = "spec assertions name the expected boolean state explicitly"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]
#![allow(
    clippy::similar_names,
    reason = "closure parameters 'avail' and 'known' are intentionally abbreviated for \
              closure-level readability in taffy measure callbacks; the parameter names \
              are terse by design and the pattern is consistent throughout"
)]
#![allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]

use shell::shell::state::FreshId as _;
use shell::shell::state::WorkspaceAuthority as _;
use std::collections::HashMap;

use taffy::{AvailableSpace, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;

use shell::shell::build_tilo_overlay_host_with_chrome;
use shell::shell::chrome_state::{Chrome, WsTab};
use shell::shell::state::WorkspaceTree;
use shell::shell::topbar_chrome::TopBarChrome;

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use ui::widget::command_palette::{CommandEntry, CommandPalette};
use ui::widget::{AppKind, TabDescriptor, TileContent, WindowGroup, WindowWidth};

use crate::support::host::{Layout, dispatch, dispatch_in, lay_out};

const W: f32 = 1600.0;
const H: f32 = 900.0;

/// Build, lay out, and render the full `ShellOverlayHost` tree at (W, H) —
/// the `e2e_shell_flows.rs` harness minus the PNG snapshot.
#[must_use]
fn render_overlay_host(tree: &WorkspaceTree) -> (Scene, Chrome) {
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(tree);
    let theme = Theme::dark();
    lay_out(&mut host, W, H);
    let mut tr = TextRenderer::new();

    let mut scene = Scene::new();
    let presence = PresenceCtx::default();
    host.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    (scene, chrome)
}

/// How many `u32`s a tag writes into `draw_data` — NOT `DrawTag::info_size()`
/// (a different buffer; the two only agree for `COLOR`). `IMAGE` reports 10
/// but writes 3, `BEGIN_CLIP` reports 1 but writes 2, etc. Over-advancing
/// silently desyncs the walker, so `None` means "unmeasured tag" and callers
/// must treat that as decode failure rather than guessing.
fn draw_data_words(tag: vello_encoding::DrawTag) -> Option<usize> {
    use vello_encoding::DrawTag;
    Some(match tag {
        DrawTag::COLOR => 1,
        DrawTag::IMAGE => 3,
        // Gradients and blur-rect coincide at 5 words; RADIAL_GRADIENT is the outlier at 7.
        DrawTag::LINEAR_GRADIENT | DrawTag::SWEEP_GRADIENT | DrawTag::BLUR_RECT => 5,
        DrawTag::RADIAL_GRADIENT => 7,
        DrawTag::BEGIN_CLIP => 2,
        DrawTag::END_CLIP => 0,
        _ => return None,
    })
}

/// Every solid-colour brush in a `Scene`'s draw stream as `(r, g, b, a)`.
/// peniko 0.6 / vello 0.9 pack as `[r, g, b, a]` LE; unpremultiplying is
/// lossless here since alpha == 0xff for every spec-relevant fill.
#[must_use]
fn collect_solid_colors(scene: &Scene) -> Vec<(u8, u8, u8, u8)> {
    use vello_encoding::DrawTag;
    let enc = scene.encoding();
    let mut out = Vec::new();
    let mut data_cursor = 0usize;
    for tag in &enc.draw_tags {
        let Some(words) = draw_data_words(*tag) else {
            panic!(
                "collect_solid_colors met an unmeasured draw tag ({:#x}); add its \
                 draw_data word count to `draw_data_words` rather than letting the \
                 cursor drift and silently corrupt every colour after this point",
                tag.0
            );
        };
        if *tag == DrawTag::COLOR && data_cursor < enc.draw_data.len() {
            let packed = enc.draw_data[data_cursor];
            let r = (packed & 0xff) as u8;
            let g = ((packed >> 8) & 0xff) as u8;
            let b = ((packed >> 16) & 0xff) as u8;
            let a = ((packed >> 24) & 0xff) as u8;
            out.push((r, g, b, a));
        }
        data_cursor = data_cursor.saturating_add(words);
    }
    out
}

/// True if any of `colors` is within `tol` per rgb channel of `target`.
#[must_use]
fn contains_color_close(colors: &[(u8, u8, u8, u8)], target: (u8, u8, u8), tol: u8) -> bool {
    colors.iter().any(|&(r, g, b, _a)| {
        r.abs_diff(target.0) <= tol && g.abs_diff(target.1) <= tol && b.abs_diff(target.2) <= tol
    })
}

fn open_quick_settings(chrome: &Chrome) {
    chrome.borrow_mut().overlays.quick_settings = true;
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn workspace_settings_renders_general_members_and_permission_tabs_from_real_state() {
    // Phase `workspace_settings_renders_general_tab_with_invite_link`.
    // Audit `concept-coverage.md:24,182-186` (concept:
    // `WorkspaceSettingsModal.tsx:95-149`): the General tab is just a Name
    // input. Asserted via an added theme-accent fill, since glyph text isn't
    // introspectable in the scene encoding.
    {
        // The person's own workspace, as a signed-in session adopts it: only
        // a person who may administer it is offered the invite action.
        let mut tree = WorkspaceTree::bootstrap_demo();
        tree.adopt_ownerless_workspaces(&shell::shell::state::local_user_id());

        let (baseline_scene, _baseline_chrome) = render_overlay_host(&tree);
        let baseline_fills = collect_solid_colors(&baseline_scene);

        let (mut host2, chrome2) = build_tilo_overlay_host_with_chrome(&tree);
        chrome2.borrow_mut().overlays.ws_settings = true;
        let theme = Theme::dark();
        lay_out(&mut host2, W, H);
        // Open, not at the start of its appearance from the pill.
        host2.force_ws_settings_settled();
        lay_out(&mut host2, W, H);
        let mut tr = TextRenderer::new();
        let mut open_scene = Scene::new();
        let presence = PresenceCtx::default();
        host2.draw(
            &mut open_scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &presence,
        );
        let open_fills = collect_solid_colors(&open_scene);

        // Assert on the theme's own accent, not a literal fill count: counting
        // indigo-500 pixels incentivized a decorative second fill over a fix.
        let accent = theme.tokens.colors.intent.accent;
        let acc = accent.to_rgba8();
        let count_accent = |fills: &[(u8, u8, u8, u8)]| {
            fills
                .iter()
                .filter(|&&(r, g, b, _)| {
                    r.abs_diff(acc.r) <= 4 && g.abs_diff(acc.g) <= 4 && b.abs_diff(acc.b) <= 4
                })
                .count()
        };
        let added_accent = count_accent(&open_fills).saturating_sub(count_accent(&baseline_fills));
        assert!(
            added_accent >= 1,
            "Opening WorkspaceSettings on the General tab must add at least one \
             accent-marked control — the invite link's Copy button is the tab's \
             primary action. Added {added_accent} accent fills over the \
             closed-modal baseline."
        );
    }

    // Phase `workspace_settings_renders_members_tab_with_the_real_roster`.
    // Audit `concept-coverage.md:24,187-191`: the per-role tab wasn't drawn.
    // The tab is now **Members**, rendering [`Workspace::roster`] (the
    // enforced membership) rather than the unused `Workspace::roles`.
    {
        let tree = tree_with_seeded_roster();

        let (general_fills, _) = render_with_ws_settings_tab(&tree, WsTab::General);
        let (roles_fills, _) = render_with_ws_settings_tab(&tree, WsTab::Members);

        // Members pane paints row-card + avatar + role-chip + Remove button per
        // member (>= 4 fills each); a 3-person roster clears the 20 threshold.
        let diff = roles_fills.abs_diff(general_fills);
        assert!(
            diff >= 20,
            "Switching to the Members tab must change the rendered output by
             at least 20 solid-colour fills (a row card + avatar + role chip +
             Remove button per member of the real roster). \
             Observed |members_fills - general_fills| = {diff} (general={general_fills}, \
             members={roles_fills}). If this drops to 0 the modal body is not \
             per-tab branching at all."
        );
    }

    // Phase `workspace_settings_renders_per_file_capabilities_as_permission_rows`.
    // Audit `concept-coverage.md:24,192-196`: the Files tab wasn't drawn.
    // Per-file access is now enforced via [`Scope::FsPath`] capabilities,
    // rendered as rows in the Permissions pane.
    {
        let tree = tree_with_seeded_roster();

        let (general_fills, _) = render_with_ws_settings_tab(&tree, WsTab::General);
        let (members_fills, _) = render_with_ws_settings_tab(&tree, WsTab::Members);
        let diff_g = members_fills.abs_diff(general_fills);
        assert!(
            diff_g > 0,
            "(precondition) the Members and General tabs must produce
             different renders before we can compare the Permissions pane; if
             this assertion fires, the Members spec test (#2) is the
             canonical place to fix it."
        );

        let (bare_permissions, _) = render_with_ws_settings_tab(&tree, WsTab::Permissions);

        let mut tree_with_files = tree_with_seeded_roster();
        let owner = shell::shell::state::local_user_id();
        if let Some(active) = tree_with_files.desktop.active_workspace_mut() {
            for i in 0..3 {
                active
                    .grant_as(
                        &owner,
                        shell::shell::state::CapOverride::grant(
                            shell::shell::state::Subject::app(format!("app-{i}")),
                            shell::shell::state::Scope::FsPath(format!(
                                "/var/workspace/doc-{i}.md"
                            )),
                            shell::shell::state::Access::Write,
                        ),
                    )
                    .expect("the owner may grant");
            }
        }
        let (files_filled, _) = render_with_ws_settings_tab(&tree_with_files, WsTab::Permissions);
        let added = files_filled.saturating_sub(bare_permissions);
        assert!(
            added >= 6,
            "The Permissions pane must paint a row per per-file capability.
             Adding 3 path-scoped grants must add ≥6 extra solid-colour fills
             (row card + avatar + expiry chip + Revoke button at minimum, per
             grant). Observed +{added} (with_3_files={files_filled},
             without={bare_permissions})."
        );
    }

    // Phase `ws_settings_members_tab_does_not_leak_the_name_edit_buffer`.
    // Regression pin: before the typed `WsTab` field, "Files" was selected via
    // `editing_workspace_name = Some("__files_tab__")`, leaking that literal
    // into the General Name input. A non-General render must be invariant to
    // `editing_workspace_name`.
    {
        let tree = WorkspaceTree::bootstrap_demo();

        let (mut host_a, chrome_a) = build_settled_workspace_settings_host(&tree);
        {
            let mut st = chrome_a.borrow_mut();
            st.overlays.ws_settings = true;
            st.ws_settings_tab = WsTab::Members;
            st.editing_workspace_name = None;
        }
        let scene_a = render_host_to_scene(&mut host_a);
        let fills_a = collect_solid_colors(&scene_a).len();

        // Post-migration, Members must ignore `editing_workspace_name` entirely,
        // so this render's fill count must match `fills_a` exactly.
        let (mut host_b, chrome_b) = build_settled_workspace_settings_host(&tree);
        {
            let mut st = chrome_b.borrow_mut();
            st.overlays.ws_settings = true;
            st.ws_settings_tab = WsTab::Members;
            st.editing_workspace_name = Some("__files_tab__".into());
        }
        let scene_b = render_host_to_scene(&mut host_b);
        let fills_b = collect_solid_colors(&scene_b).len();

        assert_eq!(
            fills_a, fills_b,
            "Members-tab render must be invariant to `editing_workspace_name` \
             contents — the typed `WsTab` field is the sole tab dispatch surface, \
             and the Name-input edit buffer must never bleed into the Members pane. \
             Got {fills_a} fills with editing_workspace_name=None vs {fills_b} \
             with editing_workspace_name=Some(\"__files_tab__\"); a difference \
             indicates the Members arm is still falling back to General body, \
             which paints the magic string into the Name input."
        );
    }

    // Phase `permissions_tab_body_is_invariant_to_the_name_edit_buffer`.
    // Regression pin: a non-General tab's pane must fully replace the General
    // body, which previously fell through underneath it. Same shape as
    // [`ws_settings_members_tab_does_not_leak_the_name_edit_buffer`].
    {
        let tree = WorkspaceTree::bootstrap_demo();

        let (mut host_a, chrome_a) = build_settled_workspace_settings_host(&tree);
        {
            let mut st = chrome_a.borrow_mut();
            st.overlays.ws_settings = true;
            st.ws_settings_tab = WsTab::Permissions;
            st.editing_workspace_name = None;
        }
        let scene_a = render_host_to_scene(&mut host_a);
        let fills_a = collect_solid_colors(&scene_a).len();

        // A leaking General body would re-emit the Name-input rect fill, the
        // editing-mode caret fill, and a thicker stroke.
        let (mut host_b, chrome_b) = build_settled_workspace_settings_host(&tree);
        {
            let mut st = chrome_b.borrow_mut();
            st.overlays.ws_settings = true;
            st.ws_settings_tab = WsTab::Permissions;
            st.editing_workspace_name = Some("Press Esc to close".into());
        }
        let scene_b = render_host_to_scene(&mut host_b);
        let fills_b = collect_solid_colors(&scene_b).len();

        assert_eq!(
            fills_a, fills_b,
            "Permissions-tab render must be invariant to `editing_workspace_name` \
             contents — when the Roles tab is active, the General body \
             (Name input, Invite Link, Copy button, Collaborators row) \
             must not run. Got {fills_a} fills \
             with editing_workspace_name=None vs {fills_b} with \
             editing_workspace_name=Some(\"Press Esc to close\"); a \
             difference indicates the General body is leaking into the \
             Permissions pane. Fix `draw_ws_settings` so the General body \
             block only runs in the `WsTab::General` arm."
        );
    }
}

#[test]
fn workspace_settings_invariance_fixture_keeps_its_scene_across_row_transitions() {
    let mut clock = tilo_test_env::ScopedEnv::new();
    clock.set("TILO_CLOCK", "09:41");
    let tree = WorkspaceTree::bootstrap_demo();
    let (mut host, chrome) = build_settled_workspace_settings_host(&tree);
    {
        let mut state = chrome.borrow_mut();
        state.overlays.ws_settings = true;
        state.ws_settings_tab = WsTab::Members;
    }
    let first = collect_solid_colors(&render_host_to_scene(&mut host));
    std::thread::sleep(ui::time::Duration::from_millis(2300));
    let later = collect_solid_colors(&render_host_to_scene(&mut host));
    assert_eq!(
        first, later,
        "the same settled pane must keep its full scene"
    );
}

/// Apply real reduced-motion preferences to the embedded apps as well as the
/// shell. A theme alone is insufficient: `EmbeddedApp` preserves its own a11y
/// preferences, so Notes' page-tree row transitions could change unrelated fills
/// between the two pane renders. Warm through the ordinary production draw.
fn build_settled_workspace_settings_host(
    tree: &WorkspaceTree,
) -> (shell::shell::overlay_host::ShellOverlayHost, Chrome) {
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(tree);
    host.set_motion_reduced(true);
    let _ = render_host_to_scene(&mut host);
    // The first layout can mount embedded apps. Apply the real preference to
    // those new children before measuring the settled pane, then draw again.
    host.set_motion_reduced(true);
    let _ = render_host_to_scene(&mut host);
    // Initial fixture record writes can finish asynchronously and show a Saved
    // bubble. Reduced motion removes its fade, not its finite lifetime. Wait
    // for the production hosts to report rest before comparing settled panes.
    let deadline = ui::time::now()
        .checked_add(ui::time::Duration::from_secs(10))
        .expect("fixture deadline fits the monotonic clock");
    let mut document = automerge::AutoCommit::new();
    loop {
        host.update(&document, &automerge::ROOT);
        host.sync(&document, &automerge::ROOT);
        host.settle(
            &mut ui::widget::Edits::lend(&mut document),
            &automerge::ROOT,
        );
        host.set_motion_reduced(true);
        let _ = render_host_to_scene(&mut host);
        if !host.has_pending_visual_work() && !host.has_transient_feedback() {
            break;
        }
        assert!(
            ui::time::now() < deadline,
            "workspace settings fixture did not settle: {:?}",
            pending_visual_names(&host)
        );
        std::thread::sleep(ui::time::Duration::from_millis(16));
    }
    (host, chrome)
}

fn pending_visual_names(widget: &dyn Widget) -> Vec<String> {
    if !widget.has_pending_visual_work() && !widget.has_transient_feedback() {
        return Vec::new();
    }
    let mut names = vec![format!("{}:{:?}", widget.name(), widget.test_id())];
    for child in widget.children() {
        names.extend(pending_visual_names(child));
    }
    names
}

/// Layout + draw an already-configured `ShellOverlayHost`, sharing plumbing
/// with `render_with_ws_settings_tab`.
#[must_use]
fn render_host_to_scene(host: &mut shell::shell::overlay_host::ShellOverlayHost) -> Scene {
    // Compare pane content, not the wall-clock sample of its entrance.
    // The real reduced-motion path opens the same production lens at its
    // settled geometry before either name-buffer invariant is measured.
    let theme = Theme::dark().with_reduce_motion(true);
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let child = host.build_layout(&mut taffy, &theme, &mut node_map);
    let root_node = taffy
        .new_with_children(
            Style {
                size: Size::from_lengths(W, H),
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
    scene
}

/// Active workspace with real membership (owner + 2 invitees) via
/// `grant_as`, since `bootstrap_demo`'s workspace is ownerless (no members,
/// the correct fail-closed default) — useless as a membership fixture.
#[must_use]
fn tree_with_seeded_roster() -> WorkspaceTree {
    use tilo_action_policy::{Change, Role};
    let mut tree = WorkspaceTree::bootstrap_demo();
    let owner = shell::shell::state::local_user_id();
    tree.adopt_ownerless_workspaces(&owner);
    if let Some(ws) = tree.desktop.active_workspace_mut() {
        for (person, role) in [
            ("ada", Role::Editor),
            ("bo", Role::Viewer),
            ("cy", Role::Commenter),
            ("dee", Role::Viewer),
            ("eli", Role::Editor),
        ] {
            ws.govern_as_at(
                &owner,
                Change::AddMember {
                    person: person.into(),
                    role,
                },
                tilo_clock::unix_now(),
            )
            .expect("the owner may write a membership");
        }
    }
    tree
}

/// Render with the modal open at the given tab. `ws_settings_tab` is the
/// typed dispatch field, replacing an `editing_workspace_name` side-channel.
#[must_use]
fn render_with_ws_settings_tab(tree: &WorkspaceTree, tab: WsTab) -> (usize, Chrome) {
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(tree);
    {
        let mut st = chrome.borrow_mut();
        st.overlays.ws_settings = true;
        st.ws_settings_tab = tab;
    }
    let theme = Theme::dark();
    lay_out(&mut host, W, H);
    host.force_ws_settings_settled();
    lay_out(&mut host, W, H);
    let mut tr = TextRenderer::new();
    let mut scene = Scene::new();
    let presence = PresenceCtx::default();
    host.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    (collect_solid_colors(&scene).len(), chrome)
}

#[test]
fn global_search_renders_its_icon_kbd_chip_and_indigo_selection() {
    // Phase `global_search_renders_leading_search_icon`.
    // Audit `concept-coverage.md:25,204-207` (`GlobalSearch.tsx:138-153`):
    // concept has a leading lucide `Search` icon, ours none. Glyphs emit no
    // `DrawTag::COLOR`, so this asserts by fill-count delta.
    {
        let mut palette = CommandPalette::new();
        palette.push(CommandEntry::new("ws.create", "Create Workspace"));
        palette.push(CommandEntry::new("file.open", "Open File"));
        let theme = Theme::dark();
        lay_out(&mut palette, 640.0, 400.0);
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        let presence = PresenceCtx::default();
        palette.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

        let fills = collect_solid_colors(&scene);

        // Baseline draw = card + input surfaces/strokes + row highlight (~6 fills
        // for 2 entries); the icon should add >= 1 more. Threshold: >= 8 present,
        // <= 7 absent.
        assert!(
            fills.len() >= 8,
            "GlobalSearch palette must paint a leading lucide Search icon
             (concept TSX GlobalSearch.tsx:138-153). Observed only {} solid-
             colour fills in the standalone palette render — the icon path
             would have added at least one. Audit row `concept-coverage.md:25,204-207`.",
            fills.len(),
        );
    }

    // Phase `global_search_renders_cmd_k_kbd_chip`.
    // Audit `concept-coverage.md:25,204-207`: same TSX line, but the trailing
    // "Cmd+K" kbd chip (pill surface + border, +2 fills minimum).
    {
        let mut palette = CommandPalette::new();
        palette.push(CommandEntry::new("ws.create", "Create Workspace"));
        let theme = Theme::dark();
        lay_out(&mut palette, 640.0, 400.0);
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        let presence = PresenceCtx::default();
        palette.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

        let fills = collect_solid_colors(&scene);

        // icon (>= 1) + input (>= 2) + kbd chip (>= 2) + row highlight (>= 1) +
        // card (2) = >= 8 minimum; kbd-chip presence needs one step above test
        // #4's threshold to isolate it.
        assert!(
            fills.len() >= 9,
            "GlobalSearch palette must paint a trailing Cmd+K kbd chip
             (concept TSX GlobalSearch.tsx:138-153). Observed {} solid-
             colour fills — chip would have added at least 2 (body + border).
             Audit row `concept-coverage.md:25,204-207`.",
            fills.len(),
        );
    }

    // Phase `global_search_selected_row_is_indigo_500_not_grey`.
    // Audit `concept-coverage.md:25,217-221` (`GlobalSearch.tsx:251`): the
    // selected row must be the accent, not `neutral[4]` mid-grey.
    {
        let mut palette = CommandPalette::new();
        palette.push(CommandEntry::new("ws.create", "Create Workspace"));
        palette.push(CommandEntry::new("file.open", "Open File"));
        let theme = Theme::dark();
        lay_out(&mut palette, 640.0, 400.0);
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        let presence = PresenceCtx::default();
        palette.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

        let fills = collect_solid_colors(&scene);

        // Assert the theme's own accent — no theme's accent need equal the
        // mockup's literal indigo-500.
        let accent = theme.tokens.colors.intent.accent.to_rgba8();
        let has_accent = contains_color_close(&fills, (accent.r, accent.g, accent.b), 4);
        let has_slate_600_fill = contains_color_close(&fills, (71, 85, 105), 4);

        assert!(
            has_accent,
            "Selected row in the GlobalSearch palette must be picked out with \
             the theme's accent ({}, {}, {}), not left as a neutral row. \
             Observed solid-colour fills: {fills:?}. \
             has_slate_600_fill={has_slate_600_fill} (when present, that's \
             the stale grey highlight).",
            accent.r, accent.g, accent.b
        );
    }
}

#[test]
fn quick_settings_renders_power_utility_white_slider_fill_and_wifi_drill_in() {
    // Power is a named footer button beside Settings and Lock, as quiet as
    // they are: a red wash marked it as danger when opening the session menu
    // destroys nothing. `qs_actions_click_routing` covers that it is there,
    // reached by keyboard and opens the session menu.

    // Phase `quick_settings_slider_fill_is_white_not_indigo`.
    // Audit `concept-coverage.md:22,132-139` (`QuickSettings.tsx:204-215`):
    // concept sliders fill white, ours `accent[5]` indigo. Whites present when
    // closed (window cards, knobs) are discounted.
    {
        let tree = WorkspaceTree::bootstrap_demo();

        let (closed_scene, _) = render_overlay_host(&tree);
        let closed_fills = collect_solid_colors(&closed_scene);
        let closed_indigo_count = closed_fills
            .iter()
            .filter(|&&(r, g, b, _)| {
                r.abs_diff(0x63) <= 4 && g.abs_diff(0x66) <= 4 && b.abs_diff(0xf1) <= 4
            })
            .count();

        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        open_quick_settings(&chrome);
        let theme = Theme::dark();
        lay_out(&mut host, W, H);
        let mut tr = TextRenderer::new();
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

        // Concept sliders are white, not indigo — opening the popover must not
        // add indigo-500 fills (ours currently adds 2, one per slider).
        let added_indigo = open_indigo_count.saturating_sub(closed_indigo_count);
        assert_eq!(
            added_indigo, 0,
            "Opening QuickSettings must not add any indigo-500 fills — the
             slider fill colour per concept TSX QuickSettings.tsx:204-215
             is `bg-white`, not the accent. Observed: closed={closed_indigo_count} \
             indigo fills, open={open_indigo_count}, delta={added_indigo}. \
             Audit row `concept-coverage.md:22,132-139`: 'no `bg-white` filled \
             ramp — just `accent[5]` (indigo) bars'. Fix: swap \
             `colors.intent.accent` for `colors.text_primary` (white in dark theme) \
             in the slider-fill paint call in `draw_quick_settings`."
        );
    }

    // Phase `quick_settings_supports_wifi_drill_in_render`.
    // Audit `concept-coverage.md:22,148` (`QuickSettings.tsx:108-128,101-162`):
    // `nav_stack` is dead state, no WiFi/BT drill-in renders. Asserts a
    // drilled-to-Wifi render differs from Root.
    {
        // `QuickSettingsState::nav_stack` is the production dispatch source read
        // by the renderer; deep-link the authoritative chrome state directly.
        let tree = WorkspaceTree::bootstrap_demo();

        let (root_fills, _) = render_quick_settings_open(&tree, |_| {});

        let (wifi_fills, _) = render_quick_settings_open(&tree, |chrome| {
            let mut chrome = chrome.borrow_mut();
            chrome.quick_settings.radios.wifi_available = true;
            chrome.quick_settings.wifi = false;
            chrome.open_quick_settings_section(shell::shell::quick_settings::QsSection::Wifi);
        });

        // EXPECTED: root_fills != wifi_fills. CURRENT: equal (drill-in unwired).
        assert_ne!(
            root_fills, wifi_fills,
            "QuickSettings drill-in into Wifi level must produce a different
             render than Root level (concept TSX QuickSettings.tsx:108-128, \
             101-162). Currently both render identically because \
             `draw_quick_settings` ignores the production navigation stack. Audit row \
             `concept-coverage.md:22,148`: 'nav_stack is dead state'."
        );
    }
}

#[must_use]
fn render_quick_settings_open(
    tree: &WorkspaceTree,
    mutate_chrome: impl FnOnce(&Chrome),
) -> (usize, Chrome) {
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(tree);
    chrome.borrow_mut().overlays.quick_settings = true;
    mutate_chrome(&chrome);
    let theme = Theme::dark();
    lay_out(&mut host, W, H);
    // Open, with any section it opens standing open: a section unfolds in
    // place on the disclosure motion, so its first frame shows none of it.
    host.force_quick_settings_settled();
    lay_out(&mut host, W, H);
    let mut tr = TextRenderer::new();
    let mut scene = Scene::new();
    let presence = PresenceCtx::default();
    host.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    (collect_solid_colors(&scene).len(), chrome)
}

/// A maximized window keeps its bar: the same bar, in the same place, as
/// when it isn't, its maximize control now offering Restore beside its
/// close. Both show at rest, and a cursor in the app changes nothing.
#[test]
fn a_maximized_window_keeps_its_bar_with_restore_and_close_at_rest() {
    let theme = Theme::dark().with_reduce_motion(true);

    let tabs: Vec<(TabDescriptor, TileContent)> = vec![(
        TabDescriptor {
            app_kind: Some(AppKind::Editor),
            ..TabDescriptor::new(1, "Notes")
        },
        TileContent::Stub {
            kind: AppKind::Editor,
            label: "notes".into(),
        },
    )];
    let mut group = WindowGroup::new(100, tabs, /* active_id */ 1, WindowWidth::Normal);
    group.maximized = true;

    let Layout { taffy, child_node } = lay_out(&mut group, 960.0, 540.0);
    let mut tr = TextRenderer::new();
    let presence = PresenceCtx::default();
    let mut fills = |group: &mut WindowGroup| {
        let mut scene = Scene::new();
        group.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        collect_solid_colors(&scene).len()
    };

    let fills_no_cursor = fills(&mut group);
    let bounds = group.presentation_bounds();
    let restore = bounds.restore.expect("the bar offers Restore at rest");
    let close = bounds.close.expect("and its close control");
    assert_eq!(
        bounds.maximize, None,
        "a maximized window offers no Maximize"
    );
    for control in [restore, close] {
        assert!(
            bounds.tab_bar.contains(control.center()),
            "the control {control:?} stands in the window's bar {:?}",
            bounds.tab_bar
        );
    }
    let client = bounds.client.expect("the window's client is laid out");
    assert!(
        client.y0 >= bounds.tab_bar.y1 - 0.5,
        "the bar takes its own room above the client: bar {:?}, client {client:?}",
        bounds.tab_bar
    );

    let cursor_event = ui::synthetic_input::cursor_moved(480.0, 200.0);
    let modifiers = winit::event::Modifiers::default();
    dispatch(&mut group, &taffy, child_node, modifiers, &cursor_event);
    let fills_in_app = fills(&mut group);
    assert_eq!(
        fills_in_app, fills_no_cursor,
        "a cursor in the app changes nothing about the bar"
    );
    assert_eq!(group.presentation_bounds().restore, Some(restore));
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn topbar_avatar_cluster_paints_exactly_the_present_collaborators() {
    // Phase `topbar_right_cluster_paints_avatar_stack_when_collaborators_present`.
    // The right cluster must show per-peer avatars when the active workspace
    // has peers, wired through `TopBarChrome::sync_from_tree` / `draw`. Uses
    // this test's own two-peer fixture so the assertion doesn't depend on
    // shipped fake participants; paired with the first-boot test below.
    {
        use shell::shell::state::Collaborator;

        let mut tree = WorkspaceTree::bootstrap_default();
        let active_id = tree.desktop.active;
        // Colours are stated explicitly so the hex checked below is a property of
        // this fixture, not whatever the local user id happens to hash to.
        assert!(tree.admit_collaborator(
            active_id,
            Collaborator::new("alice", "Alice").with_cursor_color("#ec4899"),
        ));
        assert!(tree.admit_collaborator(
            active_id,
            Collaborator::new("bob", "Bob").with_cursor_color("#f97316"),
        ));

        let active_ws = tree
            .desktop
            .active_workspace()
            .expect("the tree leaves an active workspace");
        let active_collabs = active_ws.collaborators.len();
        let tree_collabs = tree.desktop.collaborators.len();
        assert_eq!(
            active_collabs, 2,
            "the fixture must put exactly the two admitted peers on the \
             active workspace, else the paint assertions below prove nothing"
        );

        let mut chrome = TopBarChrome::from_tree(&tree.desktop);
        let theme = Theme::dark();
        lay_out(&mut chrome, 1280.0, 48.0);
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        let presence = PresenceCtx::default();
        chrome.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

        assert!(
            !chrome.collaborator_initials().is_empty(),
            "TopBarChrome failed to pick up any collaborator initials from the \
             tree (active ws has {active_collabs}, \
             tree-level has {tree_collabs}). Likely cause: \
             `sync_from_tree` reads from the wrong roster source."
        );

        let fills = collect_solid_colors(&scene);
        // Each peer's cursor colour (`#ec4899` for Alice, `#f97316` for
        // Bob) is painted as its presence mark: the theme's presence
        // palette keeps the hue and normalises lightness and chroma.
        // Tolerance: ±8 per channel.
        let presence_mark = |r, g, b| {
            let rgba = theme
                .tokens
                .colors
                .presence
                .of(Color::from_rgb8(r, g, b))
                .to_rgba8();
            (rgba.r, rgba.g, rgba.b)
        };
        let has_alice = contains_color_close(&fills, presence_mark(0xec, 0x48, 0x99), 8);
        let has_bob = contains_color_close(&fills, presence_mark(0xf9, 0x73, 0x16), 8);
        assert!(
            has_alice && has_bob,
            "TopBar right cluster must paint both peers in their own colour \
             (Alice #ec4899 present: {has_alice}; Bob #f97316 present: \
             {has_bob}; concept `tilo-concept/src/data.ts`). Observed solid \
             fills: {fills:?}. If this fails, check that \
             `TopBarChrome::sync_from_tree` reads from the active workspace's \
             own collaborator list (with a tree-level fallback) and that \
             `roster_colors` still honours an explicit `cursor_color`."
        );
    }

    // Phase `topbar_right_cluster_paints_no_avatar_on_a_first_boot`.
    // The reverse: a first boot has no collaborators, so the cluster must
    // paint none — a fabricated avatar would misrepresent presence and write a
    // fake person into the user's own workspace doc. Asserted through
    // `persist::load_or_default` -> `bootstrap_default`.
    {
        let tree = WorkspaceTree::bootstrap_default();
        let active_ws = tree
            .desktop
            .active_workspace()
            .expect("bootstrap_default leaves an active workspace");
        assert!(
            active_ws.collaborators.is_empty() && tree.desktop.collaborators.is_empty(),
            "a first-boot tree must have an empty roster — a name here is a \
             person who does not exist, persisted into the user's workspace \
             document. Active ws has {}, tree-level has {}.",
            active_ws.collaborators.len(),
            tree.desktop.collaborators.len()
        );

        let mut chrome = TopBarChrome::from_tree(&tree.desktop);
        let theme = Theme::dark();
        lay_out(&mut chrome, 1280.0, 48.0);
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        let presence = PresenceCtx::default();
        chrome.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

        assert!(
            chrome.collaborator_initials().is_empty(),
            "TopBarChrome synthesised initials {:?} from an empty first-boot \
             roster — an initial in the avatar stack names a person.",
            chrome.collaborator_initials()
        );

        // Neither canonical peer hue may appear; checking painted fills (not the
        // cached roster) catches a hard-coded placeholder avatar in `draw`.
        let fills = collect_solid_colors(&scene);
        assert!(
            !contains_color_close(&fills, (236, 72, 153), 8),
            "first-boot top bar painted Alice's cursor hue #ec4899. Observed \
             solid fills: {fills:?}"
        );
        assert!(
            !contains_color_close(&fills, (249, 115, 22), 8),
            "first-boot top bar painted Bob's cursor hue #f97316. Observed \
             solid fills: {fills:?}"
        );
    }

    // Phase `topbar_right_cluster_paints_nobody_when_the_workspace_is_empty`.
    // A workspace nobody has joined paints no avatars — the empty case is what
    // distinguishes an empty roster from a fabricated one, since both satisfy
    // "something painted".
    {
        let tree = WorkspaceTree::bootstrap_default();
        let mut chrome = TopBarChrome::from_tree(&tree.desktop);

        assert!(
            chrome.collaborator_initials().is_empty(),
            "a freshly booted workspace nobody has joined must show no \
             avatars; got {:?}",
            chrome.collaborator_initials()
        );
        assert_eq!(
            chrome.collaborator_overflow(),
            0,
            "and no \"+N\" chip standing in for peers that do not exist"
        );

        let theme = Theme::dark();
        lay_out(&mut chrome, 1280.0, 48.0);
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        let presence = PresenceCtx::default();
        chrome.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

        // None of these fills may appear — exactly what a fabricated-peer
        // regression would bring back.
        let fills = collect_solid_colors(&scene);
        assert!(
            !contains_color_close(&fills, (236, 72, 153), 8),
            "the top bar paints pink-500 (#ec4899) with nobody in the \
             workspace — a fabricated peer is back. Observed: {fills:?}"
        );
        assert!(
            !contains_color_close(&fills, (249, 115, 22), 8),
            "the top bar paints orange-500 (#f97316) with nobody in the \
             workspace — a fabricated peer is back. Observed: {fills:?}"
        );
    }

    // Phase `two_peers_arriving_off_the_wire_paint_two_avatars_on_the_bar`.
    // Two peers, wire to painted avatars, through the event path: an empty
    // first-boot tree fed the exact `InboundPresence` records
    // the network hands the compositor. Don't seed `ws.collaborators` directly
    // (the presence path must fill it) and don't drop the empty-before half.
    {
        use shell::shell::network::InboundPresence;
        use shell::shell::peer_presence::{AnchoredPointers, fold_inbound, place_pointers};
        use ui::presence::PresenceKey;

        let mut tree = WorkspaceTree::bootstrap_default();
        let room = tree
            .desktop
            .active_workspace()
            .expect("active workspace")
            .room;

        let before = TopBarChrome::from_tree(&tree.desktop);
        assert!(
            before.collaborator_initials().is_empty(),
            "the bar must start empty, or the assertions below prove nothing"
        );

        // Exactly what the network layer buffers: a pointer frame per
        // publishing session, anchored to one of the room's tabs.
        let key = serde_json::to_string(&PresenceKey::tile_pointer(room.to_string(), 1))
            .expect("a tile pointer key serialises");
        let frames = vec![
            InboundPresence {
                peer: 0x11,
                user: "bob".into(),
                key: key.clone(),
                point: (100.0, 100.0),
                ts: 0,
                cleared: false,
            },
            InboundPresence {
                peer: 0x22,
                user: "dana".into(),
                key,
                point: (400.0, 300.0),
                ts: 0,
                cleared: false,
            },
        ];

        // Names are a governance fact written by the room's authority via
        // `apply_room_changes`; the frame-announced names are deliberately not
        // consulted, so the bar below proves names came from the room, not the wire.
        let room_says = |_: &str| {
            std::collections::BTreeMap::from([
                (0x11u64, "bob".to_string()),
                (0x22u64, "dana".to_string()),
            ])
        };
        let this_room = |_: &str| true;

        let mut presence = PresenceCtx::default();
        let mut anchored = AnchoredPointers::new();
        let now = std::time::Instant::now();
        let arrived = fold_inbound(
            frames,
            &mut anchored,
            &mut presence,
            &room_says,
            &this_room,
            now,
        );
        assert_eq!(arrived.len(), 2, "two room-scoped frames must land");
        let tiles = [(1u64, vello::kurbo::Rect::new(0.0, 40.0, 800.0, 700.0))];
        assert!(
            place_pointers(&anchored, &arrived, &mut presence, &tiles, now),
            "and be placed over this desk's tile for their tab"
        );
        for (peer, name) in [(0x11u64, "bob"), (0x22u64, "dana")] {
            assert!(
                tree.admit_collaborator_in_room(
                    room,
                    tilo_desktop_model::Collaborator::new(name, name).with_session(peer)
                ),
                "{name} arriving must change the roster"
            );
        }

        let mut chrome = TopBarChrome::from_tree(&tree.desktop);
        let mut initials: Vec<char> = chrome.collaborator_initials().to_vec();
        initials.sort_unstable();
        assert_eq!(
            initials,
            vec!['B', 'D'],
            "the bar must name the two people who published, not their \
             session handles"
        );
        assert_eq!(
            chrome.collaborator_overflow(),
            0,
            "two peers is two chips and no overflow"
        );

        let fills = layout_topbar_and_collect_fills(&mut chrome, &presence);

        let colors = chrome.collaborator_colors();
        assert_eq!(colors.len(), 2, "one colour per painted chip");
        assert_ne!(
            colors[0], colors[1],
            "two people in one room must not be painted the same colour"
        );
        for c in colors {
            let rgba = c.to_rgba8();
            assert!(
                contains_color_close(&fills, (rgba.r, rgba.g, rgba.b), 8),
                "a peer's resolved colour {:?} never reached the scene; \
                 observed fills: {fills:?}",
                (rgba.r, rgba.g, rgba.b)
            );
        }
    }

    // Phase `topbar_avatar_cluster_renders_peer_hued_chips_through_shared_avatar`.
    // The cluster renders through the shared `ui::Avatar`: 5 peers -> 3 chips
    // plus a "+2" overflow, each chip in its own cursor hue rather than one
    // flat accent. Asserts all three hues plus non-empty glyph runs.
    {
        use shell::shell::state::{Collaborator, Workspace, WorkspaceId, WorkspaceTree};

        let mut ws = Workspace::new(WorkspaceId::new(), "Home");
        ws.collaborators = vec![
            Collaborator::new("alice", "Ada"),
            Collaborator::new("bob", "Bo"),
            Collaborator::new("charlie", "Cy"),
            Collaborator::new("dave", "Di"),
            Collaborator::new("erin", "Ed"),
        ];
        let ws_id = ws.id;
        let mut tree = WorkspaceTree {
            desktop: shell::shell::state::DesktopTree {
                workspaces: vec![ws],
                active: ws_id,
                active_strip: HashMap::default(),
                active_group: HashMap::default(),
                collaborators: vec![],
                ..Default::default()
            },
            ..Default::default()
        };
        tree.desktop.active = ws_id;

        let mut chrome = TopBarChrome::from_tree(&tree.desktop);
        // Contract: 3 chips (peers beyond the local user, capped at 3) + a "+2"
        // overflow.
        assert_eq!(
            chrome.collaborator_initials().len(),
            3,
            "cluster must show exactly three avatar chips for five peers",
        );
        assert_eq!(
            chrome.collaborator_overflow(),
            2,
            "the remaining two peers must flow into the overflow chip",
        );

        let theme = Theme::dark();
        lay_out(&mut chrome, 1280.0, 48.0);
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        let presence = PresenceCtx::default();
        chrome.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

        let fills = collect_solid_colors(&scene);
        // Each chip wears its own identity hue (`ui::presence::user_color`),
        // painted through the theme's presence palette: one lightness and
        // chroma for everybody.
        for who in ["alice", "bob", "charlie"] {
            let c = theme
                .tokens
                .colors
                .presence
                .of_rgb(ui::presence::user_color(who))
                .to_rgba8();
            assert!(
                contains_color_close(&fills, (c.r, c.g, c.b), 8),
                "cluster must paint {who}'s chip through the shared avatar; fills: {fills:?}",
            );
        }
        assert!(
            !scene.encoding().resources.glyph_runs.is_empty(),
            "cluster must paint initials and the overflow label as glyph runs",
        );
    }
}

/// Lay `chrome` out at the topbar's fixed 1280x48 viewport, draw it, and
/// collect the scene's solid fills. Split out of
/// [`two_peers_arriving_off_the_wire_paint_two_avatars_on_the_bar`] to keep
/// that test short.
fn layout_topbar_and_collect_fills(
    chrome: &mut TopBarChrome,
    presence: &PresenceCtx,
) -> Vec<(u8, u8, u8, u8)> {
    let theme = Theme::dark();
    lay_out(chrome, 1280.0, 48.0);
    let mut tr = TextRenderer::new();
    let mut scene = Scene::new();
    chrome.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, presence);
    collect_solid_colors(&scene)
}

/// Concept ships a create-affordance at the tail of the overview's
/// workspace list. Asserts `add_workspace_rect` is populated after
/// layout+draw and sits below the last card; the click wiring is pinned by
/// `overview_add_workspace_click_appends_workspace`.
mod overview_add_workspace {
    use super::*;
    use shell::shell::overlay_host::ShellOverlayHost;
    use ui::synthetic_input::{cursor_moved, mouse_press};
    use winit::event::Modifiers;

    /// Keeps the taffy tree + child node alive so tests can pump events after
    /// layout — `handle_event` indexes `children[0]`, so a leaf isn't enough.
    struct OverviewHarness {
        host: ShellOverlayHost,
        chrome: Chrome,
        taffy: TaffyTree<()>,
        root_node: NodeId,
    }

    /// Build the host, settle the overview open, layout+draw.
    #[must_use]
    fn build_settled_overview() -> OverviewHarness {
        let tree = WorkspaceTree::bootstrap_demo();
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        chrome.borrow_mut().overlays.overview = true;
        host.force_overview_settled(true);

        let theme = Theme::dark();
        let Layout { taffy, child_node } = lay_out(&mut host, W, H);
        let mut tr = TextRenderer::new();

        let mut scene = Scene::new();
        let presence = PresenceCtx::default();
        host.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        OverviewHarness {
            host,
            chrome,
            taffy,
            root_node: child_node,
        }
    }

    #[test]
    fn overview_add_workspace_button_renders_and_click_appends_workspace() {
        // Phase `overview_renders_add_workspace_button`.
        // Overview must paint a visible add-workspace affordance at the list
        // tail: non-empty hit rect, positioned below the last workspace card.
        {
            let h = build_settled_overview();
            let rect = h.host.add_workspace_rect_for_tests();

            assert!(
                rect.area() > 0.0,
                "overview must render a visible 'Add workspace' affordance — \
                 cached hit rect is still ZERO after a full layout + draw with \
                 the overview settled open. Concept ref: \
                 tilo-concept/src/components/OverviewView.tsx (create-affordance)."
            );

            // No public row-geometry probe, so this uses the draw path's y-coord
            // constants; the bound is loose (tail-of-list can move) but still catches
            // a regression to "above the cards".
            assert!(
                rect.y0 > 100.0,
                "add-workspace tile must sit below the header and at least one \
                 workspace row; observed y0={:.1}",
                rect.y0,
            );
        }

        // Phase `overview_add_workspace_click_appends_workspace`.
        // Clicking the add-workspace rect must append + activate a workspace
        // (create-and-switch) and release the overview's input grab
        // so the user can immediately navigate the new workspace.
        {
            let mut h = build_settled_overview();
            let rect = h.host.add_workspace_rect_for_tests();
            assert!(
                rect.area() > 0.0,
                "add-workspace rect must be populated before clicking"
            );

            let initial_count = h.chrome.borrow().tree().desktop.workspaces.len();
            assert!(
                initial_count >= 1,
                "bootstrap_demo carries at least one workspace"
            );
            let initial_active = h.chrome.borrow().tree().desktop.active;

            // Simulate winit's cursor-then-press sequence; handle_event reads
            // last_cursor on MouseInput::Pressed.
            let centre = rect.center();
            let mut doc = automerge::AutoCommit::new();
            let modifiers = Modifiers::default();
            for event in [cursor_moved(centre.x, centre.y), mouse_press()] {
                dispatch_in(
                    &mut doc,
                    &mut h.host,
                    &h.taffy,
                    h.root_node,
                    modifiers,
                    &event,
                );
            }

            let st = h.chrome.borrow();
            assert_eq!(
                st.tree().desktop.workspaces.len(),
                initial_count + 1,
                "clicking the overview's '+ Add workspace' tile must append a \
                 new workspace via chrome.add_workspace()"
            );
            assert_ne!(
                st.tree().desktop.active,
                initial_active,
                "the newly-appended workspace must be activated: \
                 create-and-switch semantics"
            );
            assert!(
                !st.overlays.overview,
                "creating a workspace must dismiss overview so navigation reaches the new workspace"
            );
            let active_id = st.tree().desktop.active;
            assert!(
                st.tree()
                    .desktop
                    .workspaces
                    .iter()
                    .any(|w| w.id == active_id),
                "active id must point at one of the workspaces in the tree"
            );
        }
    }
}

// Sanity unit tests for the helpers above

#[cfg(test)]
mod helper_tests {
    use super::*;
    use vello::kurbo::Rect;
    use vello::peniko::Fill;

    #[test]
    fn colour_helpers_find_solid_fills_within_tolerance() {
        // Phase `collect_solid_colors_reports_single_indigo_fill`.
        // Guards `collect_solid_colors`: one indigo-500 fill must decode to
        // exactly one (99, 102, 241, 255) entry.
        {
            let mut scene = Scene::new();
            let rect = Rect::new(0.0, 0.0, 10.0, 10.0);
            scene.fill(
                Fill::NonZero,
                vello::kurbo::Affine::IDENTITY,
                Color::from_rgb8(99, 102, 241),
                None,
                &rect,
            );
            let colors = collect_solid_colors(&scene);
            assert_eq!(colors.len(), 1, "expected exactly one solid-colour fill");
            assert_eq!(colors[0], (99, 102, 241, 255));
        }

        // Phase `contains_color_close_respects_tolerance`.
        // Guards the tolerance helper's boundary.
        {
            let palette = vec![(99, 102, 241, 255)];
            assert!(contains_color_close(&palette, (99, 102, 241), 0));
            assert!(contains_color_close(&palette, (101, 100, 240), 4));
            assert!(!contains_color_close(&palette, (74, 222, 128), 8));
        }
    }
}

/// The overview's access line + avatar stack must read [`Workspace::roster`]
/// (live membership) rather than `ws.collaborators` — showing someone on a
/// room states they can reach it. Asserted against `room_people`, the
/// function draw itself calls, not by counting avatar-coloured fills. Both
/// the member and listed-only-empty cases are pinned; don't relax either.
#[test]
fn overview_avatars_come_from_the_roster_not_the_collaborator_list() {
    use shell::shell::state::Collaborator;
    let room_people = |ws: &shell::shell::state::Workspace, tree: &WorkspaceTree| {
        let me = shell::shell::state::local_user_id();
        tilo_desktop::overview::room_people(
            ws,
            &tree.desktop,
            &shell::shell::overview::ShellOverview::for_user(&me),
            tilo_clock::unix_now(),
        )
    };

    // Listed but powerless: collaborator records everywhere, no capability
    // anywhere.
    let mut listed_only = WorkspaceTree::bootstrap_demo();
    for ws in &mut listed_only.desktop.workspaces {
        ws.policy.owner = None;
        ws.policy.members.clear();
        ws.collaborators = vec![
            Collaborator {
                id: "ada".into(),
                name: "Ada".into(),
                ..Default::default()
            },
            Collaborator {
                id: "bob".into(),
                name: "Bob".into(),
                ..Default::default()
            },
        ];
    }
    for ws in &listed_only.desktop.workspaces {
        assert!(
            room_people(ws, &listed_only).is_empty(),
            "a person who holds no capability in the workspace gets no face on \
             its room; workspace {:?} showed {:?}",
            ws.name,
            room_people(ws, &listed_only)
                .iter()
                .map(|p| p.id.clone())
                .collect::<Vec<_>>()
        );
    }

    let mut with_member = listed_only.clone();
    with_member.desktop.workspaces[0].admit("ada", tilo_action_policy::Role::Editor);

    let faces = room_people(&with_member.desktop.workspaces[0], &with_member);
    assert_eq!(
        faces.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
        vec!["ada"],
        "the member who holds a capability is exactly who the room shows"
    );
    assert_eq!(
        faces[0].name.as_str(),
        "Ada",
        "the collaborator record still supplies the display name"
    );

    for ws in with_member.desktop.workspaces.iter().skip(1) {
        assert!(
            room_people(ws, &with_member).is_empty(),
            "granting in one workspace must not put a face on another's row"
        );
    }
}

/// The word table in [`draw_data_words`] must account for every word of a
/// real render's `draw_data`: every colour assertion here walks that stream
/// by hand, so a wrong size doesn't fail loudly, it desyncs the cursor and
/// silently corrupts later fills — how a 10-vs-3-word `IMAGE` bug hid until
/// an image-heavy scene made the drift fatal.
#[test]
fn draw_data_word_table_accounts_for_the_whole_stream() {
    let tree = WorkspaceTree::bootstrap_demo();
    let (scene, _chrome) = render_overlay_host(&tree);
    let enc = scene.encoding();
    let mut walked = 0usize;
    for tag in &enc.draw_tags {
        let words = draw_data_words(*tag).unwrap_or_else(|| {
            panic!(
                "draw tag {:#x} has no measured draw_data size; add it to \
                 `draw_data_words` (vello_encoding::draw annotates several \
                 as `scene: N`) rather than letting the cursor drift",
                tag.0
            )
        });
        walked = walked.saturating_add(words);
    }
    assert_eq!(
        walked,
        enc.draw_data.len(),
        "the draw_data word table must consume the stream exactly; a \
         mismatch means some tag's size is wrong and every colour decoded \
         after it is suspect"
    );
}

#[allow(
    unused_imports,
    reason = "The native WorkspaceExt methods are selected by host-specific workspace code and test configurations."
)]
use shell::shell::state::WorkspaceExt as _;
