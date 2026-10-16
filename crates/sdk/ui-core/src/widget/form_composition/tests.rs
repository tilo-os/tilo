use super::*;
use crate::headless::{FrameHarness, InspectionNode};
use crate::theme::Theme;
use crate::widget::ButtonRole;
use crate::widget::TextInput;

fn fixture() -> Element {
    Box::new(
        Form::new().section(
            FormSection::new("Profile")
                .field(Field::new("Name", TextInput::local().with_id("name")))
                .field(Field::new(
                    "Email address",
                    TextInput::local().with_id("email"),
                )),
        ),
    )
}

#[test]
fn form_page_reveals_at_focus_and_preserves_manual_scroll() {
    use std::cell::Cell;
    use std::rc::Rc;

    for scale in [1.0, 2.0] {
        let offset = Rc::new(Cell::new((0.0, 0.0)));
        let mut section = FormSection::new("Details");
        for index in 0..20 {
            let name = format!("Detail {index}");
            section = section.field(Field::new(
                name.clone(),
                TextInput::local().with_id(&name).with_accessible_name(name),
            ));
        }
        let page = FormPage::new("Long form", Form::new().section(section))
            .with_persisted_offset(Rc::clone(&offset));
        let mut harness = FrameHarness::new_with_theme(
            crate::State::new(),
            Box::new(page),
            (360, 420),
            Theme::dark().with_text_scale(scale),
        );
        assert!(harness.targets()["Detail 19"].y0 > 420.0);
        assert!(offset.get().1.abs() < f32::EPSILON);

        let input_node = |harness: &FrameHarness, name: &str| {
            harness
                .a11y_nodes()
                .into_iter()
                .find(|(_, node)| {
                    node.label() == Some(name) && node.role() == crate::accesskit::Role::TextInput
                })
                .unwrap()
                .0
        };
        let last = input_node(&harness, "Detail 19");
        harness
            .perform_a11y_action(last, &crate::a11y::Action::Focus)
            .unwrap();
        assert_eq!(harness.a11y_focus().unwrap().0, last);
        let revealed = harness.targets()["Detail 19"];
        assert!(revealed.y0 >= 0.0 && revealed.y1 <= 420.0, "{revealed:?}");
        let revealed_offset = offset.get().1;
        assert!(revealed_offset > 0.0);

        // A real wheel event may move the focused field off screen. Re-layout
        // must not undo that deliberate scroll while focus stays unchanged.
        harness.send(&crate::synthetic_input::cursor_moved(180.0, 210.0));
        harness.scroll(0.0, 3.0);
        let manual_offset = offset.get().1;
        assert!(manual_offset < revealed_offset);
        assert_eq!(harness.a11y_focus().unwrap().0, last);
        harness.recompose().unwrap();
        assert!((offset.get().1 - manual_offset).abs() < f32::EPSILON);

        let first = input_node(&harness, "Detail 0");
        harness
            .perform_a11y_action(first, &crate::a11y::Action::Focus)
            .unwrap();
        assert_eq!(harness.a11y_focus().unwrap().0, first);
        let revealed = harness.targets()["Detail 0"];
        assert!(revealed.y0 >= 0.0 && revealed.y1 <= 420.0, "{revealed:?}");
        assert!(offset.get().1 < manual_offset);
    }
}

