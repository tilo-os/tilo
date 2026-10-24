//! History: every change in the workspace, newest first.
//!
//! Changes a work group holds sit under its name, the rest under their
//! day. The filter row narrows them by scope, period, person and lens; the
//! filter is the app's record ([`Filter`]), the scope is this window's.
//! Choosing a change selects it; "Select through here" stretches the
//! selection to another change, and a work group's "Select" takes all of
//! it. The selection shows in the inspector ([`crate::detail`]).

use std::collections::HashMap;

use ui::icons::Icon;
use ui::prelude::*;
use ui::tilo_actions::remote::workspace_activity as wire;
use ui::widget::{ListRow, Section, SectionHeader, Segment, ToggleGroup};

use crate::{Filter, Nav, Provider, ago, page, tr, trf, unanswered};

/// The place's content and its header's commands.
pub(crate) fn view(cx: &ViewCx<Filter>, nav: &Nav) -> (Element, Vec<Element>) {
    let narrows = cx.value(Filter::narrows);
    let clear = cx
        .actions()
        .bind(
            Button::new("activity-clear-filters", "activity-clear-filters").with_icon(Icon::X),
            Filter::CLEAR_FILTERS,
        )
        .enabled_from(&narrows);
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
                query.limit = limit;
                feed(cx, &nav, &query)
            })
        })
    });
    (content, vec![Box::new(clear)])
}

fn feed(cx: &ViewCx<Filter>, nav: &Nav, query: &wire::FeedQuery) -> Element {
    let read = Provider::of(cx).follow::<wire::Feed>(wire::FEED, Some(query));
    let nav = nav.clone();
    cx.switch(&read.answer(), move |cx, answer| {
        let Answer::Ready(feed) = answer else {
            return unanswered(answer, "activity-history-unread")
                .unwrap_or_else(|| page(Vec::new()));
        };
        let mut parts = vec![filters(cx, &nav, feed)];
        if feed.entries.is_empty() {
            parts.push(empty(feed));
            return page(parts);
        }
        let shown = feed.clone();
        let listed = nav.clone();
        parts.push(cx.switch(&nav.selection, move |_, selection| {
            list(&listed, &shown, selection.as_ref())
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
        page(parts)
    })
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

fn filters(cx: &ViewCx<Filter>, nav: &Nav, feed: &wire::Feed) -> Element {
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
    Box::new(
        Row::new()
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
            )),
    )
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

fn list(nav: &Nav, feed: &wire::Feed, selection: Option<&wire::Range>) -> Element {
    let span = selected_span(feed, selection);
    let groups: HashMap<&str, &wire::Group> = feed
        .groups
        .iter()
        .map(|group| (group.id.as_str(), group))
        .collect();
    let this_year = chrono::Local::now().date_naive();
    let mut sections: Vec<(Heading, Vec<Element>)> = Vec::new();
    for (index, entry) in feed.entries.iter().enumerate() {
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
    Box::new(column)
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
