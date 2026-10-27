//! What's new: what changed since this device last looked.
//!
//! The work sessions since then, newest first, each a row with who did it
//! and when; where you left off; the on-device summary; the morning sources
//! you chose. A work session opens selected in History. Leaving the place
//! counts what it showed as seen (the session does that once the read
//! ends); "Mark as viewed" does it at once.

use ui::icons::Icon;
use ui::prelude::*;
use ui::tilo_actions::remote::workspace_activity as wire;
use ui::widget::{DisclosureRow, DisclosureState, ListRow, Section};

use crate::{Filter, Nav, Provider, ago, page, tr, trf};

/// The place's content and its header's commands.
pub(crate) fn view(cx: &ViewCx<Filter>, nav: &Nav) -> (Element, Vec<Element>) {
    let provider = Provider::of(cx);
    let summary = provider.follow::<wire::Summary>(wire::SUMMARY, None::<&()>);
    let answer = summary.answer();
    // Disabled while there is nothing new to mark: no summary yet, or one
    // with no work sessions in it.
    let mark = cx.switch(&answer.state(), move |_, answer| {
        let entries: Vec<_> = match answer {
            Arrival::Ready(summary) => summary
                .briefs
                .iter()
                .flat_map(|brief| brief.entries.iter().cloned())
                .collect(),
            _ => Vec::new(),
        };
        let nothing_new = entries.is_empty();
        let provider = provider.clone();
        let mut mark = Button::new("activity-mark-viewed", "activity-mark-viewed")
            .with_icon(Icon::Check)
            .on_view(move || {
                provider.ask(
                    wire::MARK_VIEWED,
                    &wire::Viewed {
                        entries: entries.clone(),
                        later: false,
                    },
                );
            });
        mark.set_enabled(!nothing_new);
        Box::new(mark)
    });
    let nav = nav.clone();
    // Which work session is open, kept across the arrivals that rebuild
    // the list under it: a sync landing a change must not shut a session
    // the person just opened.
    let opened = cx.local(None::<String>);
    let content = cx.arriving(&answer, move |cx, summary| {
        page(parts(cx, &nav, &opened, summary))
    });
    (Box::new(content), vec![mark])
}

fn parts(
    cx: &ViewCx<Filter>,
    nav: &Nav,
    opened: &LocalState<Option<String>>,
    summary: &wire::Summary,
) -> Vec<Element> {
    let provider = Provider::of(cx);
    let mut parts = Vec::new();
    if let Some(resume) = &summary.resume {
        parts.push(resume_section(&provider, resume));
    }
    if let Some(narrative) = &summary.narrative {
        parts.push(narrative_section(narrative));
    }
    parts.push(briefs(nav, opened, summary));
    if !summary.sources.is_empty() {
        parts.push(sources(&provider, &summary.sources));
    }
    parts
}

/// The on-device summary: what the work adds up to, set as the page's
/// lede under the heading that says where it came from.
fn narrative_section(narrative: &str) -> Element {
    Box::new(Section::titled(
        tr("activity-summary-on-device"),
        vec![Box::new(
            Text::new(narrative)
                .with_id("activity-narrative")
                .with_role(ui::TextRole::Body),
        )],
    ))
}

fn resume_section(provider: &Provider, resume: &wire::Resume) -> Element {
    let target = resume.target.clone();
    let open = provider.clone();
    let context = trf(
        "activity-resume-context",
        &[
            (
                "app",
                resume
                    .app
                    .clone()
                    .or_else(|| resume.target.app_label.clone())
                    .unwrap_or_default()
                    .into(),
            ),
            ("when", ago(resume.at_ms).into()),
        ],
    );
    Box::new(Section::titled(
        tr("activity-resume-heading"),
        vec![Box::new(
            ListRow::new("activity-resume", resume.target.label.clone())
                .subtitle(context)
                .avatar(resume.author.name.clone(), resume.author.identity.clone())
                .trailing(Box::new(
                    Button::new("activity-resume-open", "activity-resume-button")
                        .on_view(move || open.ask(wire::OPEN, &target)),
                )),
        )],
    ))
}

