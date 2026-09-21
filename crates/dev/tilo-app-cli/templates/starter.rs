use ui::LocalState;
use ui::tokens::TextRole;
use ui::widget::resource_actions::ActionForm;
use ui::widget::{
    Button, Checkbox, Dropdown, NumberInput, Slider, Text, TextInput, Widget, labeled_check,
    labeled_field,
};

/// A complete local form → action/resource → retained output loop.
///
/// # Panics
///
/// Panics if the statically declared form fields cannot be bound. That would
/// indicate that this generated template and its form declaration diverged.
#[expect(
    clippy::expect_used,
    reason = "field names are declared on the action form immediately above"
)]
pub fn app_root() -> Box<dyn Widget> {
    crate::install_catalog();
    let name = LocalState::new(String::from("world"));
    let updates = LocalState::new(true);
    let refresh_minutes = LocalState::new(15.0);
    let volume = LocalState::new(0.65);
    let region = LocalState::new(String::from("Europe/Berlin"));

    let form = ActionForm::new(
        "generated.form",
        "Profile form",
        "update-profile",
        "Update profile",
    )
    .field("name", "Display name", &name)
    .field("updates", "Receive updates", &updates)
    .field("refresh", "Refresh minutes", &refresh_minutes)
    .field("volume", "Volume from 0 to 1", &volume)
    .field("region", "Region", &region);

    let greeting = Text::from_state(&name, |name| {
        ui::i18n::I18nManager::format("starter-greeting", &[("name", name.into())])
    })
    .with_id("greeting");
    let input = TextInput::from_state(&name)
        .with_id("name-input")
        .with_placeholder("starter-name-placeholder")
        .on_action(form.binding("name").expect("name field exists"));
    let updates_widget = Checkbox::from_state(&updates)
        .with_id("updates-checkbox")
        .on_action(form.binding("updates").expect("updates field exists"));
    let refresh = NumberInput::from_state(&refresh_minutes)
        .with_range(1.0, 120.0)
        .with_step(5.0)
        .with_suffix("min")
        .with_id("refresh-input")
        .on_action(form.binding("refresh").expect("refresh field exists"));
    let volume_widget = Slider::from_state(&volume)
        .with_id("volume-slider")
        .on_action(form.binding("volume").expect("volume field exists"));
    let region_widget = Dropdown::from_state(
        vec![
            String::from("Europe/Berlin"),
            String::from("America/New_York"),
            String::from("Asia/Tokyo"),
        ],
        &region,
    )
    .with_id("region-select")
    .on_action(form.binding("region").expect("region field exists"));
    let reset = Button::new("reset-form", "Reset form", None).on_action(
        form.reset_binding()
            .expect("initial field values are valid"),
    );

    let root = Box::new(ui::column![
        Text::new("starter-title").with_role(TextRole::Title),
        labeled_field("starter-name-label", input),
        labeled_check("starter-updates-label", updates_widget),
        labeled_field("starter-refresh-label", refresh),
        labeled_field("starter-volume-label", volume_widget),
        labeled_field("starter-region-label", region_widget),
        greeting,
        reset,
    ]);
    ui::widget::resource_actions::form(root, form)
}
