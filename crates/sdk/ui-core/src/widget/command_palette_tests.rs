//! The command palette: geometry, rows, sections and what it says to
//! assistive technology.

use super::*;
use crate::testing::{Dispatch, dispatch};
use taffy::TaffyTree;
use winit::event::Modifiers;
use winit::keyboard::SmolStr;

fn key_press(logical_key: Key) -> WindowEvent {
    crate::synthetic_input::keyboard_input(crate::synthetic_input::key_event(
        logical_key,
        None,
        ElementState::Pressed,
    ))
}

#[test]
fn palette_card_geometry_and_rows() {
    fn large_text_palette_reserves_painted_rows_and_scales_fonts_once() {
        let mut p = CommandPalette::new();
        p.push(
            CommandEntry::new("files", "Open Files")
                .description("Opens beside")
                .group("Applications"),
        );
        p.push(
            CommandEntry::new("editor", "Open Editor")
                .description("Opens beside")
                .group("Applications"),
        );
        p.open();
        let theme = Theme::dark().with_text_scale(2.0);
        let mut tree = TaffyTree::new();
        p.build_layout(&mut tree, &theme, &mut std::collections::HashMap::new());
        let (_, height) = p.desired_size();
        p.set_rect(Rect::new(16.0, 96.0, 784.0, 96.0 + height));
        let mut tr = TextRenderer::new();
        tr.set_text_scale(2.0);
        let (_, label_h) = tr.measure(
            "Open Files",
            theme.tokens.typography.size(TextRole::Caption) / 2.0,
            None,
        );
        let (_, desc_h) = tr.measure(
            "Opens beside",
            theme.tokens.typography.size(TextRole::Caption) / 2.0,
            None,
        );
        assert!(p.row_height() >= f64::from(label_h + desc_h) + 6.0);
        let mut scene = Scene::new();
        p.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &PresenceCtx::default(),
        );
        let runs = &scene.encoding().resources.glyph_runs;
        assert!(
            runs.iter()
                .all(|run| run.font_size <= theme.tokens.typography.size(TextRole::Body))
        );
        assert!(runs.iter().any(|run| {
            (run.font_size - theme.tokens.typography.size(TextRole::Caption)).abs() < 0.01
        }));
        assert!(p.row_rect(1).unwrap().y1 <= p.list_bottom_y());
        tr.set_text_scale(1.0); // palette paint's already-scaled theme space
        let mut rows_scene = Scene::new();
        p.draw_result_rows(&mut rows_scene, Affine::IDENTITY, &mut tr, &theme);
        let first = p.row_rect(0).unwrap();
        let second = p.row_rect(1).unwrap();
        let header = Rect::new(first.x0, p.list_top_y(), first.x1, first.y0);
        for run in &rows_scene.encoding().resources.glyph_runs {
            let top = f64::from(run.transform.translation[1]);
            let (_, height) = tr.measure("Ag", run.font_size, None);
            assert!(
                [header, first, second]
                    .iter()
                    .any(|band| top >= band.y0 - 0.01 && top + f64::from(height) <= band.y1 + 0.01),
                "painted glyph line escapes row/header: y={top}, h={height}"
            );
        }
    }

    fn palette_card_is_a_panel() {
        let theme = Theme::dark();
        assert_eq!(
            CommandPalette::surface_radius_px(&theme),
            f64::from(theme.tokens.radii.panel),
            "the palette is a floating panel"
        );
    }

    /// Pins that nothing may reserve a slot for a kbd chip in the query row.
    fn input_row_layout_gives_the_query_the_full_trailing_width() {
        const ROW_W: f64 = 640.0;
        const EDGE: f64 = 12.0;
        let palette = CommandPalette::new();
        let layout = palette.compute_input_layout(Rect::new(0.0, 0.0, ROW_W, 56.0));
        assert!(
            layout.icon_rect.width() >= 24.0 && layout.icon_rect.width() <= 40.0,
            "leading icon slot must be 24..=40 px wide; got {}",
            layout.icon_rect.width()
        );
        assert!(
            layout.input_rect.x0 > layout.icon_rect.x1,
            "input must start after the icon"
        );
        assert!(
            layout.input_rect.x1 <= ROW_W - EDGE,
            "input must stay inside the row's trailing inset"
        );
        assert_eq!(
            ROW_W - layout.input_rect.x1,
            EDGE,
            "the query runs to the trailing inset — no reserved chip slot \
         between it and the row edge",
        );
    }

    fn row_at_finds_visible_row_inside_the_post_layout_rect() {
        let mut p = CommandPalette::new();
        p.push(CommandEntry::new("a", "Alpha"));
        p.push(CommandEntry::new("b", "Bravo"));
        // Must accommodate input row + filter strip + 2 result rows.
        p.rect = Rect::new(0.0, 0.0, 360.0, 280.0);
        let mid_row_1 = p.row_rect(1).unwrap().center();
        assert_eq!(p.row_at(mid_row_1), Some(1));
        assert!(p.row_at(Point::new(1000.0, 1000.0)).is_none());
    }

    fn the_card_ends_with_its_last_row_and_no_footer() {
        let mut p = CommandPalette::new();
        p.set_bypass_fuzzy(true);
        p.set_entries(vec![
            CommandEntry::new("a", "Alpha"),
            CommandEntry::new("b", "Bravo"),
        ]);
        let (w, h) = p.desired_size();
        p.set_rect(Rect::new(0.0, 0.0, w, h));
        let last = p.row_rect(1).expect("the second row is painted");
        assert!(
            (p.rect.y1 - PADDING - last.y1).abs() < 0.01,
            "the card ends one inset below its last row: {last:?} in {:?}",
            p.rect
        );
    }

    fn each_row_is_as_tall_as_its_content() {
        let mut p = CommandPalette::new();
        p.set_bypass_fuzzy(true);
        p.set_entries(vec![
            CommandEntry::new("git", "Git")
                .description("Browses Git history")
                .group("Applications"),
            CommandEntry::new("bazaar", "Bazaar").group("Host Apps"),
            CommandEntry::new("chromium", "Ungoogled Chromium").group("Host Apps"),
            CommandEntry::new("web", "Search the web")
                .description("Web")
                .group("Web"),
        ]);
        let (w, h) = p.desired_size();
        p.set_rect(Rect::new(0.0, 0.0, w, h));
        let heights: Vec<f64> = (0..4)
            .map(|i| p.row_rect(i).expect("every row is painted").height())
            .collect();
        assert_eq!(
            heights,
            vec![
                TWO_LINE_ROW_HEIGHT,
                ROW_HEIGHT,
                ROW_HEIGHT,
                TWO_LINE_ROW_HEIGHT
            ],
            "a title alone is a single-line row, a title over a secondary line a two-line row"
        );
        let bazaar = p.row_rect(1).unwrap();
        let chromium = p.row_rect(2).unwrap();
        assert!(
            (chromium.y0 - bazaar.y1).abs() < 0.01,
            "rows of one group touch: {bazaar:?} {chromium:?}"
        );
        assert_eq!(p.row_at(chromium.center()), Some(2));
        let last = p.row_rect(3).unwrap();
        assert!(
            (p.rect.y1 - PADDING - last.y1).abs() < 0.01,
            "the card fits the mixed rows exactly"
        );
    }

    large_text_palette_reserves_painted_rows_and_scales_fonts_once();
    each_row_is_as_tall_as_its_content();
    palette_card_is_a_panel();
    input_row_layout_gives_the_query_the_full_trailing_width();
    row_at_finds_visible_row_inside_the_post_layout_rect();
    the_card_ends_with_its_last_row_and_no_footer();
}

