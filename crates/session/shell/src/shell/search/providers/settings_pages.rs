//! Personal Settings' pages and the settings on them, found from the
//! palette without opening Settings first.
//!
//! Each page is one row, found by its name, by what it holds and by the
//! words people use for it in either language, so a German desktop that
//! types "wifi" and an English one that types "wlan" both land on Network.
//! Each setting people look for by name ("keyboard layout", "dark mode",
//! "tap to click") is a row of its own that opens its page at the setting.
//! Both come from the one index Settings' own sidebar reads
//! ([`settings_store::index`]), and Enter opens them through Settings'
//! `settings://` links, the way every other app reaches it.

use settings_store::index;
use ui::search::FieldWeight;

use crate::shell::i18n::t;
use crate::shell::search::action_api::ActionError;
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};

pub const PROVIDER_ID: &str = "settings-pages";

/// The picture of each page: the one Settings' sidebar shows beside it,
/// so a row from search and the page it opens look alike.
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

pub struct SettingsPagesProvider {
    kinds: Vec<ResourceKind>,
    /// The rows' text, indexed while it stays the same.
    ranker: ui::search::ListIndex,
    /// The rows and their searchable text, built once per language: a
    /// keystroke must only search them, not rebuild every page's copy.
    built: std::cell::RefCell<Option<Built>>,
}

struct Built {
    locale: String,
    /// One row per page, then one per setting.
    rows: Vec<SearchResult>,
    /// Each row's searchable text, in row order.
    fields: Vec<Vec<(String, FieldWeight)>>,
    /// How many of the rows are pages; the rest are settings.
    pages: usize,
    /// For each setting row, in order, the row of the page it is on.
    setting_pages: Vec<usize>,
}

/// Settings the palette already offers as quick actions under the same
/// name ("Wi-Fi", "Volume", "Do not disturb"): typing the name does the
/// thing there, so a second row opening Settings would only outrank it.
const QUICK_ACTION_SETTINGS: &[&str] = &[
    "network.wifi",
    "network.airplane",
    "bluetooth.enabled",
    "sound.volume",
    "sound.mute",
    "display.brightness",
    "notifications.dnd",
];

impl SettingsPagesProvider {
    pub fn new() -> Self {
        Self {
            kinds: vec![ResourceKind::Custom(t("search-kind-settings-pages"))],
            ranker: ui::search::ListIndex::default(),
            built: std::cell::RefCell::default(),
        }
    }
}

/// A row opening `uri` in Settings.
fn opening(
    kind: &ResourceKind,
    id: String,
    title: String,
    subtitle: String,
    uri: String,
) -> SearchResult {
    SearchResult::nullary_fallible(
        kind.clone(),
        PROVIDER_ID,
        id,
        title,
        Some(subtitle),
        0,
        move |_doc, _root, chrome| {
            crate::shell::uri_open::open_in_workspace(&uri, chrome)
                .map(|_| ())
                .map_err(|error| {
                    tracing::info!(?error, %uri, "settings page did not open");
                    ActionError::Explained(error.user_message())
                })
        },
    )
}

impl SettingsPagesProvider {
    fn build(kind: &ResourceKind, locale: String) -> Built {
        let mut rows = Vec::new();
        let mut fields = Vec::new();
        for page in index::PAGES {
            let title = t(&page.title_key());
            let subtitle = t(&page.subtitle_key());
            let mut row = vec![
                (title.to_lowercase(), FieldWeight::TITLE),
                (subtitle.to_lowercase(), FieldWeight::SUBTITLE),
            ];
            row.extend(
                index::names_in_every_locale(&page.title_key())
                    .into_iter()
                    .chain(index::words_in_every_locale(&page.words_key()))
                    .map(|word| (word, FieldWeight::KEYWORD)),
            );
            fields.push(row);
            rows.push(opening(
                kind,
                format!("page:{}", page.slug),
                title,
                subtitle,
                page.link(),
            ));
        }
        let pages = rows.len();
        let mut setting_pages = Vec::new();
        let app = t("search-settings-page-subtitle");
        for setting in index::SETTINGS {
            if QUICK_ACTION_SETTINGS.contains(&setting.id) {
                continue;
            }
            let Some((page_row, page)) = index::PAGES
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
            // A setting's name ranks as a word for its page does: "touchpad"
            // finds the Mouse & touchpad page before the touchpad's speed,
            // and "touchpad speed" finds the speed.
            let mut row = vec![(name.to_lowercase(), FieldWeight::KEYWORD)];
            row.extend(
                index::names_in_every_locale(&setting.name_key())
                    .into_iter()
                    .chain(index::words_in_every_locale(&setting.words_key()))
                    .map(|word| (word, FieldWeight::KEYWORD)),
            );
            fields.push(row);
            setting_pages.push(page_row);
            rows.push(opening(
                kind,
                format!("setting:{}", setting.id),
                name,
                format!("{page_title} · {app}"),
                setting.link(),
            ));
        }
        Built {
            locale,
            rows,
            fields,
            pages,
            setting_pages,
        }
    }

