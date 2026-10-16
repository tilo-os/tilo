use super::*;
use crate::headless::{FrameHarness, InspectionNode};
use crate::widget::{Column, Element};
use std::cell::Cell;
use std::rc::Rc;

fn face_height(face: &ListRowFace<'_>, scale: f32) -> f64 {
    let theme = Theme::dark().with_text_scale(scale);
    let mut text = TextRenderer::new();
    face.height(&mut text, &theme)
}

fn geometry(face: &ListRowFace<'_>, rect: Rect) -> RowGeometry {
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    face.geometry(rect, &RowMetrics::new(&theme), &mut text, &theme)
}

fn find<'a>(node: &'a InspectionNode, widget: &str) -> Option<&'a InspectionNode> {
    if node.widget == widget {
        return Some(node);
    }
    node.children.iter().find_map(|child| find(child, widget))
}

fn rect_of(node: &InspectionNode) -> Rect {
    let [x, y, w, h] = node.rect;
    Rect::new(x, y, x + w, y + h)
}

#[test]
fn a_title_cut_short_reads_in_full_in_the_tooltip() {
    let theme = crate::theme::Theme::dark();
    let message = "Expected `&[String]`, found `&Vec<String>` in the argument to parse_limit";
    let long = ListRow::new("long", message).meta("Ln 14, Col 29");
    let h = FrameHarness::for_root(crate::State::new(), long, (240, 60), theme.clone());
    let row = h.root::<ListRow>();
    let band = row.text_band();
    let (rect, text) = Widget::tooltip_target_at(row, band.center()).expect("elided title");
    assert_eq!(text, message);
    assert_eq!(rect, band);

    let short = ListRow::new("short", "Unused import").meta("Ln 2, Col 5");
    let h = FrameHarness::for_root(crate::State::new(), short, (480, 60), theme);
    let row = h.root::<ListRow>();
    assert_eq!(
        Widget::tooltip_target_at(row, row.text_band().center()),
        None,
        "a title that fits needs no tooltip"
    );
}

#[test]
fn a_row_tooltip_reads_anywhere_the_row_has_nothing_closer_to_say() {
    let theme = crate::theme::Theme::dark();
    let row = ListRow::new("used", "Deploy").tooltip("Used 3 times");
    let h = FrameHarness::for_root(crate::State::new(), row, (480, 60), theme);
    let row = h.root::<ListRow>();
    let (_, text) = Widget::tooltip_target_at(row, row.text_band().center()).expect("row tooltip");
    assert_eq!(text, "Used 3 times");
}

#[test]
fn a_badge_column_fits_every_badge_of_the_list() {
    let theme = crate::theme::Theme::dark();
    let badges: Rc<[String]> = vec!["SH".to_owned(), "RUST".to_owned()].into();
    let row = ListRow::new("short", "Deploy")
        .badge("SH")
        .badge_column(Rc::clone(&badges));
    let h = FrameHarness::for_root(crate::State::new(), row, (480, 60), theme.clone());
    let wide = crate::text_render::with_layout_renderer(theme.text_scale, |text| {
        badge_column_width(text, &theme, &["SH", "RUST"])
    });
    let own = crate::text_render::with_layout_renderer(theme.text_scale, |text| {
        badge_column_width(text, &theme, &["SH"])
    });
    assert!(wide > own);
    assert_eq!(h.root::<ListRow>().leading_column, Some(wide));
}

fn hovered() -> Interaction {
    Interaction {
        hovered: true,
        ..Interaction::default()
    }
}

#[test]
fn row_height_comes_from_measured_line_boxes_and_grows_with_text() {
    let single = ListRowFace::new("Inbox");
    let two = ListRowFace::new("Ada Lovelace").secondary("ada@example.org");
    let three = ListRowFace::new("Ada Lovelace")
        .secondary("The analytical engine weaves algebraic patterns")
        .two_line_secondary();
    let message = ListRowFace::new("Ada Lovelace")
        .subtitle("Notes on the engine")
        .secondary("The analytical engine weaves algebraic patterns")
        .two_line_secondary();
    let theme = Theme::dark();
    let floor = f64::from(theme.tokens.metrics.min_pointer_target + theme.tokens.spacing.s2);
    assert!(
        (face_height(&single, 1.0) - floor).abs() < 0.5,
        "a one-line row is 40"
    );
    assert!(face_height(&two, 1.0) > face_height(&single, 1.0));
    assert!(face_height(&three, 1.0) > face_height(&two, 1.0));
    assert!(
        face_height(&message, 1.0) > face_height(&three, 1.0),
        "a subtitle is its own line"
    );
    assert!(
        face_height(&ListRowFace::new("A long name").two_line_title(), 1.0)
            > face_height(&single, 1.0) - 0.5
    );
    assert!(face_height(&single, 2.0) > face_height(&single, 1.0));
    for face in [&two, &three] {
        assert!(
            face_height(face, 2.0) > face_height(face, 1.0) * 1.5,
            "rows grow with the text scale"
        );
    }
}

