//! Personal Settings' pages, found from the palette without opening
//! Settings first.
//!
//! Each page is one row: found by its name, by what it holds ("night light"
//! finds Displays, "keyboard layout" finds Keyboard) and by the words
//! people use for it in either language, so a German desktop that types
//! "wifi" and an English one that types "wlan" both land on Network. Enter
//! opens the page through Settings' own `settings://` link, the way every
//! other app reaches it.

use ui::search::FieldWeight;

use crate::shell::i18n::{t, words_in_every_locale};
use crate::shell::search::action_api::ActionError;
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};

pub const PROVIDER_ID: &str = "settings-pages";

/// Every page, by the slug Settings' links name it with, in sidebar order.
const PAGES: &[&str] = &[
    "network",
    "bluetooth",
    "devices_sync",
    "sound",
    "speakers",
    "displays",
    "keyboard",
    "pointing",
    "spelling",
    "keyboard_shortcuts",
    "time_date",
    "users",
    "printers",
    "power",
    "appearance",
    "accessibility",
    "notifications",
    "default-apps",
    "storage",
    "external_accounts",
    "agent_models",
    "region_formats",
    "permissions",
    "updates",
    "about",
];

/// The catalogue keys of a page, which spell its slug with hyphens.
fn key(slug: &str) -> String {
    slug.replace('_', "-")
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
    rows: Vec<SearchResult>,
    /// Each page's searchable text, in row order, followed by one entry per
    /// [`Built::settings`].
    fields: Vec<Vec<(String, FieldWeight)>>,
    /// The settings a page holds that the session also serves by name
    /// ([`super::system_settings::named_settings`]): the row of the page
    /// they are on, and their name in the desktop's language.
    settings: Vec<(usize, String)>,
}

impl SettingsPagesProvider {
    pub fn new() -> Self {
        Self {
            kinds: vec![ResourceKind::Custom(t("search-kind-settings-pages"))],
            ranker: ui::search::ListIndex::default(),
            built: std::cell::RefCell::default(),
        }
    }
}

impl SettingsPagesProvider {
    fn build(kind: &ResourceKind, locale: String) -> Built {
        let mut rows = Vec::new();
        let mut fields = Vec::new();
        for slug in PAGES {
            let key = key(slug);
            let title = t(&format!("search-settings-page-{key}"));
            let subtitle = t(&format!("search-settings-page-{key}-subtitle"));
            let mut row = vec![
                (title.to_lowercase(), FieldWeight::TITLE),
                (subtitle.to_lowercase(), FieldWeight::SUBTITLE),
            ];
            row.extend(
                words_in_every_locale(&format!("search-kw-settings-{key}"))
                    .into_iter()
                    .map(|word| (word, FieldWeight::KEYWORD)),
            );
            fields.push(row);
            let uri = format!("settings://{slug}");
            rows.push(SearchResult::nullary_fallible(
                kind.clone(),
                PROVIDER_ID,
                format!("page:{slug}"),
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
            ));
        }
        let mut settings = Vec::new();
        for (slug, name) in super::system_settings::named_settings() {
            let Some(page) = PAGES.iter().position(|page| *page == slug) else {
                continue;
            };
            fields.push(vec![(name.to_lowercase(), FieldWeight::TITLE)]);
            settings.push((page, name));
        }
        Built {
            locale,
            rows,
            fields,
            settings,
        }
    }

    /// The pages `needle` finds, best first. A page found by the exact name
    /// of a setting it holds ("keyboard layout") is offered under that name,
    /// so the row says what the person typed and Enter opens the page it is
    /// on, where it is changed by choosing rather than by typing a value.
    fn ranked(&self, built: &Built, needle: &str) -> Vec<SearchResult> {
        let mut ranks = self
            .ranker
            .ranks(
                needle,
                built
                    .fields
                    .iter()
                    .map(|row| row.iter().map(|(text, weight)| (text.as_str(), *weight))),
            )
            .into_iter();
        let mut out: Vec<Option<SearchResult>> = built
            .rows
            .iter()
            .zip(ranks.by_ref())
            .map(|(row, rank)| {
                Some(SearchResult {
                    score: crate::shell::search::provider_score(rank?),
                    ..row.clone()
                })
            })
            .collect();
        for ((page, name), rank) in built.settings.iter().zip(ranks) {
            if rank != Some(ui::search::MAX_FIELD_RANK) || needle.trim().is_empty() {
                continue;
            }
            let (Some(slot), Some(base)) = (out.get_mut(*page), built.rows.get(*page)) else {
                continue;
            };
            *slot = Some(SearchResult {
                title: name.clone(),
                subtitle: Some(format!(
                    "{} · {}",
                    base.title,
                    t("search-settings-page-subtitle")
                )),
                score: crate::shell::search::provider_score(ui::search::MAX_FIELD_RANK),
                ..base.clone()
            });
        }
        out.into_iter().flatten().collect()
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

    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        let Some(kind) = self.kinds.first().filter(|kind| ctx.wanted.contains(kind)) else {
            return Vec::new();
        };
        let locale = crate::shell::i18n::current_locale();
        let mut built = self.built.borrow_mut();
        if built.as_ref().is_none_or(|built| built.locale != locale) {
            *built = Some(Self::build(kind, locale));
        }
        let Some(built) = built.as_ref() else {
            return Vec::new();
        };
        crate::shell::search::usage::order_ties_after(self.ranked(built, ctx.needle.trim()))
    }
}