fn collect<'a>(node: &'a InspectionNode, name: &str, out: &mut Vec<&'a InspectionNode>) {
    if node.widget == name {
        out.push(node);
    }
    for child in &node.children {
        collect(child, name, out);
    }
}
#[test]
fn form_labels_adapt_by_measurement_width_and_type_size() {
    fn wide_fields_align_controls_beside_measured_labels() {
        let harness = FrameHarness::new_with_theme(
            crate::State::new(),
            fixture(),
            (1000, 700),
            Theme::dark(),
        );
        let snapshot = harness.inspection();
        let mut labels = Vec::new();
        let mut controls = Vec::new();
        collect(&snapshot.root, "Text", &mut labels);
        collect(&snapshot.root, "TextInput", &mut controls);
        assert_eq!(controls.len(), 2);
        assert_eq!(controls[0].rect[0], controls[1].rect[0]);
        assert!(
            controls[0].rect[0] >= labels[2].rect[0] + labels[2].rect[2] + 4.0,
            "wide form should place controls beside measured labels: {:?} {:?}",
            labels[1].rect,
            controls[0].rect
        );
    }

    fn narrow_and_large_type_stack_long_labels_and_scroll_to_actions() {
        for scale in [1.0, 2.0] {
            let mut harness = FrameHarness::new_with_theme(
                crate::State::new(),
                Box::new(FormPage::new("Account", long_form()).with_description(
                    "Manage your personal details and notification preferences.",
                )),
                (340, 420),
                Theme::dark().with_text_scale(scale),
            );
            let snapshot = harness.inspection();
            let mut fields = Vec::new();
            collect(&snapshot.root, "Field", &mut fields);
            for field in fields {
                let label = &field.children[0];
                let body = &field.children[1];
                assert!(
                    body.rect[1] >= label.rect[1] + label.rect[3],
                    "label overlaps body"
                );
                assert_eq!(body.rect[0], label.rect[0]);
                for copy in &body.children {
                    assert!(copy.rect[0] + copy.rect[2] <= 317.0);
                }
            }
            let mut content = Vec::new();
            collect(&snapshot.root, "FormPageContent", &mut content);
            assert!(content[0].rect[3] > 420.0);
            harness.click_at(5.0, 5.0);
            harness.scroll(0.0, -10000.0);
            let end = harness.inspection();
            let save = named(&end.root, "save");
            assert!(save.rect[1] >= 0.0 && save.rect[1] + save.rect[3] <= 420.0);
        }
    }

    fn measured_long_labels_and_large_text_change_arrangement_without_breakpoints() {
        let theme = Theme::dark();
        let mut short = Field::new("Name", TextInput::local());
        let mut long = Field::new(
            "Full name as shown to all your collaborators",
            TextInput::local(),
        );
        solve(&mut short, 400.0, &theme);
        solve(&mut long, 400.0, &theme);
        assert_eq!(
            short.effective_layout().unwrap().resolved_placement,
            LabelPlacement::Beside
        );
        assert_eq!(
            long.effective_layout().unwrap().resolved_placement,
            LabelPlacement::Above
        );
        solve(&mut short, 400.0, &theme.with_text_scale(2.0));
        assert_eq!(
            short.effective_layout().unwrap().resolved_placement,
            LabelPlacement::Above
        );
    }

    fn wide_label_aligns_to_first_control_even_with_long_help_and_validation() {
        fn visible_label(widget: &dyn Widget) -> Rect {
            if widget.name() == "Text" {
                return widget.get_rect();
            }
            visible_label(widget.children()[0])
        }
        let mut field = Field::new("Email", TextInput::local())
            .with_help("Supporting information for this account. ".repeat(8))
            .with_error("Enter a valid email address.");
        solve(&mut field, 700.0, &Theme::dark());
        let label = visible_label(field.children()[0]);
        let control = field.children()[1].children()[0].get_rect();
        assert!(
            (label.center().y - control.center().y).abs() < 1.0,
            "label {label:?} should align with first control {control:?}"
        );
        solve(&mut field, 220.0, &Theme::dark());
        let label = visible_label(field.children()[0]);
        let control = field.children()[1].children()[0].get_rect();
        assert!(label.y1 <= control.y0);
    }

    fn fixed_control_needs_do_not_inherit_an_expandable_entry_minimum() {
        let mut field = Field::new(
            "Count",
            super::super::NumberInput::new(3.0)
                .with_range(0.0, 99.0)
                .compact(),
        );
        solve(&mut field, 220.0, &Theme::dark());
        assert_eq!(
            field.effective_layout().unwrap().resolved_placement,
            LabelPlacement::Beside
        );
        let control = field.children()[1].children()[0].get_rect();
        assert!(control.width() < 140.0, "{control:?}");
        assert_eq!(
            control.height(),
            super::super::ControlSize::Regular.height(&Theme::dark())
        );
    }

    wide_fields_align_controls_beside_measured_labels();
    narrow_and_large_type_stack_long_labels_and_scroll_to_actions();
    measured_long_labels_and_large_text_change_arrangement_without_breakpoints();
    wide_label_aligns_to_first_control_even_with_long_help_and_validation();
    fixed_control_needs_do_not_inherit_an_expandable_entry_minimum();
}
fn named<'a>(node: &'a InspectionNode, id: &str) -> &'a InspectionNode {
    fn find<'a>(node: &'a InspectionNode, id: &str) -> Option<&'a InspectionNode> {
        if node.test_id.as_deref() == Some(id) {
            return Some(node);
        }
        node.children.iter().find_map(|child| find(child, id))
    }
    if node.test_id.as_deref() == Some(id) {
        return node;
    }
    find(node, id).unwrap()
}
fn long_form() -> Form {
    Form::new().section(FormSection::new("Profile")
            .field(Field::new("Full name as shown to your collaborators", TextInput::local().with_id("name"))
                .with_help("This name appears on your shared documents and team workspace."))
            .field(Field::new("Email address for account recovery", TextInput::local().with_id("email"))
                .with_help("We use this address to help you sign in and send important account notifications.")
                .with_error("Enter a valid email address, including the domain after the at sign.")))
            .section(FormSection::new("Preferences").content(Box::new(super::super::Checkbox::new("updates", "updates")
                .with_label("Send me occasional product updates and tips for working with my team."))))
            .with_actions(Actions::new(super::super::Button::new("save", "Save account changes").role(ButtonRole::Primary))
                .secondary(super::super::Button::new("cancel", "Cancel")))
}
/// A field with a validation error marks its control invalid: the control
/// reports it to assistive technology (and edges itself in danger ink),
/// while a field without one stays valid.
#[test]
fn a_validation_error_marks_its_control_invalid() {
    let harness = FrameHarness::new_with_theme(
        crate::State::new(),
        Box::new(long_form()),
        (900, 700),
        Theme::dark(),
    );
    let root = harness.inspection().root;
    let invalid = |id: &str| {
        named(&root, id)
            .semantics
            .as_ref()
            .is_some_and(|node| node.state.invalid)
    };
    assert!(invalid("email"), "the field with an error is invalid");
    assert!(!invalid("name"), "a field without one is not");
}
#[test]
fn form_page_edges_and_resize_match_fresh_layout() {
    fn page_readable_width_and_single_gutter_leave_common_content_edges() {
        for width in [340, 1200] {
            let harness = FrameHarness::new_with_theme(
                crate::State::new(),
                Box::new(FormPage::new("Account", long_form())),
                (width, 1200),
                Theme::dark(),
            );
            let snapshot = harness.inspection();
            let mut forms = Vec::new();
            collect(&snapshot.root, "Form", &mut forms);
            let mut sections = Vec::new();
            collect(&snapshot.root, "FormSection", &mut sections);
            let theme = Theme::dark();
            assert!(forms[0].rect[2] <= f64::from(theme.tokens.metrics.readable_width));
            // On the page's leading edge, one gutter in, whatever the width.
            let gutter = f64::from(theme.tokens.spacing.rhythm.group);
            assert!(
                (forms[0].rect[0] - gutter).abs() < 0.5,
                "{:?}",
                forms[0].rect
            );
            for section in sections {
                assert_eq!(section.rect[0], forms[0].rect[0]);
            }
            assert_eq!(
                named(&snapshot.root, "save").rect[0] + named(&snapshot.root, "save").rect[2],
                forms[0].rect[0] + forms[0].rect[2]
            );
        }
    }

    fn resized_layout_matches_fresh_and_retains_focus_order() {
        fn geometry(node: &InspectionNode) -> Vec<[f64; 4]> {
            let mut all = vec![node.rect];
            for child in &node.children {
                all.extend(geometry(child));
            }
            all
        }
        fn keys(node: &InspectionNode, found: &mut std::collections::HashSet<String>) {
            if let Some(key) = node
                .semantics
                .as_ref()
                .and_then(|access| access.semantic_key.as_ref())
            {
                assert!(found.insert(key.clone()), "duplicate semantic key {key}");
            }
            for child in &node.children {
                keys(child, found);
            }
        }

        let mut resized = FrameHarness::new_with_theme(
            crate::State::new(),
            Box::new(FormPage::new("Account", long_form())),
            (1200, 1400),
            Theme::dark().with_text_scale(2.0),
        );
        resized.tab(false);
        resized.resize((340, 1400)).unwrap();
        let fresh = FrameHarness::new_with_theme(
            crate::State::new(),
            Box::new(FormPage::new("Account", long_form())),
            (340, 1400),
            Theme::dark().with_text_scale(2.0),
        );
        assert_eq!(
            geometry(&resized.inspection().root),
            geometry(&fresh.inspection().root)
        );
        assert_eq!(
            resized.focused_access_node().unwrap().name.as_deref(),
            Some("Full name as shown to your collaborators")
        );
        assert_eq!(
            resized
                .tab_through()
                .iter()
                .filter(|(role, _)| *role != crate::a11y::Role::Group)
                .map(|(_, name)| name.as_deref().unwrap_or(""))
                .collect::<Vec<_>>(),
            [
                "Full name as shown to your collaborators",
                "Email address for account recovery",
                "Send me occasional product updates and tips for working with my team.",
                "Cancel",
                "Save account changes"
            ]
        );
        keys(
            &resized.inspection().root,
            &mut std::collections::HashSet::new(),
        );
    }

    /// A settings card's choosers: one label fits beside its control, the other
    /// does not. The section decides once for both, and a trailing control keeps
    /// its own width — at the trailing edge beside its label, at the leading edge
    /// under it once the section stacks.
    fn trailing_controls_switch_with_their_section_and_share_the_trailing_edge() {
        let wide = FrameHarness::new_with_theme(
            crate::State::new(),
            trailing_fixture(),
            (1000, 400),
            Theme::dark(),
        );
        let snapshot = wide.inspection();
        let mut fields = Vec::new();
        collect(&snapshot.root, "Field", &mut fields);
        assert_eq!(fields.len(), 2);
        let mut sections = Vec::new();
        collect(&snapshot.root, "FormSection", &mut sections);
        let section = sections[0].rect;
        let section_x1 = section[0] + section[2];
        let controls = [
            named(&snapshot.root, "currency").rect,
            named(&snapshot.root, "first-day").rect,
        ];
        for (field, control) in fields.iter().zip(controls) {
            let mut labels = Vec::new();
            collect(field, "Text", &mut labels);
            let label = labels[0].rect;
            assert!(
                control[0] >= label[0] + label[2],
                "wide: the control sits beside its label: label {label:?} control {control:?}"
            );
            assert!(
                (control[0] + control[2] - section_x1).abs() < 0.5,
                "wide: a trailing control ends on the section's trailing edge {section_x1}: {control:?}"
            );
            assert!(
                control[2] < section[2] / 2.0,
                "wide: a trailing control keeps its own width instead of filling: {control:?}"
            );
        }
        assert!(
            (controls[0][0] + controls[0][2] - controls[1][0] - controls[1][2]).abs() < 0.5,
            "wide: sibling choosers share one trailing edge: {:?} {:?}",
            controls[0],
            controls[1]
        );

        let narrow = FrameHarness::new_with_theme(
            crate::State::new(),
            trailing_fixture(),
            (240, 600),
            Theme::dark(),
        );
        let snapshot = narrow.inspection();
        let mut fields = Vec::new();
        collect(&snapshot.root, "Field", &mut fields);
        assert_eq!(fields.len(), 2);
        let controls = [
            named(&snapshot.root, "currency").rect,
            named(&snapshot.root, "first-day").rect,
        ];
        for (field, control) in fields.iter().zip(controls) {
            let mut labels = Vec::new();
            collect(field, "Text", &mut labels);
            let label = labels[0].rect;
            assert!(
                control[1] >= label[1] + label[3],
                "narrow: every row of the section stacks, the short label too: label {label:?} control {control:?}"
            );
            assert!(
                (control[0] - label[0]).abs() < 0.5,
                "narrow: a stacked trailing control starts under its label: label {label:?} control {control:?}"
            );
        }
    }

    /// In a plain form a chooser keeps its own width but starts where the
    /// fields around it start: on the control column's leading edge.
    fn a_plain_forms_chooser_starts_on_the_fields_edge() {
        use crate::widget::{Dropdown, TextInput};
        let form: Element = Box::new(
            Form::new().section(
                FormSection::new("Machine")
                    .field(Field::new("Host", TextInput::local().with_id("host")))
                    .field(
                        Field::new(
                            "Protocol",
                            Dropdown::new(vec!["SSH".into(), "VNC".into()], "protocol")
                                .with_id("protocol"),
                        )
                        .with_trailing_control(),
                    ),
            ),
        );
        let wide =
            FrameHarness::new_with_theme(crate::State::new(), form, (1000, 400), Theme::dark());
        let snapshot = wide.inspection();
        let host = named(&snapshot.root, "host").rect;
        let protocol = named(&snapshot.root, "protocol").rect;
        assert!(
            (protocol[0] - host[0]).abs() < 0.5,
            "the chooser starts on the field's edge: {host:?} {protocol:?}"
        );
        assert!(
            protocol[2] < host[2] / 2.0,
            "and keeps its own width: {protocol:?}"
        );
    }

    page_readable_width_and_single_gutter_leave_common_content_edges();
    resized_layout_matches_fresh_and_retains_focus_order();
    trailing_controls_switch_with_their_section_and_share_the_trailing_edge();
    a_plain_forms_chooser_starts_on_the_fields_edge();
}
fn solve(widget: &mut dyn Widget, width: f32, theme: &Theme) {
    let mut tree = TaffyTree::new();
    let mut nodes = HashMap::new();
    let node = widget.build_layout(&mut tree, theme, &mut nodes);
    let mut bindings = crate::bound_dispatch::BoundNodeMap::new();
    let mut text = crate::font_catalog::fixture_renderer();
    // SAFETY: exact live, exclusively owned widget tree and matching maps.
    unsafe {
        bindings.rebuild(widget, &tree, node, &nodes);
        super::super::compute_widget_layout(
            &mut tree,
            node,
            Size {
                width: AvailableSpace::Definite(width),
                height: AvailableSpace::Definite(1000.0),
            },
            &nodes,
            &bindings,
            &mut text,
        )
        .unwrap();
    }
    widget.apply_layout(&tree, node, Point::ZERO);
}
#[test]
fn form_overrides_apply_and_reset_independently() {
    fn independent_overrides_reset_geometry_and_keep_nearest_origins() {
        let theme = Theme::dark();
        let mut form = Form::new()
            .with_density(Some(Density::Compact))
            .with_section_spacing(Some(73.0))
            .with_field_spacing(Some(31.0))
            .with_label_placement(Some(LabelPlacement::Above))
            .section(
                FormSection::new("Profile")
                    .with_density(Some(Density::Comfortable))
                    .with_label_placement(Some(LabelPlacement::Beside))
                    .field(
                        Field::new("Name", TextInput::local())
                            .with_label_placement(Some(LabelPlacement::Above)),
                    )
                    .field(Field::new("Email", TextInput::local())),
            )
            .section(FormSection::new("Preferences").content(Box::new(Text::new("Details"))));
        solve(&mut form, 900.0, &theme);
        let info = form.effective_layout().unwrap();
        assert_eq!(
            info.section_spacing,
            Effective::new(73.0, LayoutOrigin::Form)
        );
        let children = form.children();
        let Some(LayoutInfo::Section(section)) = children[0].layout_info() else {
            panic!("section info")
        };
        assert_eq!(
            section.field_spacing,
            Effective::new(31.0, LayoutOrigin::Form)
        );
        assert_eq!(section.density.origin, LayoutOrigin::Section);
        let fields = children[0].children();
        let Some(LayoutInfo::Field(first)) = fields[1].layout_info() else {
            panic!("field info")
        };
        let Some(LayoutInfo::Field(second)) = fields[2].layout_info() else {
            panic!("field info")
        };
        assert_eq!(first.label_placement.origin, LayoutOrigin::Field);
        assert_eq!(first.resolved_placement, LabelPlacement::Above);
        assert_eq!(second.label_placement.origin, LayoutOrigin::Section);
        assert_eq!(second.resolved_placement, LabelPlacement::Beside);
        assert!((children[1].get_rect().y0 - children[0].get_rect().y1 - 73.0).abs() < 1.0);
        form = form.with_section_spacing(None);
        solve(&mut form, 900.0, &theme);
        let info = form.effective_layout().unwrap();
        assert_eq!(info.density.value, Density::Compact);
        assert_eq!(info.field_spacing.value, 31.0);
        assert!(info.section_spacing.value < 73.0);
        assert_eq!(info.label_placement.value, LabelPlacement::Above);
        form = form
            .with_field_spacing(None)
            .with_density(None)
            .with_label_placement(None);
        solve(&mut form, 340.0, &theme);
        let info = form.effective_layout().unwrap();
        assert_eq!(info.density.origin, LayoutOrigin::Pattern);
        assert_eq!(info.label_placement.origin, LayoutOrigin::Pattern);
        assert_eq!(info.field_spacing.origin, LayoutOrigin::Theme);
    }

    fn page_width_override_and_reset_leave_label_adaptation_active() {
        let mut page = FormPage::new("Account", long_form()).with_max_content_width(Some(400.0));
        solve(&mut page, 1200.0, &Theme::dark());
        assert_eq!(
            page.effective_layout().unwrap().max_content_width,
            Effective::new(400.0, LayoutOrigin::Page)
        );
        let form = page.children()[0].children()[0]
            .children()
            .last()
            .unwrap()
            .get_rect();
        assert_eq!(form.width(), 400.0);
        page = page.with_max_content_width(None);
        solve(&mut page, 1200.0, &Theme::dark());
        assert_eq!(
            page.effective_layout().unwrap().max_content_width.origin,
            LayoutOrigin::Pattern
        );
        let form = page.children()[0].children()[0]
            .children()
            .last()
            .unwrap()
            .get_rect();
        assert_eq!(form.width(), 720.0);
    }

    fn clearing_section_and_field_overrides_restores_only_the_selected_property() {
        let theme = Theme::dark();
        let mut section = FormSection::new("Profile")
            .with_density(Some(Density::Compact))
            .with_field_spacing(Some(39.0))
            .with_label_placement(Some(LabelPlacement::Beside))
            .field(Field::new("Name", TextInput::local()));
        solve(&mut section, 340.0, &theme);
        section = section.with_density(None);
        solve(&mut section, 340.0, &theme);
        let info = section.effective_layout().unwrap();
        assert_eq!(info.density.origin, LayoutOrigin::Pattern);
        assert_eq!(info.field_spacing.value, 39.0);
        assert_eq!(info.label_placement.value, LabelPlacement::Beside);
        section = section.with_field_spacing(None).with_label_placement(None);
        solve(&mut section, 340.0, &theme);
        assert_eq!(
            section.effective_layout().unwrap().field_spacing.origin,
            LayoutOrigin::Theme
        );
        assert_eq!(
            section.effective_layout().unwrap().label_placement.origin,
            LayoutOrigin::Pattern
        );
        let mut field = Field::new("Long field label for a narrow window", TextInput::local())
            .with_label_placement(Some(LabelPlacement::Beside));
        solve(&mut field, 300.0, &theme);
        assert_eq!(
            field.effective_layout().unwrap().resolved_placement,
            LabelPlacement::Beside
        );
        field = field.with_label_placement(None);
        solve(&mut field, 300.0, &theme);
        assert_eq!(
            field.effective_layout().unwrap().resolved_placement,
            LabelPlacement::Above
        );
        assert_eq!(
            field.effective_layout().unwrap().label_placement.origin,
            LayoutOrigin::Pattern
        );
    }

    independent_overrides_reset_geometry_and_keep_nearest_origins();
    page_width_override_and_reset_leave_label_adaptation_active();
    clearing_section_and_field_overrides_restores_only_the_selected_property();
}
#[test]
fn grouped_sections_are_settings_rows_with_separators() {
    use crate::widget::{Dropdown, Toggle};
    let theme = Theme::dark();
    let section = FormSection::new("Camera")
        .grouped()
        .field(Field::new("Grid lines", Toggle::new("grid", "grid")))
        .field(Field::new(
            "Photo format",
            Dropdown::new(vec!["HEIF".into(), "JPEG".into()], "format"),
        ));
    let harness = FrameHarness::new_with_theme(
        crate::State::new(),
        Box::new(Form::new().section(section)),
        (720, 400),
        theme.clone(),
    );
    let snapshot = harness.inspection();
    let mut fields = Vec::new();
    collect(&snapshot.root, "Field", &mut fields);
    assert_eq!(fields.len(), 2);
    let row = f64::from(theme.tokens.metrics.panel_row_height + theme.tokens.spacing.s2);
    for field in &fields {
        assert!(
            field.rect[3] >= row - 0.5,
            "a grouped row is at least {row} tall"
        );
    }
    // Rows sit edge to edge; the separator lives between them.
    let gap = fields[1].rect[1] - (fields[0].rect[1] + fields[0].rect[3]);
    assert!(gap.abs() < 0.5, "rows touch, got {gap}");
    let mut titles = Vec::new();
    collect(&snapshot.root, "Text", &mut titles);
    let title = titles.first().expect("section title");
    let expected = f64::from(theme.text(TextRole::Heading).line_height_px());
    assert!(
        (title.rect[3] - expected).abs() < 1.0,
        "the title is a Heading"
    );
}