#[test]
fn text_sits_on_the_content_edge_and_the_lift_bleeds_past_it() {
    let theme = Theme::dark();
    let bleed = crate::chrome::row_bleed(&theme);
    let outer = Rect::new(0.0, 0.0, 400.0, 40.0);
    let plain = geometry(&ListRowFace::new("Inbox"), outer);
    assert!(
        (plain.text_x - bleed).abs() < 0.01,
        "a face's text starts one bleed inside its outer rect"
    );
    // A retained row in a padded column: its title starts on the column's
    // content edge, which is the row's own layout box.
    let rows: Element = Box::new(
        Column::new()
            .with_horizontal_padding(24.0, 24.0)
            .push(Box::new(ListRow::new("a", "Ada").on_activate(|| {}))),
    );
    let harness = FrameHarness::new(crate::State::new(), rows, (400, 200));
    let row = rect_of(find(&harness.inspection().root, "ListRow").unwrap());
    assert!((row.x0 - 24.0).abs() < 0.5);
    let mut list_row = ListRow::new("a", "Ada");
    list_row.metrics = Some(RowMetrics::new(&theme));
    list_row.rect = row;
    let mut text = TextRenderer::new();
    let x = list_row.with_face(|face| {
        face.text_x(
            list_row.outer(),
            &RowMetrics::new(&theme),
            &mut text,
            &theme,
        )
    });
    assert!(
        (x - row.x0).abs() < 0.01,
        "title on the content edge: {x} vs {}",
        row.x0
    );
    assert!(list_row.outer().x0 < row.x0, "the lift bleeds outward");
}

#[test]
fn hover_actions_replace_the_meta_but_not_the_value() {
    let theme = Theme::dark();
    let rect = Rect::new(0.0, 0.0, 400.0, 48.0);
    let actions = [
        RowAction {
            icon: Icon::Reply,
            label: "Reply",
        },
        RowAction {
            icon: Icon::Trash,
            label: "Delete message",
        },
    ];
    let idle = ListRowFace::new("Ada").meta("09:41").actions(&actions);
    assert!(idle.action_rects(rect, &theme).is_empty(), "hidden at rest");
    let hover = idle.clone().interaction(hovered());
    let rects = hover.action_rects(rect, &theme);
    assert_eq!(rects.len(), 2);
    assert!(rects[0].x1 < rects[1].x0, "in order, not touching");
    for r in &rects {
        assert!(rect.contains(r.origin()) && r.x1 <= rect.x1);
        assert!(
            (r.center().y - rect.center().y).abs() < 0.01,
            "centred on the row"
        );
    }
    // A value stays: the actions stand before it.
    let valued = ListRowFace::new("Tokyo")
        .title_value("09:41")
        .actions(&actions)
        .interaction(hovered());
    let geo = geometry(&valued, rect);
    let rects = valued.action_rects(rect, &theme);
    let value_left = geo.value_right
        - f64::from(
            TextRenderer::new()
                .measure_style("09:41", None, &RowMetrics::new(&theme).value_title)
                .0,
        );
    assert!(rects.last().unwrap().x1 <= value_left + 0.01);
    // A touch pointer has no hover: a selected row shows its actions.
    let touch = ListRowFace::new("Ada")
        .actions(&actions)
        .reveal_actions(true);
    assert_eq!(touch.action_rects(rect, &theme).len(), 2);
    // A disabled row shows none.
    assert!(
        hover
            .clone()
            .enabled(false)
            .action_rects(rect, &theme)
            .is_empty()
    );
}

