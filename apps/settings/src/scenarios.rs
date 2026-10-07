//! Screenshot scenarios: which pane a capture shows, at which size, and the
//! stand-ins it shows in place of this device's hardware and accounts.
//!
//! Every pane is a scenario of its own, named by its id (`network`,
//! `region_formats`), captured wide. The narrow ones are drawn at a phone's
//! width: `narrow` keeps a pane's content on screen with the back arrow (what
//! a collapse shows), `narrow-list` the category list one back-tap away.
//! `search` shows the sidebar filtered by a query, `account-form` the form
//! for adding an account, `accounts-narrow` the account cards at a phone's
//! width. A plain `--shots` shows Appearance.

use std::path::PathBuf;
use std::sync::LazyLock;

use ui::framework::Fixture;

use crate::accounts::AccountKind;
use crate::categories::Category;
use crate::resource::Settings;

/// The wide viewport every capture is drawn at unless it is narrow.
pub const WIDE_VIEWPORT: (u32, u32) = (1200, 760);

/// The phone-sized viewport of the narrow scenarios.
pub const NARROW_VIEWPORT: (u32, u32) = (440, 820);

/// The query the `search` scenario types into the sidebar's field.
pub const SEARCH_QUERY: &str = "light";

const DEVICE_DISPLAYS: &str = "device-displays";

const NARROW: &str = "narrow";
const NARROW_LIST: &str = "narrow-list";
const SEARCH: &str = "search";
const ACCOUNT_FORM: &str = "account-form";
const ACCOUNTS_NARROW: &str = "accounts-narrow";

/// Every scenario: one per pane, then the layouts and flows.
pub fn names() -> &'static [&'static str] {
    static NAMES: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
        Category::ALL
            .iter()
            .map(|category| category.slug())
            .chain([
                NARROW,
                NARROW_LIST,
                SEARCH,
                ACCOUNT_FORM,
                ACCOUNTS_NARROW,
                DEVICE_DISPLAYS,
            ])
            .collect()
    });
    &NAMES
}

/// The viewport `scenario` is drawn at.
pub fn viewport(scenario: Option<&str>) -> (u32, u32) {
    match scenario {
        Some(NARROW | NARROW_LIST | ACCOUNTS_NARROW) => NARROW_VIEWPORT,
        Some(DEVICE_DISPLAYS) => (1200, 1100),
        _ => WIDE_VIEWPORT,
    }
}

/// What a capture shows in place of the person's own: accounts from the
/// bundled sample, kept in the capture's scratch directory, and the form a
/// scenario opens.
#[derive(Clone, Debug)]
pub struct CaptureScene {
    /// Where the capture keeps its account stores.
    pub account_root: Option<PathBuf>,
    /// The account form the scenario shows open.
    pub account_form: Option<AccountKind>,
    /// Synthetic certified-peer observations; available only to capture fixtures.
    pub device_displays: Vec<settings_store::device_displays::DeviceDisplay>,
}

/// Prepare `fixture`'s capture: every capture shows the sample accounts.
pub fn prepare(fixture: &mut Fixture<Settings>) {
    let account_form = (fixture.scenario() == Some(ACCOUNT_FORM)).then_some(AccountKind::Nextcloud);
    let account_root = fixture
        .directory()
        .ok()
        .map(|directory| directory.join("tilo"));
    fixture.provide(CaptureScene {
        account_root,
        account_form,
        device_displays: if fixture.scenario() == Some(DEVICE_DISPLAYS) {
            device_display_fixture()
        } else {
            Vec::new()
        },
    });
}

/// Two fixture devices with device-qualified connector identities and logical sizes.
fn device_display_fixture() -> Vec<settings_store::device_displays::DeviceDisplay> {
    use settings_store::device_displays::DeviceDisplay;
    vec![
        DeviceDisplay {
            device: "fixture-laptop".into(),
            device_label: "Laptop".into(),
            connector: "eDP-1".into(),
            label: "Built-in display".into(),
            online: true,
            local: true,
            x: 0,
            y: 0,
            width: 960,
            height: 540,
            scale_milli: 2000,
            revision: 1,
        },
        DeviceDisplay {
            device: "fixture-desktop".into(),
            device_label: "Desktop".into(),
            connector: "DP-1".into(),
            label: "External display".into(),
            online: true,
            local: false,
            x: 960,
            y: 0,
            width: 1280,
            height: 720,
            scale_milli: 1250,
            revision: 1,
        },
    ]
}

/// How a scenario lays the app out once its pane is chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Staged {
    /// `category`'s content.
    Pane(Category),
    /// The narrow category list, one back-tap from `category`.
    List(Category),
    /// `category` with the sidebar filtered by [`SEARCH_QUERY`].
    Search(Category),
}

/// What `scenario` shows.
pub fn staged(scenario: Option<&str>) -> Staged {
    match scenario {
        None => Staged::Pane(Category::Appearance),
        Some(DEVICE_DISPLAYS) => Staged::Pane(Category::Displays),
        Some(NARROW) => Staged::Pane(Category::RegionFormats),
        Some(NARROW_LIST) => Staged::List(Category::RegionFormats),
        Some(SEARCH) => Staged::Search(Category::Appearance),
        Some(ACCOUNT_FORM | ACCOUNTS_NARROW) => Staged::Pane(Category::ExternalAccounts),
        Some(slug) => Staged::Pane(Category::from_slug(slug).unwrap_or(Category::Appearance)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each scenario names one layout: its pane, its screen, its size.
    #[test]
    fn scenarios_name_their_pane_and_size() {
        assert_eq!(names().len(), Category::ALL.len() + 6);
        for category in Category::ALL {
            assert_eq!(staged(Some(category.slug())), Staged::Pane(*category));
            assert_eq!(viewport(Some(category.slug())), WIDE_VIEWPORT);
        }
        assert_eq!(staged(None), Staged::Pane(Category::Appearance));
        assert_eq!(
            staged(Some("narrow")),
            Staged::Pane(Category::RegionFormats)
        );
        assert_eq!(viewport(Some("narrow")), NARROW_VIEWPORT);
        assert_eq!(
            staged(Some("narrow-list")),
            Staged::List(Category::RegionFormats)
        );
        assert_eq!(staged(Some("search")), Staged::Search(Category::Appearance));
        assert_eq!(viewport(Some("accounts-narrow")), NARROW_VIEWPORT);
    }
}
