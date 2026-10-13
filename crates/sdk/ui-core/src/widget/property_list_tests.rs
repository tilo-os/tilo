use super::*;
use crate::headless::{FrameHarness, InspectionNode};

#[test]
fn published_property_controls_invoke_real_clipboard_and_custom_actions() {
    use crate::clipboard::{ClipboardBackend, ClipboardTarget, InProcessClipboard};
    let clipboard = std::sync::Arc::new(InProcessClipboard::default());
    let services = crate::host::HostServices::null().with_clipboard(clipboard.clone());
    crate::host::scoped(&services, || {
        let activated = Rc::new(Cell::new(0));
        let signal = activated.clone();
        let mut driver = FrameHarness::new(
            crate::State::new(),
            Box::new(
                PropertyList::new(vec![
                    Property::new("Commit", "123456789abcdef").copyable(),
                    Property::new("Website", "https://example.org").action(
                        Icon::ExternalLink,
                        "Open",
                        move || signal.set(signal.get() + 1),
                    ),
                ])
                .with_id("evidence-properties"),
            ),
            (640, 240),
        );
        let audit = driver.native_app_audit();
        assert!(audit.is_certified(), "{audit:?}");
        for label in ["Copy Commit", "Open Website"] {
            let resource = driver
                .resources()
                .into_iter()
                .find(|resource| {
                    resource
                        .id
                        .as_str()
                        .starts_with(crate::headless::CONTROL_RESOURCE_PREFIX)
                        && resource.label == label
                })
                .expect("the actual property action is published through the resource door");
            driver
                .invoke_action(
                    &tilo_actions::ActionAddress::new(resource.id.as_str(), "activate"),
                    &serde_json::Map::new(),
                    false,
                    None,
                )
                .unwrap();
        }
        assert_eq!(
            clipboard.text(ClipboardTarget::Clipboard).as_deref(),
            Some("123456789abcdef")
        );
        assert_eq!(activated.get(), 1);
        assert!(
            driver
                .perform_semantic_named(
                    "evidence-properties",
                    Some("tilo.key-action:evidence-properties:0/9"),
                    &crate::a11y::Action::Activate
                )
                .is_err()
        );
        assert_eq!(activated.get(), 1);

        let mut value = FrameHarness::new(
            crate::State::new(),
            Box::new(
                PropertyValue::new("acct_123")
                    .with_id("account-value")
                    .copyable()
                    .with_accessible_name("Account"),
            ),
            (320, 120),
        );
        let audit = value.native_app_audit();
        assert!(audit.is_certified(), "{audit:?}");
        let resource = value
            .resources()
            .into_iter()
            .find(|resource| {
                resource
                    .id
                    .as_str()
                    .starts_with(crate::headless::CONTROL_RESOURCE_PREFIX)
                    && resource.label == "Copy"
            })
            .expect("read-only property's Copy is an ordinary published action");
        value
            .invoke_action(
                &tilo_actions::ActionAddress::new(resource.id.as_str(), "activate"),
                &serde_json::Map::new(),
                false,
                None,
            )
            .unwrap();
        assert_eq!(
            clipboard.text(ClipboardTarget::Clipboard).as_deref(),
            Some("acct_123")
        );
    });
}

#[test]
fn editable_text_still_requires_an_action_binding() {
    struct UnboundEditable;
    impl Widget for UnboundEditable {
        crate::testing::inert_widget_methods!(leaf_layout);
        fn name(&self) -> &'static str {
            "UnboundEditable"
        }
        fn test_id(&self) -> Option<&str> {
            Some("editable-unbound")
        }
        fn accessibility(&self) -> Option<AccessNode> {
            Some(AccessNode::new(Role::TextInput).with_name("Editable field"))
        }
    }
    let driver = FrameHarness::new(crate::State::new(), Box::new(UnboundEditable), (320, 120));
    let audit = driver.native_app_audit();
    assert!(
        audit
            .issues
            .iter()
            .any(|issue| issue.control == "editable-unbound"
                && issue.code == "missing-action-binding"),
        "editable fields still need actual action provenance: {audit:?}"
    );
}

fn rows() -> Vec<PropertyRow<'static>> {
    vec![
        PropertyRow::new("Email", "ada.lovelace@example.org"),
        PropertyRow::new("Phone", "+44 20 7946 0958").with_kind(TextKind::Numeric),
        PropertyRow::new(
            "Address",
            "12 St James's Square, London SW1Y 4JH, United Kingdom",
        ),
        PropertyRow::new("Birthday", "10 December 1815").action(Icon::Copy, "Copy"),
    ]
}

fn regular(theme: &Theme) -> ListMetrics {
    ListMetrics::new(theme, Density::Regular)
}

