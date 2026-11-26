use super::*;
use crate::widget::ButtonRole;
use crate::widget::TextInput;
use tilo_ui_base::theme::Theme;

fn long_form() -> Form {
    Form::new().section(FormSection::new("Profile")
            .field(Field::new("Full name as shown to your members", TextInput::local().with_id("name"))
                .with_help("This name appears on your shared documents and team workspace."))
            .field(Field::new("Email address for account recovery", TextInput::local().with_id("email"))
                .with_help("We use this address to help you sign in and send important account notifications.")
                .with_error("Enter a valid email address, including the domain after the at sign.")))
            .section(FormSection::new("Preferences").content(Box::new(super::super::Checkbox::bound("updates", "updates")
                .with_label("Send me occasional product updates and tips for working with my team."))))
            .with_actions(Actions::new(super::super::Button::new("save", "Save account changes").role(ButtonRole::Primary))
                .secondary(super::super::Button::new("cancel", "Cancel")))
}
#[test]
fn a_one_question_forms_field_stands_without_a_label() {
    fn control(field: &mut Field, spare: f32) -> Rect {
        solve(field, spare, &Theme::dark());
        field.children()[1].children()[0].get_rect()
    }
    let mut alone = Field::unlabeled(Box::new(TextInput::local().with_id("when")));
    let mut labelled = Field::new("After", TextInput::local().with_id("after"));
    let lonely = control(&mut alone, 400.0);
    let beside = control(&mut labelled, 400.0);
    assert_eq!(
        labelled.effective_layout().unwrap().resolved_placement,
        LabelPlacement::Beside,
        "the field with a label keeps the columns it would stand in"
    );
    assert!(
        lonely.x0 < beside.x0,
        "the unlabelled control starts left of a labelled one: {lonely:?} {beside:?}"
    );
    assert!(
        lonely.width() > beside.width(),
        "and takes the area its label would leave: {lonely:?} {beside:?}"
    );
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

    page_width_override_and_reset_leave_label_adaptation_active();
    clearing_section_and_field_overrides_restores_only_the_selected_property();
}
