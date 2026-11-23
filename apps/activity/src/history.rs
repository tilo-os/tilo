//! History: every change in the workspace, newest first — and, beside them, the log's
//! outcomes: what the system did that needs no decision.

use std::collections::HashMap;

use ui::icons::Icon;
use ui::prelude::*;
use ui::tilo_actions::remote::workspace_activity as wire;
use ui::widget::{FilterCount, ListRow, QueryView, Section, SectionHeader, Segment, ToggleGroup};

use crate::{ActivityEffect, Filters, Nav, Provider, Window, ago, page, tr, trf};

/// The place's content and its header's commands.
///
/// The filter is the window's own: the feed is read again for the period, person and lens it
/// names, and the search narrows what the read returned. Nothing of it is a change to the
/// workspace.
pub(crate) fn view(cx: &ViewCx<Window>, nav: &Nav) -> (Element, Vec<Element>) {
    let scoped = nav.clone();
    let content = cx.switch(&nav.everywhere, move |cx, everywhere| {
        let scope = if *everywhere {
            wire::Scope::AllWorkspaces
        } else {
            wire::Scope::ThisWorkspace
        };
        let scoped = scoped.clone();
        let limit = scoped.limit.clone();
        cx.switch(&limit, move |cx, limit| {
            let scoped = scoped.clone();
            let limit = *limit;
            let period = scoped.period.clone();
            cx.switch(&period, move |cx, _| {
                let scoped = scoped.clone();
                let author = scoped.author.clone();
                cx.switch(&author, move |cx, _| {
                    let scoped = scoped.clone();
                    let lens = scoped.lens.clone();
                    cx.switch(&lens, move |cx, _| {
                        let scoped = scoped.clone();
                        let query = scoped.query.clone();
                        cx.switch(&query, move |cx, _| {
                            let filters = scoped.filters();
                            let mut asked = filters.query(scope);
                            asked.limit = if filters.searching() {
                                limit.max(wire::FEED_PAGE).saturating_mul(4)
                            } else {
                                limit
                            };
                            feed(cx, &scoped, &asked, &filters)
                        })
                    })
                })
            })
        })
    });
    (content, Vec::new())
}

/// The feed `query` reads, under the filter row that asked for it.
fn feed(cx: &ViewCx<Window>, nav: &Nav, query: &wire::FeedQuery, filters: &Filters) -> Element {
    let read = Provider::of(cx).follow::<wire::Feed>(wire::FEED, Some(query));
    let nav = nav.clone();
    let filters = filters.clone();
    Box::new(cx.arriving(&read.answer(), move |cx, feed| {
        let mut parts = vec![filter_row(&nav, feed, filters.narrows())];
        let found = found(feed, &filters);
        if feed.entries.is_empty() {
            parts.push(empty(feed, filters.narrows()));
        } else {
            let shown = feed.clone();
            let listed = nav.clone();
            let narrowed = filters.clone();
            let actions = cx.actions();
            parts.push(cx.switch(&nav.selection, move |_, selection| {
                list(&listed, &actions, &shown, selection.as_ref(), &narrowed)
            }));
            if feed.total > feed.entries.len() {
                let more = nav.clone();
                parts.push(Box::new(
                    Button::new("activity-show-more", "activity-show-more").on_open(move || {
                        more.limit.update(|limit| {
                            *limit = (*limit)
                                .max(wire::FEED_PAGE)
                                .saturating_add(wire::FEED_PAGE);
                        });
                    }),
                ));
            }
        }
        searchable(page(parts), &nav, &filters, found.len(), feed.entries.len())
    }))
}

/// The page, as the window bar's search narrows it: what is typed reaches the window's
/// filter, and the bar counts what the filter left of the `total` the read returned.
fn searchable(
    content: Element,
    nav: &Nav,
    filters: &Filters,
    found: usize,
    total: usize,
) -> Element {
    let current = filters.query.clone();
    let typed = nav.query.clone();
    Box::new(
        QueryView::new(content, move |text| {
            if text == current {
                return;
            }
            typed.set(text.to_owned());
        })
        .kept(FilterCount::new(found, total)),
    )
}

/// What the search leaves of the feed: the changes whose words, person or app contain its
/// query.
fn found(feed: &wire::Feed, filter: &Filters) -> Vec<wire::Entry> {
    feed.entries
        .iter()
        .filter(|entry| {
            let mut words: Vec<&str> = vec![entry.summary.as_str(), entry.author.name.as_str()];
            if let Some(place) = entry.resource.as_deref().or(entry.app.as_deref()) {
                words.push(place);
            }
            if let Some(workspace) = entry.workspace.as_deref() {
                words.push(workspace);
            }
            filter.matches(&words)
        })
        .cloned()
        .collect()
}