fn briefs(nav: &Nav, opened: &LocalState<Option<String>>, summary: &wire::Summary) -> Element {
    let heading = tr(if summary.away {
        "activity-title-catch-up"
    } else {
        "activity-sessions"
    });
    if summary.briefs.is_empty() {
        // Nothing to list: no heading over nothing, and the way on from here.
        if summary.pending {
            return Box::new(
                EmptyState::new(tr("activity-history-pending"))
                    .with_icon(Icon::Clock)
                    .with_id("activity-caught-up"),
            );
        }
        let history = nav.clone();
        return Box::new(
            EmptyState::new(tr("activity-caught-up"))
                .with_icon(Icon::Check)
                .with_action(tr("activity-see-history"))
                .with_action_id("activity-see-history")
                .on_view(move || history.show(crate::Place::History)),
        );
    }
    let rows = summary
        .briefs
        .iter()
        .enumerate()
        .map(|(index, brief)| brief_card(nav, opened, index, brief))
        .collect();
    Box::new(Section::titled(heading, rows))
}

/// The run of changes a work session made, oldest to newest.
fn range(brief: &wire::Brief) -> Option<wire::Range> {
    Some(wire::Range {
        from: brief.entries.first()?.clone(),
        to: brief.entries.last()?.clone(),
    })
}

/// One work session as an outcome card: what came of it and when, a person
/// who did it, and the changes it made behind a disclosure. Which card is
/// open is the view's, so it survives the reads that rebuild the list; each
/// card carries its own row state, so opening one leaves the others as they
/// were.
fn brief_card(
    nav: &Nav,
    opened: &LocalState<Option<String>>,
    index: usize,
    brief: &wire::Brief,
) -> Element {
    let key = format!("activity-brief-{index}");
    let state = DisclosureState::new();
    state.set_open(opened.with(|open| open.as_deref() == Some(key.as_str())));
    let remembered = opened.clone();
    let authors: Vec<&str> = brief
        .authors
        .iter()
        .map(|author| author.name.as_str())
        .collect();
    let name = key.clone();
    let mut row = DisclosureRow::new(&brief.title, brief_body(index, brief))
        .with_id(key)
        .with_state(&state)
        .icon(Icon::History)
        .on_open_toggle(move |open| match open {
            true => remembered.set(Some(name.clone())),
            false if remembered.with(|kept| kept.as_deref() == Some(name.as_str())) => {
                remembered.set(None);
            }
            false => {}
        });
    if !brief.detail.is_empty() {
        row = row.secondary(&brief.detail);
    }
    if !authors.is_empty() {
        // Who and when sit at the far end of the title's own line, in the
        // order a person reads them: the work first, the hand that did it
        // and the time it landed second.
        row = row.secondary_side(format!("{} {}", ago(brief.last_ms), authors.join(", ")));
    }
    if let Some(range) = range(brief) {
        let history = nav.clone();
        row = row.trailing(Box::new(
            Button::new(
                &format!("activity-brief-{index}-history"),
                "activity-brief-open-explorer",
            )
            .on_view(move || history.open_in_history(range.clone())),
        ));
    }
    Box::new(row)
}

/// The changes a work session made, oldest to newest, each with who made it
/// and when.
fn brief_body(index: usize, brief: &wire::Brief) -> Element {
    if brief.actions.is_empty() {
        return Box::new(
            Text::new(&tr("activity-brief-no-changes"))
                .with_id(format!("activity-brief-{index}-empty"))
                .dimmed(),
        );
    }
    let mut column = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(ui::tokens::Rhythm::Tight);
    for (at, action) in brief.actions.iter().enumerate() {
        column = column.push(Box::new(
            ListRow::new(
                format!("activity-brief-{index}-action-{at}"),
                action.summary.clone(),
            )
            .secondary(action.author.name.clone())
            .meta(ago(action.at_ms))
            .avatar(action.author.name.clone(), action.author.identity.clone()),
        ));
    }
    Box::new(column)
}

fn sources(provider: &Provider, sources: &[wire::Source]) -> Element {
    let rows = sources
        .iter()
        .map(|source| {
            let ask = |command| {
                let (provider, key) = (provider.clone(), source.key.clone());
                move || {
                    provider.ask(
                        wire::SOURCE,
                        &wire::SourceRequest {
                            key: key.clone(),
                            command,
                        },
                    );
                }
            };
            let mut row = ListRow::new(
                format!("activity-source-{}", source.key),
                source.label.clone(),
            )
            .subtitle(source.text.clone())
            .two_line_secondary()
            .on_view(ask(wire::SourceCommand::Open));
            if let Some(status) = &source.status {
                row = row.meta(status.clone());
            }
            if source.refreshable {
                row = row.action(
                    Icon::RotateCcw,
                    tr("activity-morning-refresh"),
                    ask(wire::SourceCommand::Refresh),
                );
            }
            Box::new(row.action(
                Icon::X,
                tr("activity-morning-remove"),
                ask(wire::SourceCommand::Remove),
            )) as Element
        })
        .collect();
    Box::new(Section::titled(tr("activity-morning-heading"), rows))
}
