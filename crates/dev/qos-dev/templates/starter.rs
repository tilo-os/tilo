use ui::LocalState;
use ui::tokens::TextRole;
use ui::widget::{
    Checkbox, Dropdown, NumberInput, Slider, Text, TextInput, Widget, labeled_check, labeled_field,
};

/// A complete local form → typed state → retained output loop.
pub fn app_root() -> Box<dyn Widget> {
    crate::install_catalog();
    let name = LocalState::new(String::from("world"));
    let updates = LocalState::new(true);
    let refresh_minutes = LocalState::new(15.0);
    let volume = LocalState::new(0.65);
    let region = LocalState::new(String::from("Europe/Berlin"));
    let greeting = Text::from_state(&name, |name| {
        ui::i18n::I18nManager::format("starter-greeting", &[("name", name.into())])
    })
    .with_id("greeting");
    let input = TextInput::from_state(&name)
        .with_id("name-input")
        .with_placeholder("starter-name-placeholder");
    let updates = Checkbox::from_state(&updates).with_id("updates-checkbox");
    let refresh = NumberInput::from_state(&refresh_minutes)
        .with_range(1.0, 120.0)
        .with_step(5.0)
        .with_suffix("min")
        .with_id("refresh-input");
    let volume = Slider::from_state(&volume).with_id("volume-slider");
    let region = Dropdown::from_state(
        vec![
            String::from("Europe/Berlin"),
            String::from("America/New_York"),
            String::from("Asia/Tokyo"),
        ],
        &region,
    )
    .with_id("region-select");

    Box::new(ui::column![
        Text::new("starter-title").with_role(TextRole::Title),
        labeled_field("starter-name-label", input),
        labeled_check("starter-updates-label", updates),
        labeled_field("starter-refresh-label", refresh),
        labeled_field("starter-volume-label", volume),
        labeled_field("starter-region-label", region),
        greeting,
    ])
}