fn value_fixture(german: bool) -> Element {
    let pairs = if german {
        [
            ("Lokale Modelle", "Bleiben auf diesem Gerät"),
            ("Downloads", "Nur mit deiner Zustimmung"),
            ("Kompatibilität", "Nicht genug Speicher"),
            (
                "Empfohlenes Modell",
                "Warte auf den signierten Modellkatalog",
            ),
        ]
    } else {
        [
            ("Local models", "Stay on this device"),
            ("Downloads", "Only with your approval"),
            ("Compatibility", "Not enough memory"),
            ("Recommended model", "Waiting for signed model catalog"),
        ]
    };
    Box::new(pairs.into_iter().fold(
        FormSection::untitled().grouped(),
        |section, (label, value)| section.field(Field::value(label, value)),
    ))
}

#[test]
fn read_only_values_reserve_readable_phrase_width_in_both_themes() {
    // The content width left by an ordinary Settings window and its sidebar.
    for theme in [Theme::light(), Theme::dark()] {
        for german in [false, true] {
            let harness = FrameHarness::new_with_theme(
                crate::State::new(),
                value_fixture(german),
                (500, 768),
                theme.clone(),
            );
            let snapshot = harness.inspection();
            let mut fields = Vec::new();
            collect(&snapshot.root, "Field", &mut fields);
            assert_eq!(fields.len(), 4);
            for field in fields {
                let label = &field.children[0];
                let body = &field.children[1];
                assert!(body.rect[0] >= label.rect[0] + label.rect[2]);
                assert!(body.rect[2] >= 0.4 * field.rect[2], "{:?}", body.rect);
                assert!(body.rect[0] + body.rect[2] <= 500.5);
                let mut texts = Vec::new();
                collect(body, "Text", &mut texts);
                assert_eq!(texts.len(), 1);
                let line = f64::from(theme.text(TextRole::Body).line_height_px());
                assert!(texts[0].rect[3] <= 2.0 * line + 1.0, "{:?}", texts[0].rect);
            }
        }
    }
}

