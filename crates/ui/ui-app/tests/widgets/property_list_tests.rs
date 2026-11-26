use std::cell::*;
use std::collections::*;
use std::path::*;
use std::rc::*;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use tilo_ui::app::driver::Driver;
use tilo_ui::headless::*;
use tilo_ui::kurbo::*;
use tilo_ui::retained_present::*;
use tilo_ui::testing::*;
use tilo_ui::vello::*;
use tilo_ui::widget::ListRow;
use tilo_ui::widget::internals::property_list::*;
use tilo_ui::widget::internals::property_list::{Density, ListLayout, ListMetrics, lay_out};
use tilo_ui::widget::*;
use tilo_ui_base::a11y::*;
use tilo_ui_base::component::*;
use tilo_ui_base::i18n::*;
use tilo_ui_base::icons::*;
use tilo_ui_base::interaction::*;
use tilo_ui_base::paint::*;
use tilo_ui_base::presence::*;
use tilo_ui_base::synthetic_input::*;
use tilo_ui_base::text_render::TextKind;
use tilo_ui_base::text_selection::*;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::colors::SMALL_TEXT_MIN_CONTRAST;
use tilo_ui_base::tokens::*;
use tilo_ui_base::widget::Tone;
use tilo_ui_base::widget::*;
use winit::event::*;
use winit::keyboard::*;

tilo_test_env::scenario! {
    fn property_actions_and_values_are_published_and_read_aloud;

    phase accessibility_reads_each_value_and_its_actions;
    {
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

    phase a_read_only_value_reads_as_a_read_only_field;
    {
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

    phase published_property_controls_invoke_real_clipboard_and_custom_actions;
    {
        use tilo_ui::clipboard::{ClipboardBackend, ClipboardTarget, InProcessClipboard};
        let clipboard = std::sync::Arc::new(InProcessClipboard::default());
        let services = tilo_ui::process::ProcessServices::null().with_clipboard(clipboard.clone());
        tilo_ui::process::scoped(&services, || {
            let activated = Rc::new(Cell::new(0));
            let signal = activated.clone();
            let mut driver = Driver::new(
                tilo_ui::State::new(),
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
                            .starts_with(tilo_ui::headless::CONTROL_RESOURCE_PREFIX)
                            && resource.label == label
                    })
                    .expect("the actual property action is published through the resource api");
                driver
                    .invoke_action(
                        &tilo_actions::ActionAddress::new(resource.id.as_str(), "activate"),
                        &serde_json::Map::new(),
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
                        &tilo_ui_base::a11y::Action::Activate
                    )
                    .is_err()
            );
            assert_eq!(activated.get(), 1);

            let mut value = Driver::new(
                tilo_ui::State::new(),
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
                        .starts_with(tilo_ui::headless::CONTROL_RESOURCE_PREFIX)
                        && resource.label == "Copy"
                })
                .expect("read-only property's Copy is an ordinary published action");
            value
                .invoke_action(
                    &tilo_actions::ActionAddress::new(resource.id.as_str(), "activate"),
                    &serde_json::Map::new(),
                    None,
                )
                .unwrap();
            assert_eq!(
                clipboard.text(ClipboardTarget::Clipboard).as_deref(),
                Some("acct_123")
            );
        });
    }
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

tilo_test_env::scenario! {
    fn labels_values_and_stripes_line_up_at_every_width;



    phase a_narrow_list_stacks_labels_above_values;
    {
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
        let big = Theme::dark().with_text_scale(2.0);
        assert!(!face.is_stacked(&mut text, &theme, 360.0));
        assert!(face.is_stacked(&mut text, &big, 360.0));
        assert!(face.height(&mut text, &big, 640.0) > face.height(&mut text, &theme, 640.0) * 1.5);
    }

    phase a_label_wider_than_its_column_wraps_and_never_runs_into_its_value;
    {
        let theme = Theme::dark();
        let m = regular(&theme);
        let mut text = TextRenderer::new();
        let rows = vec![
            PropertyRow::new("Printers responding", "1 of 1"),
            PropertyRow::new("Reporting ink or toner levels", "1 printer"),
            PropertyRow::new("Scanners available", "1"),
        ];
        let width = 380.0;
        let laid = layout(&mut text, &theme, &rows, width);
        assert!(!laid.stacked, "the labels wrap beside their values");
        for (index, row) in rows.iter().enumerate() {
            let (label, value) = painted(&mut text, &m, &laid, index, row);
            let geo = &laid.rows[index];
            assert!(
                label.x1 <= value.x0 && label.intersect(value).area() <= 0.0,
                "{:?}'s label {label:?} runs into its value {value:?}",
                row.label
            );
            assert!(
                label.y0 >= geo.rect.y0 && label.y1 <= geo.rect.y1 && label.x1 <= geo.rect.x1,
                "{:?}'s label {label:?} is cut off by its row {:?}",
                row.label,
                geo.rect
            );
        }
        assert!(
            laid.rows[1].label_wrap.is_some(),
            "the longest label wraps within its column"
        );

        // A word wider than the column cannot wrap: the labels stand over the
        // values, each shown whole.
        let rows = vec![
            PropertyRow::new("Interoperability", "Yes"),
            PropertyRow::new("Scanners", "1"),
        ];
        let narrow = 200.0;
        let laid = layout(&mut text, &theme, &rows, narrow);
        assert!(laid.stacked);
        for (index, row) in rows.iter().enumerate() {
            let (label, value) = painted(&mut text, &m, &laid, index, row);
            assert!(
                label.y1 <= value.y0,
                "{:?} stands over its value",
                row.label
            );
            assert!(
                label.x1 <= laid.rows[index].rect.x1,
                "{:?} is whole",
                row.label
            );
        }
    }


    phase unbreakable_values_elide_before_the_actions_and_prose_wraps;
    {
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

    phase a_compact_list_is_denser_and_toned_values_stay_legible;
    {
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
            assert!(tilo_ui_base::a11y::contrast_ratio(ink, ground) >= SMALL_TEXT_MIN_CONTRAST);
        }
    }


}

