//! Settings' pages and the settings on them, found while Settings is
//! closed.
//!
//! Each page is one row, found by its name, by what it holds and by the
//! words people use for it in any language Tilo speaks ("wlan" on an English
//! desktop finds Network). A setting people look for by name ("keyboard
//! layout", "tap to click") is a row of its own, offered where it matches
//! better than its page. Both come from the one index Settings' own sidebar
//! reads ([`settings_store::index`]). A page's row runs the `open-pane` of the
//! page's own row of Settings, and a setting's runs Settings' `show-setting`,
//! told which setting it is: an action Settings publishes, at the address it
//! publishes it on, with the app launched first when it is closed.
//!
//! What a setting is set to is not this source's: a setting its section's
//! own verb changes where it stands (its field's setter) is that action,
//! found through [`crate::providers::Published`], and left out here
//! ([`Settings::changing_in_place`]).

use settings_store::index;
use ui::search::system::{ResourceKind, provider_score};
use ui::text_match::FieldWeight;

use crate::model::{Hit, InvokeTarget, Source, Target};

pub use ui::shell::SETTINGS_APP;
/// The resource Settings' package publishes to programs (`tl:<app>`): where
/// the actions that open one of its panes are published.
const SETTINGS_RESOURCE: &str = "tl:so.tilo.settings";
/// The anchor of a page of Settings, by its slug.
const PAGE_ANCHOR: &str = "settings.section:";
/// The anchor of one setting, by its id.
const SETTING_ANCHOR: &str = "settings.setting:";
/// Settings' action that shows a page, and the parameter naming its slug.
const OPEN_PANE_ACTION: &str = "open_pane";
const PANE_ARGUMENT: &str = "pane";
/// The action Settings publishes to show a setting, and the parameter naming
/// it.
const SHOW_SETTING_ACTION: &str = "show_setting";
const SETTING_ARGUMENT: &str = "setting";

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

/// The index's copy, in the desktop's language: every window offers the same
/// settings, so these words ride along in its language instead of deciding it.
static CATALOG: ui::i18n::EmbeddedCatalog = {
    let catalog = ui::i18n::EmbeddedCatalog::new("en-US", index::CATALOGUES)
        .with_system_locale()
        .as_shared();
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
        "billing" => Icon::Landmark,
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
    /// The workspace Settings opens in.
    workspace: String,
    rows: Vec<Row>,
    fields: Vec<Vec<(String, FieldWeight)>>,
    ranker: ui::text_match::ListIndex,
}

impl Settings {
    /// The pages and settings, opening Settings in `workspace`.
    #[must_use]
    pub fn new(workspace: impl Into<String>) -> Self {
        Self::changing_in_place(workspace, &|_| false)
    }

    /// Settings' pages in `workspace`, leaving out the settings `published` changes in place: the
    /// ones whose section's verb (their field's setter) is published, which search finds as that
    /// action. A row opening Settings at them would only stand beside the setter itself.
    ///
    /// Every desktop that runs a session offers this one source, so a page finds the pages its own
    /// Settings shows ([`settings_store::index::Page::here`]) exactly as a computer does.
    #[must_use]
    pub fn of_published(
        workspace: impl Into<String>,
        published: &[tilo_actions::ActionDescriptor],
    ) -> Self {
        let in_place: Vec<&str> = published
            .iter()
            .filter_map(|action| {
                index::changed_by(
                    action.address.resource.as_str(),
                    action.address.action.as_str(),
                )
            })
            .map(|setting| setting.id)
            .collect();
        Self::changing_in_place(workspace, &|id| in_place.contains(&id))
    }

    /// [`Self::new`], leaving out the settings `in_place` names by id: those
    /// a published setter changes in place, which search finds as that
    /// action. A row opening Settings at them would only stand beside the
    /// setter itself.
    #[must_use]
    pub fn changing_in_place(
        workspace: impl Into<String>,
        in_place: &dyn Fn(&str) -> bool,
    ) -> Self {
        let mut rows = Vec::new();
        let mut fields = Vec::new();
        // A page Settings has not here (a device's hardware, on the web)
        // is not offered, nor any setting on it.
        for page in index::PAGES.iter().filter(|page| page.here()) {
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
                .find(|(_, page)| page.slug == setting.page && page.here())
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
            workspace: workspace.into(),
            rows,
            fields,
            ranker: ui::text_match::ListIndex::default(),
        }
    }

    fn hit(&self, row: &Row, score: i32) -> Hit {
        Hit {
            row: None,
            preview: None,
            faces: Vec::new(),
            outcome: None,
            kind: settings_kind(),
            provider: self.id().into(),
            id: row.id.clone(),
            title: row.title.clone(),
            subtitle: None,
            facts: row.facts.clone(),
            score,
            icon: page_icon(row.slug),
            target: Target::Invoke(Box::new(opening(&self.workspace, &row.anchor))),
        }
    }
}