/// The empty feed; the hint to clear filters only shows when a filter narrows it.
fn empty(feed: &wire::Feed, narrows: bool) -> Element {
    let state = if feed.pending {
        EmptyState::new(tr("activity-history-pending")).with_icon(Icon::Clock)
    } else if narrows {
        EmptyState::new(tr("activity-history-no-match")).with_icon(Icon::History)
    } else {
        EmptyState::new(tr("activity-history-empty")).with_icon(Icon::History)
    };
    Box::new(state.with_id("activity-history-empty"))
}

/// A dropdown of `options` (value, label) bound to `state`, the window's own field.
fn picker(
    state: &LocalState<String>,
    id: &'static str,
    name: &str,
    options: Vec<(String, String)>,
) -> Element {
    let name = tr(name);
    let mut picker = Dropdown::from_state(Vec::new(), state)
        .with_id(id)
        .with_accessible_name(name);
    picker.set_value_options(options);
    Box::new(picker)
}

/// The filter row, with a way back to every change while the filter narrows them, and only
/// then.
fn filter_row(nav: &Nav, feed: &wire::Feed, narrows: bool) -> Element {
    let scope = ToggleGroup::from_state(
        vec![
            (false, Segment::new(tr("activity-scope-this-workspace"))),
            (true, Segment::new(tr("activity-scope-all-workspaces"))),
        ],
        &nav.everywhere,
    );
    let periods = [
        ("", "activity-period-all-time"),
        ("today", "activity-period-today"),
        ("week", "activity-period-week"),
        ("month", "activity-period-month"),
    ]
    .into_iter()
    .map(|(value, label)| (value.to_owned(), tr(label)))
    .collect();
    let people = std::iter::once((String::new(), tr("activity-people-everyone")))
        .chain(
            feed.people
                .iter()
                .map(|person| (person.clone(), person.clone())),
        )
        .collect();
    let lenses = std::iter::once((String::new(), tr("activity-lens-everything")))
        .chain(
            feed.lenses
                .iter()
                .map(|lens| (lens.key.clone(), lens.label.clone())),
        )
        .collect();
    let mut row = Row::new()
        .with_rhythm(ui::tokens::Rhythm::Related)
        .push(Box::new(scope))
        .push(picker(
            &nav.period,
            "activity-filter-period",
            "activity-filter-time",
            periods,
        ))
        .push(picker(
            &nav.author,
            "activity-filter-person",
            "activity-filter-people",
            people,
        ))
        .push(picker(
            &nav.lens,
            "activity-filter-lens",
            "activity-browse-by",
            lenses,
        ));
    if narrows {
        let cleared = nav.clone();
        row = row.push(Box::new(
            Button::new("activity-clear-filters", "activity-clear-filters")
                .with_icon(Icon::X)
                .on_open(move || cleared.clear_filters()),
        ));
    }
    Box::new(row)
}

/// Where in the feed (newest first) the selection starts and ends.
fn selected_span(feed: &wire::Feed, selection: Option<&wire::Range>) -> Option<(usize, usize)> {
    let selection = selection?;
    let at = |id: &str| feed.entries.iter().position(|entry| entry.id == id);
    let (from, to) = (at(&selection.from)?, at(&selection.to)?);
    Some((from.min(to), from.max(to)))
}

/// What a run of rows sits under: its work group, or else its day.
#[derive(Clone, PartialEq, Eq)]
enum Heading {
    Group(String),
    Day(Option<jiff::civil::Date>),
}

fn heading(entry: &wire::Entry) -> Heading {
    entry.group.clone().map_or_else(
        || {
            Heading::Day(
                jiff::Timestamp::from_millisecond(entry.at_ms)
                    .ok()
                    .map(|at| ui::clock::at(at).date()),
            )
        },
        Heading::Group,
    )
}

