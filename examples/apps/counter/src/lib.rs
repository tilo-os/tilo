//! Counter: resources and actions.
//!
//! The smallest program that shows the whole contract. The app's state is a
//! model; setting a field is an edit at its typed path, and every other change
//! is a verb with typed parameters. The buttons, the action palette, agents,
//! scripts and tests all make the same edits and invoke the same verbs
//! through the same door. A refused change changes nothing. A destructive verb
//! needs confirmation wherever it is invoked.

/// The application id.
pub const APP_ID: &str = "so.tilo.example.counter";
/// The window and launcher title.
pub const APP_TITLE: &str = "Counter";

pub mod locales;

use ui::i18n::I18nManager;
use ui::prelude::*;

/// The largest count, either way. Keeping it far below `i64::MAX` gives
/// every action simple validation.
pub const LIMIT: i64 = 1_000_000;

/// The model: the state the framework stores, syncs, versions and undoes.
/// Its `check` holds after every change, however it is made: setting the
/// count at its path (`Counter::paths().count.set(42)`) or a verb.
#[derive(Model, Default, Debug, Clone, PartialEq)]
#[model(check = Counter::within_limit)]
pub struct Counter {
    pub count: i64,
}

impl Counter {
    /// The count stays within a million either way.
    ///
    /// # Errors
    /// Refuses a count past the limit.
    pub fn within_limit(&self) -> Result<(), Refusal> {
        if self.count.abs() > LIMIT {
            return Err(Refusal::new("the count is limited to a million either way"));
        }
        Ok(())
    }
}

/// `kind` names the resource type for the palette, agents and search. Each
/// `#[action]` method is a verb: its doc comment is its label, its parameters
/// are typed and described with `#[param]`, and returning `Err` refuses it
/// without changing anything.
#[resource(kind = "example.counter", label = "Counter")]
impl Counter {
    /// Add an amount to the count.
    #[action]
    pub fn add(
        &mut self,
        #[param("Amount to add; negative subtracts")] amount: i64,
    ) -> Result<(), &'static str> {
        self.count = self
            .count
            .checked_add(amount)
            .ok_or("the count would pass its limit")?;
        Ok(())
    }

    /// Reset the count to zero. `destructive` and undoable: every surface,
    /// from the button to an agent, runs it at once and offers Undo.
    #[action(destructive)]
    pub fn reset(&mut self) {
        self.count = 0;
    }
}

pub struct App;

impl ResourceApp for App {
    type Resource = Counter;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = crate::APP_TITLE;

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::locales::CATALOG)
    }

    /// How the palette, search and history name the open counter.
    fn label(counter: &Counter) -> String {
        I18nManager::format("counter-label", &[("count", counter.count.into())])
    }

    /// Apps start empty. A screenshot that needs a busier state stages it with
    /// the same edits a user would make: `--shots --scenario counted`.
    fn scenarios() -> &'static [&'static str] {
        &["counted"]
    }

    fn scenario(name: Option<&str>) -> Vec<Invoke<Counter>> {
        match name {
            Some("counted") => Invoke::edit(&Counter::paths().count.set(42))
                .into_iter()
                .collect(),
            _ => Vec::new(),
        }
    }

    fn view(cx: &mut ViewCx<Counter>) -> Element {
        let count = cx
            .text(|counter| counter.count.to_string())
            .with_role(TextRole::Display)
            .with_id("count");
        // One verb, three fixed invocations: each button carries its own
        // arguments, and each stays disabled while the verb is forbidden.
        let actions = cx.actions();
        let step = |id: &str, label: &str, amount: i64| {
            actions.bind_invocation(Button::new(id, label), Counter::ADD.with(amount))
        };
        let reset = cx.button("counter-reset-button", Counter::RESET.invoke());
        Box::new(ui::column![
            Text::new("counter-heading").with_role(TextRole::Title),
            count,
            ui::row![
                step("subtract-one", "counter-subtract-one", -1),
                step("add-one", "counter-add-one", 1),
                step("add-ten", "counter-add-ten", 10),
            ],
            reset,
        ])
    }
}

ui::dev_reload_entry!(App);