/// The action of Settings that shows `anchor`, run in `workspace`: a page is a
/// row of Settings' own (`settings.section:<slug>`), which its `open_pane`
/// shows, told the slug, and a setting is shown by Settings' `show_setting`,
/// told which setting it is. An anchor neither names opens Settings at it.
#[must_use]
pub fn opening(workspace: &str, anchor: &str) -> InvokeTarget {
    if let Some(slug) = anchor.strip_prefix(PAGE_ANCHOR) {
        return InvokeTarget::action(SETTINGS_APP, workspace, SETTINGS_RESOURCE, OPEN_PANE_ACTION)
            .with_argument(PANE_ARGUMENT, slug);
    }
    if let Some(setting) = anchor.strip_prefix(SETTING_ANCHOR) {
        return InvokeTarget::action(
            SETTINGS_APP,
            workspace,
            SETTINGS_RESOURCE,
            SHOW_SETTING_ACTION,
        )
        .with_argument(SETTING_ARGUMENT, setting);
    }
    InvokeTarget::open(SETTINGS_APP, workspace, SETTINGS_RESOURCE, anchor)
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

    /// The notification switch Settings publishes is offered as the action "Notification
    /// banners", with every language's word for it as a keyword. A query finds it by a word
    /// of its own name, typed whole or begun, never by the first letters of a word in
    /// another language: "Ben" is a person, not those banners.
    #[test]
    fn a_setting_is_found_by_its_name_and_not_by_the_first_letters_of_a_translation() {
        let setting = index::SETTINGS
            .iter()
            .find(|setting| setting.id == "notifications.banners")
            .expect("Settings indexes notification banners");
        let keywords = setting.search_words();
        let row = (
            "Notification banners",
            keywords.as_slice(),
            Some("Notifications"),
        );
        let found = |needle| crate::rank::published_action_match_scores(needle, [row])[0].is_some();
        assert!(
            !found("Ben"),
            "the German name does not surface it: {keywords:?}"
        );
        assert!(found("ban"), "the English name does: {keywords:?}");
        assert!(found("banners"), "{keywords:?}");
        assert!(found("toasts"), "a word people use for it: {keywords:?}");
    }

    /// The target of `hit`, as (resource, action, argument) of the action it runs.
    fn opens(hit: &Hit) -> (String, String, Option<(String, String)>) {
        let Target::Invoke(target) = &hit.target else {
            panic!("a settings row runs one of Settings' actions by address");
        };
        assert_eq!(target.app, SETTINGS_APP);
        (
            target.address.resource.clone(),
            target.address.action.clone(),
            target
                .arguments
                .iter()
                .next()
                .map(|(name, value)| (name.clone(), value.as_str().unwrap_or_default().to_owned())),
        )
    }

    tilo_test_env::scenario! {
        fn settings_pages_and_settings_are_found_by_name_and_run_their_own_actions;

        phase a_page_is_found_by_its_name_in_any_language_and_its_row_shows_it;
        // A page is a row of Settings' own, and shows through the action that
        // row publishes: nothing is asked of Settings' `open`, which a page
        // does not offer.
        {
            let settings = Settings::new("workspace");
            for needle in ["network", "wlan"] {
                let hits = settings.query(needle);
                let page = hits
                    .iter()
                    .find(|hit| hit.id == "page:network")
                    .unwrap_or_else(|| panic!("{needle} finds Network"));
                assert_eq!(
                    opens(page),
                    (
                        SETTINGS_RESOURCE.to_owned(),
                        OPEN_PANE_ACTION.to_owned(),
                        Some((PANE_ARGUMENT.to_owned(), "network".to_owned()))
                    ),
                    "the page's own row shows it, told which page"
                );
                assert_eq!(page.icon, Some(ui::icons::Icon::Wifi));
            }
            assert!(settings.query("").is_empty());
        }

        phase a_setting_named_leads_its_page_and_settings_shows_it;
        {
            let settings = Settings::new("workspace");
            let hits = settings.query("keyboard layout");
            let best = hits
                .iter()
                .max_by_key(|hit| hit.score)
                .expect("the layout is found");
            assert_eq!(best.id, "setting:keyboard.layout");
            assert_eq!(
                opens(best),
                (
                    SETTINGS_RESOURCE.to_owned(),
                    SHOW_SETTING_ACTION.to_owned(),
                    Some((
                        SETTING_ARGUMENT.to_owned(),
                        "keyboard.layout".to_owned()
                    ))
                )
            );
            assert!(
                settings
                    .query("wifi")
                    .iter()
                    .all(|hit| hit.id != "setting:network.wifi"),
                "a quick action answers Wi-Fi"
            );
        }

        phase a_word_merely_beginning_alike_finds_no_page;
        // A page is found by its name in every language it is known by, never
        // by a word merely beginning alike: "hello" is not "hell" (light), so
        // it finds neither Appearance nor Brightness.
        {
            let settings = Settings::new("workspace");
            assert!(settings.query("hello").is_empty());
            assert!(
                settings
                    .query("theme")
                    .iter()
                    .any(|hit| hit.id == "page:appearance"),
                "the theme is Appearance"
            );
        }

        phase a_setting_changed_where_it_stands_is_left_to_its_action;
        // A setting whose section's verb is published is found as that action, which is the
        // change itself: a row opening Settings at it would only stand beside it.
        {
            let switch = tilo_actions::ResourceSpec::new(
                "settings/input",
                "settings.input",
                "Input",
            )
            .with_action(tilo_actions::ActionSpec::immediate_encoded(
                "choose-layout",
                "Keyboard layout",
                tilo_actions::ExecutionLocus::ClientLocal,
                tilo_actions::EncodedAction {
                    type_id: "test.setting".into(),
                    payload: Vec::new(),
                },
            ));
            let published = tilo_actions::api::survey(&[switch]);
            let offered = |settings: &Settings| {
                settings
                    .query("keyboard layout")
                    .into_iter()
                    .any(|hit| hit.id == "setting:keyboard.layout")
            };
            assert!(offered(&Settings::new("workspace")), "Settings offers it");
            assert!(
                !offered(&Settings::of_published("workspace", &published)),
                "the change is the row"
            );
        }
    }
}
