//! The person's region and formats and their time zone, kept by the server their workspace is
//! on.
//!
//! A view the server renders for the person (what their agent reads of an app, a page the
//! server draws for them) shows dates and clocks as the person's own windows do: in the formats
//! they chose in Region & Formats (the `locale` settings section) and in their zone. A person
//! with only a browser open has no device that could say either, so their page tells the server
//! here: it sends the formats whenever the person changes them ([`SET`]), and the zone the
//! browser is in ([`BROWSER_ZONE`]). A zone the person chose themselves ([`CHOOSE_ZONE`]) is the
//! one their views use, whatever the browser says.
//!
//! The setting is the person's, in every workspace on that server; only the person on a device
//! of theirs reads or changes it, and an agent's request is refused.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{ActionAddress, RemoteActionTarget};

/// The provider that answers the person's formats under.
pub const PROVIDER: &str = "tilo.formats";
/// The resource the formats are actions of.
pub const RESOURCE: &str = "formats";
/// What the server keeps, as [`PersonFormats`].
pub const READ: &str = "read";
/// Keep the person's formats ([`SetFormats`]), answering [`PersonFormats`].
pub const SET: &str = "set";
/// Keep the zone the person chose ([`ChooseZone`]), or none to follow their browser's again,
/// answering [`PersonFormats`].
pub const CHOOSE_ZONE: &str = "choose-zone";
/// Say which zone the person's browser is in ([`BrowserZone`]), answering [`PersonFormats`].
pub const BROWSER_ZONE: &str = "browser-zone";

/// The target of action `action` of [`RESOURCE`].
#[must_use]
pub fn target(action: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(RESOURCE, action),
    }
}

/// What the server keeps of the person's region.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PersonFormats {
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

impl PersonFormats {
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
        let mut kept = PersonFormats {
            browser_zone: Some("Pacific/Auckland".into()),
            ..PersonFormats::default()
        };
        assert_eq!(kept.zone(), Some("Pacific/Auckland"));
        kept.chosen_zone = Some("Europe/Vienna".into());
        assert_eq!(kept.zone(), Some("Europe/Vienna"));
    }
}
