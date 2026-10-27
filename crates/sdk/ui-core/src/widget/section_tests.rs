use super::*;
use crate::headless::{FrameHarness, InspectionNode};
use crate::widget::ListRow;

fn rect(node: &InspectionNode) -> Rect {
    let [x, y, w, h] = node.rect;
    Rect::new(x, y, x + w, y + h)
}

fn rows_of(section: &mut Section) -> Vec<&mut ListRow> {
    section
        .parts
        .children
        .iter_mut()
        .filter_map(|c| c.as_any_mut().and_then(|a| a.downcast_mut::<ListRow>()))
        .collect()
}

#[test]
fn items_sit_a_sibling_step_apart_and_the_header_closer() {
    let theme = Theme::dark();
    let rhythm = theme.tokens.spacing.rhythm;
    let section = Section::new(
        (0..3)
            .map(|i| Box::new(Text::new(&format!("file {i}"))) as Element)
            .collect(),
    )
    .with_header(
        SectionHeader::heading("Changes")
            .count(3)
            .action(Button::new("stage-all", "Stage all")),
    );
    let harness = FrameHarness::new_with_theme(
        crate::State::new(),
        Box::new(section),
        (500, 400),
        theme.clone(),
    );
    let root = harness.inspection().root;
    let header = &root.children[0];
    let items = &root.children[1..];
    assert!((rect(&items[0]).y0 - rect(header).y1 - f64::from(rhythm.related)).abs() < 0.5);
    assert!((rect(&items[1]).y0 - rect(&items[0]).y1 - f64::from(rhythm.sibling)).abs() < 0.5);
    // Title on the content edge, count, then the action whose label ends on
    // the trailing edge: a labelled plain button is laid out as its label,
    // and its lift bleeds past it.
    assert_eq!(header.children.len(), 3);
    let title = rect(&header.children[0]);
    let count = rect(&header.children[1]);
    let action = rect(&header.children[2]);
    assert!(title.x0.abs() < 0.5, "title on the edge: {title:?}");
    assert!(count.x0 > title.x1);
    assert!(
        (action.x1 - 500.0).abs() < 1.0,
        "the label, not its lift, ends on the edge: {action:?}"
    );
    assert!(
        (count.center().y - title.center().y).abs() < 1.0,
        "one centre line"
    );
}

#[test]
fn leading_identity_keeps_its_measured_size_beside_a_wrapping_heading() {
    for (width, scale) in [(500, 1.0), (180, 2.0)] {
        let image = crate::widget::Image::from_svg(
            br##"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32"><rect width="32" height="32" fill="#f00"/></svg>"##,
            32,
            32,
        ).unwrap().with_alt("Source logo");
        let section = Section::new(vec![Box::new(Text::new("Content"))])
            .with_header(SectionHeader::group("Browser personal profile").leading(Box::new(image)))
            .collapsible();
        let mut harness = FrameHarness::new_with_theme(
            crate::State::new(),
            Box::new(section),
            (width, 500),
            Theme::dark().with_text_scale(scale),
        );
        let root = harness.inspection().root;
        let header = &root.children[0];
        let image = rect(&header.children[0]);
        let title = rect(&header.children[1]);
        let chevron = rect(&header.children[2]);
        assert_eq!(image.width(), 32.0);
        assert_eq!(image.height(), 32.0);
        assert!(image.x1 < title.x0);
        assert!(title.x1 <= chevron.x0);
        assert!(chevron.x1 <= f64::from(width));
        assert!(rect(header).height() >= title.height().max(image.height()));
        assert!(
            harness
                .a11y_nodes()
                .iter()
                .any(|(_, node)| node.role() == crate::accesskit::Role::Image
                    && node.label() == Some("Source logo"))
        );
        harness.click_at(chevron.center().x, chevron.center().y);
        assert_eq!(harness.inspection().root.children.len(), 1);
    }
}

#[test]
fn a_header_is_one_height_with_or_without_an_action() {
    let height = |header: SectionHeader| {
        let harness = FrameHarness::new_with_theme(
            crate::State::new(),
            Box::new(Section::new(vec![Box::new(Text::new("body"))]).with_header(header)),
            (400, 300),
            Theme::dark(),
        );
        rect(&harness.inspection().root.children[0]).height()
    };
    let plain = height(SectionHeader::group("Recent"));
    let with_action =
        height(SectionHeader::group("Recent").action(Button::new("clear", "Clear all")));
    assert!(
        (plain - with_action).abs() < 0.5,
        "{plain} vs {with_action}"
    );
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let face = SectionHeaderFace::group("Recent").action("Clear all");
    assert!((face.height(&mut text, &theme, 400.0) - plain).abs() < 0.5);
    let target = face
        .action_rect(Rect::new(0.0, 0.0, 400.0, plain), &mut text, &theme)
        .unwrap();
    assert!((target.x1 - (400.0 + crate::chrome::row_bleed(&theme))).abs() < 0.5);
    let mut scene = Scene::new();
    face.paint(
        &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme),
        Rect::new(0.0, 0.0, 400.0, plain),
        Interaction::default(),
    );
    assert!(!scene.encoding().resources.glyph_runs.is_empty());
}