#[test]
fn the_meta_and_count_narrow_only_their_own_line() {
    let rect = Rect::new(0.0, 0.0, 400.0, 64.0);
    let face = ListRowFace::new("Ada Lovelace")
        .secondary("A preview that runs the full width of the row")
        .meta("Yesterday");
    let geo = geometry(&face, rect);
    assert!(
        geo.title_right < geo.text_right,
        "the meta narrows the title"
    );
    assert!(
        (geo.second_right - geo.text_right).abs() < 0.01,
        "the preview keeps the full width"
    );
    let thread = face.clone().count(4);
    let geo = geometry(&thread, rect);
    assert!(
        geo.second_right < geo.text_right,
        "a count narrows the line under the title"
    );
}

#[test]
fn trailing_columns_stack_from_the_edge_and_reserve_a_control() {
    let theme = Theme::dark();
    let rect = Rect::new(0.0, 0.0, 480.0, 40.0);
    let face = ListRowFace::new("Wi-Fi")
        .disclosure()
        .checked(true)
        .status(RowStatus::Active)
        .trailing_width(52.0);
    let geo = geometry(&face, rect);
    let edge = rect.x1 - crate::chrome::row_bleed(&theme);
    let chevron = geo.disclosure.unwrap();
    let check = geo.check.unwrap();
    let slot = geo.trailing.unwrap();
    assert!((chevron.x1 - edge).abs() < 0.01, "the chevron is outermost");
    assert!(check.x1 < chevron.x0);
    assert!(geo.dot_x.unwrap() < check.x0);
    assert!(slot.x1 < geo.dot_x.unwrap());
    assert!((slot.width() - 52.0).abs() < 0.01);
    assert!(geo.text_right < slot.x0);
    let mut text = TextRenderer::new();
    assert_eq!(face.trailing_rect(rect, &mut text, &theme), Some(slot));
}

#[test]
fn the_status_dot_sits_on_the_title_line_of_a_two_line_row() {
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let face = ListRowFace::new("Ada")
        .secondary("Online")
        .status(RowStatus::Active);
    let h = face.height(&mut text, &theme);
    let rect = Rect::new(0.0, 0.0, 300.0, h);
    let mut scene = Scene::new();
    face.paint(
        &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme),
        rect,
    );
    let m = RowMetrics::new(&theme);
    let block = face.text_block(&mut text, &m);
    let title_cy = rect.center().y - block / 2.0 + RowMetrics::line(&mut text, &m.title) / 2.0;
    assert!(title_cy < rect.center().y - 1.0, "fixture: two lines");
    let dot = face.dot_centre(rect, &mut text, &theme).unwrap();
    assert!(
        (dot.y - title_cy).abs() < 0.5,
        "the dot is on the title's line"
    );

    // Beside a chevron the dot centres with it: one trailing line.
    let face = face.disclosure();
    let dot = face.dot_centre(rect, &mut text, &theme).unwrap();
    assert!(
        (dot.y - rect.center().y).abs() < 0.5,
        "the dot centres with the chevron"
    );
}

#[test]
fn separators_hide_around_the_selection() {
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let count = |face: ListRowFace<'_>, text: &mut TextRenderer| {
        let mut scene = Scene::new();
        face.paint(
            &mut PaintCtx::new(&mut scene, Affine::IDENTITY, text, &theme),
            Rect::new(0.0, 0.0, 300.0, 40.0),
        );
        scene.encoding().draw_tags.len()
    };
    let plain = count(ListRowFace::new("a"), &mut text);
    let separated = count(ListRowFace::new("a").separator(false), &mut text);
    let above_selection = count(ListRowFace::new("a").separator(true), &mut text);
    let selected_plain = count(ListRowFace::new("a").selected(true), &mut text);
    let selected = count(
        ListRowFace::new("a").selected(true).separator(false),
        &mut text,
    );
    assert_eq!(separated, plain + 1);
    assert_eq!(above_selection, plain);
    assert_eq!(selected, selected_plain);
}

#[test]
fn a_selected_row_is_the_lift_on_every_preset() {
    for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
        let ground = theme.tokens.colors.surfaces.content;
        let fill = row_fill(&theme, true, false).unwrap();
        let moved = crate::surface::edge_delta(fill, ground);
        assert!(
            moved >= crate::tokens::colors::HAIRLINE_MIN_DELTA,
            "{}: selection moved the ground only {moved:.4}",
            theme.name
        );
        assert!(
            row_fill(&theme, false, false).is_none(),
            "idle is transparent"
        );
    }
}

