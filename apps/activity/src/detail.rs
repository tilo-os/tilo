//! The inspector of History: what the selected run of changes is.
//!
//! Who made it and what it touched; what can be done with it (open what it
//! changed, compare before and after, undo, restore, split or merge its
//! work groups, choose it as a morning source); what changed, once
//! compared; the agent task it was part of; and its name as a work session.

use ui::icons::Icon;
use ui::prelude::*;
use ui::tilo_actions::remote::workspace_activity as wire;
use ui::widget::{ListRow, ObjectHeader, Property, PropertyList, Section};

use crate::{Filter, Nav, Provider, tr, trf};

pub(crate) fn view(cx: &ViewCx<Filter>, nav: &Nav) -> Element {
    let nav = nav.clone();
    cx.switch(&nav.selection.clone(), move |cx, selection| {
        let Some(range) = selection.clone() else {
            return Box::new(
                EmptyState::new(tr("activity-select-prompt"))
                    .with_message(tr("activity-select-hint"))
                    .with_icon(Icon::History)
                    .with_id("activity-detail-none"),
            );
        };
        let nav = nav.clone();
        cx.switch(&nav.compare.clone(), move |cx, compare| {
            let query = wire::DetailQuery {
                range: range.clone(),
                compare: *compare,
            };
            let read = Provider::of(cx).follow::<wire::Detail>(wire::DETAIL, Some(&query));
            let (nav, range) = (nav.clone(), range.clone());
            Box::new(cx.arriving(&read.answer(), move |cx, detail| {
                content(cx, &nav, &range, detail)
            }))
        })
    })
}

