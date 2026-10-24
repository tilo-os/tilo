//! Global search before anything is typed: the launcher.
//!
//! With nothing typed and no kind chosen, global search shows what is going
//! on and where to start, top to bottom:
//!
//! 1. **Happening now**: who is in which workspace and what they are in.
//! 2. **Waiting on you**: calls, messages and questions addressed to the
//!    person.
//! 3. **Continue**: what they opened lately, opening where it was.
//! 4. **Apps**: every app the person has, installed Tilo apps and the
//!    device's own, the ones they use most first, then the rest by what they
//!    are for. Apps the catalogue offers but the person does not have
//!    (Flathub's, Tilo's not yet downloaded) are found by typing, never
//!    listed here.
//!
//! A section with nothing real to show is left out. Each section is filled
//! by the providers that answer the empty query for it
//! ([`super::SearchProvider::launcher_section`]); this module only orders and
//! groups their rows. The scope is the query's: the current workspace
//! ([`SearchScope::Workspace`]) or every workspace ([`SearchScope::All`]).
//! Typing turns the same surface into results; clearing the query brings
//! the launcher back.

use std::collections::BTreeMap;

use crate::shell::chrome_state::SearchScope;
use crate::shell::i18n::t;

use super::{
    KindGroup, QueryArgs, ResourceKind, ResourceKindText as _, SearchContext, SearchRegistry,
    SearchResult,
};

/// The apps the launcher leads its Apps section with: the ones used most.
pub const MOST_USED_APPS: usize = 6;

/// One section of the launcher, in the order they are shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LauncherSection {
    HappeningNow,
    WaitingOnYou,
    Continue,
    Apps,
}

impl LauncherSection {
    pub const ALL: [Self; 4] = [
        Self::HappeningNow,
        Self::WaitingOnYou,
        Self::Continue,
        Self::Apps,
    ];

    /// Stable id, as the search action API names the section.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::HappeningNow => "happening-now",
            Self::WaitingOnYou => "waiting-on-you",
            Self::Continue => "continue",
            Self::Apps => "apps",
        }
    }

    /// The section's header, in the desktop's language.
    #[must_use]
    pub fn label(self) -> String {
        t(match self {
            Self::HappeningNow => "search-launcher-happening-now",
            Self::WaitingOnYou => "search-launcher-waiting-on-you",
            Self::Continue => "search-launcher-continue",
            Self::Apps => "search-launcher-apps",
        })
    }
}

/// What an app is for, as the launcher groups the apps not used most.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AppPurpose {
    Office,
    Internet,
    Media,
    Development,
    Games,
    Learning,
    Utilities,
    System,
    Other,
}

impl AppPurpose {
    /// The purpose of an app whose categories are `categories`, as desktop
    /// files and Tilo manifests declare them (freedesktop main categories,
    /// one or several separated by `;`).
    #[must_use]
    pub fn of(categories: Option<&str>) -> Self {
        categories
            .into_iter()
            .flat_map(|categories| categories.split(';'))
            .map(str::trim)
            .find_map(|category| {
                Some(match category.to_ascii_lowercase().as_str() {
                    "office" => Self::Office,
                    "network" | "chat" | "email" | "webbrowser" | "instantmessaging" => {
                        Self::Internet
                    }
                    "audiovideo" | "audio" | "video" | "multimedia" | "graphics" | "music"
                    | "photography" => Self::Media,
                    "development" => Self::Development,
                    "game" | "games" => Self::Games,
                    "education" | "science" => Self::Learning,
                    "utility" | "accessibility" => Self::Utilities,
                    "system" | "settings" => Self::System,
                    _ => return None,
                })
            })
            .unwrap_or(Self::Other)
    }

    /// The group's header, in the desktop's language.
    #[must_use]
    pub fn label(self) -> String {
        t(match self {
            Self::Office => "search-launcher-apps-office",
            Self::Internet => "search-launcher-apps-internet",
            Self::Media => "search-launcher-apps-media",
            Self::Development => "search-launcher-apps-development",
            Self::Games => "search-launcher-apps-games",
            Self::Learning => "search-launcher-apps-learning",
            Self::Utilities => "search-launcher-apps-utilities",
            Self::System => "search-launcher-apps-system",
            Self::Other => "search-launcher-apps-other",
        })
    }
}

