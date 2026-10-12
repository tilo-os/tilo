use ui::i18n::I18nManager;
use ui::prelude::*;

/// The app's state. The framework stores it, syncs it to collaborators and
/// keeps its history; add a field here and it is all of those at once.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Greeting {
    /// Collaborative text: two people typing in it merge character by character.
    pub name: String,
}

/// The app's public surface. Every field is edited at its typed path
/// (`Greeting::paths().name`); every `#[action]` verb is an operation beyond
/// setting a field. The controls below, the action palette, agents, scripts
/// and tests make the same edits and invoke the same verbs. A capability
/// without one exists only for the pointer.
#[resource(kind = "generated.greeting", label = "Greeting")]
impl Greeting {
    /// Forget the name.
    #[action]
    pub fn clear(&mut self) {
        self.name.clear();
    }
}

/// The greeting for `name`, or a prompt while there is none.
fn greeting(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        I18nManager::lookup_plain("starter-greeting-empty")
    } else {
        I18nManager::format("starter-greeting", &[("name", name.into())])
    }
}

pub struct App;

impl ResourceApp for App {
    type Resource = Greeting;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = crate::APP_TITLE;

    fn view(cx: &mut ViewCx<Greeting>) -> Element {
        crate::install_catalog();
        // Every string is a Fluent message id from locales/en-US.ftl.
        let name = cx
            .input(cx.paths().name)
            .with_id("name-input")
            .with_placeholder("starter-name-placeholder");
        // Read from the resource, so it follows every edit: this window's, a
        // collaborator's, an undo.
        let message = cx.text(|state| greeting(&state.name)).with_id("greeting");
        let clear = cx.button("starter-clear", Greeting::CLEAR.invoke());
        Box::new(ui::column![
            Text::new("starter-title").with_role(TextRole::Title),
            labeled_field("starter-name-label", name),
            message,
            clear,
        ])
    }
}
