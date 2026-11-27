//! A form page's fields, labels, sections and actions stand in their places
//! and are named once, at each width and text scale.

use std::cell::*;
use std::collections::*;
use std::path::*;
use std::rc::*;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use taffy::{AvailableSpace, Size};
use tilo_ui_app::app::driver::Driver;
use tilo_ui_app::headless::*;
use tilo_ui_app::kurbo::*;
use tilo_ui_app::retained_present::*;
use tilo_ui_app::testing::*;
use tilo_ui_app::vello::*;
use tilo_ui_app::widget::*;
use tilo_ui_base::a11y::*;
use tilo_ui_base::component::*;
use tilo_ui_base::i18n::*;
use tilo_ui_base::icons::*;
use tilo_ui_base::interaction::*;
use tilo_ui_base::motion::*;
use tilo_ui_base::offscreen::*;
use tilo_ui_base::paint::*;
use tilo_ui_base::peniko::*;
use tilo_ui_base::presence::*;
use tilo_ui_base::synthetic_input::*;
use tilo_ui_base::text_render::*;
use tilo_ui_base::text_selection::*;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::*;
use tilo_ui_base::widget::*;
use winit::event::*;
use winit::keyboard::*;

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

fn collect<'a>(node: &'a InspectionNode, name: &str, out: &mut Vec<&'a InspectionNode>) {
    if node.widget == name {
        out.push(node);
    }
    for child in &node.children {
        collect(child, name, out);
    }
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
            .field(Field::new("Full name as shown to your members", TextInput::local().with_id("name"))
                .with_help("This name appears on your shared documents and team workspace."))
            .field(Field::new("Email address for account recovery", TextInput::local().with_id("email"))
                .with_help("We use this address to help you sign in and send important account notifications.")
                .with_error("Enter a valid email address, including the domain after the at sign.")))
            .section(FormSection::new("Preferences").content(Box::new(Checkbox::bound("updates", "updates")
                .with_label("Send me occasional product updates and tips for working with my team."))))
            .with_actions(Actions::new(Button::new("save", "Save account changes").role(ButtonRole::Primary))
                .secondary(Button::new("cancel", "Cancel")))
}

