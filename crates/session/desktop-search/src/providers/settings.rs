//! Settings' pages and the settings on them, found while Settings is
//! closed.
//!
//! Each page is one row, found by its name, by what it holds and by the
//! words people use for it in any language Tilo speaks ("wlan" on an English
//! desktop finds Network). A setting people look for by name ("keyboard
//! layout", "tap to click") is a row of its own, offered where it matches
//! better than its page. Both come from the one index Settings' own sidebar
//! reads ([`settings_store::index`]). A row runs Settings' `open` at the
//! page's or the setting's anchor, by address, the app launched first when
//! it is closed.
//!
//! What a setting is set to is not this source's: a switch or a value the
//! settings service publishes is an action of its own, found and flipped
//! through [`crate::providers::Published`], and left out here
//! ([`Settings::changing_in_place`]).

use settings_store::index;
use ui::search::FieldWeight;
use ui::search::system::{ResourceKind, provider_score};

use crate::model::{Hit, InvokeTarget, Source, Target};

/// Settings, by its package id.
pub const SETTINGS_APP: &str = "so.tilo.settings";
/// The anchor of a page of Settings, by its slug.
const PAGE_ANCHOR: &str = "settings.section:";
/// The anchor of one setting, by its id.
const SETTING_ANCHOR: &str = "settings.setting:";

/// Settings the desktop offers as quick actions under the same name
/// ("Wi-Fi", "Volume"): typing the name does the thing there, so a row
/// opening Settings would only outrank it.
const QUICK_ACTION_SETTINGS: &[&str] = &[
    "network.wifi",
    "network.airplane",
    "bluetooth.enabled",
    "sound.volume",
    "sound.mute",
    "display.brightness",
    "notifications.dnd",
];

/// The index's copy, in the desktop's language.
static CATALOG: ui::i18n::EmbeddedCatalog = {
    let catalog = ui::i18n::EmbeddedCatalog::new("en-US", index::CATALOGUES).with_system_locale();
    #[cfg(test)]
    let catalog = catalog.with_pinned_locale("en-US");
    catalog
};

fn t(key: &str) -> String {
    CATALOG.t(key)
}

/// The picture of each page: the one Settings' sidebar shows beside it.
fn page_icon(slug: &str) -> Option<ui::icons::Icon> {
    use ui::icons::Icon;
    Some(match slug {
        "network" => Icon::Wifi,
        "bluetooth" => Icon::Bluetooth,
        "devices_sync" => Icon::Cloud,
        "sound" => Icon::Volume2,
        "speakers" => Icon::Speaker,
        "displays" => Icon::Monitor,
        "keyboard" | "keyboard_shortcuts" => Icon::Keyboard,
        "pointing" => Icon::Mouse,
        "spelling" => Icon::Type,
        "time_date" => Icon::Calendar,
        "users" => Icon::Users,
        "printers" => Icon::Printer,
        "power" => Icon::Battery,
        "appearance" => Icon::Palette,
        "accessibility" => Icon::Eye,
        "notifications" => Icon::Bell,
        "default-apps" => Icon::LayoutGrid,
        "storage" => Icon::HardDrive,
        "external_accounts" => Icon::Mail,
        "agent_models" => Icon::Sparkles,
        "region_formats" => Icon::Globe,
        "permissions" => Icon::Shield,
        "updates" => Icon::Refresh,
        "about" => Icon::Info,
        _ => return None,
    })
}

/// The kind Settings' rows are grouped under, in the desktop's language.
#[must_use]
pub fn settings_kind() -> ResourceKind {
    ResourceKind::Custom(crate::i18n::t("search-kind-settings"))
}

struct Row {
    id: String,
    title: String,
    facts: Vec<String>,
    anchor: String,
    /// The page it is on.
    slug: &'static str,
    /// The row of its page, for a setting.
    page: Option<usize>,
}

/// Settings' pages, then the settings on them.
pub struct Settings {
    /// The room Settings opens in.
    room: String,
    rows: Vec<Row>,
    fields: Vec<Vec<(String, FieldWeight)>>,
    ranker: ui::search::ListIndex,
}

impl Settings {
    /// The pages and settings, opening Settings in `room`.
    #[must_use]
    pub fn new(room: impl Into<String>) -> Self {
        Self::changing_in_place(room, &|_| false)
    }

