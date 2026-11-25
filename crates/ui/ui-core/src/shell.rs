//! The shell actions, which an app asks of the shell it runs in through
//! [`Call::host`](tilo_app::framework::Call::host): named here so an app reaches them through `ui`
//! rather than depending on the host crate.
#[cfg(feature = "model")]
use tilo_desktop_model::commands::{RESOURCE as SESSION, layout};

/// The provider the session's broker answers the shell's actions under.
#[cfg(feature = "model")]
pub const PROVIDER: &str = layout::PROVIDER;
/// The shell's session resource the actions are on.
#[cfg(feature = "model")]
pub const RESOURCE: &str = SESSION;
/// Open a resource, content or web address in a window (or, with a `place` of `tab-of` and a `group`, a
/// tab of that group).
#[cfg(feature = "model")]
pub const OPEN: &str = layout::OPEN;

/// Settings, by its package id: the app a `settings:` link opens.
pub use tilo_app_id::apps::SETTINGS as SETTINGS_APP;
/// The anchor of a page of Settings, by its slug, as Settings' `open` reads it.
const SETTINGS_PAGE: &str = "settings.section:";

/// Where a link to Settings opens it: the anchor Settings' `open` shows. Every
/// shell (a device's session, a page) reads a link here, so a link opens the
/// same page wherever it is followed.
///
/// `settings://keyboard` and `settings:keyboard` name the page
/// `settings.section:keyboard`; an anchor already made (`settings.section:…`,
/// `settings.setting:…`, as a person's card hands one) is taken as it is.
/// `None` for a link that names no page (`settings://`, or nothing at all):
/// that opens Settings itself.
#[must_use]
pub fn settings_anchor(link: &str) -> Option<String> {
    let page = link
        .strip_prefix("settings://")
        .or_else(|| link.strip_prefix("settings:"))
        .unwrap_or(link);
    if page.is_empty() || page == SETTINGS_PAGE {
        return None;
    }
    if page.len() != link.len() {
        return Some(format!("{SETTINGS_PAGE}{page}"));
    }
    Some(link.to_owned())
}

#[cfg(test)]
mod tests {
    use super::settings_anchor;

    #[test]
    fn a_settings_link_names_its_page_however_it_is_spelled() {
        for link in [
            "settings://keyboard",
            "settings:keyboard",
            "settings.section:keyboard",
        ] {
            assert_eq!(
                settings_anchor(link).as_deref(),
                Some("settings.section:keyboard"),
                "{link}"
            );
        }
        assert_eq!(
            settings_anchor("settings://models/u2net-subject").as_deref(),
            Some("settings.section:models/u2net-subject")
        );
        assert_eq!(
            settings_anchor("settings.setting:keyboard.layout").as_deref(),
            Some("settings.setting:keyboard.layout"),
            "an anchor already made is taken as it is"
        );
        for none in ["", "settings://", "settings:", "settings.section:"] {
            assert_eq!(
                settings_anchor(none),
                None,
                "{none:?} opens Settings itself"
            );
        }
    }
}
