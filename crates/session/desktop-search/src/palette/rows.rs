//! What a result row says and what the kind chips over the rows offer: the
//! palette's rows built from the hits a search found, and the facets that
//! narrow them by what they are.

use tilo_actions::FlatAction;
use ui::CommandEntry;
use ui::search::system::{KindGroup, ResourceKind};

use super::{ALL_APPS_ID, ALL_RESULTS_ID, MORE_PREFIX, Recents, Sources, Switches, switches};
use crate::model::{Hit, Source, Target};

/// The facet the kinds are picked by: one chip per kind this search found.
const KIND_FACET: &str = "kind";

/// The chips over the rows: one per kind this search found, so a person
/// narrows what came back by what it is instead of reading every group.
pub(super) fn kind_facets(groups: &[KindGroup<Hit>]) -> Vec<ui::search::Facet> {
    let mut facet = ui::search::Facet::new(KIND_FACET, "Kind");
    for group in groups {
        facet = facet.value(group.kind.id(), group.kind.display_label());
    }
    vec![facet]
}

/// `groups` as the picked kind chips leave them: nothing picked is every
/// kind.
pub(super) fn narrowed(
    groups: Vec<KindGroup<Hit>>,
    picked: &[ui::search::FacetFilter],
) -> Vec<KindGroup<Hit>> {
    let kinds: Vec<&str> = picked
        .iter()
        .filter(|filter| filter.key == KIND_FACET)
        .map(|filter| filter.value.as_str())
        .collect();
    if kinds.is_empty() {
        return groups;
    }
    groups
        .into_iter()
        .filter(|group| kinds.contains(&group.kind.id()))
        .collect()
}

/// Whether the row a hit becomes stands two lines tall: it says something
/// under its name. Asked of the row the palette would build, so the height
/// the capacity is counted in is the height the row is drawn at.
pub(super) fn stands_tall(hit: &Hit) -> bool {
    entry_for(hit, String::new()).description.is_some()
}

/// The rows `groups` list, in order: every hit as its palette row, and
/// under a group the budget cut short the row that offers the rest of it.
/// `rows` collects the hits the `hit:{i}` ids name, `more` the kinds the
/// `more:{i}` ids expand.
pub(super) fn entries_for(
    groups: &[KindGroup<Hit>],
    browsing: bool,
    catalogue: bool,
    refusal: Option<&(Target, String)>,
    more: &mut Vec<ResourceKind>,
    rows: &mut Vec<Hit>,
) -> Vec<CommandEntry> {
    more.clear();
    rows.clear();
    let mut entries = Vec::new();
    for group in groups {
        let label = group.kind.display_label();
        let capped = group.is_capped();
        let applications = browsing && !catalogue && group.kind == ResourceKind::Application;
        for hit in &group.results {
            let mut entry = entry_for(hit, format!("hit:{}", rows.len()));
            // The row the host could not carry out keeps the reason under its
            // name, where the person chose it, instead of closing over it.
            if let Some((target, why)) = refusal
                && target == &hit.target
            {
                entry.description = Some(why.clone());
            }
            entries.push(entry.group(label.clone()).group_total(group.total));
            rows.push(hit.clone());
        }
        // The apps the empty card lists end in the one way to every app:
        // "All apps", never a second "Show all" beside it.
        if capped && !applications {
            entries.push(ui::search::system::show_all_entry(
                format!("{MORE_PREFIX}{}", more.len()),
                &label,
                group.total,
            ));
            more.push(group.kind.clone());
        }
        if applications {
            // The rest of what the person has: every app, by its name,
            // where the few they reach for most are listed.
            entries.push(
                CommandEntry::new(ALL_APPS_ID, crate::i18n::t("search-launcher-all-apps"))
                    .icon(ui::icons::Icon::LayoutGrid)
                    .group(label)
                    .group_total(group.total),
            );
        }
    }
    entries
}

/// Whether `groups` hold nothing but the web search every query is offered
/// ("Search the web for …"): what was typed matched nothing. A service a query
/// names ("youtube cats") is a match, and stays, and so does the row handing
/// what was typed to the person's agent: it takes text nothing matched, as the
/// canvas does.
pub(super) fn nothing_matched(groups: &[KindGroup<Hit>]) -> bool {
    groups
        .iter()
        .flat_map(|group| group.results.iter())
        .all(crate::providers::WebSearch::is_every_query_row)
}

/// The row that lists every result the groups above had no room for.
pub(super) fn all_results_entry(rest: usize) -> CommandEntry {
    CommandEntry::new(
        ALL_RESULTS_ID,
        crate::i18n::t_args(
            "search-show-all-results",
            &[("count", rest.to_string().into())],
        ),
    )
    .icon(ui::icons::Icon::ChevronDown)
}

/// Kinds a crowded card never leaves out: the answer to the question, the
/// way to the web, and where the person was just at — Continue is what an
/// empty card is for, so no budget may take it away.
pub(super) fn always_shown(kind: &ResourceKind) -> bool {
    [
        crate::providers::answers_kind(),
        crate::providers::web_kind(),
        crate::providers::places::continue_kind(),
    ]
    .contains(kind)
        || *kind == crate::providers::ask_kind()
}

