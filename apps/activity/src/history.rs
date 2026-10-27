//! History: every change in the workspace, newest first — and, beside them,
//! the ledger's outcomes: what the system did that needs no decision.
//!
//! Changes a work group holds sit under its name, the rest under their
//! day. The filter row narrows them by scope, period, person and lens, and
//! the window bar's search by what they say; the filter is the app's record
//! ([`Filter`]), the scope is this window's. The feed answers the bar's
//! search as a query view ([`ui::widget::QueryView`]): what is typed reaches
//! the filter, and the bar counts what the filter left of the page it read.
//! Choosing a change selects it; "Select through here" stretches the
//! selection to another change, and a work group's "Select" takes all of
//! it. The selection shows in the inspector ([`crate::detail`]).

use std::collections::HashMap;

use ui::icons::Icon;
use ui::prelude::*;
use ui::tilo_actions::remote::workspace_activity as wire;
use ui::widget::{FilterCount, ListRow, QueryView, Section, SectionHeader, Segment, ToggleGroup};

use crate::{Filter, Nav, Provider, ago, page, tr, trf};

/// The place's content and its header's commands.
pub(crate) fn view(cx: &ViewCx<Filter>, nav: &Nav) -> (Element, Vec<Element>) {
    let filter = cx.value(Filter::clone);
    let (scoped, limited) = (nav.clone(), nav.clone());
    let content = cx.switch(&nav.everywhere, move |cx, everywhere| {
        let scope = if *everywhere {
            wire::Scope::AllWorkspaces
        } else {
            wire::Scope::ThisWorkspace
        };
        let nav = scoped.clone();
        let filter = filter.clone();
        cx.switch(&limited.limit, move |cx, limit| {
            let nav = nav.clone();
            let limit = *limit;
            cx.switch(&filter, move |cx, filter| {
                let mut query = filter.query(scope);
                // A search reaches past one page: the feed keeps reading
                // while someone is looking for something.
                query.limit = if filter.searching() {
                    limit.max(wire::FEED_PAGE).saturating_mul(4)
                } else {
                    limit
                };
                feed(cx, &nav, &query, filter.narrows())
            })
        })
    });
    (content, Vec::new())
}

/// The feed `query` reads, under the filter row; `narrows` says whether the
/// filter narrows it.
fn feed(cx: &ViewCx<Filter>, nav: &Nav, query: &wire::FeedQuery, narrows: bool) -> Element {
    let read = Provider::of(cx).follow::<wire::Feed>(wire::FEED, Some(query));
    let nav = nav.clone();
    Box::new(cx.arriving(&read.answer(), move |cx, feed| {
        let filter = cx.value(Filter::clone).get();
        let mut parts = vec![filters(cx, &nav, feed, narrows)];
        let found = found(feed, &filter);
        if feed.entries.is_empty() {
            parts.push(empty(feed));
        } else {
            let shown = feed.clone();
            let listed = nav.clone();
            let narrowed = filter.clone();
            parts.push(cx.switch(&nav.selection, move |_, selection| {
                list(&listed, &shown, selection.as_ref(), &narrowed)
            }));
            if feed.total > feed.entries.len() {
                let more = nav.clone();
                parts.push(Box::new(
                    Button::new("activity-show-more", "activity-show-more").on_view(move || {
                        more.limit.update(|limit| {
                            *limit = (*limit)
                                .max(wire::FEED_PAGE)
                                .saturating_add(wire::FEED_PAGE);
                        });
                    }),
                ));
            }
        }
        searchable(cx, page(parts), &filter, found.len(), feed.entries.len())
    }))
}

/// The page, as the window bar's search narrows it: what is typed reaches the
/// filter, and the bar counts what the filter left of the `total` the read
/// returned.
fn searchable(
    cx: &ViewCx<Filter>,
    content: Element,
    filter: &Filter,
    found: usize,
    total: usize,
) -> Element {
    let dispatcher = cx.dispatcher();
    let path = Filter::paths().query;
    let current = filter.query.clone();
    Box::new(
        QueryView::new(content, move |text| {
            if text == current {
                return;
            }
            // A text field's edit always serializes.
            if let Ok(invoke) = Invoke::edit(&path.set(text.to_owned())) {
                dispatcher.submit(invoke);
            }
        })
        .kept(FilterCount::new(found, total)),
    )
}