fn content(cx: &ViewCx<Filter>, nav: &Nav, range: &wire::Range, detail: &wire::Detail) -> Element {
    if detail.gone {
        return Box::new(
            EmptyState::new(tr("activity-history-gone"))
                .with_icon(Icon::History)
                .with_id("activity-detail-gone"),
        );
    }
    let provider = Provider::of(cx);
    let mut parts: Vec<Element> = vec![Box::new(
        ObjectHeader::new(detail.title.clone()).caption(detail.provenance.clone()),
    )];
    let mut facts = vec![Property::new(
        tr("activity-detail-authors"),
        detail.authors.join(", "),
    )];
    if !detail.resources.is_empty() {
        facts.push(Property::new(
            tr("activity-detail-resources"),
            detail
                .resources
                .iter()
                .map(|resource| resource.label.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }
    if let Some(label) = &detail.group_label {
        facts.push(Property::new(tr("activity-detail-group"), label.clone()));
    }
    parts.push(Box::new(PropertyList::new(facts)));
    if !detail.offers.is_empty() {
        parts.push(offers(&provider, nav, range, detail));
    }
    // What each field held either side, before the lines that only name the
    // change: "Title — was Q3 budget" says nothing a person can act on.
    if !detail.changes.is_empty() {
        let rows: Vec<Element> = detail
            .changes
            .iter()
            .enumerate()
            .map(|(index, change)| {
                let mut row = ListRow::new(format!("activity-field-{index}"), &change.field)
                    .value(change.after.clone())
                    .secondary(trf(
                        "activity-was",
                        &[("before", change.before.clone().into())],
                    ));
                if change.after.is_empty() {
                    row = row.secondary(tr("activity-removed"));
                }
                Box::new(row) as Element
            })
            .collect();
        parts.push(Box::new(Section::titled(
            tr("activity-changed-fields"),
            rows,
        )));
    }
    match &detail.comparison {
        Some(wire::Comparison::Changes { lines }) => {
            let rows = if lines.is_empty() {
                vec![Box::new(Text::new(&tr("activity-no-differences")).dimmed()) as Element]
            } else {
                lines
                    .iter()
                    .enumerate()
                    .map(|(index, line)| {
                        Box::new(ListRow::new(
                            format!("activity-change-{index}"),
                            line.clone(),
                        )) as Element
                    })
                    .collect()
            };
            parts.push(Box::new(Section::titled(tr("activity-what-changed"), rows)));
        }
        Some(wire::Comparison::Unavailable) => parts.push(Box::new(
            EmptyState::new(tr("activity-preview-unavailable"))
                .with_message(tr("activity-preview-unavailable-detail"))
                .with_id("activity-comparison-unavailable"),
        )),
        None => {}
    }
    if let Some(run) = &detail.run {
        parts.extend(agent_run(run));
    }
    parts.push(name(cx, &provider, range, detail));
    crate::page(parts)
}

fn offers(provider: &Provider, nav: &Nav, range: &wire::Range, detail: &wire::Detail) -> Element {
    let mut row = Row::new().with_rhythm(ui::tokens::Rhythm::Related);
    for offer in &detail.offers {
        let id = format!("activity-offer-{}", offer.action.action());
        let button = Button::new(&id, &offer.label);
        let button = match offer.action {
            wire::DetailAction::Compare => {
                let compare = nav.compare.clone();
                button
                    .toggled(nav.compare.get())
                    .on_view(move || compare.update(|on| *on = !*on))
            }
            wire::DetailAction::Open => {
                let (provider, target) = (provider.clone(), detail.resources.first().cloned());
                button.on_view(move || {
                    if let Some(target) = &target {
                        provider.ask(wire::OPEN, target);
                    }
                })
            }
            action => {
                let (provider, range) = (provider.clone(), range.clone());
                button.on_view(move || provider.ask(action.action(), &range))
            }
        };
        row = row.push(Box::new(button));
    }
    Box::new(row)
}

fn agent_run(run: &wire::Run) -> Vec<Element> {
    let facts = vec![
        Property::new(tr("activity-why-this-work"), run.instruction.clone()),
        Property::new(tr("activity-agent-task-by"), run.byline.clone()),
        Property::new(
            tr("activity-agent-decision"),
            run.decision
                .clone()
                .unwrap_or_else(|| tr("activity-no-decision")),
        ),
    ];
    let mut items: Vec<Element> = vec![Box::new(PropertyList::new(facts))];
    for status in [&run.undo_blocked, &run.undo_status].into_iter().flatten() {
        items.push(Box::new(Text::new(status).dimmed()));
    }
    let mut parts: Vec<Element> = vec![Box::new(Section::titled(tr("activity-agent-task"), items))];
    if !run.records.is_empty() {
        let rows = run
            .records
            .iter()
            .enumerate()
            .map(|(index, changed)| -> Element {
                Box::new(
                    ListRow::new(
                        format!("activity-run-record-{index}"),
                        changed.record.clone(),
                    )
                    .subtitle(changed.change.clone())
                    .secondary(changed.app.clone()),
                )
            })
            .collect();
        parts.push(Box::new(Section::titled(
            tr("activity-affected-records"),
            rows,
        )));
    }
    parts
}

/// The run's name as a work session: typed, then saved with the button.
fn name(
    cx: &ViewCx<Filter>,
    provider: &Provider,
    range: &wire::Range,
    detail: &wire::Detail,
) -> Element {
    let draft = cx.local(detail.group_label.clone().unwrap_or_default());
    let input = TextInput::from_state(&draft)
        .with_id("activity-name")
        .with_placeholder("activity-name-placeholder");
    let (provider, range) = (provider.clone(), range.clone());
    let save = Button::new("activity-name-save", "activity-name-save").on_view(move || {
        provider.ask(
            wire::NAME,
            &wire::Name {
                range: range.clone(),
                record: None,
                label: draft.get(),
            },
        );
    });
    Box::new(Section::titled(
        tr("activity-name-heading"),
        vec![Box::new(
            Row::new()
                .no_wrap()
                .with_rhythm(ui::tokens::Rhythm::Related)
                .push(Box::new(
                    Container::new()
                        .with_flex_grow()
                        .with_no_padding()
                        .with_child(Box::new(input)),
                ))
                .push(Box::new(save)),
        )],
    ))
}