#[test]
fn read_only_values_stack_without_overlap_at_narrow_width_and_large_type() {
    for theme in [Theme::light(), Theme::dark()] {
        for german in [false, true] {
            for scale in [1.0, 2.0] {
                let harness = FrameHarness::new_with_theme(
                    crate::State::new(),
                    value_fixture(german),
                    (320, 1600),
                    theme.clone().with_text_scale(scale),
                );
                let snapshot = harness.inspection();
                let mut fields = Vec::new();
                collect(&snapshot.root, "Field", &mut fields);
                assert_eq!(fields.len(), 4);
                for field in &fields {
                    let label = &field.children[0];
                    let body = &field.children[1];
                    assert!(body.rect[1] >= label.rect[1] + label.rect[3]);
                    assert!((body.rect[0] - label.rect[0]).abs() < 0.5);
                    assert!(body.rect[0] + body.rect[2] <= 320.5);
                    let mut names = Vec::new();
                    semantic_names(field, &mut names);
                    assert_eq!(names.len(), 2, "each field retains its label and value");
                }
                for pair in fields.windows(2) {
                    assert!(pair[1].rect[1] >= pair[0].rect[1] + pair[0].rect[3]);
                }
            }
        }
    }
}

fn trailing_fixture() -> Element {
    use crate::widget::Dropdown;
    Box::new(
        Form::new().section(
            FormSection::new("Formats")
                .grouped()
                .field(
                    Field::new(
                        "Currency",
                        Dropdown::new(vec!["USD".into(), "EUR".into()], "currency")
                            .with_id("currency"),
                    )
                    .with_trailing_control(),
                )
                .field(
                    Field::new(
                        "First day of the week in calendars",
                        Dropdown::new(vec!["Monday".into(), "Sunday".into()], "first-day")
                            .with_id("first-day"),
                    )
                    .with_trailing_control(),
                ),
        ),
    )
}