/// What the search leaves of the feed: the changes whose words, person or
/// app contain its query.
fn found(feed: &wire::Feed, filter: &Filter) -> Vec<wire::Entry> {
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

fn empty(feed: &wire::Feed) -> Element {
    let state = if feed.pending {
        EmptyState::new(tr("activity-history-pending")).with_icon(Icon::Clock)
    } else {
        EmptyState::new(tr("activity-history-empty"))
            .with_message(tr("activity-history-empty-message"))
            .with_icon(Icon::History)
    };
    Box::new(state.with_id("activity-history-empty"))
}

/// A dropdown of `options` (value, label) bound to `field` of the filter.
fn picker(
    cx: &ViewCx<Filter>,
    id: &'static str,
    name: &str,
    options: Vec<(String, String)>,
    field: Path<Filter, String>,
) -> Element {
    let name = tr(name);
    Box::new(cx.control(
        move |state| {
            let mut picker = Dropdown::from_state(Vec::new(), state)
                .with_id(id)
                .with_accessible_name(name);
            picker.set_value_options(options);
            picker
        },
        field,
    ))
}

/// The filter row, with a way back to every change while the filter
/// narrows them, and only then.
fn filters(cx: &ViewCx<Filter>, nav: &Nav, feed: &wire::Feed, narrows: bool) -> Element {
    let paths = Filter::paths();
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
            cx,
            "activity-filter-period",
            "activity-filter-time",
            periods,
            paths.period,
        ))
        .push(picker(
            cx,
            "activity-filter-person",
            "activity-filter-people",
            people,
            paths.author,
        ))
        .push(picker(
            cx,
            "activity-filter-lens",
            "activity-browse-by",
            lenses,
            paths.lens,
        ));
    if narrows {
        row = row.push(Box::new(cx.actions().bind(
            Button::new("activity-clear-filters", "activity-clear-filters").with_icon(Icon::X),
            Filter::CLEAR_FILTERS,
        )));
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
    Day(Option<chrono::NaiveDate>),
}

fn heading(entry: &wire::Entry) -> Heading {
    entry.group.clone().map_or_else(
        || {
            Heading::Day(
                chrono::DateTime::from_timestamp_millis(entry.at_ms)
                    .map(|at| at.with_timezone(&chrono::Local).date_naive()),
            )
        },
        Heading::Group,
    )
}

fn list(nav: &Nav, feed: &wire::Feed, selection: Option<&wire::Range>, filter: &Filter) -> Element {
    let span = selected_span(feed, selection);
    let groups: HashMap<&str, &wire::Group> = feed
        .groups
        .iter()
        .map(|group| (group.id.as_str(), group))
        .collect();
    let this_year = chrono::Local::now().date_naive();
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
        let row = entry_row(nav, feed, entry, selected, selection);
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
                        .on_view(move || chosen.select(Some(range.clone()))),
                    );
                }
                Section::new(rows).with_header(header)
            }
            Heading::Day(day) => {
                Section::new(rows).with_header(SectionHeader::group(day.map_or_else(
                    || tr("activity-day-unknown"),
                    |day| {
                        ui::format::day_heading(
                            day,
                            chrono::Datelike::year(&day) != chrono::Datelike::year(&this_year),
                        )
                    },
                )))
            }
        };
        column = column.push(Box::new(section));
    }
    if let Some(ledger) = ledger_section(filter) {
        column = column.push(ledger);
    }
    Box::new(column)
}

/// What the system did, from the ledger: outcomes that needed nobody, with
/// the app that did them and when. They sit at the end of the history and
/// the window bar's search narrows them with the changes.
fn ledger_section(filter: &Filter) -> Option<Element> {
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
            let mut row = ListRow::new(format!("activity-ledger-{}", entry.id()), entry.what())
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
        SectionHeader::group(tr("activity-ledger-heading")),
    )))
}

fn entry_row(
    nav: &Nav,
    feed: &wire::Feed,
    entry: &wire::Entry,
    selected: bool,
    selection: Option<&wire::Range>,
) -> Element {
    let author = entry.author.on_behalf_of.as_ref().map_or_else(
        || entry.author.name.clone(),
        |person| {
            trf(
                "activity-entry-on-behalf",
                &[
                    ("agent", entry.author.name.clone().into()),
                    ("person", person.clone().into()),
                ],
            )
        },
    );
    let mut place: Vec<&str> = Vec::new();
    if feed.scope == wire::Scope::AllWorkspaces
        && let Some(workspace) = &entry.workspace
    {
        place.push(workspace);
    }
    place.extend(entry.resource.as_deref().or(entry.app.as_deref()));
    let mut row = ListRow::new(
        format!("activity-entry-{}", entry.id),
        entry.summary.clone(),
    )
    .subtitle(author)
    .meta(ago(entry.at_ms))
    .avatar(entry.author.name.clone(), entry.author.identity.clone())
    .selected(selected);
    if !place.is_empty() {
        row = row.secondary(place.join(", "));
    }
    let (chosen, stretched) = (nav.clone(), nav.clone());
    let (one, id) = (wire::Range::one(&entry.id), entry.id.clone());
    let from = selection.map(|range| range.from.clone());
    row = row.on_view(move || chosen.select(Some(one.clone())));
    if let Some(from) = from.filter(|from| *from != entry.id) {
        row = row.action(Icon::ListChecks, tr("activity-select-through"), move || {
            stretched.select(Some(wire::Range {
                from: from.clone(),
                to: id.clone(),
            }));
        });
    }
    Box::new(row)
}
