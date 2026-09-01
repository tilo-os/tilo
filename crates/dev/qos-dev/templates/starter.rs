use ui::LocalState;
use ui::widget::{Column, Text, TextInput, Widget};

/// A complete local input → typed state → retained output loop.
pub fn app_root() -> Box<dyn Widget> {
    crate::install_catalog();
    let name = LocalState::new(String::from("world"));
    let greeting = Text::from_state(&name, |name| {
        ui::i18n::I18nManager::format("starter-greeting", &[("name", name.into())])
    })
    .with_id("greeting");
    let input = TextInput::local()
        .with_initial_value(&name.get())
        .with_id("name-input")
        .with_accessible_name("starter-name-label")
        .with_placeholder("starter-name-placeholder")
        .on_change(name.text_sink());

    Box::new(
        Column::new()
            .push(Box::new(Text::new("starter-title").with_size(28.0)))
            .push(Box::new(Text::new("starter-name-label")))
            .push(Box::new(input))
            .push(Box::new(greeting)),
    )
}