fn semantic_names(node: &InspectionNode, out: &mut Vec<(crate::a11y::Role, String)>) {
    if let Some(access) = &node.semantics
        && let Some(name) = &access.name
    {
        out.push((access.role, name.clone()));
    }
    for child in &node.children {
        semantic_names(child, out);
    }
}

fn first_text(node: &InspectionNode) -> &InspectionNode {
    let mut texts = Vec::new();
    collect(node, "Text", &mut texts);
    texts[0]
}

#[test]
fn untitled_sections_custom_fields_and_control_rows() {
    use crate::widget::{Button, Row};
    let form = Form::new()
        .section(
            FormSection::untitled()
                .field(Field::new("Name", TextInput::local().with_id("name")))
                .field(Field::custom(
                    "Version",
                    Box::new(Text::new("2.4.1").with_id("version")),
                ))
                .control_row(Box::new(
                    Row::new()
                        .push(Box::new(Button::new("export", "Export")))
                        .push(Box::new(Button::new("import", "Import"))),
                )),
        )
        .section(
            FormSection::new("Account details for this device")
                .field(Field::new("Email", TextInput::local().with_id("email"))),
        );
    let harness = FrameHarness::new_with_theme(
        crate::State::new(),
        Box::new(form),
        (1000, 700),
        Theme::dark(),
    );
    let root = harness.inspection().root;
    let mut names = Vec::new();
    semantic_names(&root, &mut names);
    // No heading for the untitled section; the titled one keeps its own.
    let headings: Vec<_> = names
        .iter()
        .filter(|(role, _)| matches!(role, crate::a11y::Role::Heading { .. }))
        .collect();
    assert_eq!(headings.len(), 1, "{headings:?}");
    // A control's label is read once, through the control.
    assert_eq!(
        names.iter().filter(|(_, name)| name == "Name").count(),
        1,
        "{names:?}"
    );
    assert!(
        names
            .iter()
            .any(|(role, name)| *role == crate::a11y::Role::Label && name == "Version"),
        "a custom field's label is a label: {names:?}"
    );
    // One label column across both sections: the controls line up even
    // though the second section's label is the longer one.
    let name = named(&root, "name").rect;
    let email = named(&root, "email").rect;
    let version = named(&root, "version").rect;
    assert!((name[0] - email[0]).abs() < 0.5, "{name:?} {email:?}");
    assert!((version[0] - email[0]).abs() < 0.5, "{version:?} {email:?}");
    // The control row spans both columns: its first button starts on the
    // label column.
    let export = named(&root, "export").rect;
    let mut labels = Vec::new();
    collect(&root, "FieldLabel", &mut labels);
    assert!((export[0] - labels[0].rect[0]).abs() < 0.5, "{export:?}");
}