/// One hit as a palette row: an application wears its own glyph, the one
/// its windows wear, never the generic grid every app shared.
pub(super) fn entry_for(hit: &Hit, id: String) -> CommandEntry {
    let (mut entry, icon) = match &hit.target {
        Target::Action(action) => {
            let flat = FlatAction {
                key: id.clone(),
                resource_id: action.resource.clone(),
                resource_kind: action.resource_kind.clone(),
                resource_label: action.resource_label.clone(),
                resource_presentation: None,
                action: &action.spec,
                depth: 0,
            };
            let mut entry = CommandEntry::from_flat_action(&flat);
            if action.preset.is_some() {
                // The row is the hit (a video, a search of the service), not
                // the generic `open` it runs through: only the hit's own
                // subtitle describes it, and it runs at the hit the row
                // already carries instead of asking for it.
                entry = entry.with_preset();
                entry.description = None;
            }
            entry.id = id;
            entry.label.clone_from(&hit.title);
            (
                entry,
                hit.icon
                    .unwrap_or_else(|| ui::search::system::icon_for_kind(&hit.kind)),
            )
        }
        Target::Invoke(invoke) => (
            match &invoke.descriptor {
                Some(descriptor) => {
                    let mut entry = CommandEntry::from_descriptor(id, descriptor);
                    // A row whose one line is already decided — a capture,
                    // typed as the search itself — runs on Enter instead of
                    // asking for the line again.
                    if descriptor
                        .prompt_param()
                        .is_some_and(|param| invoke.arguments.contains_key(param))
                    {
                        entry = entry.with_preset();
                    }
                    entry
                }
                None => CommandEntry::new(id, hit.title.clone()),
            },
            hit.icon
                .unwrap_or_else(|| ui::search::system::icon_for_kind(&hit.kind)),
        ),
        // A source's switch wears what it is set to now, as a published
        // switch does: `enabled` is what choosing it makes it, so the badge
        // says the other.
        Target::Prefer { enabled, .. } => (
            CommandEntry::new(id, hit.title.clone()).badge(crate::i18n::t(if *enabled {
                "search-source-badge-off"
            } else {
                "search-source-badge-on"
            })),
            hit.icon
                .unwrap_or_else(|| ui::search::system::icon_for_kind(&hit.kind)),
        ),
        Target::Launch { app, .. } => (
            CommandEntry::new(id, hit.title.clone())
                .alternate(
                    ui::search::ActivationModifier::Shift,
                    crate::i18n::t("search-open-as-tab"),
                )
                .alternate(
                    ui::search::ActivationModifier::Ctrl,
                    crate::i18n::t("search-open-beside"),
                ),
            ui::icons::app_glyph(app, crate::availability::category_for(app)),
        ),
        Target::Window { app: Some(app), .. } => (
            CommandEntry::new(id, hit.title.clone()),
            ui::icons::app_glyph(app, crate::availability::category_for(app)),
        ),
        _ => (
            CommandEntry::new(id, hit.title.clone()),
            hit.icon
                .unwrap_or_else(|| ui::search::system::icon_for_kind(&hit.kind)),
        ),
    };
    // What the row says under its title, never the title again: a source
    // that names the row twice (a page whose subtitle is its own name, a
    // window whose place is the tab's label) says nothing new.
    let (subtitle, facts) = under_title(hit);
    if let Some(subtitle) = subtitle {
        entry.description = Some(subtitle);
    }
    if !facts.is_empty() {
        entry = entry.facts(facts);
    }
    if !hit.faces.is_empty() {
        entry = entry.faces(hit.faces.clone());
    }
    if let Some(outcome) = &hit.outcome {
        entry = entry.outcome(outcome.clone());
    }
    if let Some(row) = &hit.row {
        entry = entry.drawn(row.clone());
    }
    if let Some(preview) = &hit.preview {
        entry = entry.previewed(preview.clone());
    }
    entry.icon(icon)
}

/// The sources a search over everything asks, in the order it asks them:
/// the desktop's own, the ones the host adds, and the row that switches
/// each of them off and on again.
pub(super) fn everywhere<'a>(
    sources: &'a Sources,
    recents: &'a Recents,
) -> (Vec<&'a dyn Source>, Option<Switches>) {
    // Each source is asked through the handle it is held by: `Arc<S>` is a
    // `Source` that can move to a worker, where the bare `S` is not.
    let mut all: Vec<&dyn Source> = vec![
        recents as &dyn Source,
        &sources.applications,
        &sources.windows,
        &sources.content,
        &sources.actions,
        &sources.captures,
        &sources.published,
        &sources.commands,
    ];
    if let Some((answers, web)) = &sources.answers {
        all.extend([answers as &dyn Source, web]);
    }
    all.extend(sources.host.iter().map(|source| source as &dyn Source));
    let switches = switches(sources, &all);
    all.retain(|source| sources.preferences.enabled(source.id()));
    (all, switches)
}

/// What a row says under its title: its subtitle and its facts, each
/// dropped when it repeats the title or says what another already said.
fn under_title(hit: &Hit) -> (Option<String>, Vec<String>) {
    let title = crate::model::spoken(hit.title.trim());
    let mut said: Vec<String> = vec![title];
    let subtitle = hit
        .subtitle
        .as_ref()
        .map(|subtitle| subtitle.trim())
        .filter(|subtitle| !subtitle.is_empty())
        .filter(|subtitle| {
            let folded = crate::model::spoken(subtitle);
            if said.contains(&folded) {
                return false;
            }
            said.push(folded);
            true
        })
        .map(str::to_owned);
    let mut facts = Vec::new();
    for fact in &hit.facts {
        let fact = fact.trim();
        if fact.is_empty() {
            continue;
        }
        let folded = crate::model::spoken(fact);
        if said.contains(&folded) {
            continue;
        }
        said.push(folded);
        facts.push(fact.to_owned());
    }
    (subtitle, facts)
}
