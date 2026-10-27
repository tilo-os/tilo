//! Global search before anything is typed: the launcher.
//!
//! With nothing typed and no kind chosen, global search shows what is going
//! on and where to start, top to bottom:
//!
//! 1. **Happening now**: who is in which workspace and what they are in.
//! 2. **Waiting on you**: calls, messages and questions addressed to the
//!    person.
//! 3. **Continue**: what they opened lately, opening where it was.
//! 4. **Apps**: the few apps the person uses most, newest use first; on a
//!    device where few or none are used yet, Tilo's starting points fill
//!    the rest. Every other app is one "All apps" away, which the palette
//!    puts under them. Apps the catalogue offers but the person does not
//!    have (Flathub's, Tilo's not yet downloaded) are found by typing, never
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

use super::{KindGroup, QueryArgs, ResourceKind, SearchContext, SearchRegistry, SearchResult};

/// How many apps the launcher's Apps section lists.
pub const LISTED_APPS: usize = 6;

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

/// The section a launcher group labelled `label` is.
#[must_use]
pub fn section_of(label: &str) -> Option<LauncherSection> {
    LauncherSection::ALL
        .into_iter()
        .find(|section| section.label() == label)
}

/// One section of launcher rows under its header.
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
    let mut sections: BTreeMap<LauncherSection, Vec<SearchResult>> = BTreeMap::new();
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
        sections.entry(section).or_default().extend(rows);
    }
    sections
        .into_iter()
        .map(|(section, rows)| {
            let rows = if section == LauncherSection::Apps {
                listed_apps(rows)
            } else {
                rows
            };
            LauncherGroup {
                section,
                label: section.label(),
                rows,
            }
        })
        .filter(|group| !group.rows.is_empty())
        .collect()
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

/// The Apps section's rows: the apps used most, newest use breaking ties,
/// then Tilo's starting points the person has, up to [`LISTED_APPS`]. One
/// row per app.
fn listed_apps(rows: Vec<SearchResult>) -> Vec<SearchResult> {
    let mut seen = std::collections::HashSet::new();
    let (mut used, rest): (Vec<_>, Vec<_>) = rows
        .into_iter()
        .filter(|row| seen.insert((row.provider_id.clone(), row.id.clone())))
        .map(|row| {
            let share = super::usage::share(&row.provider_id, &row.id);
            (row, share)
        })
        .partition(|(_, share)| *share > 0.0);
    used.sort_by(|a, b| {
        b.1.total_cmp(&a.1)
            .then_with(|| a.0.title.to_lowercase().cmp(&b.0.title.to_lowercase()))
    });
    let mut starting: Vec<SearchResult> = rest
        .into_iter()
        .map(|(row, _)| row)
        .filter(|row| ui::search::system::starting_point_rank(&row.id) > 0)
        .collect();
    starting.sort_by_key(|row| std::cmp::Reverse(ui::search::system::starting_point_rank(&row.id)));
    used.into_iter()
        .map(|(row, _)| with_last_use(described(row)))
        .chain(starting.into_iter().map(described))
        .take(LISTED_APPS)
        .collect()
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
