//! Counter: resources and actions.
//!
//! The smallest program that shows the whole contract. The app's state is a
//! resource, and every change to it is an action with typed parameters. The
//! buttons, the action palette, agents, scripts and tests all invoke the same
//! actions through the same door. A refused action changes nothing. A
//! destructive one needs confirmation wherever it is invoked.

/// The application id.
pub const APP_ID: &str = "so.tilo.example.counter";
/// The window and launcher title.
pub const APP_TITLE: &str = "Counter";

pub mod locales;

fn install_catalog() {
    locales::install();
}

use ui::i18n::I18nManager;
use ui::prelude::*;

/// The largest count, either way. Keeping it far below `i64::MAX` gives
/// every action simple validation.
pub const LIMIT: i64 = 1_000_000;

/// The resource: the state the framework stores, syncs, versions and undoes.
/// A resource has no methods that change it other than its actions.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Counter {
    pub count: i64,
}

/// `kind` names the resource type for the palette, agents and search. Each
/// `#[action]` method becomes an action: its doc comment is its label, its
/// parameters are typed and described with `#[param]`, and returning `Err`
/// refuses it without changing anything.
#[resource(kind = "example.counter", label = "Counter")]
impl Counter {
    /// Add an amount to the count.
    #[action]
    pub fn add(
        &mut self,
        #[param("Amount to add; negative subtracts")] amount: i64,
    ) -> Result<(), &'static str> {
        let count = self
            .count
            .checked_add(amount)
            .filter(|count| count.abs() <= LIMIT)
            .ok_or("the count would pass its limit")?;
        self.count = count;
        Ok(())
    }

    /// Set the count.
    #[action]
    pub fn set(&mut self, #[param("New count")] count: i64) -> Result<(), &'static str> {
        if count.abs() > LIMIT {
            return Err("the count is limited to a million either way");
        }
        self.count = count;
        Ok(())
    }

    /// Reset the count to zero. `destructive` makes every surface, from the
    /// button to an agent, confirm before it runs.
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

    /// How the palette, search and history name the open counter.
    fn label(counter: &Counter) -> String {
        I18nManager::format("counter-label", &[("count", counter.count.into())])
    }

    /// Apps start empty. A screenshot that needs a busier state stages it with
    /// the same actions a user would invoke: `--shots --scenario counted`.
    fn scenarios() -> &'static [&'static str] {
        &["counted"]
    }

    fn scenario(name: Option<&str>) -> Vec<Invoke<Counter>> {
        match name {
            Some("counted") => vec![Counter::SET.with(42)],
            _ => Vec::new(),
        }
    }

    fn view(cx: &mut ViewCx<Counter>) -> Element {
        install_catalog();
        let count = cx
            .text(|counter| counter.count.to_string())
            .with_role(TextRole::Display)
            .with_id("count");
        // One action, three fixed invocations: each button carries its own
        // arguments, and each stays disabled while the action is forbidden.
        let actions = cx.actions();
        let step = |id: &str, label: &str, amount: i64| {
            actions.bind_invocation(Button::new(id, label, None), Counter::ADD.with(amount))
        };
        let reset = cx.button("counter-reset", Counter::RESET.invoke());
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
