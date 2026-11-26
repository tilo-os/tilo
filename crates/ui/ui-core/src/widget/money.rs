//! An amount of money as people read it: `$5`, `$0.42`, `$1,250`.

use crate::widget::Text;
use tilo_ui_base::format::Formats;
use tilo_ui_base::tokens::TextRole;
use tilo_ui_base::widget::Decorator;
use tilo_ui_base::{component::Component, component::Described};

/// An amount of money in millionths of the currency (`$5`, `$0.42`, `$1,250`), as the toolkit
/// writes money: whole amounts without cents, fractional ones with two places, grouped and
/// marked by the person's number style.
#[derive(Clone, Component, Debug, Default, PartialEq)]
#[component(name = "Money", role = "Label")]
pub struct MoneyProps {
    /// The amount, in millionths of the currency.
    #[component(required, positional)]
    pub micros: i64,
    /// What the text is, which sets its size and weight: `body` unless said.
    pub role: TextRole,
    /// Beside the content it is about, in secondary ink: what a turn cost, under its answer.
    pub secondary: bool,
}

/// [`MoneyProps`]' widget: a [`Text`] that writes an amount the person's way.
pub struct Money {
    text: Text,
    micros: i64,
    role: TextRole,
    secondary: bool,
}

impl Money {
    /// The amount `micros` (millionths of the currency) as it reads now.
    #[must_use]
    pub fn new(micros: i64) -> Self {
        let mut money = Self {
            text: Text::new(""),
            micros,
            role: TextRole::Body,
            secondary: false,
        };
        money.show();
        money
    }

    #[must_use]
    pub fn label(&self) -> String {
        self.text.resolved_label()
    }

    /// Write the amount in the person's number style.
    fn show(&mut self) {
        let micros = u64::try_from(self.micros).unwrap_or(0);
        self.text.show_content(Formats::current().money(micros));
    }
}

impl Decorator for Money {
    type Inner = Text;

    fn inner(&self) -> &Text {
        &self.text
    }

    fn inner_mut(&mut self) -> &mut Text {
        &mut self.text
    }

    fn name(&self) -> &'static str {
        "Money"
    }
}

impl Described for Money {
    type Props = MoneyProps;

    fn from_props(key: &str, props: MoneyProps) -> Self {
        let MoneyProps {
            micros,
            role,
            secondary,
        } = props;
        let mut text = Text::new("").with_role(role).with_id(key);
        if secondary {
            text = text.dimmed();
        }
        let mut money = Self {
            text,
            micros,
            role,
            secondary,
        };
        money.show();
        money
    }

    /// Another amount writes in place; another role or ink builds it again.
    fn set_props(&mut self, props: MoneyProps) -> bool {
        if props.role != self.role || props.secondary != self.secondary {
            return false;
        }
        if props.micros != self.micros {
            self.micros = props.micros;
            self.show();
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_ui_base::format::speak_locale as speak;

    fn written(micros: i64) -> String {
        Money::from_props(
            "m",
            MoneyProps {
                micros,
                ..MoneyProps::default()
            },
        )
        .label()
    }

    #[test]
    fn an_amount_reads_as_the_person_writes_money() {
        speak("en-US");
        assert_eq!(written(5_000_000), "$5");
        assert_eq!(written(420_000), "$0.42");
        assert_eq!(written(1_250_000_000), "$1,250");
        // The person's number style marks the cents and groups the units.
        speak("de-DE");
        assert_eq!(written(420_000), "$0,42");
        assert_eq!(written(1_250_000_000), "$1.250");
        speak("en-US");
    }

    #[test]
    fn another_amount_writes_where_it_stands() {
        let mut money = Money::new(100_000);
        assert_eq!(money.label(), "$0.10");
        assert!(money.set_props(MoneyProps {
            micros: 200_000,
            ..MoneyProps::default()
        }));
        assert_eq!(money.label(), "$0.20");
        // Another ink is the widget built again, so the person's view stands as it is.
        assert!(!money.set_props(MoneyProps {
            micros: 200_000,
            secondary: true,
            ..MoneyProps::default()
        }));
    }
}