#[test]
fn toned_meta_spans_stay_legible_on_the_ground_and_the_lift() {
    for theme in [Theme::dark(), Theme::light()] {
        let ground = theme.tokens.colors.surfaces.content;
        let lift = crate::tokens::colors::composite(row_fill(&theme, true, false).unwrap(), ground);
        for tone in [Tone::Success, Tone::Danger, Tone::Warning] {
            let ink = theme.tokens.colors.ink_on_any(
                &[ground, lift],
                tone.colour(&theme),
                SMALL_TEXT_MIN_CONTRAST,
            );
            for under in [ground, lift] {
                assert!(crate::a11y::contrast_ratio(ink, under) >= SMALL_TEXT_MIN_CONTRAST);
            }
        }
    }
    // The spans paint without a plain meta.
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let mut scene = Scene::new();
    ListRowFace::new("AAPL")
        .meta_spans(&[("+2.4%", Tone::Success), ("182.10", Tone::Neutral)])
        .selected(true)
        .paint(
            &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme),
            Rect::new(0.0, 0.0, 320.0, 40.0),
        );
    assert!(scene.encoding().resources.glyph_runs.len() >= 3);
}

#[test]
fn every_leading_kind_paints_in_its_column_and_titles_line_up() {
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let column = badge_column_width(&mut text, &theme, &["GET", "POST", "DELETE"]);
    let m = RowMetrics::new(&theme);
    let rect = Rect::new(0.0, 0.0, 400.0, 40.0);
    let x = |badge: &str, text: &mut TextRenderer| {
        ListRowFace::new("/users")
            .leading(RowLeading::Badge(badge))
            .leading_column(column)
            .text_x(rect, &m, text, &theme)
    };
    let get = x("GET", &mut text);
    assert!((get - x("DELETE", &mut text)).abs() < 0.01);
    assert!(get > column);

    let image = crate::raster::rgba8(vec![128_u8; 4 * 4 * 4], 4, 4);
    let painted = Cell::new(false);
    let painter = |_: &mut PaintCtx<'_>, _: Rect| painted.set(true);
    let kinds = [
        RowLeading::Icon(Icon::Folder),
        RowLeading::Avatar(AvatarFace::new("Ada", "ada").square()),
        RowLeading::Thumbnail(&image),
        RowLeading::Media(MediaArt::Placeholder, MediaAspect::Wide),
        RowLeading::Checkbox { checked: true },
        RowLeading::Swatch(theme.tokens.colors.semantic.info),
        RowLeading::Paint(&painter),
    ];
    for kind in kinds {
        let face = ListRowFace::new("Quarterly report").leading(kind);
        let mut scene = Scene::new();
        face.paint(
            &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme),
            rect,
        );
        let lead = face.leading_rect(rect, &mut text, &theme);
        assert!(lead.width() > 0.0, "{kind:?} has a column");
        assert!(face.text_x(rect, &m, &mut text, &theme) >= lead.x1 - 0.01);
    }
    assert!(painted.get(), "a leading painter runs");
    // A wide thumbnail takes a wider column than a square one.
    let wide = geometry(
        &ListRowFace::new("Clip")
            .leading(RowLeading::Media(MediaArt::Placeholder, MediaAspect::Wide)),
        rect,
    );
    let square = geometry(
        &ListRowFace::new("Clip").leading(RowLeading::Media(
            MediaArt::Placeholder,
            MediaAspect::Square,
        )),
        rect,
    );
    assert!(wide.leading.width() > square.leading.width());
}

#[test]
fn accessibility_names_the_badge_and_reads_parts_and_actions() {
    let actions = [RowAction {
        icon: Icon::Copy,
        label: "Copy URL",
    }];
    let face = ListRowFace::new("/users")
        .leading(RowLeading::Badge("GET"))
        .meta("200")
        .tags(&[("Beta", ChipStyle::Neutral)])
        .count(3)
        .checked(true)
        .actions(&actions);
    let node = face.accessibility(Rect::new(0.0, 0.0, 300.0, 40.0), "row");
    assert_eq!(node.name.as_deref(), Some("GET /users"));
    let value = node.value.unwrap();
    assert!(value.contains("Beta") && value.contains("200") && value.contains('3'));
    assert_eq!(node.state.checked, Some(true));
    assert_eq!(node.children.len(), 1);
    assert_eq!(node.children[0].role, Role::Button);
    assert_eq!(node.children[0].name.as_deref(), Some("Copy URL"));

    let row = ListRow::new("r", "/users")
        .badge("GET")
        .leading_tooltip("Retrieves the resource");
    let node = Widget::accessibility(&row).unwrap();
    assert_eq!(node.name.as_deref(), Some("GET /users"));
    assert_eq!(node.description.as_deref(), Some("Retrieves the resource"));

    // An icon that names a kind says it the same way, and keeps the title
    // as the row's whole name.
    let row = ListRow::new("r", "Build server")
        .icon(Icon::SquareTerminal)
        .leading_tooltip("SSH");
    let node = Widget::accessibility(&row).unwrap();
    assert_eq!(node.name.as_deref(), Some("Build server"));
    assert_eq!(node.description.as_deref(), Some("SSH"));
}