    /// [`Self::new`], leaving out the settings `in_place` names by id: those
    /// the settings service publishes as actions that change them in place
    /// (a switch, a choice's values, a value), which search finds as those
    /// actions. A row opening Settings at them would only stand beside the
    /// switch itself.
    #[must_use]
    pub fn changing_in_place(room: impl Into<String>, in_place: &dyn Fn(&str) -> bool) -> Self {
        let mut rows = Vec::new();
        let mut fields = Vec::new();
        for page in index::PAGES {
            let title = t(&page.title_key());
            let subtitle = t(&page.subtitle_key());
            let mut found = vec![
                (title.to_lowercase(), FieldWeight::TITLE),
                (subtitle.to_lowercase(), FieldWeight::SUBTITLE),
            ];
            found.extend(
                index::names_in_every_locale(&page.title_key())
                    .into_iter()
                    .chain(index::words_in_every_locale(&page.words_key()))
                    .map(|word| (word, FieldWeight::KEYWORD)),
            );
            fields.push(found);
            rows.push(Row {
                id: format!("page:{}", page.slug),
                title,
                facts: vec![subtitle],
                anchor: format!("{PAGE_ANCHOR}{}", page.slug),
                slug: page.slug,
                page: None,
            });
        }
        for setting in index::SETTINGS {
            if QUICK_ACTION_SETTINGS.contains(&setting.id) || in_place(setting.id) {
                continue;
            }
            let Some((at, page)) = index::PAGES
                .iter()
                .enumerate()
                .find(|(_, page)| page.slug == setting.page)
            else {
                continue;
            };
            let name = t(&setting.name_key());
            let page_title = t(&page.title_key());
            // A setting named as its page is ("Bluetooth") is found as the
            // page.
            if name.to_lowercase() == page_title.to_lowercase() {
                continue;
            }
            // A setting's name ranks as a word for its page does:
            // "touchpad" finds the page before the touchpad's speed, and
            // "touchpad speed" finds the speed.
            let mut found = vec![(name.to_lowercase(), FieldWeight::KEYWORD)];
            found.extend(
                setting
                    .search_words()
                    .into_iter()
                    .map(|word| (word, FieldWeight::KEYWORD)),
            );
            fields.push(found);
            rows.push(Row {
                id: format!("setting:{}", setting.id),
                title: name,
                facts: vec![page_title],
                anchor: format!("{SETTING_ANCHOR}{}", setting.id),
                slug: page.slug,
                page: Some(at),
            });
        }
        Self {
            room: room.into(),
            rows,
            fields,
            ranker: ui::search::ListIndex::default(),
        }
    }

    fn hit(&self, row: &Row, score: i32) -> Hit {
        Hit {
            kind: settings_kind(),
            provider: self.id().into(),
            id: row.id.clone(),
            title: row.title.clone(),
            subtitle: None,
            facts: row.facts.clone(),
            score,
            icon: page_icon(row.slug),
            target: Target::Invoke(Box::new(InvokeTarget::open(
                SETTINGS_APP,
                &self.room,
                &row.anchor,
            ))),
        }
    }

    /// The row `id` names (`page:network`, `setting:keyboard.layout`),
    /// whatever was typed.
    #[must_use]
    pub fn resolve(&self, id: &str) -> Option<Hit> {
        let row = self.rows.iter().find(|row| row.id == id)?;
        Some(self.hit(row, ui::search::system::MAX_PROVIDER_SCORE))
    }
}

impl Source for Settings {
    fn id(&self) -> &'static str {
        "settings-pages"
    }

    fn name(&self) -> String {
        crate::i18n::t("search-kind-settings")
    }

    /// The pages and settings `needle` finds. A setting is offered only
    /// where it matches better than its page: "battery" is the Power page,
    /// "charge limit" the setting on it.
    fn query(&self, needle: &str) -> Vec<Hit> {
        let needle = needle.trim();
        if needle.is_empty() {
            return Vec::new();
        }
        let ranks = self.ranker.ranks(
            needle,
            self.fields
                .iter()
                .map(|row| row.iter().map(|(text, weight)| (text.as_str(), *weight))),
        );
        self.rows
            .iter()
            .zip(&ranks)
            .filter_map(|(row, rank)| {
                let rank = (*rank)?;
                let page_rank = row.page.and_then(|page| ranks.get(page).copied().flatten());
                if page_rank.is_some_and(|page| page >= rank) {
                    return None;
                }
                let score = provider_score(rank);
                // A page leads a setting that matches as well.
                let score = if row.page.is_some() {
                    score.saturating_sub(1)
                } else {
                    score
                };
                Some(self.hit(row, score))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opens(hit: &Hit) -> (String, String) {
        let Target::Invoke(target) = &hit.target else {
            panic!("a settings row opens Settings by address");
        };
        assert_eq!(target.app, SETTINGS_APP);
        assert_eq!(target.address.action, crate::model::OPEN_ACTION);
        (
            target.address.resource.clone(),
            target.arguments[crate::model::OPEN_ANCHOR]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
        )
    }

    #[test]
    fn a_page_is_found_by_its_name_in_any_language_and_opens_at_its_anchor() {
        let settings = Settings::new("room");
        for needle in ["network", "wlan"] {
            let hits = settings.query(needle);
            let page = hits
                .iter()
                .find(|hit| hit.id == "page:network")
                .unwrap_or_else(|| panic!("{needle} finds Network"));
            assert_eq!(
                opens(page),
                (
                    "settings.section:network".to_owned(),
                    "settings.section:network".to_owned()
                )
            );
            assert_eq!(page.icon, Some(ui::icons::Icon::Wifi));
        }
        assert!(settings.query("").is_empty());
    }

    #[test]
    fn a_setting_named_leads_its_page_and_opens_at_the_setting() {
        let settings = Settings::new("room");
        let hits = settings.query("keyboard layout");
        let best = hits
            .iter()
            .max_by_key(|hit| hit.score)
            .expect("the layout is found");
        assert_eq!(best.id, "setting:keyboard.layout");
        assert_eq!(opens(best).0, "settings.setting:keyboard.layout");
        assert!(
            settings
                .query("wifi")
                .iter()
                .all(|hit| hit.id != "setting:network.wifi"),
            "a quick action answers Wi-Fi"
        );
        assert_eq!(
            settings
                .resolve("setting:keyboard.layout")
                .map(|hit| hit.id),
            Some("setting:keyboard.layout".into())
        );
    }
}