#[test]
fn typing_narrows_the_list_with_feedback() {
    fn typing_filters_visible_entries() {
        let mut p = CommandPalette::new();
        p.push(CommandEntry::new("a", "Alpha"));
        p.push(CommandEntry::new("h", "Hotel"));
        p.push(CommandEntry::new("b", "Bravo"));
        p.set_focus(true);

        let total = p.entries.len();
        assert_eq!(p.visible_entries().len(), total);

        let event = key_press(Key::Character(SmolStr::new("h")));
        let handled = dispatch(&mut p, &event);

        assert_eq!(p.query(), "h", "typing must update the query string");
        assert!(
            p.visible_entries().len() < total,
            "typing 'h' must filter out non-matching entries (visible={}, total={})",
            p.visible_entries().len(),
            total,
        );
        assert!(handled, "the palette must consume the keystroke");
    }

    fn empty_search_exposes_feedback_and_recovery() {
        let mut p = CommandPalette::new();
        p.open();
        p.set_rect(Rect::new(20.0, 30.0, 380.0, 310.0));
        p.set_query("missing");
        let node = Widget::accessibility(&p).unwrap();
        let results = &node.children[1];
        assert!(results.children.iter().any(|child| child.name.as_deref()
            == Some("No results for “missing”")
            && child.description.as_deref() == Some("Check the spelling, or try another word")));
    }

    typing_filters_visible_entries();
    empty_search_exposes_feedback_and_recovery();
}