#[test]
fn retained_row_activates_by_pointer_and_accessibility() {
    let hits = Rc::new(Cell::new(0));
    let counter = Rc::clone(&hits);
    let rows: Element = Box::new(
        Column::new().with_no_padding().push(Box::new(
            ListRow::new("contact-ada", "Ada Lovelace")
                .secondary("ada@example.org")
                .avatar("Ada Lovelace", "ada@example.org")
                .meta("Today")
                .on_activate(move || counter.set(counter.get() + 1)),
        )),
    );
    let mut harness = FrameHarness::new(crate::State::new(), rows, (400, 200));
    let [x, y, w, h] = find(&harness.inspection().root, "ListRow")
        .expect("row laid out")
        .rect;
    assert!(h >= 40.0);
    harness.click_at(x + w / 2.0, y + h / 2.0);
    assert_eq!(hits.get(), 1);
    let mut row = ListRow::new("r", "Row").on_activate(|| {});
    let node = Widget::accessibility(&row).unwrap();
    assert_eq!(node.role, Role::ListItem);
    assert_eq!(node.name.as_deref(), Some("Row"));
    let mut doc = automerge::AutoCommit::new();
    assert!(Widget::perform_action(
        &mut row,
        &crate::a11y::Action::Activate,
        &[],
        &mut crate::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
    ));
}

#[test]
fn hover_actions_run_from_assistive_technology_and_the_keyboard() {
    let replied = Rc::new(Cell::new(0));
    let deleted = Rc::new(Cell::new(0));
    let (r, d) = (Rc::clone(&replied), Rc::clone(&deleted));
    let mut row = ListRow::new("m", "Ada")
        .on_activate(|| {})
        .action(Icon::Reply, "Reply", move || r.set(r.get() + 1))
        .action(Icon::Trash, "Delete message", move || d.set(d.get() + 1));
    let mut doc = automerge::AutoCommit::new();
    assert!(Widget::perform_action(
        &mut row,
        &crate::a11y::Action::Activate,
        &[1],
        &mut crate::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
    ));
    assert_eq!((replied.get(), deleted.get()), (0, 1));
    // Arrow keys arm the actions of a focused row; Enter runs the armed one.
    row.set_focus(true);
    row.armed_action = Some(0);
    assert!(Widget::perform_action(
        &mut row,
        &crate::a11y::Action::Activate,
        &[],
        &mut crate::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
    ));
    assert_eq!(replied.get(), 1);
    row.set_focus(false);
    assert_eq!(row.armed_action, None, "leaving the row disarms");
    // Every hover action is reported, so a native audit can see it.
    let bindings = crate::widget::controls::ActionControls::control_action_bindings(&row);
    assert_eq!(bindings.len(), 3);
}

#[test]
fn on_view_publishes_the_row_as_an_action_instead_of_a_callback() {
    let opened = Rc::new(Cell::new(0));
    let counter = Rc::clone(&opened);
    let mut row = ListRow::new("contact-ada", "Ada Lovelace").on_view(move || {
        counter.set(counter.get() + 1);
    });
    let bindings = crate::widget::controls::ActionControls::control_action_bindings(&row);
    assert_eq!(bindings.len(), 1);
    assert!(
        !matches!(
            bindings[0].kind,
            crate::widget::ControlBindingKind::LegacyCallback
        ),
        "a view action is not a legacy callback"
    );
    let doc = automerge::AutoCommit::new();
    let resources = crate::resources::PublishesResources::resources(&row, &doc);
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0].label, "Ada Lovelace");
    assert!(
        Widget::resource_operation_handlers(&row)
            .contains(crate::widget::activation::NATIVE_INVOKE)
    );
    let mut doc = automerge::AutoCommit::new();
    let invocation = tilo_actions::InvocationId::new();
    let payload = serde_json::to_vec("contact-ada").unwrap();
    let started = crate::widget::ResourceOperation::local(
        invocation,
        tilo_actions::OperationKind::View,
        crate::widget::activation::NATIVE_INVOKE,
        &payload,
    )
    .start(
        &mut row,
        &mut crate::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
    )
    .unwrap();
    assert!(started.is_some());
    assert_eq!(opened.get(), 1);
}