#[test]
fn list_rows_in_a_section_are_one_list() {
    let section = Section::titled(
        "People",
        vec![
            Box::new(ListRow::new("a", "Ada")),
            Box::new(ListRow::new("b", "Grace").selected(true)),
            Box::new(ListRow::new("c", "Linus")),
            Box::new(Text::new("3 people")),
        ],
    );
    let harness = FrameHarness::new_with_theme(
        crate::State::new(),
        Box::new(section),
        (500, 400),
        Theme::dark(),
    );
    let root = harness.inspection().root;
    let rows: Vec<Rect> = root.children[1..4].iter().map(rect).collect();
    assert!((rows[1].y0 - rows[0].y1).abs() < 0.5, "rows touch");
    assert!((rows[2].y0 - rows[1].y1).abs() < 0.5);
    let footer = rect(&root.children[4]);
    let sibling = f64::from(Theme::dark().tokens.spacing.rhythm.sibling);
    assert!((footer.y0 - rows[2].y1 - sibling).abs() < 0.5);
    // Separators: under Ada hidden (Grace below is selected), under
    // Grace hidden (selected), none under Linus (no row follows).
    let mut section = Section::new(vec![
        Box::new(ListRow::new("a", "Ada")),
        Box::new(ListRow::new("b", "Grace").selected(true)),
        Box::new(ListRow::new("c", "Linus")),
        Box::new(ListRow::new("d", "Mary")),
    ]);
    section.sync_row_separators();
    let flags: Vec<(bool, bool)> = rows_of(&mut section)
        .into_iter()
        .map(|row| row.separator_state())
        .collect();
    assert_eq!(
        flags,
        [(true, true), (true, false), (true, false), (false, false)]
    );
}

#[test]
fn adjacent_selected_rows_join_into_one_lift() {
    let mut section = Section::new(vec![
        Box::new(ListRow::new("a", "Ada").selected(true)),
        Box::new(ListRow::new("b", "Grace").selected(true)),
        Box::new(ListRow::new("c", "Linus").selected(true)),
        Box::new(ListRow::new("d", "Mary")),
    ]);
    section.sync_row_separators();
    let joins: Vec<RowJoin> = rows_of(&mut section)
        .into_iter()
        .map(|row| row.join())
        .collect();
    assert_eq!(
        joins,
        [
            RowJoin::new(false, true),
            RowJoin::new(true, true),
            RowJoin::new(true, false),
            RowJoin::new(true, false),
        ]
    );
}

#[test]
fn consecutive_badged_rows_share_one_badge_column() {
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let mut section = Section::new(vec![
        Box::new(ListRow::new("a", "/users").badge("GET")),
        Box::new(ListRow::new("b", "/users").badge("DELETE")),
        Box::new(Text::new("break")),
        Box::new(ListRow::new("c", "/health").badge("GET")),
    ]);
    section.sync_rows(&mut text, &theme);
    let shared: Vec<Option<f64>> = rows_of(&mut section)
        .into_iter()
        .map(|row| row.shared_leading)
        .collect();
    let width = crate::widget::badge_column_width(&mut text, &theme, &["GET", "DELETE"]);
    assert_eq!(shared, [Some(width), Some(width), None]);
}