#[test]
fn help_reads_under_the_label_beside_and_under_the_control_stacked() {
    let field = || {
        Field::new("Name", TextInput::local().with_id("name")).with_help("Shown to collaborators.")
    };
    let wide = FrameHarness::new_with_theme(
        crate::State::new(),
        Box::new(Form::new().section(FormSection::untitled().field(field()))),
        (900, 400),
        Theme::dark(),
    );
    let root = wide.inspection().root;
    let mut fields = Vec::new();
    collect(&root, "Field", &mut fields);
    let label = fields[0].children[0].rect;
    let help = fields[0].children[2].rect;
    let control = named(&root, "name").rect;
    assert!(help[1] >= label[1] + label[3], "help under the label");
    assert!((help[0] - label[0]).abs() < 0.5, "in the label column");
    assert!(help[0] + help[2] <= control[0] + 0.5, "beside the control");

    let narrow = FrameHarness::new_with_theme(
        crate::State::new(),
        Box::new(Form::new().section(FormSection::untitled().field(field()))),
        (220, 400),
        Theme::dark().with_text_scale(2.0),
    );
    let root = narrow.inspection().root;
    let mut fields = Vec::new();
    collect(&root, "Field", &mut fields);
    let help = fields[0].children[2].rect;
    let control = named(&root, "name").rect;
    assert!(
        help[1] >= control[1] + control[3],
        "stacked: help under the control"
    );
}

