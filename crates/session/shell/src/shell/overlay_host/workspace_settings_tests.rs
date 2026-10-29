//! Workspace settings' name field, the shortcut guide's pill and the text
//! scale of the surfaces drawn over the workspace: what a person types, sees
//! and presses there.

use super::*;
use crate::shell::chrome_state::ChromeState;
use crate::shell::state::WorkspaceTree;
use ui::TextRole;
use ui::widget::Container;

/// A host laid out over a desktop-sized viewport, as the shots harness and
/// the live binary both produce.
fn host_with(tree: WorkspaceTree) -> ShellOverlayHost {
    let chrome = ChromeState::new(tree);
    chrome.borrow_mut().overlays.overview = true;
    let mut host = ShellOverlayHost::new(Box::new(Container::new())).with_chrome(chrome);
    host.rect = Rect::new(0.0, 0.0, 1600.0, 900.0);
    host.force_overview_settled(true);
    host
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn workspace_rename_selection_is_directional_accessible_and_owned_by_the_palette() {
    fn rename_arrows_collapse_selection_to_its_directional_edge() {
        for (key, expected) in [(NamedKey::ArrowLeft, 0), (NamedKey::ArrowRight, 4)] {
            for selection in [
                ui::text_selection::Selection {
                    anchor: 0,
                    cursor: 4,
                },
                ui::text_selection::Selection {
                    anchor: 4,
                    cursor: 0,
                },
            ] {
                let mut name = "Home".to_string();
                let (_, _, actual) = ShellOverlayHost::apply_workspace_rename_key(
                    &Key::Named(key),
                    false,
                    false,
                    &mut name,
                    selection,
                );
                assert_eq!(actual, ui::text_selection::Selection::collapsed(expected));
            }
        }
        let mut name = "Home".to_string();
        let (_, _, shifted) = ShellOverlayHost::apply_workspace_rename_key(
            &Key::Named(NamedKey::ArrowLeft),
            false,
            true,
            &mut name,
            ui::text_selection::Selection {
                anchor: 0,
                cursor: 4,
            },
        );
        assert_eq!(
            shifted,
            ui::text_selection::Selection {
                anchor: 0,
                cursor: 3
            }
        );
    }

    fn workspace_select_all_invalidates_painted_selection() {
        let mut name = "Home".to_string();
        let (_, bump, selection) = ShellOverlayHost::apply_workspace_rename_key(
            &Key::Character("a".into()),
            true,
            false,
            &mut name,
            ui::text_selection::Selection::collapsed(4),
        );
        assert_eq!(selection.range(), 0..4);
        assert!(bump, "selection changes need a fresh frame");
    }

    fn workspace_name_accessibility_exposes_current_selection() {
        fn find(node: &ui::a11y::AccessNode) -> Option<&ui::a11y::AccessNode> {
            if node.name.as_deref() == Some("Workspace name") {
                return Some(node);
            }
            node.children.iter().find_map(find)
        }
        let h = host_with(WorkspaceTree::bootstrap_default());
        {
            let mut state = h.chrome.as_ref().unwrap().borrow_mut();
            state.overlays.ws_settings = true;
            state.editing_workspace_name = Some("Home".into());
            state.editing_workspace_name_selection = ui::text_selection::Selection {
                anchor: 0,
                cursor: 4,
            };
        }
        let node = h.accessibility().unwrap();
        let selection = find(&node)
            .unwrap()
            .text_selection
            .as_ref()
            .expect("selection must be exposed");
        assert_eq!((selection.anchor, selection.focus), (0, 4));
    }

    fn desktop_palette_owns_keys_above_an_active_rename_field() {
        let mut h = host_with(WorkspaceTree::bootstrap_default());
        {
            let mut chrome = h.chrome.as_ref().unwrap().borrow_mut();
            chrome.overlays.ws_settings = true;
            chrome.editing_workspace_name = Some("Home".into());
        }
        h.palette_visible = true;
        let mut handled = false;
        assert!(!h.handle_workspace_rename_key(
            &Key::Character("Files".into()),
            winit::event::Modifiers::default(),
            &mut handled
        ));
        assert!(!handled);
        assert_eq!(
            h.chrome
                .as_ref()
                .unwrap()
                .borrow()
                .editing_workspace_name
                .as_deref(),
            Some("Home")
        );
    }

    fn an_input_method_types_into_the_rename_field_at_its_caret() {
        let mut h = host_with(WorkspaceTree::bootstrap_default());
        {
            let mut chrome = h.chrome.as_ref().unwrap().borrow_mut();
            chrome.overlays.ws_settings = true;
            chrome.editing_workspace_name = Some("Home".into());
            chrome.editing_workspace_name_selection = ui::text_selection::Selection::collapsed(4);
        }
        assert!(h.commit_into_chrome_field(" 家"));
        let mut handled = false;
        assert!(h.handle_workspace_rename_key(
            &Key::Named(NamedKey::Backspace),
            winit::event::Modifiers::default(),
            &mut handled
        ));
        assert!(h.commit_into_chrome_field("👍🏽"));
        let chrome = h.chrome.as_ref().unwrap().borrow();
        assert_eq!(chrome.editing_workspace_name.as_deref(), Some("Home 👍🏽"));
        assert_eq!(
            chrome.editing_workspace_name_selection,
            ui::text_selection::Selection::collapsed("Home 👍🏽".len())
        );
    }

    rename_arrows_collapse_selection_to_its_directional_edge();
    an_input_method_types_into_the_rename_field_at_its_caret();
    workspace_select_all_invalidates_painted_selection();
    workspace_name_accessibility_exposes_current_selection();
    desktop_palette_owns_keys_above_an_active_rename_field();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn help_and_hidden_controls_expose_no_stray_targets_and_scale_once() {
    fn app_content_has_no_floating_help_hit_target() {
        let mut h = host_with(WorkspaceTree::bootstrap_default());
        let chrome = h.chrome.clone().unwrap();
        crate::shell::window_actions::open_hosted(
            &mut chrome.borrow_mut(),
            crate::shell::state::AppKind::FileExplorer,
            "Files",
            crate::shell::window_actions::Destination::Beside,
        );
        chrome.borrow_mut().overlays = crate::shell::chrome_state::OverlayFlags::default();
        assert!(h.shortcut_help_button_is_suppressed());
        h.last_cursor = Some(h.shortcut_help_button_rect().center());
        let mut handled = false;
        h.handle_shell_mouse_click_event(&ui::synthetic_input::mouse_press(), &mut handled);
        assert!(!chrome.borrow().overlays.shortcut_help);
    }

    fn desktop_scrolled_settings_name_cannot_receive_projected_focus_or_hits() {
        let mut host = host_with(WorkspaceTree::bootstrap_default());
        {
            let mut state = host.chrome.as_ref().unwrap().borrow_mut();
            state.overlays.overview = false;
            state.overlays.ws_settings = true;
        }
        // The General page is laid out by its draw, as in a real frame.
        host.rect = Rect::new(0.0, 0.0, 960.0, 540.0);
        host.force_ws_settings_settled();
        host.draw_overview_and_ws_settings_overlays(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::dark(),
            host.overlay_flags(),
        );
        let root = host.accessibility().unwrap();
        let overlay = root
            .children
            .iter()
            .position(|node| node.name.as_deref() == Some("Workspace settings"))
            .unwrap();
        let name = root.children[overlay]
            .children
            .iter()
            .position(|node| node.name.as_deref() == Some("Workspace name"))
            .unwrap();
        let path = [overlay, name];
        assert!(host.synthetic_accessibility_rect(&path).is_some());
        assert!(host.workspace_settings_overlay.scroll_general(1000.0));
        let root = host.accessibility().unwrap();
        assert!(root.children[overlay].children[name].state.hidden);
        assert!(host.synthetic_accessibility_rect(&path).is_none());
        assert!(!host.focus_synthetic_accessibility(&path));
        assert!(
            host.chrome
                .as_ref()
                .unwrap()
                .borrow()
                .editing_workspace_name
                .is_none()
        );
    }

    fn desktop_help_text_is_scaled_once_and_inside_its_hit_target() {
        let mut host = host_with(WorkspaceTree::bootstrap_default());
        host.chrome.as_ref().unwrap().borrow_mut().overlays.overview = false;
        host.force_overview_settled(false);
        host.rect = Rect::new(0.0, 0.0, 960.0, 540.0);
        let theme = Theme::dark().with_text_scale(2.0);
        let mut renderer = TextRenderer::new();
        renderer.set_text_scale(2.0);
        let mut scene = Scene::new();
        host.draw_shortcut_help_button(&mut scene, Affine::IDENTITY, &mut renderer, &theme);
        let rect = host.shortcut_help_button_rect();
        let (width, height) = renderer.measure_style(
            &crate::shell::i18n::t("help-pill"),
            None,
            &theme.text(TextRole::Label).strong(),
        );
        assert!(
            rect.width() >= f64::from(width) + 24.0 && rect.height() >= f64::from(height) + 12.0
        );
        assert!(
            rect.x0 >= 0.0 && rect.y0 >= 0.0 && rect.x1 <= host.rect.x1 && rect.y1 <= host.rect.y1
        );
        assert!(
            scene
                .encoding()
                .resources
                .glyph_runs
                .iter()
                .all(|run| run.font_size == theme.tokens.typography.size(TextRole::Label))
        );
    }

    fn desktop_overlay_text_applies_accessibility_scale_once() {
        let mut h = host_with(WorkspaceTree::bootstrap_default());
        let theme = Theme::dark().with_text_scale(1.5);
        let mut renderer = TextRenderer::new();
        let mut first = Scene::new();
        h.draw_overview_and_ws_settings_overlays(
            &mut first,
            Affine::IDENTITY,
            &mut renderer,
            &theme,
            h.overlay_flags(),
        );
        renderer.set_text_scale(1.5);
        let mut scaled = Scene::new();
        h.draw_overview_and_ws_settings_overlays(
            &mut scaled,
            Affine::IDENTITY,
            &mut renderer,
            &theme,
            h.overlay_flags(),
        );
        let sizes = |scene: &Scene| {
            scene
                .encoding()
                .resources
                .glyph_runs
                .iter()
                .map(|r| r.font_size)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            sizes(&first),
            sizes(&scaled),
            "already-scaled overlay tokens must render once"
        );
        assert_eq!(
            renderer.text_scale(),
            1.5,
            "restore the shared renderer for child widgets"
        );
    }

    app_content_has_no_floating_help_hit_target();
    desktop_scrolled_settings_name_cannot_receive_projected_focus_or_hits();
    desktop_help_text_is_scaled_once_and_inside_its_hit_target();
    desktop_overlay_text_applies_accessibility_scale_once();
}