/// The section a launcher group labelled `label` is in: a section's own
/// group, or the apps for one purpose, in Apps.
#[must_use]
pub fn section_of(label: &str) -> Option<LauncherSection> {
    const PURPOSES: [AppPurpose; 9] = [
        AppPurpose::Office,
        AppPurpose::Internet,
        AppPurpose::Media,
        AppPurpose::Development,
        AppPurpose::Games,
        AppPurpose::Learning,
        AppPurpose::Utilities,
        AppPurpose::System,
        AppPurpose::Other,
    ];
    LauncherSection::ALL
        .into_iter()
        .find(|section| section.label() == label)
        .or_else(|| {
            PURPOSES
                .iter()
                .any(|purpose| purpose.label() == label)
                .then_some(LauncherSection::Apps)
        })
}

/// One group of launcher rows under its header. A section is one group,
/// except Apps: its most used apps, then one group per purpose.
#[derive(Debug)]
pub struct LauncherGroup {
    pub section: LauncherSection,
    pub label: String,
    pub rows: Vec<SearchResult>,
}

impl LauncherGroup {
    fn into_kind_group(self) -> KindGroup {
        KindGroup {
            kind: ResourceKind::Custom(self.label),
            total: self.rows.len(),
            results: self.rows,
        }
    }
}

/// Whether `args` asks for the launcher: nothing typed, no kind chosen, and
/// a scope wider than one app.
#[must_use]
pub fn wanted(args: &QueryArgs<'_>) -> bool {
    args.needle.trim().is_empty()
        && args.wanted_kinds.is_empty()
        && matches!(args.scope, SearchScope::Workspace | SearchScope::All)
}

/// The launcher's groups for `args`, top to bottom, leaving out every
/// section with nothing to show.
pub fn groups(registry: &SearchRegistry, args: &QueryArgs<'_>) -> Vec<LauncherGroup> {
    let mut sections: BTreeMap<LauncherSection, Vec<(SearchResult, Option<String>)>> =
        BTreeMap::new();
    for provider in registry.providers_iter() {
        let Some(section) = provider.launcher_section() else {
            continue;
        };
        let wanted = provider.resource_kinds().to_vec();
        let rows = provider.launcher_rows(SearchContext {
            query_scope: Some(args.scope),
            include_programmatic: false,
            needle: "",
            wanted: &wanted,
            chrome: args.chrome,
            doc: args.doc,
            doc_root: args.doc_root,
        });
        let rows = rows.into_iter().map(|row| {
            let category = provider.app_category(&row);
            (row, category)
        });
        sections.entry(section).or_default().extend(rows);
    }
    let mut out = Vec::new();
    for (section, rows) in sections {
        if rows.is_empty() {
            continue;
        }
        if section == LauncherSection::Apps {
            out.extend(app_groups(rows));
        } else {
            out.push(LauncherGroup {
                section,
                label: section.label(),
                rows: rows.into_iter().map(|(row, _)| row).collect(),
            });
        }
    }
    out
}

/// Every row `provider` answers `ctx` with, past its first page: the Apps
/// section lists every app, more than one page holds.
pub(super) fn every_page<P: super::SearchProvider + ?Sized>(
    provider: &P,
    ctx: SearchContext<'_>,
) -> Vec<SearchResult> {
    let mut rows = Vec::new();
    let mut cursor = None;
    let mut seen = std::collections::HashSet::new();
    for _ in 0..super::HEADLESS_MAX_PAGES {
        let page = provider.query_page(ctx, cursor.as_ref(), super::SearchBudget::PALETTE);
        rows.extend(page.results);
        if page.exhausted || page.stale_cursor {
            break;
        }
        match page.next_cursor {
            Some(next) if seen.insert(next.clone()) => cursor = Some(next),
            _ => break,
        }
    }
    rows
}

/// [`groups`] as the palette lists them.
pub fn kind_groups(registry: &SearchRegistry, args: &QueryArgs<'_>) -> Vec<KindGroup> {
    groups(registry, args)
        .into_iter()
        .map(LauncherGroup::into_kind_group)
        .collect()
}