/// Where `row`'s label and value are painted in `laid`, as rects.
fn painted(
    text: &mut TextRenderer,
    m: &ListMetrics,
    laid: &ListLayout,
    index: usize,
    row: &PropertyRow<'_>,
) -> (Rect, Rect) {
    let geo = &laid.rows[index];
    let label_style = if laid.stacked {
        &m.stacked_label
    } else {
        &m.label
    };
    let (w, h) = text.measure_style(
        row.label,
        geo.label_wrap.map(tilo_ui_base::num::f32_of_f64),
        label_style,
    );
    let label = Rect::from_origin_size(geo.label, (f64::from(w), f64::from(h)));
    let (w, h) = text.measure_style(
        row.value,
        Some(tilo_ui_base::num::f32_of_f64(geo.value_width)),
        &m.value_style(row),
    );
    let value = Rect::from_origin_size(geo.value, (f64::from(w), f64::from(h)));
    (label, value)
}

#[test]
fn published_property_action_bounds_follow_the_painted_hit_target() {
    for scale in [1.0, 2.0] {
        let activated = Rc::new(Cell::new(false));
        let signal = activated.clone();
        let theme = Theme::dark().with_text_scale(scale);
        let mut harness = Driver::new_with_theme(
            tilo_ui::State::new(),
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

#[test]
fn a_bound_property_action_runs_the_apps_action_on_every_path() {
    use tilo_ui_base::widget::controls::ActionControls;
    let resource = tilo_actions::ResourceSpec::new("contact", "test", "Contact").with_action(
        tilo_actions::ActionSpec::operation(
            "edit",
            "Edit",
            tilo_actions::OperationKind::View,
            "test.edit",
            &(),
        )
        .unwrap(),
    );
    let binding = tilo_actions::ActionBinding::unit_for(&resource, "edit").unwrap();
    let mut harness = Driver::for_root(
        tilo_ui::State::new(),
        tilo_ui::widget::Unattached(
            PropertyList::new(vec![
                Property::new("Phone", "+31 20 123 4567")
                    .copyable()
                    .action_binding(Icon::Pencil, "Edit", binding),
            ])
            .with_id("facts"),
        ),
        (480, 240),
        Theme::dark(),
    );
    let mut root = harness.root_mut::<tilo_ui::widget::Unattached<PropertyList>>();
    let list = &mut root.0;
    let bindings = list.control_action_bindings();
    let bound = bindings
        .iter()
        .find(|binding| binding.control_id == "tilo.key-action:facts:0/1")
        .expect("the bound action is the button's control");
    assert!(matches!(
        &bound.kind,
        tilo_ui_base::widget::ControlBindingKind::Action(address) if address.action == "edit"
    ));
    assert!(list.has_action_control_bindings());
    drop(root);
    let (_, action) = harness
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| node.label() == Some("Edit Phone"))
        .expect("the bound action is published");
    let bounds = action.bounds().expect("bounds");
    let center = Rect::new(bounds.x0, bounds.y0, bounds.x1, bounds.y1).center();
    harness.click_at(center.x, center.y);
    let mut root = harness.root_mut::<tilo_ui::widget::Unattached<PropertyList>>();
    let list = &mut root.0;
    let invocation = list
        .take_control_invocation()
        .expect("the press runs the action");
    assert_eq!(invocation.address.action, "edit");
    assert!(list.take_control_invocation().is_none());

    list.refresh_control_action_availability(&std::collections::HashSet::new());
    let disabled = Widget::accessibility(&*list)
        .and_then(|node| node.children.first().cloned())
        .and_then(|row| row.children.get(1).cloned())
        .is_some_and(|button| button.state.disabled);
    assert!(disabled, "an unavailable action is published disabled");
    assert!(!list.run_action(0, 1));
    assert!(list.take_control_invocation().is_none());
}
