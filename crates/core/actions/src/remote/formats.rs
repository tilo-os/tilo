//! The person's region and formats and their time zone, in the person's own workspace.
//!
//! A view one of their devices renders for the person (what their agent reads of an app, a page
//! a server of theirs draws for them) shows dates and clocks as their own windows do: in the
//! formats they chose in Region & Formats (the `locale` settings section) and in their zone. A
//! person with only a browser open has no device that could say either, so their page tells a
//! device of theirs here: it sends the formats whenever the person changes them ([`SET`]), and
//! the zone the browser is in ([`BROWSER_ZONE`]). A zone the person chose themselves
//! ([`CHOOSE_ZONE`]) is the one their views use, whatever the browser says.
//!
//! The setting is the person's alone, the same on every device of theirs; only the person on a
//! device of theirs reads or changes it, and an agent's request is refused.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{ActionAddress, RemoteActionTarget};

/// The provider that answers the person's formats under.
pub const PROVIDER: &str = "tilo.formats";
/// The resource the formats are actions of.
pub const RESOURCE: &str = "formats";
/// The person's region, as [`Formats`].
pub const READ: &str = "read";
/// Keep the person's formats ([`SetFormats`]), answering [`Formats`].
pub const SET: &str = "set";
/// Keep the zone the person chose ([`ChooseZone`]), or none to follow their browser's again,
/// answering [`Formats`].
pub const CHOOSE_ZONE: &str = "choose-zone";
/// Say which zone the person's browser is in ([`BrowserZone`]), answering [`Formats`].
pub const BROWSER_ZONE: &str = "browser-zone";

/// The target of action `action` of [`RESOURCE`].
#[must_use]
pub fn target(action: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(RESOURCE, action),
    }
}

/// The person's region and formats and their zones.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Formats {
    /// Their formats, as the `locale` settings section keeps them (`date_format`,
    /// `time_format`, `number_style`, `first_day_of_week`); none until a device of theirs sent
    /// any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub formats: Option<Map<String, Value>>,
    /// The zone they chose themselves; none while they follow their browser's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chosen_zone: Option<String>,
    /// The zone their browser said it is in last.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser_zone: Option<String>,
}

impl Formats {
    /// The zone the person's views are rendered in: the one they chose, else their browser's.
    #[must_use]
    pub fn zone(&self) -> Option<&str> {
        self.chosen_zone.as_deref().or(self.browser_zone.as_deref())
    }
}

/// The argument of [`SET`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetFormats {
    /// The formats, as the `locale` settings section keeps them; keys it does not know are
    /// not kept.
    pub formats: Map<String, Value>,
}

/// The argument of [`CHOOSE_ZONE`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChooseZone {
    /// The zone they chose; none to follow their browser's again.
    #[serde(default)]
    pub zone: Option<String>,
}

/// The argument of [`BROWSER_ZONE`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserZone {
    /// The zone the browser names (`Intl`'s `timeZone`).
    pub zone: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chosen_zone_wins_over_the_browsers() {
        let mut kept = Formats {
            browser_zone: Some("Pacific/Auckland".into()),
            ..Formats::default()
        };
        assert_eq!(kept.zone(), Some("Pacific/Auckland"));
        kept.chosen_zone = Some("Europe/Vienna".into());
        assert_eq!(kept.zone(), Some("Europe/Vienna"));
    }
}