#[test]
fn grouped_switches_end_on_the_trailing_edge_and_stack_full_width() {
    use crate::widget::{Dropdown, Toggle};
    let section = || {
        FormSection::new("Camera")
            .grouped()
            .field(Field::new(
                "Grid lines",
                Toggle::new("grid", "grid").with_id("grid"),
            ))
            .field(Field::new(
                "Photo format",
                Dropdown::new(vec!["HEIF".into(), "JPEG".into()], "format").with_id("format"),
            ))
    };
    let wide = FrameHarness::new_with_theme(
        crate::State::new(),
        Box::new(Form::new().section(section())),
        (800, 400),
        Theme::dark(),
    );
    let root = wide.inspection().root;
    let mut sections = Vec::new();
    collect(&root, "FormSection", &mut sections);
    let edge = sections[0].rect[0] + sections[0].rect[2];
    let grid = named(&root, "grid").rect;
    let format = named(&root, "format").rect;
    assert!(
        (grid[0] + grid[2] - edge).abs() < 0.5,
        "switch on the trailing edge: {grid:?} {edge}"
    );
    assert!((format[0] + format[2] - edge).abs() < 0.5, "{format:?}");

    let narrow = FrameHarness::new_with_theme(
        crate::State::new(),
        Box::new(Form::new().section(section())),
        (200, 500),
        Theme::dark().with_text_scale(2.0),
    );
    let root = narrow.inspection().root;
    let mut fields = Vec::new();
    collect(&root, "Field", &mut fields);
    let gap = fields[1].rect[1] - (fields[0].rect[1] + fields[0].rect[3]);
    let theme = Theme::dark();
    assert!(
        gap >= f64::from(theme.tokens.spacing.s3) - 0.5,
        "stacked rows are spaced: {gap}"
    );
    let format = named(&root, "format").rect;
    let mut sections = Vec::new();
    collect(&root, "FormSection", &mut sections);
    // A stacked chooser leaves the trailing edge for its row's leading
    // edge, under its label (as wide as the column lets an expandable
    // control be; a dropdown sized to its options keeps that size).
    assert!(
        (format[0] - sections[0].rect[0]).abs() < 0.5,
        "a stacked chooser starts under its label: {format:?}"
    );
    assert!(format[0] + format[2] <= sections[0].rect[0] + sections[0].rect[2] + 0.5);
}