fn list(
    nav: &Nav,
    actions: &Actions<Window>,
    feed: &wire::Feed,
    selection: Option<&wire::Range>,
    filter: &Filters,
) -> Element {
    let span = selected_span(feed, selection);
    let groups: HashMap<&str, &wire::Group> = feed
        .groups
        .iter()
        .map(|group| (group.id.as_str(), group))
        .collect();
    let this_year = ui::clock::local_now().date();
    let mut sections: Vec<(Heading, Vec<Element>)> = Vec::new();
    for (index, entry) in feed.entries.iter().enumerate() {
        let mut words: Vec<&str> = vec![entry.summary.as_str(), entry.author.name.as_str()];
        if let Some(place) = entry.resource.as_deref().or(entry.app.as_deref()) {
            words.push(place);
        }
        if !filter.matches(&words) {
            continue;
        }
        let selected = span.is_some_and(|(first, last)| (first..=last).contains(&index));
        let row = entry_row(nav, actions, feed, entry, selected, selection);
        let under = heading(entry);
        match sections.last_mut() {
            Some((last, rows)) if *last == under => rows.push(row),
            _ => sections.push((under, vec![row])),
        }
    }
    let mut column = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(ui::tokens::Rhythm::Group);
    for (index, (under, rows)) in sections.into_iter().enumerate() {
        let section = match under {
            Heading::Group(id) => {
                let group = groups.get(id.as_str()).copied();
                let label = group.map_or_else(|| tr("activity-group-unnamed"), |g| g.label.clone());
                let mut header = SectionHeader::group(label);
                if let Some(group) = group {
                    let range = wire::Range {
                        from: group.start.clone(),
                        to: group.end.clone().unwrap_or_else(|| group.start.clone()),
                    };
                    let chosen = nav.clone();
                    header = header.action(
                        Button::new(
                            &format!("activity-group-select-{index}"),
                            "activity-group-select",
                        )
                        .on_open(move || chosen.select(Some(range.clone()))),
                    );
                }
                Section::new(rows).with_header(header)
            }
            Heading::Day(day) => {
                Section::new(rows).with_header(SectionHeader::group(day.map_or_else(
                    || tr("activity-day-unknown"),
                    |day| ui::format::day_heading(day, day.year() != this_year.year()),
                )))
            }
        };
        column = column.push(Box::new(section));
    }
    if let Some(log) = log_section(filter) {
        column = column.push(log);
    }
    Box::new(column)
}

/// What the system did, from the log: outcomes that needed nobody, with the app that did
/// them and when.
fn log_section(filter: &Filters) -> Option<Element> {
    let entries: Vec<ui::attention::Entry> = ui::attention::entries()
        .into_iter()
        .filter(|entry| {
            filter.matches(&[
                entry.what(),
                entry.owner().name.as_str(),
                entry.about_line().unwrap_or_default(),
            ])
        })
        .collect();
    if entries.is_empty() {
        return None;
    }
    let rows: Vec<Element> = entries
        .iter()
        .rev()
        .map(|entry| {
            let mut row = ListRow::new(format!("activity-log-{}", entry.id()), entry.what())
                .icon(entry.owner().icon)
                .subtitle(entry.owner().name.clone())
                .meta(ago(i64::try_from(entry.at_ms()).unwrap_or(i64::MAX)));
            let detail = entry.detail_line();
            if !detail.is_empty() {
                row = row.secondary(detail);
            }
            Box::new(row) as Element
        })
        .collect();
    Some(Box::new(Section::new(rows).with_header(
        SectionHeader::group(tr("activity-log-heading")),
    )))
}

fn entry_row(
    nav: &Nav,
    actions: &Actions<Window>,
    feed: &wire::Feed,
    entry: &wire::Entry,
    selected: bool,
    selection: Option<&wire::Range>,
) -> Element {
    let who = ui::wire_author_line(&entry.author);
    let mut place: Vec<String> = Vec::new();
    if feed.scope == wire::Scope::AllWorkspaces
        && let Some(workspace) = &entry.workspace
    {
        place.push(workspace.clone());
    }
    place.extend(entry.resource.clone().or_else(|| entry.app.clone()));
    // Where the change is not, and why: a device that refused it says so on the change's
    // own line, so its author reads which of their devices hold it.
    let refused = entry
        .refused
        .iter()
        .map(|refusal| {
            trf(
                "activity-entry-not-on",
                &[
                    ("device", refusal.label.clone().into()),
                    ("reason", refusal.reason.clone().into()),
                ],
            )
        })
        .collect::<Vec<_>>()
        .join(". ");
    let mut row = ListRow::new(
        format!("activity-entry-{}", entry.id),
        entry.summary.clone(),
    )
    .subtitle(who)
    .meta(ago(entry.at_ms))
    .selected(selected);
    row = ui::author_face(row, &entry.author);
    if !place.is_empty() {
        row = row.secondary(place.join(", "));
    }
    if !refused.is_empty() {
        row = row.note(refused);
    }
    let chosen = nav.clone();
    let one = wire::Range::one(&entry.id);
    row = row.on_open(move || chosen.select(Some(one.clone())));
    let through = selection
        .map(|range| range.from.clone())
        .filter(|from| *from != entry.id)
        .and_then(|from| {
            actions.effect(&ActivityEffect::SelectThrough {
                from,
                to: entry.id.clone(),
            })
        });
    if let Some(through) = through {
        row = row.action_binding(Icon::ListChecks, tr("activity-select-through"), through);
    }
    Box::new(row)
}