/// A disclosure only shows what the page already holds, so the native app
/// contract finds its header bound as a view-local control, addressed by
/// its title or by the id a page gives it.
#[test]
fn a_disclosure_header_is_a_bound_view_local_control() {
    let harness = FrameHarness::new_with_theme(
        crate::State::new(),
        Box::new(
            crate::widget::Column::new()
                .with_no_padding()
                .push(Box::new(
                    Section::new(vec![Box::new(ListRow::new("a", "Ada"))])
                        .with_header(SectionHeader::group("Details"))
                        .collapsed(),
                ))
                .push(Box::new(
                    Section::new(vec![Box::new(ListRow::new("b", "Grace"))])
                        .with_header(SectionHeader::group("Details"))
                        .collapsed()
                        .disclosure_id("wired-details"),
                )),
        ),
        (400, 300),
        crate::theme::Theme::light(),
    );
    let audit = harness.native_app_audit();
    assert!(audit.issues.is_empty(), "{audit:?}");
    fn targets(node: &InspectionNode, out: &mut Vec<String>) {
        out.extend(
            node.semantics
                .as_ref()
                .and_then(|s| s.action_target.clone()),
        );
        for child in &node.children {
            targets(child, out);
        }
    }
    let mut ids = Vec::new();
    targets(&harness.inspection().root, &mut ids);
    assert!(ids.iter().any(|id| id == "disclosure:Details"), "{ids:?}");
    assert!(ids.iter().any(|id| id == "wired-details"), "{ids:?}");
}

#[test]
fn a_collapsible_section_hides_its_items_and_toggles_from_its_header() {
    let opened = std::rc::Rc::new(std::cell::Cell::new(None));
    let seen = std::rc::Rc::clone(&opened);
    let mut section = Section::new(vec![
        Box::new(ListRow::new("a", "Ada")),
        Box::new(ListRow::new("b", "Grace")),
    ])
    .with_header(SectionHeader::group("Windows").count(2))
    .collapsed()
    .on_toggle(move |open| seen.set(Some(open)));
    assert!(!section.is_expanded());
    assert_eq!(Widget::children(&section).len(), 1, "only the header");
    let header = section.parts.children[0].as_mut();
    let node = header.accessibility().unwrap();
    assert_eq!(node.role, Role::Button);
    assert_eq!(node.state.expanded, Some(false));
    assert!(header.is_focusable());
    let mut doc = AutoCommit::new();
    assert!(header.perform_action(
        &crate::a11y::Action::Activate,
        &[],
        &mut crate::widget::Edits::lend(&mut doc),
        &automerge::ROOT
    ));
    assert!(header.take_structure_dirty(), "the section relays out");
    assert_eq!(opened.get(), Some(true));
    assert!(section.is_expanded());
    assert_eq!(Widget::children(&section).len(), 3);

    let harness = FrameHarness::new_with_theme(
        crate::State::new(),
        Box::new(
            crate::widget::Column::new()
                .with_no_padding()
                .push(Box::new(
                    Section::new(vec![Box::new(ListRow::new("a", "Ada"))])
                        .with_header(SectionHeader::group("Windows"))
                        .collapsed(),
                )),
        ),
        (400, 300),
        Theme::dark(),
    );
    let root = harness.inspection().root;
    let section = &root.children[0];
    assert_eq!(section.children.len(), 1);
    assert!(
        (rect(section).height() - rect(&section.children[0]).height()).abs() < 0.5,
        "a collapsed section is as tall as its header"
    );
}

#[test]
fn headings_take_their_role_and_follow_the_text_scale() {
    let height = |header: SectionHeader, scale: f32| {
        let harness = FrameHarness::new_with_theme(
            crate::State::new(),
            Box::new(Section::new(vec![Box::new(Text::new("body"))]).with_header(header)),
            (400, 300),
            Theme::dark().with_text_scale(scale),
        );
        harness.inspection().root.children[0].children[0].rect[3]
    };
    let theme = Theme::dark();
    let heading = height(SectionHeader::heading("Library"), 1.0);
    let group = height(SectionHeader::group("Recently added"), 1.0);
    assert!((heading - f64::from(theme.text(TextRole::Heading).line_height_px())).abs() < 1.0);
    assert!((group - f64::from(theme.text(TextRole::Subheading).line_height_px())).abs() < 1.0);
    assert!(height(SectionHeader::heading("Library"), 2.0) > heading * 1.8);
    // No header, no edge spacing.
    let plain = Section::new(vec![Box::new(Text::new("one"))]);
    assert_eq!(plain.parts.children.len(), 1);
}