#[test]
fn a_page_keeps_sections_on_the_leading_readable_column_with_pinned_actions() {
    use crate::widget::{Button, Section};
    let theme = Theme::dark();
    let page = Page::new()
        .head(Box::new(Text::new("Summary").with_id("head")))
        .section(Box::new(Section::titled(
            "First",
            vec![Box::new(Text::new("One").with_id("one"))],
        )))
        .section(Box::new(Section::titled(
            "Second",
            vec![Box::new(Text::new("Two").with_id("two"))],
        )))
        .with_pinned_actions(Actions::new(
            Button::new("save", "Save").role(ButtonRole::Primary),
        ));
    let harness = FrameHarness::new_with_theme(
        crate::State::new(),
        Box::new(page),
        (1600, 600),
        theme.clone(),
    );
    let root = harness.inspection().root;
    let gutter = f64::from(theme.tokens.spacing.rhythm.group);
    let head = named(&root, "head").rect;
    assert!((head[0] - gutter).abs() < 0.5, "leading edge: {head:?}");
    let mut sections = Vec::new();
    collect(&root, "Section", &mut sections);
    let between = sections[1].rect[1] - (sections[0].rect[1] + sections[0].rect[3]);
    assert!(
        (between - gutter).abs() < 0.5,
        "sections a group apart: {between}"
    );
    let save = named(&root, "save").rect;
    let readable = f64::from(theme.tokens.metrics.readable_width);
    assert!(
        (save[0] + save[2] - gutter - readable).abs() < 0.5,
        "the pinned action ends where the readable column ends: {save:?}"
    );
    assert!(
        save[1] + save[3] <= 600.0 && save[1] > 500.0,
        "pinned to the bottom: {save:?}"
    );
}

#[test]
fn a_form_page_titles_in_large_title_or_not_at_all() {
    use crate::widget::Button;
    let theme = Theme::dark();
    let titled = FrameHarness::new_with_theme(
        crate::State::new(),
        Box::new(
            FormPage::new("Account", long_form()).with_header_action(Button::new("reset", "Reset")),
        ),
        (1000, 800),
        theme.clone(),
    );
    let root = titled.inspection().root;
    let title = first_text(&root);
    let expected = f64::from(theme.text(TextRole::LargeTitle).line_height_px());
    assert!((title.rect[3] - expected).abs() < 1.0, "{:?}", title.rect);
    let reset = named(&root, "reset").rect;
    assert!(
        reset[0] > title.rect[0] + title.rect[2],
        "the action beside the title"
    );

    let untitled = FrameHarness::new_with_theme(
        crate::State::new(),
        Box::new(FormPage::untitled(long_form())),
        (1000, 800),
        theme,
    );
    let mut names = Vec::new();
    semantic_names(&untitled.inspection().root, &mut names);
    assert!(
        !names
            .iter()
            .any(|(role, _)| matches!(role, crate::a11y::Role::Heading { level: 1 })),
        "{names:?}"
    );
}