fn layout(
    text: &mut TextRenderer,
    theme: &Theme,
    rows: &[PropertyRow<'_>],
    width: f64,
) -> ListLayout {
    lay_out(
        text,
        &regular(theme),
        &[(None, rows)],
        Point::ZERO,
        width,
        None,
    )
}

fn rect(node: &InspectionNode) -> Rect {
    let [x, y, w, h] = node.rect;
    Rect::new(x, y, x + w, y + h)
}

#[test]
fn labels_share_one_measured_column_and_values_line_up() {
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let rows = rows();
    let m = regular(&theme);
    let layout = layout(&mut text, &theme, &rows, 640.0);
    assert!(!layout.stacked);
    let x = layout.rows[0].value.x;
    for row in &layout.rows {
        assert!((row.value.x - x).abs() < 0.01, "values share a column");
        assert!(row.value.x > row.label.x);
        assert!(row.rect.height() >= m.min_row);
    }
    // The longest label decides the column: values start after it.
    let longest = f64::from(text.measure_style("Birthday", None, &m.label).0);
    assert!(x >= m.pad_x + longest + m.column_gap - 0.5);
}

#[test]
fn labels_start_on_the_content_edge_and_stripes_bleed_past_it() {
    let theme = Theme::dark();
    let bleed = crate::chrome::row_bleed(&theme);
    // A face's rect is the outer rect: labels sit one bleed in.
    let mut text = TextRenderer::new();
    let rows = rows();
    let laid = layout(&mut text, &theme, &rows, 400.0);
    assert!((laid.rows[0].label.x - bleed).abs() < 0.01);
    // A retained list in a padded column: labels on its content edge.
    let harness = FrameHarness::new(
        crate::State::new(),
        Box::new(
            crate::widget::Column::new()
                .with_horizontal_padding(24.0, 24.0)
                .push(Box::new(
                    PropertyList::new(vec![Property::new("Size", "4 KB")]).striped(),
                )),
        ),
        (400, 200),
    );
    fn find(node: &InspectionNode) -> Option<&InspectionNode> {
        if node.widget == "PropertyList" {
            return Some(node);
        }
        node.children.iter().find_map(find)
    }
    let list = rect(find(&harness.inspection().root).unwrap());
    assert!((list.x0 - 24.0).abs() < 0.5);
    let mut retained = PropertyList::new(vec![Property::new("Size", "4 KB")]);
    retained.metrics = Some(regular(&theme));
    retained.rect = list;
    let laid = retained.lay_out_at(
        &mut text,
        &regular(&theme),
        retained.outer().origin(),
        retained.outer().width(),
    );
    assert!(
        (laid.rows[0].label.x - list.x0).abs() < 0.01,
        "label on the edge"
    );
    assert!(laid.rows[0].rect.x0 < list.x0, "the stripe bleeds outward");
}

#[test]
fn a_narrow_list_stacks_labels_above_values() {
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let rows = rows();
    let face = PropertyListFace::new(&rows);
    assert!(!face.is_stacked(&mut text, &theme, 640.0));
    assert!(face.is_stacked(&mut text, &theme, 220.0));
    let laid = layout(&mut text, &theme, &rows, 220.0);
    for row in &laid.rows {
        assert!((row.value.x - row.label.x).abs() < 0.01, "one column");
        assert!(row.value.y > row.label.y, "label above value");
    }
    // Larger text stacks sooner.
    let big = Theme::dark().with_text_scale(2.0);
    assert!(!face.is_stacked(&mut text, &theme, 360.0));
    assert!(face.is_stacked(&mut text, &big, 360.0));
    assert!(face.height(&mut text, &big, 640.0) > face.height(&mut text, &theme, 640.0) * 1.5);
}

#[test]
fn up_to_two_actions_sit_at_the_trailing_edge_of_their_row() {
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let rows = rows();
    let face = PropertyListFace::new(&rows);
    let targets = face.action_rects(&mut text, &theme, Point::new(10.0, 20.0), 500.0);
    assert_eq!(targets.len(), 1);
    let (index, rect) = targets[0];
    assert_eq!(index, 3);
    assert!(rect.x1 <= 510.0 && rect.x1 > 480.0);
    let two = [PropertyRow::new("Password", "hunter2")
        .with_kind(TextKind::Secret)
        .action(Icon::Eye, "Reveal")
        .action(Icon::Copy, "Copy")
        .action(Icon::Trash, "Ignored")];
    let targets = PropertyListFace::new(&two).action_targets(&mut text, &theme, Point::ZERO, 500.0);
    assert_eq!(targets.len(), 2, "a row holds two actions");
    assert!(targets[0].1.x1 < targets[1].1.x0, "in order");
}

#[test]
fn unbreakable_values_elide_before_the_actions_and_prose_wraps() {
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let key = "a".repeat(200);
    let rows = [
        PropertyRow::new("Key", &key)
            .with_kind(TextKind::Identifier)
            .action(Icon::Copy, "Copy"),
        PropertyRow::new(
            "Notes",
            "A long note that wraps across several lines at this width, as prose does.",
        ),
    ];
    let laid = layout(&mut text, &theme, &rows, 360.0);
    let id = &laid.rows[0];
    assert!(id.elided.is_some(), "the identifier is elided");
    let line = regular(&theme).line(&mut text);
    assert!(id.rect.height() < line * 2.0 + 16.0, "one line");
    assert!(id.value.x + id.value_width <= id.actions[0].unwrap().x0 + 0.5);
    assert!(laid.rows[1].rect.height() > line * 2.0, "prose wraps");
}

#[test]
fn a_compact_list_is_denser_and_toned_values_stay_legible() {
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let rows = rows();
    let regular = PropertyListFace::new(&rows).height(&mut text, &theme, 640.0);
    let compact = PropertyListFace::new(&rows)
        .compact()
        .height(&mut text, &theme, 640.0);
    assert!(compact < regular);
    for theme in [Theme::dark(), Theme::light()] {
        let ground = theme.tokens.colors.surfaces.content;
        let ink = theme.tokens.colors.ink_on(
            ground,
            Tone::Danger.colour(&theme),
            SMALL_TEXT_MIN_CONTRAST,
        );
        assert!(crate::a11y::contrast_ratio(ink, ground) >= SMALL_TEXT_MIN_CONTRAST);
    }
}

#[test]
fn groups_and_lists_in_a_section_share_one_label_column() {
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let short = [PropertyRow::new("Age", "36")];
    let long = [PropertyRow::new("Date of birth", "10 December 1815")];
    let laid = lay_out(
        &mut text,
        &regular(&theme),
        &[(Some("Personal"), &short), (Some("Dates"), &long)],
        Point::ZERO,
        640.0,
        None,
    );
    assert_eq!(laid.groups.len(), 2);
    assert!((laid.rows[0].value.x - laid.rows[1].value.x).abs() < 0.01);
    assert!(laid.groups[1].title_at.y > laid.rows[0].rect.y1);
    // Two retained lists in one section line their values up.
    let harness = FrameHarness::new_with_theme(
        crate::State::new(),
        Box::new(crate::widget::Section::new(vec![
            Box::new(PropertyList::new(vec![Property::new("Age", "36")]).with_id("a")),
            Box::new(
                PropertyList::new(vec![Property::new("Date of birth", "10 December 1815")])
                    .with_id("b"),
            ),
        ])),
        (600, 400),
        theme.clone(),
    );
    let width = property_label_column(&mut text, &theme, &long);
    let root = harness.inspection().root;
    assert_eq!(root.children.len(), 2);
    let column = PropertyColumn::new();
    column.offer(width);
    assert!((column.get() - width).abs() < 0.01);
}

#[test]
fn accessibility_reads_each_value_and_its_actions() {
    let list = PropertyList::new(vec![
        Property::new("Password", "hunter2")
            .with_kind(TextKind::Secret)
            .with_accessible_value("Hidden")
            .copyable(),
    ])
    .with_id("login");
    let node = Widget::accessibility(&list).unwrap();
    assert_eq!(list.test_id(), Some("login"));
    let row = &node.children[0];
    assert_eq!(row.name.as_deref(), Some("Password"));
    assert_eq!(row.value.as_deref(), Some("Hidden"));
    assert_eq!(row.children[0].role, Role::Button);
    assert_eq!(row.children[0].name.as_deref(), Some("Copy Password"));
}

#[test]
fn a_read_only_value_reads_as_a_read_only_field() {
    let value = PropertyValue::new("acct_1234567890")
        .with_kind(TextKind::Identifier)
        .copyable()
        .with_accessible_name("Account");
    let node = Widget::accessibility(&value).unwrap();
    assert_eq!(node.role, Role::TextInput);
    assert_eq!(node.name.as_deref(), Some("Account"));
    assert!(node.state.read_only);
    assert_eq!(node.children.len(), 1);
}

#[test]
fn stripes_are_a_derived_tint_that_registers() {
    for theme in [Theme::dark(), Theme::light()] {
        let ground = theme.tokens.colors.surfaces.content;
        let stripe =
            crate::tokens::colors::composite(Colors::hairline_on(ground, STRIPE_ALPHA), ground);
        assert!(crate::surface::edge_delta(stripe, ground) > 0.0);
    }
}

#[test]
fn published_property_action_bounds_follow_the_painted_hit_target() {
    for scale in [1.0, 2.0] {
        let activated = Rc::new(Cell::new(false));
        let signal = activated.clone();
        let theme = Theme::dark().with_text_scale(scale);
        let mut harness = FrameHarness::new_with_theme(
            crate::State::new(),
            Box::new(PropertyList::new(vec![
                Property::new("Website", "https://example.org").action(
                    Icon::ExternalLink,
                    "Open",
                    move || signal.set(true),
                ),
            ])),
            (480, 240),
            theme,
        );
        let (_, action) = harness
            .a11y_nodes()
            .into_iter()
            .find(|(_, node)| node.label() == Some("Open Website"))
            .expect("the measured property publishes its action");
        let bounds = action.bounds().expect("the synthetic action has bounds");
        assert!(bounds.width() > 0.0 && bounds.height() > 0.0);
        let center = Rect::new(bounds.x0, bounds.y0, bounds.x1, bounds.y1).center();
        harness.click_at(center.x, center.y);
        assert!(
            activated.get(),
            "published bounds reach the real painted action at {scale}x"
        );
    }
}
