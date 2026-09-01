use ui::LocalState;
use ui::tokens::TextRole;
use ui::widget::{Checkbox, Text, TextInput, Widget, labeled_check, labeled_field};

/// A complete local form → typed state → retained output loop.
pub fn app_root() -> Box<dyn Widget> {
    crate::install_catalog();
    let name = LocalState::new(String::from("world"));
    let updates = LocalState::new(true);
    let greeting = Text::from_state(&name, |name| {
        ui::i18n::I18nManager::format("starter-greeting", &[("name", name.into())])
    })
    .with_id("greeting");
    let input = TextInput::from_state(&name)
        .with_id("name-input")
        .with_placeholder("starter-name-placeholder");
    let updates = Checkbox::from_state(&updates).with_id("updates-checkbox");

    Box::new(ui::column![
        Text::new("starter-title").with_role(TextRole::Title),
        labeled_field("starter-name-label", input),
        labeled_check("starter-updates-label", updates),
        greeting,
    ])
}