/// The launcher's groups cut to the `row_budget` rows the card has room
/// for: what is going on, what waits and what to continue keep every row
/// (each section's provider answers with only its few), and the apps share
/// what is left, front to back, a purpose cut short offering the rest in
/// place. A zero budget keeps everything.
#[must_use]
pub fn trim(
    groups: Vec<KindGroup>,
    row_budget: usize,
    expanded: &[ResourceKind],
) -> Vec<KindGroup> {
    let (front, apps): (Vec<KindGroup>, Vec<KindGroup>) = groups.into_iter().partition(|group| {
        section_of(&group.kind.localized_label()) != Some(LauncherSection::Apps)
    });
    let shown: usize = front.iter().map(|group| group.results.len()).sum();
    let budget = if row_budget == 0 {
        0
    } else {
        row_budget.saturating_sub(shown).max(apps.len())
    };
    front
        .into_iter()
        .chain(ui::search::system::trim_groups(apps, budget, expanded))
        .collect()
}

/// The Apps section: the apps used most, newest use breaking ties, under
/// the section's own header; then every other app, one row each, grouped by
/// what it is for and named in order within a group.
fn app_groups(rows: Vec<(SearchResult, Option<String>)>) -> Vec<LauncherGroup> {
    let mut seen = std::collections::HashSet::new();
    let mut apps: Vec<(SearchResult, Option<String>, f32)> = rows
        .into_iter()
        .filter(|(row, _)| seen.insert((row.provider_id.clone(), row.id.clone())))
        .map(|(row, category)| {
            let row = described(row);
            let share = super::usage::share(&row.provider_id, &row.id);
            (row, category, share)
        })
        .collect();
    apps.sort_by(|a, b| {
        b.2.total_cmp(&a.2)
            .then_with(|| a.0.title.to_lowercase().cmp(&b.0.title.to_lowercase()))
    });
    let used = apps
        .iter()
        .take(MOST_USED_APPS)
        .take_while(|(_, _, share)| *share > 0.0)
        .count();
    let rest = apps.split_off(used);
    let mut out = Vec::new();
    if !apps.is_empty() {
        out.push(LauncherGroup {
            section: LauncherSection::Apps,
            label: LauncherSection::Apps.label(),
            rows: apps
                .into_iter()
                .map(|(row, _, _)| with_last_use(row))
                .collect(),
        });
    }
    let mut by_purpose: BTreeMap<AppPurpose, Vec<SearchResult>> = BTreeMap::new();
    for (row, category, _) in rest {
        by_purpose
            .entry(AppPurpose::of(category.as_deref()))
            .or_default()
            .push(row);
    }
    for (purpose, mut rows) in by_purpose {
        rows.sort_by_cached_key(|row| row.title.to_lowercase());
        out.push(LauncherGroup {
            section: LauncherSection::Apps,
            label: purpose.label(),
            rows,
        });
    }
    out
}

/// An app row with a second line: what the app is when its source says.
/// A Linux app's row always says which kind of app it is; a Tilo app that
/// says nothing is one this device downloaded.
fn described(mut row: SearchResult) -> SearchResult {
    let said = !row.facts.is_empty()
        || row
            .subtitle
            .as_deref()
            .is_some_and(|subtitle| !subtitle.trim().is_empty());
    if !said {
        row.subtitle = Some(t("search-launcher-app-downloaded"));
    }
    row
}

/// A most-used app's row, saying why it leads: when it was last opened,
/// after what the app is.
fn with_last_use(mut row: SearchResult) -> SearchResult {
    let Some(at) = super::usage::last_used(&row.provider_id, &row.id) else {
        return row;
    };
    let when = ui::widget::history::format_relative_time(at, std::time::SystemTime::now());
    if when.is_empty() {
        return row;
    }
    let reason = crate::shell::i18n::t_args("search-launcher-opened", &[("when", when.into())]);
    let mut facts = if row.facts.is_empty() {
        row.subtitle.clone().into_iter().collect()
    } else {
        std::mem::take(&mut row.facts)
    };
    facts.push(reason);
    row.with_facts(facts)
}

#[cfg(test)]
#[path = "launcher_tests.rs"]
mod launcher_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_app_is_grouped_by_the_first_purpose_its_categories_name() {
        assert_eq!(AppPurpose::of(Some("Office")), AppPurpose::Office);
        assert_eq!(
            AppPurpose::of(Some("GTK;Network;WebBrowser;")),
            AppPurpose::Internet
        );
        assert_eq!(AppPurpose::of(Some("AudioVideo;Player")), AppPurpose::Media);
        assert_eq!(AppPurpose::of(Some("Qt;KDE")), AppPurpose::Other);
        assert_eq!(AppPurpose::of(None), AppPurpose::Other);
    }
}