#[test]
fn cursor_selection_clamps_and_tracks_filtered_entries() {
    fn set_cursor_clamps_to_visible_len() {
        let mut p = CommandPalette::new();
        p.push(CommandEntry::new("a", "Alpha"));
        p.push(CommandEntry::new("b", "Bravo"));
        p.set_cursor(99);
        assert_eq!(p.cursor(), 1);
    }

    fn selected_id_reflects_cursor_position() {
        let mut p = CommandPalette::new();
        p.push(CommandEntry::new("a", "Alpha"));
        p.push(CommandEntry::new("b", "Bravo"));
        assert_eq!(p.selected_id(), Some("a"));
        p.set_cursor(1);
        assert_eq!(p.selected_id(), Some("b"));
    }

    fn selected_id_is_none_when_filter_removes_everything() {
        let mut p = CommandPalette::new();
        p.push(CommandEntry::new("a", "Alpha"));
        p.set_filter(Box::new(|_| false));
        assert_eq!(p.selected_id(), None);
    }

    fn pointer_hover_and_click_select_the_painted_row_through_enter_path() {
        let mut p = CommandPalette::new();
        p.push(CommandEntry::new("alpha", "Alpha"));
        p.push(CommandEntry::new("bravo", "Bravo"));
        p.open();
        p.set_rect(Rect::new(0.0, 0.0, 360.0, 280.0));
        let row = p.row_rect(1).expect("second result is painted");
        let modifiers = Modifiers::default();
        let mut doc = automerge::AutoCommit::new();

        assert!(p.handle_overlay_key(
            &crate::synthetic_input::cursor_moved(row.center().x, row.center().y),
            modifiers,
            &mut crate::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert_eq!(p.cursor(), 1, "hover highlights the hit-tested row");
        assert_eq!(
            Widget::cursor_shape(&p, row.center()),
            Some(winit::window::CursorIcon::Pointer)
        );
        assert!(p.handle_overlay_key(
            &crate::synthetic_input::mouse_press(),
            modifiers,
            &mut crate::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert_eq!(
            p.take_outcome(),
            Some(CommandOutcome {
                id: "bravo".into(),
                argument: None
            })
        );
    }

    fn semantic_result_focus_and_activation_use_the_same_selected_entry_path() {
        let mut p = CommandPalette::commands();
        p.push(CommandEntry::new("alpha", "Alpha"));
        p.push(CommandEntry::new("bravo", "Bravo"));
        p.open();
        p.set_rect(Rect::new(0.0, 0.0, 480.0, 260.0));
        let target = Widget::accessibility(&p).unwrap().children[1].children[1]
            .action_target
            .clone()
            .expect("result has a stable semantic target");
        let mut doc = automerge::AutoCommit::new();

        assert!(Widget::perform_semantic_action(
            &mut p,
            &target,
            &crate::a11y::Action::Focus,
            &mut crate::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert_eq!(p.selected_id(), Some("bravo"));
        assert!(Widget::perform_semantic_action(
            &mut p,
            &target,
            &crate::a11y::Action::Activate,
            &mut crate::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert_eq!(
            p.take_outcome(),
            Some(CommandOutcome {
                id: "bravo".into(),
                argument: None
            })
        );
        assert!(!p.is_open(), "command activation closes exactly like Enter");
    }

    set_cursor_clamps_to_visible_len();
    selected_id_reflects_cursor_position();
    selected_id_is_none_when_filter_removes_everything();
    pointer_hover_and_click_select_the_painted_row_through_enter_path();
    semantic_result_focus_and_activation_use_the_same_selected_entry_path();
}

#[test]
fn resource_actions_enqueue_addresses_and_bind_arguments() {
    fn immediate_resource_action_enqueues_an_address_not_a_private_outcome() {
        let spec = tilo_actions::ActionSpec::operation(
            "save",
            "Save",
            tilo_actions::OperationKind::Effect,
            "test.save",
            &(),
        )
        .unwrap();
        let resources = vec![
            tilo_actions::Resource::new("document:1", "document", "Document").with_action(spec),
        ];
        let flat = tilo_actions::flatten(&resources);
        let mut palette = CommandPalette::commands();
        palette.set_entries(vec![CommandEntry::from_flat_action(&flat[0])]);
        palette.open();
        palette.activate_selected(
            &mut crate::widget::Edits::lend(&mut automerge::AutoCommit::new()),
            &automerge::ROOT,
        );

        assert!(palette.take_outcome().is_none());
        let invocation = crate::widget::controls::take_control_invocation(&mut palette).unwrap();
        assert_eq!(invocation.address, ActionAddress::new("document:1", "save"));
        assert!(invocation.arguments.is_empty());
    }

    fn prompted_resource_action_binds_its_declared_argument() {
        let spec = tilo_actions::ActionSpec::prompted_param(
            "rename",
            "Rename",
            tilo_actions::ActionParam::required(
                PROMPTED_PARAM,
                tilo_actions::ParamType::Text,
                "New name",
            ),
            "name…",
            tilo_actions::ExecutionLocus::ClientLocal,
            |name| tilo_actions::EncodedAction::encode_operation("test.rename", &name).unwrap(),
        );
        let resources = vec![
            tilo_actions::Resource::new("document:1", "document", "Document").with_action(spec),
        ];
        let flat = tilo_actions::flatten(&resources);
        let entry = CommandEntry::from_flat_action(&flat[0]);
        let mut palette = CommandPalette::commands();
        palette.open_action_argument(entry, Some("Project Atlas".into()));

        let handled = dispatch(&mut palette, &key_press(Key::Named(NamedKey::Enter)));
        assert!(handled);
        assert!(palette.take_outcome().is_none());
        let invocation = crate::widget::controls::take_control_invocation(&mut palette).unwrap();
        assert_eq!(
            invocation.address,
            ActionAddress::new("document:1", "rename")
        );
        assert_eq!(invocation.arguments[PROMPTED_PARAM], "Project Atlas");

        let encoded = flat[0].action.build_json(&invocation.arguments).unwrap();
        assert_eq!(encoded.type_id, "test.rename");
    }

    /// The palette's typed text lands in the parameter the action names,
    /// whatever it is called: the prompt is the description.
    fn prompted_action_with_a_named_parameter_gets_the_typed_text_under_that_name() {
        let spec = tilo_actions::ActionSpec::prompted_param(
            "rename",
            "Rename",
            tilo_actions::ActionParam::required(
                "title",
                tilo_actions::ParamType::Text,
                "The note's new title",
            ),
            "title…",
            tilo_actions::ExecutionLocus::ClientLocal,
            |title| tilo_actions::EncodedAction::encode_operation("test.rename", &title).unwrap(),
        );
        let resources = vec![
            tilo_actions::Resource::new("document:1", "document", "Document").with_action(spec),
        ];
        let flat = tilo_actions::flatten(&resources);
        let entry = CommandEntry::from_flat_action(&flat[0]);
        let mut palette = CommandPalette::commands();
        palette.open_action_argument(entry, Some("Project Atlas".into()));
        assert!(dispatch(
            &mut palette,
            &key_press(Key::Named(NamedKey::Enter))
        ));
        let invocation = crate::widget::controls::take_control_invocation(&mut palette).unwrap();
        assert_eq!(invocation.arguments["title"], "Project Atlas");
        assert!(!invocation.arguments.contains_key(PROMPTED_PARAM));
        let encoded = flat[0].action.build_json(&invocation.arguments).unwrap();
        assert_eq!(encoded.type_id, "test.rename");
    }

    fn typed_resource_action_binds_its_primary_parameter() {
        let spec = tilo_actions::ActionSpec::typed_operation(
            "go-to-cell",
            "Go to cell",
            "Cell reference",
            tilo_actions::OperationKind::View,
            "test.go",
            vec![tilo_actions::ActionParam::required_owned(
                "reference",
                tilo_actions::ParamType::Text,
                "Cell reference",
            )],
            |args| {
                tilo_actions::EncodedAction::encode_operation(
                    "test.go",
                    &args.text("reference").unwrap_or_default(),
                )
                .unwrap()
            },
        );
        let resources = vec![
            tilo_actions::Resource::new("document:1", "document", "Document").with_action(spec),
        ];
        let flat = tilo_actions::flatten(&resources);
        let entry = CommandEntry::from_flat_action(&flat[0]);
        let mut palette = CommandPalette::commands();
        palette.open_action_argument(entry, Some("B12".into()));

        dispatch(&mut palette, &key_press(Key::Named(NamedKey::Enter)));
        let invocation = crate::widget::controls::take_control_invocation(&mut palette).unwrap();
        assert_eq!(invocation.arguments["reference"], "B12");
        let encoded = flat[0].action.build_json(&invocation.arguments).unwrap();
        assert_eq!(
            encoded.payload,
            serde_json::to_vec("B12").unwrap(),
            "the typed builder reads the text"
        );
    }

    fn semantic_shortcut_labels_follow_the_palette_platform() {
        use crate::platform::{Platform, PlatformConventions};

        let entry = CommandEntry::new("find", "Find").shortcut(Shortcut::command("F"));
        assert_eq!(
            entry.keybinding_label(PlatformConventions::new(Platform::MacOs)),
            Some("⌘F".into())
        );
        assert_eq!(
            entry.keybinding_label(PlatformConventions::new(Platform::Windows)),
            Some("Ctrl+F".into())
        );
    }

    immediate_resource_action_enqueues_an_address_not_a_private_outcome();
    prompted_resource_action_binds_its_declared_argument();
    prompted_action_with_a_named_parameter_gets_the_typed_text_under_that_name();
    typed_resource_action_binds_its_primary_parameter();
    semantic_shortcut_labels_follow_the_palette_platform();
}

#[test]
fn search_filters_toggle_scope_and_filetypes() {
    fn search_filter_full_text_toggles() {
        let mut f = SearchFilter::default();
        assert!(!f.full_text);
        f.toggle_full_text();
        assert!(f.full_text);
        f.toggle_full_text();
        assert!(!f.full_text);
    }

    fn search_filter_filetypes_multi_select() {
        let mut f = SearchFilter::default();
        f.toggle_filetype(Filetype::Code);
        f.toggle_filetype(Filetype::Docs);
        assert!(f.filetypes.contains(&Filetype::Code));
        assert!(f.filetypes.contains(&Filetype::Docs));
        f.toggle_filetype(Filetype::Code);
        assert!(!f.filetypes.contains(&Filetype::Code));
        assert!(f.filetypes.contains(&Filetype::Docs));
    }

    fn search_filter_default_scope_is_workspace() {
        let f = SearchFilter::default();
        assert_eq!(f.scope, SearchScope::Workspace);
    }

    fn filter_strip_sits_below_input_row_and_above_results() {
        let mut p = CommandPalette::new();
        p.push(CommandEntry::new("a", "Alpha"));
        p.rect = Rect::new(0.0, 0.0, 360.0, 280.0);
        let input = p.input_row_rect();
        let strip = p.filter_strip_rect();
        let row0 = p.row_rect(0).expect("seeded row 0 must lay out");
        assert!(
            strip.y0 >= input.y1,
            "filter strip must start at or below input row (input.y1={}, strip.y0={})",
            input.y1,
            strip.y0,
        );
        assert!(
            strip.y1 <= row0.y0,
            "results list must start at or below filter strip (strip.y1={}, row0.y0={})",
            strip.y1,
            row0.y0,
        );
        assert!(
            strip.height() >= 32.0,
            "filter strip must be tall enough for chip pills (got {})",
            strip.height(),
        );
    }

    fn a_host_filter_strip_is_reserved_on_the_cards_inset() {
        let mut p = CommandPalette::new();
        p.set_host_filter_strip(true);
        p.set_rect(Rect::new(0.0, 0.0, 640.0, 400.0));
        let band = p.filter_strip_rect();
        let field = p.input_row_rect();
        assert_eq!(
            band.x0, field.x0,
            "the host band must share the field's left edge"
        );
        assert_eq!(band.x1, field.x1);
        assert_eq!(band.x0, p.rect.x0 + p.content_inset_px());
        assert!(band.y0 >= field.y1, "the band sits under the field");
        assert!(
            p.list_top_y() >= band.y1,
            "rows start below the band, not behind it"
        );
    }

    fn a_host_can_reserve_more_than_one_row_of_filter_band() {
        let mut one = CommandPalette::new();
        one.set_host_filter_strip(true);
        one.set_rect(Rect::new(0.0, 0.0, 640.0, 400.0));
        let (_, one_high) = one.desired_size();
        let one_band = one.filter_strip_rect();

        let mut two = CommandPalette::new();
        two.set_host_filter_strip(true);
        assert!(
            two.set_host_filter_strip_rows(2),
            "asking for a different row count is a change"
        );
        assert!(
            !two.set_host_filter_strip_rows(2),
            "asking for the same row count again is not"
        );
        two.set_rect(Rect::new(0.0, 0.0, 640.0, 400.0));
        let (_, two_high) = two.desired_size();
        let band = two.filter_strip_rect();

        assert_eq!(two.host_filter_strip_rows(), 2);
        assert_eq!(
            band.height(),
            one_band.height() + two.filter_strip_row_height(),
            "the band grows by exactly one row"
        );
        assert_eq!(
            two_high - one_high,
            two.filter_strip_row_height(),
            "and so does the card, so nothing else moves"
        );
        assert!(
            two.list_top_y() >= band.y1,
            "rows still start below the band: {} vs {}",
            two.list_top_y(),
            band.y1
        );
        assert_eq!(
            band.y0, one_band.y0,
            "the band still starts under the field — it grows downward"
        );
    }

    search_filter_full_text_toggles();
    search_filter_filetypes_multi_select();
    search_filter_default_scope_is_workspace();
    filter_strip_sits_below_input_row_and_above_results();
    a_host_filter_strip_is_reserved_on_the_cards_inset();
    a_host_can_reserve_more_than_one_row_of_filter_band();
}

#[test]
fn query_editing_follows_the_caret_selection_and_ime() {
    fn ime_preedit_is_visible_but_filters_only_after_atomic_commit() {
        let mut palette = CommandPalette::new();
        palette.push(CommandEntry::new("jp", "東京"));
        palette.push(CommandEntry::new("en", "London"));
        palette.set_focus(true);
        palette.rect = Rect::new(0.0, 0.0, 420.0, 260.0);
        let mut document = automerge::AutoCommit::new();
        let modifiers = Modifiers::default();

        assert!(palette.handle_overlay_key(
            &WindowEvent::Ime(winit::event::Ime::Preedit(
                "東京".into(),
                Some(("東".len(), "東".len())),
            )),
            modifiers,
            &mut crate::widget::Edits::lend(&mut document),
            &automerge::ROOT,
        ));
        assert_eq!(palette.query(), "", "preedit is not committed query state");
        assert_eq!(
            palette.visible_entries().len(),
            2,
            "preedit does not filter"
        );

        let mut scene = Scene::new();
        let mut renderer = TextRenderer::new();
        Widget::draw(
            &mut palette,
            &mut scene,
            Affine::IDENTITY,
            &mut renderer,
            &Theme::dark(),
            &PresenceCtx::empty(),
        );
        let candidate = Widget::ime_cursor_rect(&palette).expect("query publishes IME caret");
        assert!(palette.input_row_rect().contains(candidate.center()));

        assert!(palette.handle_overlay_key(
            &WindowEvent::Ime(winit::event::Ime::Commit("東京".into())),
            modifiers,
            &mut crate::widget::Edits::lend(&mut document),
            &automerge::ROOT,
        ));
        assert_eq!(palette.query(), "東京");
        assert_eq!(palette.visible_entries().len(), 1);
        assert_eq!(palette.visible_entries()[0].label, "東京");
    }

    /// `ArrowLeft` / `ArrowRight` move the caret inside the query without
    /// touching the row cursor.
    fn arrow_keys_move_caret_inside_query() {
        let mut p = CommandPalette::new();
        p.set_focus(true);
        p.set_query("hello");
        assert_eq!(p.query_selection.cursor, 5);
        dispatch(&mut p, &key_press(Key::Named(NamedKey::ArrowLeft)));
        assert_eq!(p.query_selection.cursor, 4);
        dispatch(&mut p, &key_press(Key::Named(NamedKey::Home)));
        assert_eq!(p.query_selection.cursor, 0);
    }

    fn backspace_deletes_at_caret_not_end() {
        let mut p = CommandPalette::new();
        p.set_focus(true);
        p.set_query("abc");
        p.query_selection.cursor = 1;
        p.query_selection.anchor = 1;
        dispatch(&mut p, &key_press(Key::Named(NamedKey::Backspace)));
        assert_eq!(p.query(), "bc");
        assert_eq!(p.query_selection.cursor, 0);
    }

    fn typing_inserts_at_caret() {
        let mut p = CommandPalette::new();
        p.set_focus(true);
        p.set_query("ac");
        p.query_selection.cursor = 1;
        p.query_selection.anchor = 1;
        dispatch(&mut p, &key_press(Key::Character(SmolStr::new("b"))));
        assert_eq!(p.query(), "abc");
        assert_eq!(p.query_selection.cursor, 2);
    }

    fn ctrl_a_selects_query() {
        let mut p = CommandPalette::new();
        p.set_focus(true);
        p.set_query("hello");
        let mut mods_state = winit::keyboard::ModifiersState::empty();
        mods_state.set(winit::keyboard::ModifiersState::CONTROL, true);
        Dispatch::new()
            .modifiers(mods_state)
            .send(&mut p, &key_press(Key::Character(SmolStr::new("a"))));
        assert_eq!(p.query_selection.anchor, 0);
        assert_eq!(p.query_selection.cursor, 5);
    }

    fn typing_replaces_selection() {
        let mut p = CommandPalette::new();
        p.set_focus(true);
        p.set_query("hello");
        p.query_selection.anchor = 1;
        p.query_selection.cursor = 4;
        dispatch(&mut p, &key_press(Key::Character(SmolStr::new("i"))));
        assert_eq!(p.query(), "hio");
        assert_eq!(p.query_selection.cursor, 2);
    }

    ime_preedit_is_visible_but_filters_only_after_atomic_commit();
    arrow_keys_move_caret_inside_query();
    backspace_deletes_at_caret_not_end();
    typing_inserts_at_caret();
    ctrl_a_selects_query();
    typing_replaces_selection();
}

#[test]
fn palette_accessibility_reports_open_modal_and_focus() {
    fn accessibility_reports_open_state_modal_mode_and_painted_result_children() {
        use crate::a11y::Role;
        let mut p = CommandPalette::new();
        p.push(CommandEntry::new("alpha", "Alpha"));
        p.push(
            CommandEntry::new("bravo", "Bravo")
                .description("Second result")
                .badge("Live"),
        );
        p.set_rect(Rect::new(20.0, 30.0, 380.0, 310.0));

        let closed = Widget::accessibility(&p).expect("palette is semantic while closed");
        assert!(
            closed.state.hidden,
            "closed palettes leave the reading order"
        );
        assert_eq!(closed.state.expanded, Some(false));
        assert!(!closed.state.modal, "search chrome is not modal");
        assert!(
            closed.children.is_empty(),
            "closed palettes expose no stale rows"
        );

        p.open();
        let node = Widget::accessibility(&p).expect("open palette is semantic");
        assert_eq!(node.role, Role::Dialog);
        assert_eq!(node.name.as_deref(), Some("Command palette"));
        assert!(!node.state.hidden);
        assert_eq!(node.state.expanded, Some(true));
        assert!(!node.state.modal, "search chrome is an ordinary overlay");
        assert!(node.state.focusable);
        assert!(node.state.focused, "open palette owns its query focus");
        assert_eq!(node.children.len(), 2, "query plus its result list");
        let query = &node.children[0];
        assert_eq!(query.role, Role::TextInput);
        assert!(
            query.text_selection.is_some(),
            "the query carries a caret, so AT-SPI exposes it as editable text"
        );
        assert_eq!(query.value.as_deref(), Some(""));
        assert!(
            query.action_target.is_some(),
            "query has a stable synthetic id"
        );
        assert_eq!(query.bounds, Some(p.input_row_rect().into()));

        let results = &node.children[1];
        assert_eq!(results.role, Role::List);
        assert_eq!(
            results.children.len(),
            2,
            "one synthetic item per painted row"
        );
        let selected = &results.children[0];
        assert_eq!(selected.name.as_deref(), Some("Alpha"));
        assert!(selected.state.selected);
        assert_eq!(selected.pos_in_set, Some(1));
        assert_eq!(selected.set_size, Some(2));
        assert_eq!(selected.bounds, p.row_rect(0).map(Into::into));
        assert_eq!(
            results.children[1].description.as_deref(),
            Some("Second result. Status: Live")
        );
        assert_eq!(
            selected.action_target.as_deref(),
            Some("command-palette:result:alpha"),
            "row ids are model-stable rather than paint ordinals"
        );
        assert_eq!(
            p.synthetic_accessibility_rect(&[0]),
            Some(p.input_row_rect())
        );
        assert_eq!(p.synthetic_accessibility_rect(&[1, 1]), p.row_rect(1));

        let mut commands = CommandPalette::commands();
        commands.open();
        assert!(
            Widget::accessibility(&commands).unwrap().state.modal,
            "command chrome advertises its focus trap as modal"
        );
    }

    fn accessibility_focus_tracks_focus_state() {
        let mut p = CommandPalette::new();
        p.set_focus(true);
        let node = Widget::accessibility(&p).unwrap();
        assert!(node.state.focused);
    }

    fn command_mode_traps_focus_search_does_not() {
        let mut cmd = seeded_command_palette();
        cmd.set_rect(Rect::new(0.0, 0.0, 480.0, 400.0));
        assert!(Widget::traps_focus(&cmd), "command palette must trap focus");
        assert!(Widget::modal_capture(&cmd).is_some());
        cmd.close();
        assert!(!Widget::traps_focus(&cmd), "closed palette releases focus");

        let mut search = CommandPalette::new();
        search.push(CommandEntry::new("a", "Alpha"));
        search.set_focus(true);
        assert!(!Widget::traps_focus(&search), "search palette never traps");
        assert!(Widget::modal_capture(&search).is_none());
    }

    accessibility_reports_open_state_modal_mode_and_painted_result_children();
    accessibility_focus_tracks_focus_state();
    command_mode_traps_focus_search_does_not();
}

fn seeded_command_palette() -> CommandPalette {
    let mut p = CommandPalette::commands();
    p.push(
        CommandEntry::new("goto", "Go to cell\u{2026}")
            .description("Jump to and select a cell by reference")
            .with_argument("Go to cell", "e.g. B12")
            .with_argument_prefill("A1"),
    );
    p.push(
        CommandEntry::new("sum", "Sum selection")
            .description("Write =SUM() of the selected range below it")
            .keybinding("Alt+="),
    );
    p.push(
        CommandEntry::new("export", "Export CSV")
            .description("Write the active sheet to a .csv file"),
    );
    p.open();
    p
}

#[test]
fn command_mode_navigates_filters_and_scrolls() {
    fn command_mode_arrows_and_enter_are_captured_and_navigate() {
        let mut p = seeded_command_palette();
        assert_eq!(p.selected_id(), Some("goto"));

        for _ in 0..2 {
            let handled = dispatch(&mut p, &key_press(Key::Named(NamedKey::ArrowDown)));
            assert!(handled, "ArrowDown must be consumed by the palette");
        }
        assert_eq!(p.selected_id(), Some("export"), "cursor advanced two rows");

        let handled = dispatch(&mut p, &key_press(Key::Named(NamedKey::ArrowUp)));
        assert!(handled);
        assert_eq!(p.selected_id(), Some("sum"));

        let handled = dispatch(&mut p, &key_press(Key::Named(NamedKey::Enter)));
        assert!(handled, "Enter is consumed");
        let outcome = p.take_outcome().expect("Enter emits an outcome");
        assert_eq!(outcome.id, "sum");
        assert_eq!(outcome.argument, None);
        assert!(!p.is_open(), "running a command closes the palette");
    }

    fn command_mode_filter_matches_label_and_description() {
        let mut p = seeded_command_palette();
        // "csv" only appears in the Export entry's description.
        p.set_query("csv");
        let ids: Vec<&str> = p.visible_entries().iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["export"], "description text is searchable");

        p.set_query("sum");
        let ids: Vec<&str> = p.visible_entries().iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["sum"]);
    }

    fn command_mode_geometry_differs_from_search() {
        let cmd = seeded_command_palette();
        let (cw, _) = cmd.desired_size();
        assert_eq!(cw, CMD_CARD_WIDTH);
        assert_eq!(cmd.row_height(), TWO_LINE_ROW_HEIGHT);
        assert_eq!(cmd.filter_block(), 0.0, "no filter strip in command mode");

        let search = CommandPalette::new();
        let (sw, _) = search.desired_size();
        assert_eq!(sw, SEARCH_CARD_WIDTH);
        assert_eq!(search.row_height(), ROW_HEIGHT);
        assert!(search.filter_block() > 0.0, "search keeps its filter strip");
    }

    fn command_mode_scrolls_to_keep_cursor_visible() {
        let mut p = CommandPalette::commands();
        for i in 0..20 {
            p.push(CommandEntry::new(format!("id{i}"), format!("Command {i}")));
        }
        p.open();
        let (w, h) = p.desired_size();
        p.set_rect(Rect::new(0.0, 0.0, w, h));
        assert!(p.row_rect(0).is_some());
        assert!(
            p.row_rect(crate::num::usize_of_f64(DEFAULT_LIST_ROWS))
                .is_none(),
            "row past the cap is scrolled off"
        );

        p.set_cursor(19);
        assert!(
            p.row_rect(19).is_some(),
            "highlighted last row stays visible"
        );
        assert!(p.row_rect(0).is_none(), "the top row scrolled out of view");
    }

    command_mode_arrows_and_enter_are_captured_and_navigate();
    command_mode_filter_matches_label_and_description();
    command_mode_geometry_differs_from_search();
    command_mode_scrolls_to_keep_cursor_visible();
}

#[test]
fn command_argument_prompts_return_values_or_back_out() {
    fn command_mode_argument_flow_returns_typed_value() {
        let mut p = seeded_command_palette();
        assert_eq!(p.selected_id(), Some("goto"));

        let handled = dispatch(&mut p, &key_press(Key::Named(NamedKey::Enter)));
        assert!(handled);
        assert!(
            p.in_argument_mode(),
            "entry with an argument opens the arg step"
        );
        assert_eq!(p.argument_buffer(), Some("A1"));
        assert!(
            p.take_outcome().is_none(),
            "no outcome until the argument is confirmed"
        );

        for ch in ["B", "1", "2"] {
            let h = dispatch(&mut p, &key_press(Key::Character(SmolStr::new(ch))));
            assert!(h, "typing in the arg field is consumed");
        }
        assert_eq!(p.argument_buffer(), Some("B12"));

        let handled = dispatch(&mut p, &key_press(Key::Named(NamedKey::Enter)));
        assert!(handled);
        let outcome = p.take_outcome().expect("confirming emits an outcome");
        assert_eq!(outcome.id, "goto");
        assert_eq!(outcome.argument.as_deref(), Some("B12"));
        assert!(!p.is_open());
    }

    fn direct_argument_mode_skips_the_artificial_command_row() {
        let mut p = CommandPalette::commands();
        p.open_argument(
            "goto-line",
            "Go to line",
            "Line number\u{2026}",
            Some("42".into()),
        );
        assert!(p.is_open());
        assert!(p.in_argument_mode());
        assert_eq!(p.argument_buffer(), Some("42"));
        let handled = dispatch(&mut p, &key_press(Key::Named(NamedKey::Enter)));
        assert!(handled);
        assert_eq!(
            p.take_outcome(),
            Some(CommandOutcome {
                id: "goto-line".into(),
                argument: Some("42".into()),
            })
        );
        assert!(!p.is_open());
    }

    fn escape_closes_a_direct_argument_prompt() {
        let mut p = CommandPalette::commands();
        p.open_argument("goto-line", "Go to line", "Line number…", None);
        let handled = dispatch(&mut p, &key_press(Key::Named(NamedKey::Escape)));
        assert!(handled);
        assert!(!p.is_open());
        assert!(!p.in_argument_mode());
    }

    fn command_mode_escape_from_argument_returns_to_list() {
        let mut p = seeded_command_palette();
        dispatch(&mut p, &key_press(Key::Named(NamedKey::Enter)));
        assert!(p.in_argument_mode());
        let h = dispatch(&mut p, &key_press(Key::Named(NamedKey::Escape)));
        assert!(h, "Escape is consumed");
        assert!(!p.in_argument_mode(), "Escape leaves the argument step");
        assert!(p.is_open(), "…but the palette is still open on the list");
        assert!(p.take_outcome().is_none());
    }

    command_mode_argument_flow_returns_typed_value();
    direct_argument_mode_skips_the_artificial_command_row();
    escape_closes_a_direct_argument_prompt();
    command_mode_escape_from_argument_returns_to_list();
}

#[test]
fn grouped_results_plan_headers_counts_and_painted_hits() {
    fn grouped_entries_plan_one_header_per_section() {
        let mut p = CommandPalette::new();
        p.set_bypass_fuzzy(true);
        p.set_entries(vec![
            CommandEntry::new("a", "Open Calculator").group("Applications"),
            CommandEntry::new("b", "calc.rs").group("Files"),
            CommandEntry::new("c", "calculator.rs").group("Files"),
        ]);
        let headers: Vec<&str> = p
            .rows
            .iter()
            .filter_map(|r| match r {
                VisualRow::Header { label, .. } => Some(label.as_str()),
                VisualRow::Entry(_) => None,
            })
            .collect();
        assert_eq!(headers, vec!["Applications", "Files"]);
        assert_eq!(
            p.rows.len(),
            5,
            "two headers plus three entries: {:?}",
            p.rows
        );
    }

    fn a_trimmed_section_reports_its_untrimmed_count() {
        let mut p = CommandPalette::new();
        p.set_bypass_fuzzy(true);
        p.set_entries(vec![
            CommandEntry::new("a", "one").group("Files").group_total(40),
            CommandEntry::new("b", "two").group("Files").group_total(40),
        ]);
        match &p.rows[0] {
            VisualRow::Header { label, total, .. } => {
                assert_eq!(label, "Files");
                assert_eq!(*total, 40);
            }
            VisualRow::Entry(i) => panic!("expected a header, got entry row {i}"),
        }
    }

    fn a_scrolled_list_hit_tests_the_row_it_painted() {
        let mut p = CommandPalette::new();
        p.set_bypass_fuzzy(true);
        p.set_entries(
            (0..30)
                .map(|i| {
                    CommandEntry::new(format!("id{i}"), format!("Result {i}"))
                        .description(format!("~/path/to/{i}"))
                        .group("Files")
                })
                .collect(),
        );
        let (w, h) = p.desired_size();
        p.set_rect(Rect::new(0.0, 0.0, w, h));

        p.set_cursor(29);
        let painted: Vec<usize> = (0..30).filter(|&i| p.row_rect(i).is_some()).collect();
        assert!(
            painted.contains(&29),
            "the cursor's row must be painted: {painted:?}"
        );
        assert!(
            !painted.contains(&0),
            "row 0 must have scrolled off: {painted:?}"
        );
        for i in painted {
            let r = p.row_rect(i).expect("painted");
            assert_eq!(
                p.row_at(r.center()),
                Some(i),
                "row {i} painted at {r:?} but hit-tests as {:?}",
                p.row_at(r.center())
            );
        }
    }

    fn no_painted_row_runs_past_the_card() {
        let mut p = CommandPalette::new();
        p.set_bypass_fuzzy(true);
        p.set_entries(
            (0..40)
                .map(|i| {
                    CommandEntry::new(format!("id{i}"), format!("Result {i}"))
                        .description("sub")
                        .group(if i < 3 { "Actions" } else { "Files" })
                })
                .collect(),
        );
        let (w, h) = p.desired_size();
        p.set_rect(Rect::new(0.0, 0.0, w, h));
        let bottom = p.list_bottom_y();
        for i in 0..40 {
            if let Some(r) = p.row_rect(i) {
                assert!(
                    r.y1 <= bottom,
                    "row {i} at {r:?} runs past the list's end at {bottom}"
                );
                assert!(
                    r.y0 >= p.list_top_y() - 0.01,
                    "row {i} starts above the list"
                );
            }
        }
    }

    fn an_empty_result_set_still_gets_a_readable_card() {
        let mut p = CommandPalette::new();
        p.set_bypass_fuzzy(true);
        p.set_query("nothing matches this");
        p.set_entries(vec![]);
        let (_, h) = p.desired_size();
        p.set_rect(Rect::new(0.0, 0.0, 640.0, h));
        let room = p.list_bottom_y() - p.list_top_y();
        assert!(
            room >= EMPTY_STATE_HEIGHT - 1.0,
            "the zero-state needs {EMPTY_STATE_HEIGHT} px of list area, got {room}"
        );
    }

    grouped_entries_plan_one_header_per_section();
    a_trimmed_section_reports_its_untrimmed_count();
    a_scrolled_list_hit_tests_the_row_it_painted();
    no_painted_row_runs_past_the_card();
    an_empty_result_set_still_gets_a_readable_card();
}