#[test]
fn status_lines_and_object_headers_sit_on_the_content_edge() {
    let harness = FrameHarness::new_with_theme(
        crate::State::new(),
        Box::new(Section::new(vec![
            Box::new(
                ObjectHeader::new("Ada Lovelace")
                    .avatar("Ada Lovelace", "ada@example.org")
                    .caption("ada@example.org"),
            ),
            Box::new(StatusLine::active("Engine running")),
        ])),
        (400, 300),
        Theme::dark(),
    );
    let root = harness.inspection().root;
    let head = rect(&root.children[0]);
    let status = rect(&root.children[1]);
    let theme = Theme::dark();
    assert!(head.x0.abs() < 0.5 && status.x0.abs() < 0.5);
    assert!(head.height() >= crate::widget::AvatarSize::Large.side(&theme) - 0.5);
    assert!(
        (status.height() - f64::from(theme.text(TextRole::Caption).line_height_px())).abs() < 1.0
    );
    let line = StatusLine::warning("Update available");
    let node = Widget::accessibility(&line).unwrap();
    assert_eq!(
        node.value.as_deref(),
        Some("Needs attention: Update available")
    );
    let head = ObjectHeader::new("example.org")
        .icon(Icon::Globe)
        .caption("Login");
    let node = Widget::accessibility(&head).unwrap();
    assert_eq!(node.name.as_deref(), Some("example.org"));
}

#[test]
fn a_section_says_whether_the_pointer_is_over_it() {
    let over = Rc::new(Cell::new(false));
    let section = Section::new(vec![
        Box::new(ListRow::new("a", "Ada")),
        Box::new(ListRow::new("b", "Grace")),
    ])
    .pointer_over(Rc::clone(&over));
    let mut harness = FrameHarness::new_with_theme(
        crate::State::new(),
        Box::new(
            crate::widget::Column::new()
                .with_no_padding()
                .push(Box::new(section)),
        ),
        (500, 400),
        Theme::dark(),
    );
    let whole = rect(&harness.inspection().root.children[0]);
    let rows = whole;
    assert!(whole.y1 < 380.0, "the section is shorter than the window");
    harness.send(&crate::synthetic_input::cursor_moved(
        rows.center().x,
        rows.center().y,
    ));
    assert!(over.get(), "over a row");
    harness.send(&crate::synthetic_input::cursor_moved(10.0, whole.y1 + 10.0));
    assert!(!over.get(), "below the section");
    harness.send(&crate::synthetic_input::cursor_moved(
        rows.center().x,
        rows.center().y,
    ));
    harness.send(&WindowEvent::CursorLeft {
        device_id: winit::event::DeviceId::dummy(),
    });
    assert!(!over.get(), "the pointer left the window");
}

/// The right edge of every switch in `node`, in tree order.
fn switch_edges(node: &InspectionNode, found: &mut Vec<f64>) {
    if node.widget == "Toggle" {
        found.push(rect(node).x1);
    }
    for child in &node.children {
        switch_edges(child, found);
    }
}

/// The switch edges of a section of `rows` laid out 600 wide.
fn switch_edges_of(rows: Vec<Element>) -> Vec<f64> {
    let harness = FrameHarness::new_with_theme(
        crate::State::new(),
        Box::new(Section::new(rows)),
        (600, 400),
        Theme::light(),
    );
    let root = harness.inspection().root;
    let mut edges = Vec::new();
    switch_edges(&root, &mut edges);
    edges
}

fn switch(id: &str) -> Element {
    Box::new(crate::widget::Toggle::new(id, id))
}

#[test]
fn a_lists_switches_stand_in_one_column_beside_its_chevrons() {
    let edges = switch_edges_of(vec![
        Box::new(
            ListRow::new("home", "Home")
                .secondary("Connected")
                .trailing(switch("home"))
                .disclosure(),
        ),
        Box::new(ListRow::new("desk", "Through Desk PC").trailing(switch("desk"))),
        Box::new(
            ListRow::new("work", "Work")
                .trailing(switch("work"))
                .disclosure(),
        ),
        Box::new(ListRow::new("mini", "Through Studio Mac mini").trailing(switch("mini"))),
        Box::new(ListRow::new("add", "Add a VPN").disclosure()),
    ]);
    assert_eq!(edges.len(), 4, "every switch laid out: {edges:?}");
    assert!(
        edges.iter().all(|edge| (edge - edges[0]).abs() < 0.5),
        "one column of switches: {edges:?}"
    );
}

#[test]
fn a_list_without_marks_after_its_switches_keeps_them_on_the_edge() {
    let alone = switch_edges_of(vec![Box::new(
        ListRow::new("share", "Let my other devices use this device").trailing(switch("share")),
    )]);
    let beside_drill = switch_edges_of(vec![
        Box::new(
            ListRow::new("share", "Let my other devices use this device").trailing(switch("share")),
        ),
        Box::new(ListRow::new("more", "More options").disclosure()),
    ]);
    assert_eq!(alone.len(), 1);
    assert!(
        (alone[0] - beside_drill[0]).abs() < 0.5,
        "a chevron with no switch before it moves no switch: {alone:?} vs {beside_drill:?}"
    );
}
