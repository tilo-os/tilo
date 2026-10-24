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
use ui::widget::{ListRow, Section};

use crate::{Filter, Nav, Provider, ago, page, tr, trf};

/// The place's content and its header's commands.
pub(crate) fn view(cx: &ViewCx<Filter>, nav: &Nav) -> (Element, Vec<Element>) {
    let provider = Provider::of(cx);
    let summary = provider.follow::<wire::Summary>(wire::SUMMARY, None::<&()>);
    let answer = summary.answer();
    let marked = answer.clone();
    let mark = Button::new("activity-mark-viewed", "activity-mark-viewed")
        .with_icon(Icon::Check)
        .on_view(move || {
            let Arrival::Ready(summary) = marked.get() else {
                return;
            };
            provider.ask(
                wire::MARK_VIEWED,
                &wire::Viewed {
                    entries: summary
                        .briefs
                        .iter()
                        .flat_map(|brief| brief.entries.iter().cloned())
                        .collect(),
                    later: false,
                },
            );
        });
    let nav = nav.clone();
    let content = cx.arriving(&answer, move |cx, summary| page(parts(cx, &nav, summary)));
    (Box::new(content), vec![Box::new(mark)])
}

fn parts(cx: &ViewCx<Filter>, nav: &Nav, summary: &wire::Summary) -> Vec<Element> {
    let provider = Provider::of(cx);
    let mut parts = Vec::new();
    if let Some(resume) = &summary.resume {
        parts.push(resume_section(&provider, resume));
    }
    if let Some(narrative) = &summary.narrative {
        parts.push(Box::new(Section::titled(
            tr("activity-summary-on-device"),
            vec![Box::new(Text::new(narrative))],
        )) as Element);
    }
    parts.push(briefs(nav, summary));
    if !summary.sources.is_empty() {
        parts.push(sources(&provider, &summary.sources));
    }
    parts
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

fn briefs(nav: &Nav, summary: &wire::Summary) -> Element {
    let heading = tr(if summary.away {
        "activity-title-catch-up"
    } else {
        "activity-sessions"
    });
    if summary.briefs.is_empty() {
        return Box::new(Section::titled(
            heading,
            vec![Box::new(
                EmptyState::new(tr(if summary.pending {
                    "activity-history-pending"
                } else {
                    "activity-caught-up"
                }))
                .with_icon(Icon::Check)
                .with_id("activity-caught-up"),
            )],
        ));
    }
    let rows = summary
        .briefs
        .iter()
        .enumerate()
        .map(|(index, brief)| brief_row(nav, index, brief))
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

fn brief_row(nav: &Nav, index: usize, brief: &wire::Brief) -> Element {
    let mut row = ListRow::new(format!("activity-brief-{index}"), brief.title.clone())
        .two_line_title()
        .meta(ago(brief.last_ms));
    if !brief.detail.is_empty() {
        row = row.subtitle(brief.detail.clone());
    }
    let mut byline: Vec<&str> = brief.apps.iter().map(String::as_str).collect();
    byline.extend(brief.resources.iter().map(String::as_str));
    if !byline.is_empty() {
        row = row.secondary(byline.join(", "));
    }
    if let Some(author) = brief.authors.first() {
        row = row.avatar(author.name.clone(), author.identity.clone());
    }
    if brief.changes > 1 {
        row = row.count(brief.changes);
    }
    let Some(range) = range(brief) else {
        return Box::new(row);
    };
    let (opened, shown) = (nav.clone(), nav.clone());
    let again = range.clone();
    Box::new(
        row.on_view(move || opened.open_in_history(range.clone()))
            .action(
                Icon::History,
                tr("activity-brief-open-explorer"),
                move || {
                    shown.open_in_history(again.clone());
                },
            ),
    )
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