fn solve(widget: &mut dyn Widget, width: f32, theme: &Theme) {
    let mut tree = TaffyTree::new();
    let mut nodes = HashMap::new();
    let node = widget.build_layout(&mut tree, theme, &mut nodes);
    let mut bindings = tilo_ui_base::widget::bound_nodes::BoundNodeMap::new();
    let mut text = tilo_ui_base::font_catalog::fixture_renderer();
    // SAFETY: exact live, exclusively owned widget tree and matching maps.
    unsafe {
        bindings.rebuild(widget, &tree, node, &nodes);
        tilo_ui_base::widget::compute_widget_layout(
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
        let mut harness = Driver::new_with_theme(
            tilo_ui_app::State::new(),
            Box::new(page),
            (360, 420),
            Theme::dark().with_text_scale(scale),
        );
        assert!(harness.targets()["Detail 19"].y0 > 420.0);
        assert!(offset.get().1.abs() < f32::EPSILON);

        let input_node = |harness: &Driver, name: &str| {
            harness
                .a11y_nodes()
                .into_iter()
                .find(|(_, node)| {
                    node.label() == Some(name) && node.role() == accesskit::Role::TextInput
                })
                .unwrap()
                .0
        };
        let last = input_node(&harness, "Detail 19");
        harness
            .perform_a11y_action(last, &tilo_ui_base::a11y::Action::Focus)
            .unwrap();
        assert_eq!(harness.a11y_focus().unwrap().0, last);
        let revealed = harness.targets()["Detail 19"];
        assert!(revealed.y0 >= 0.0 && revealed.y1 <= 420.0, "{revealed:?}");
        let revealed_offset = offset.get().1;
        assert!(revealed_offset > 0.0);

        // A real wheel event may move the focused field off screen. Re-layout
        // must not undo that deliberate scroll while focus stays unchanged.
        harness.send(&tilo_ui_base::synthetic_input::cursor_moved(180.0, 210.0));
        harness.scroll(0.0, 3.0);
        let manual_offset = offset.get().1;
        assert!(manual_offset < revealed_offset);
        assert_eq!(harness.a11y_focus().unwrap().0, last);
        harness.recompose().unwrap();
        assert!((offset.get().1 - manual_offset).abs() < f32::EPSILON);

        let first = input_node(&harness, "Detail 0");
        harness
            .perform_a11y_action(first, &tilo_ui_base::a11y::Action::Focus)
            .unwrap();
        assert_eq!(harness.a11y_focus().unwrap().0, first);
        let revealed = harness.targets()["Detail 0"];
        assert!(revealed.y0 >= 0.0 && revealed.y1 <= 420.0, "{revealed:?}");
        assert!(offset.get().1 < manual_offset);
    }
}

#[test]
fn form_labels_adapt_by_measurement_width_and_type_size() {
    fn wide_fields_align_controls_beside_measured_labels() {
        let harness = Driver::new_with_theme(
            tilo_ui_app::State::new(),
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
            let mut harness = Driver::new_with_theme(
                tilo_ui_app::State::new(),
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

    fn measured_long_labels_and_large_text_change_layout_without_breakpoints() {
        let theme = Theme::dark();
        let mut short = Field::new("Name", TextInput::local());
        let mut long = Field::new("Full name as shown to all your members", TextInput::local());
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

    wide_fields_align_controls_beside_measured_labels();
    narrow_and_large_type_stack_long_labels_and_scroll_to_actions();
    measured_long_labels_and_large_text_change_layout_without_breakpoints();
    wide_label_aligns_to_first_control_even_with_long_help_and_validation();
}

#[test]
fn a_labelled_text_input_is_named_by_its_label() {
    let harness = Driver::new_with_theme(
        tilo_ui_app::State::new(),
        fixture(),
        (900, 700),
        Theme::dark(),
    );
    let root = harness.inspection().root;
    let name = |id: &str| {
        named(&root, id)
            .semantics
            .as_ref()
            .and_then(|node| node.name.clone())
    };
    assert_eq!(name("name").as_deref(), Some("Name"));
    assert_eq!(name("email").as_deref(), Some("Email address"));
}

#[test]
fn a_validation_error_marks_its_control_invalid() {
    let harness = Driver::new_with_theme(
        tilo_ui_app::State::new(),
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
fn grouped_sections_are_settings_rows_with_separators() {
    use tilo_ui_app::widget::{Dropdown, Toggle};
    let theme = Theme::dark();
    let section = FormSection::new("Camera")
        .grouped()
        .field(Field::new("Grid lines", Toggle::bound("grid", "grid")))
        .field(Field::new(
            "Photo format",
            Dropdown::new(vec!["HEIF".into(), "JPEG".into()], "format"),
        ));
    let harness = Driver::new_with_theme(
        tilo_ui_app::State::new(),
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

#[test]
fn read_only_values_reserve_readable_phrase_width_in_both_themes() {
    for theme in [Theme::light(), Theme::dark()] {
        for german in [false, true] {
            let harness = Driver::new_with_theme(
                tilo_ui_app::State::new(),
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
                let harness = Driver::new_with_theme(
                    tilo_ui_app::State::new(),
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

#[test]
fn untitled_sections_custom_fields_and_control_rows() {
    use tilo_ui_app::widget::{Button, Row};
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
    let harness = Driver::new_with_theme(
        tilo_ui_app::State::new(),
        Box::new(form),
        (1000, 700),
        Theme::dark(),
    );
    let root = harness.inspection().root;
    let mut names = Vec::new();
    semantic_names(&root, &mut names);
    let headings: Vec<_> = names
        .iter()
        .filter(|(role, _)| matches!(role, tilo_ui_base::a11y::Role::Heading { .. }))
        .collect();
    assert_eq!(headings.len(), 1, "{headings:?}");
    assert_eq!(
        names.iter().filter(|(_, name)| name == "Name").count(),
        1,
        "{names:?}"
    );
    assert!(
        names
            .iter()
            .any(|(role, name)| *role == tilo_ui_base::a11y::Role::Label && name == "Version"),
        "a custom field's label is a label: {names:?}"
    );
    let name = named(&root, "name").rect;
    let email = named(&root, "email").rect;
    let version = named(&root, "version").rect;
    assert!((name[0] - email[0]).abs() < 0.5, "{name:?} {email:?}");
    assert!((version[0] - email[0]).abs() < 0.5, "{version:?} {email:?}");
    let export = named(&root, "export").rect;
    let mut labels = Vec::new();
    collect(&root, "FieldLabel", &mut labels);
    assert!((export[0] - labels[0].rect[0]).abs() < 0.5, "{export:?}");
}

#[test]
fn help_reads_under_the_label_beside_and_under_the_control_stacked() {
    let field =
        || Field::new("Name", TextInput::local().with_id("name")).with_help("Shown to members.");
    let wide = Driver::new_with_theme(
        tilo_ui_app::State::new(),
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

    let narrow = Driver::new_with_theme(
        tilo_ui_app::State::new(),
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
    use tilo_ui_app::widget::{Dropdown, Toggle};
    let section = || {
        FormSection::new("Camera")
            .grouped()
            .field(Field::new(
                "Grid lines",
                Toggle::bound("grid", "grid").with_id("grid"),
            ))
            .field(Field::new(
                "Photo format",
                Dropdown::new(vec!["HEIF".into(), "JPEG".into()], "format").with_id("format"),
            ))
    };
    let wide = Driver::new_with_theme(
        tilo_ui_app::State::new(),
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

    let narrow = Driver::new_with_theme(
        tilo_ui_app::State::new(),
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
    assert!(
        (format[0] - sections[0].rect[0]).abs() < 0.5,
        "a stacked chooser starts under its label: {format:?}"
    );
    assert!(format[0] + format[2] <= sections[0].rect[0] + sections[0].rect[2] + 0.5);
}

#[test]
fn a_page_keeps_sections_on_the_leading_readable_column_with_pinned_actions() {
    use tilo_ui_app::widget::{Button, Section};
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
    let harness = Driver::new_with_theme(
        tilo_ui_app::State::new(),
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
    use tilo_ui_app::widget::Button;
    let theme = Theme::dark();
    let titled = Driver::new_with_theme(
        tilo_ui_app::State::new(),
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

    let untitled = Driver::new_with_theme(
        tilo_ui_app::State::new(),
        Box::new(FormPage::untitled(long_form())),
        (1000, 800),
        theme,
    );
    let mut names = Vec::new();
    semantic_names(&untitled.inspection().root, &mut names);
    assert!(
        !names
            .iter()
            .any(|(role, _)| matches!(role, tilo_ui_base::a11y::Role::Heading { level: 1 })),
        "{names:?}"
    );
}

#[test]
fn the_forms_of_a_pages_sections_share_one_label_column_and_name_their_controls() {
    use tilo_ui_app::widget::{ColorSwatch, ColorSwatchPicker, Row, Section, Toggle};
    let form = |label: &str, id: &str| -> Element {
        Box::new(FormSection::untitled().field(Field::custom(
            label,
            Box::new(TextInput::local().with_id(id)),
        )))
    };
    let page = Page::new()
        .section(Box::new(Section::titled(
            "Numbers",
            vec![form("Share of the time", "share")],
        )))
        .section(Box::new(Section::titled(
            "Choices",
            vec![form("Room", "room")],
        )))
        .section(Box::new(Section::titled(
            "Layers",
            vec![
                Box::new(
                    Row::new()
                        .push(Box::new(Text::new("Background")))
                        .push(Box::new(Toggle::bound("background", "layers.background"))),
                ),
                Box::new(
                    ColorSwatchPicker::new([ColorSwatch::new(vello::peniko::Color::BLACK)], 1)
                        .with_accessible_name("Accent"),
                ),
            ],
        )));
    let harness = Driver::new_with_theme(
        tilo_ui_app::State::new(),
        Box::new(page),
        (1280, 800),
        Theme::light(),
    );
    let root = harness.inspection().root;
    let (share, room) = (named(&root, "share").rect, named(&root, "room").rect);
    assert!(
        (share[0] - room[0]).abs() < 0.5,
        "one label column across the page's forms: {share:?} {room:?}"
    );
    let mut names = Vec::new();
    semantic_names(&root, &mut names);
    for (role, name) in [
        (tilo_ui_base::a11y::Role::TextInput, "Share of the time"),
        (tilo_ui_base::a11y::Role::TextInput, "Room"),
        (tilo_ui_base::a11y::Role::Switch, "Background"),
        (tilo_ui_base::a11y::Role::List, "Accent"),
    ] {
        assert!(
            names.iter().any(|(r, n)| *r == role && n == name),
            "{role:?} {name:?} in {names:?}"
        );
    }
}

#[test]
fn a_language_pages_forms_share_one_label_column_through_the_panes_they_are_shown_in() {
    use tilo_ui_app::widget::Section;
    use tilo_ui_app::widget::arrange::{Page as Arranged, Stack};
    let form = |label: &str, id: &str| -> Element {
        Box::new(Shown(Box::new(FormSection::untitled().field(
            Field::custom(label, Box::new(TextInput::local().with_id(id))),
        ))))
    };
    let page = Arranged::new().push(Box::new(
        Stack::stack()
            .push(Box::new(Section::titled(
                "Numbers",
                vec![form("Share of the time", "share")],
            )))
            .push(Box::new(Section::titled(
                "Choices",
                vec![form("Room", "room")],
            ))),
    ));
    let harness = Driver::new_with_theme(
        tilo_ui_app::State::new(),
        Box::new(page),
        (1280, 800),
        Theme::light(),
    );
    let root = harness.inspection().root;
    let (share, room) = (named(&root, "share").rect, named(&root, "room").rect);
    assert!(
        (share[0] - room[0]).abs() < 0.5,
        "one label column across the page's forms: {share:?} {room:?}"
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

fn semantic_names(node: &InspectionNode, out: &mut Vec<(tilo_ui_base::a11y::Role, String)>) {
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

/// What a language view wraps each form in: a pane that answers as itself and shows the form.
struct Shown(Element);

impl tilo_ui_base::widget::Decorator for Shown {
    type Inner = dyn Widget;
    fn inner(&self) -> &(dyn Widget + 'static) {
        self.0.as_ref()
    }
    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.0.as_mut()
    }
}