#[test]
fn a_trailing_control_reserves_its_width_and_wraps_under_narrow_text() {
    let layout = |width: u32| {
        let row = ListRow::new("wifi", "Wi-Fi")
            .secondary("Home network")
            .trailing(Box::new(crate::widget::Toggle::new("wifi-toggle", "wifi")));
        let harness = FrameHarness::new(
            crate::State::new(),
            Box::new(Column::new().with_no_padding().push(Box::new(row))),
            (width, 300),
        );
        let root = harness.inspection().root;
        let row = find(&root, "ListRow").unwrap();
        let toggle = rect_of(&row.children[0]);
        (rect_of(row), toggle)
    };
    let (row, toggle) = layout(600);
    assert!(toggle.x1 <= row.x1 + 0.5, "the control ends on the edge");
    assert!(
        toggle.y0 >= row.y0 && toggle.y1 <= row.y1,
        "beside the text: {toggle:?} in {row:?}"
    );
    let (narrow, wrapped) = layout(150);
    assert!(narrow.height() > row.height(), "the row grows to hold it");
    assert!(
        wrapped.y0 > narrow.y0 + row.height() / 2.0,
        "under the text"
    );
}

#[test]
fn a_leading_checkbox_toggles_apart_from_activation() {
    let toggled = Rc::new(Cell::new(None));
    let activated = Rc::new(Cell::new(0));
    let (t, a) = (Rc::clone(&toggled), Rc::clone(&activated));
    let mut row = ListRow::new("m", "Ada")
        .checkbox(false, move |on| t.set(Some(on)))
        .on_activate(move || a.set(a.get() + 1));
    let mut doc = automerge::AutoCommit::new();
    assert!(Widget::perform_action(
        &mut row,
        &crate::a11y::Action::ToggleCheck,
        &[],
        &mut crate::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
    ));
    assert_eq!(toggled.get(), Some(true));
    assert_eq!(activated.get(), 0);
    let node = Widget::accessibility(&row).unwrap();
    assert_eq!(node.state.checked, Some(true));
}

#[test]
fn a_disabled_row_is_inert_and_skipped_by_focus() {
    let mut row = ListRow::new("r", "Row")
        .on_activate(|| {})
        .with_enabled(false);
    assert!(!row.is_focusable());
    assert!(!Widget::is_enabled(&row));
    let mut doc = automerge::AutoCommit::new();
    assert!(!Widget::perform_action(
        &mut row,
        &crate::a11y::Action::Activate,
        &[],
        &mut crate::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
    ));
}

#[test]
fn a_touch_pointer_shows_a_selected_rows_actions() {
    let mut row = ListRow::new("m", "Ada")
        .selected(true)
        .action(Icon::Trash, "Delete", || {});
    assert!(!row.reveal_actions());
    row.set_interaction_environment(crate::interaction::InteractionEnvironment::TOUCH);
    assert!(row.reveal_actions());
}

#[test]
fn a_view_row_is_a_certified_action_a_click_and_an_agent_both_reach() {
    let calls = Rc::new(Cell::new(0));
    let invoked = Rc::clone(&calls);
    let row = ListRow::new("contact-ada", "Ada Lovelace")
        .secondary("ada@example.org")
        .on_view(move || invoked.set(invoked.get() + 1));
    let mut driver = FrameHarness::new(
        crate::State::new(),
        Box::new(Column::new().with_no_padding().push(Box::new(row))),
        (400, 200),
    );
    assert!(driver.native_app_audit().is_certified());
    assert!(driver.resources().iter().any(|resource| {
        resource.id.as_str() == "tilo.control:contact-ada"
            && resource.actions.iter().any(|action| action.id == "invoke")
    }));
    driver
        .perform_named("contact-ada", &crate::a11y::Action::Activate)
        .expect("the row enters through its action");
    assert_eq!(calls.get(), 1);
    let [x, y, w, h] = find(&driver.inspection().root, "ListRow").unwrap().rect;
    driver.click_at(x + w / 2.0, y + h / 2.0);
    assert_eq!(calls.get(), 2, "a click runs the same action");
}