    /// The pages and settings `needle` finds, best first. A setting is
    /// offered only where it matches better than its page: "battery" is the
    /// Power page, "charge cap" the setting on it.
    fn ranked(&self, built: &Built, needle: &str) -> Vec<SearchResult> {
        let ranks = self.ranker.ranks(
            needle,
            built
                .fields
                .iter()
                .map(|row| row.iter().map(|(text, weight)| (text.as_str(), *weight))),
        );
        let browsing = needle.trim().is_empty();
        let page_rank = |at: usize| {
            at.checked_sub(built.pages)
                .and_then(|setting| built.setting_pages.get(setting))
                .and_then(|page| ranks.get(*page).copied().flatten())
        };
        built
            .rows
            .iter()
            .zip(&ranks)
            .enumerate()
            .filter_map(|(at, (row, rank))| {
                let rank = (*rank)?;
                // Browsing lists the pages, not every setting on them.
                if browsing && at >= built.pages {
                    return None;
                }
                if page_rank(at).is_some_and(|page| page >= rank) {
                    return None;
                }
                let score = crate::shell::search::provider_score(rank);
                Some(SearchResult {
                    score: if at >= built.pages {
                        score.saturating_sub(1)
                    } else {
                        score
                    },
                    ..row.clone()
                })
            })
            .collect()
    }
}

impl Default for SettingsPagesProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchProvider for SettingsPagesProvider {
    fn id(&self) -> &str {
        PROVIDER_ID
    }

    fn name(&self) -> &'static str {
        "Settings pages"
    }

    fn display_name(&self) -> String {
        t("search-provider-settings-pages")
    }

    fn resource_kinds(&self) -> &[ResourceKind] {
        &self.kinds
    }

    fn row_icon(&self, result: &SearchResult) -> Option<ui::icons::Icon> {
        // A setting wears the picture of the page it is on.
        let slug = result.id.strip_prefix("page:").or_else(|| {
            let id = result.id.strip_prefix("setting:")?;
            index::SETTINGS
                .iter()
                .find(|setting| setting.id == id)
                .map(|setting| setting.page)
        })?;
        page_icon(slug)
    }

    fn match_reason(&self, result: &SearchResult, needle: &str) -> Option<String> {
        let built = self.built.borrow();
        let built = built.as_ref()?;
        let at = built.rows.iter().position(|row| row.id == result.id)?;
        crate::shell::search::match_reason_in(needle, result, built.fields.get(at)?)
    }

    /// A setting the system settings offer in place (its value on the row,
    /// changed without opening Settings) is one row for people, that one;
    /// its "open the page at it" row stays for agents and the page itself
    /// still lists it.
    fn hidden_from_people(
        &self,
        result: &SearchResult,
        _needle: &str,
        _chrome: &crate::shell::chrome_state::ChromeState,
    ) -> bool {
        result
            .id
            .strip_prefix("setting:")
            .is_some_and(super::system_settings::offers_in_place)
    }

    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        self.with_built(&ctx, |built| {
            crate::shell::search::usage::order_ties_after(self.ranked(built, ctx.needle.trim()))
        })
        .unwrap_or_default()
    }

    /// A page or a setting by its id, whatever was typed: a setting row is
    /// listed only for the words that find it, but its address always
    /// opens it.
    fn resolve(&self, id: &str, ctx: SearchContext<'_>) -> Option<SearchResult> {
        self.with_built(&ctx, |built| {
            built.rows.iter().find(|row| row.id == id).cloned()
        })
        .flatten()
    }
}

impl SettingsPagesProvider {
    /// `read` over the rows built for the desktop's language, building them
    /// first if the language changed; `None` when the kind is not wanted.
    fn with_built<R>(&self, ctx: &SearchContext<'_>, read: impl FnOnce(&Built) -> R) -> Option<R> {
        let kind = self
            .kinds
            .first()
            .filter(|kind| ctx.wanted.contains(kind))?;
        let locale = crate::shell::i18n::current_locale();
        let mut built = self.built.borrow_mut();
        if built.as_ref().is_none_or(|built| built.locale != locale) {
            *built = Some(Self::build(kind, locale));
        }
        built.as_ref().map(read)
    }
}

#[cfg(test)]
mod icon_tests {
    #[test]
    fn every_page_wears_its_own_picture_not_the_magnifier() {
        for page in settings_store::index::PAGES {
            let slug = page.slug;
            let icon = super::page_icon(slug).unwrap_or_else(|| panic!("{slug} has no icon"));
            assert_ne!(icon, ui::icons::Icon::Search, "{slug}");
        }
    }
}
