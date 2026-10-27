//! The history panel: how the artwork got here, newest first, and who did
//! each step — a person, or an agent's run, whose steps read as one entry
//! that can be taken back whole. Choosing a version shows it in place of
//! the artwork until it is restored or now is chosen again. Undo, redo,
//! restore and checkpoints are the history actions every app answers.

use std::cell::RefCell;
use std::rc::Rc;

use ui::framework::history::HistoryVersion;
use ui::icons::Icon;
use ui::prelude::*;
use ui::widget::ButtonRole;
use ui::widget::ControlRow;

use super::{caption, icon_button};
use crate::model::Artwork;
use crate::tools::Tools;

/// How many recent versions the panel lists.
const LISTED: usize = 200;

/// One row: a person's step, or an agent run's consecutive steps.
#[derive(Clone, Debug, PartialEq)]
struct Entry {
    /// The newest version of the entry: what choosing it shows.
    change: String,
    action: String,
    author: String,
    run: Option<String>,
    steps: usize,
    time_ms: i64,
    undone: bool,
    checkpoints: Vec<String>,
}

/// The rows, newest first, an agent run's consecutive steps folded into
/// one. Naming a checkpoint is shown on the version it names, not as a
/// step of its own.
fn entries(versions: &[HistoryVersion]) -> Vec<Entry> {
    let mut entries: Vec<Entry> = Vec::new();
    for version in versions.iter().rev() {
        if version.housekeeping {
            continue;
        }
        if let Some(last) = entries.last_mut()
            && version.run.is_some()
            && last.run == version.run
            && last.author == version.author
        {
            last.steps = last.steps.saturating_add(1);
            last.undone &= version.undone;
            last.checkpoints.extend(version.checkpoints.iter().cloned());
            continue;
        }
        entries.push(Entry {
            change: version.change.clone(),
            action: version.action.clone(),
            author: version.author.clone(),
            run: version.run.clone(),
            steps: 1,
            time_ms: version.time_ms,
            undone: version.undone,
            checkpoints: version.checkpoints.clone(),
        });
    }
    entries
}

/// Whether `author` is an identity key rather than a name.
fn is_key(author: &str) -> bool {
    author.len() >= 32 && author.chars().all(|c| c.is_ascii_hexdigit())
}

/// "3 minutes ago".
fn ago(time_ms: i64) -> String {
    let Ok(ms) = u64::try_from(time_ms) else {
        return String::new();
    };
    if ms == 0 {
        return String::new();
    }
    let Some(at) =
        ui::time::SystemTime::UNIX_EPOCH.checked_add(std::time::Duration::from_millis(ms))
    else {
        return String::new();
    };
    ui::widget::history::format_relative_time(at, ui::time::wall())
}

pub fn panel(cx: &ViewCx<Artwork>, tools: &Tools) -> Element {
    let versions = cx.history(LISTED);
    let tools = tools.clone();
    let newest: Rc<RefCell<Option<String>>> = Rc::default();
    cx.switch(&versions, move |cx, versions| {
        // A person's new step ends a look at the past: they restored it, or
        // undid or redid from here.
        let latest = versions.last();
        let moved = latest.map(|version| version.change.clone()) != *newest.borrow();
        if moved && newest.borrow().is_some() && latest.is_some_and(|version| version.run.is_none())
        {
            tools.preview.set(None);
        }
        newest.replace(latest.map(|version| version.change.clone()));
        let entries = entries(versions);
        let mut list = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(ui::tokens::Spacing::BASE.s1)
            .push(now_row(&tools));
        // A rule where the hand changes, between a person's steps and an
        // agent's run or one run and the next, so each reads as its own.
        let mut hand: Option<(Option<&str>, &str)> = None;
        for entry in &entries {
            let this = (entry.run.as_deref(), entry.author.as_str());
            if hand.is_some_and(|before| before != this) {
                list = list.push(Box::new(ui::widget::Divider::horizontal()));
            }
            hand = Some(this);
            list = list.push(entry_row(cx, &tools, entry));
        }
        Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_gap(0.0)
                .push(checkpoint_controls(cx, &tools))
                .push(viewing(cx, &tools))
                .push(Box::new(
                    Scrollable::new_vertical()
                        .with_max_height(LIST_HEIGHT)
                        .with_child(Box::new(
                            Column::new()
                                .with_intrinsic_height()
                                .with_vertical_padding(ui::tokens::Spacing::BASE.s1)
                                .with_horizontal_padding(
                                    ui::tokens::Spacing::BASE.s2,
                                    ui::tokens::Spacing::BASE.s2,
                                )
                                .push(Box::new(list)),
                        )),
                )),
        )
    })
}

/// How tall the list of versions grows before it scrolls.
const LIST_HEIGHT: f32 = 7.0 * ui::tokens::Metrics::BASE.panel_row_height;

/// A person names a meaningful point without committing every typed letter.
fn checkpoint_controls(cx: &ViewCx<Artwork>, tools: &Tools) -> Element {
    let name = tools.checkpoint_name.clone();
    Box::new(
        Column::new()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Tight)
            .with_padding_rhythm(ui::tokens::Rhythm::Sibling, ui::tokens::Rhythm::Tight)
            .push(Box::new(
                TextInput::from_state(&name)
                    .with_id("canvas-checkpoint-label")
                    .with_placeholder(ui::i18n::I18nManager::lookup_plain(
                        "canvas-checkpoint-label",
                    ))
                    .with_accessible_name(ui::i18n::I18nManager::lookup_plain(
                        "canvas-checkpoint-label",
                    )),
            ))
            .push(cx.switch(&name, |cx, name| {
                Box::new(
                    cx.actions().bind_invocation(
                        ui::widget::Button::new("canvas-checkpoint-save", "canvas-checkpoint-save")
                            .with_icon(Icon::Flag)
                            .with_enabled(!name.trim().is_empty()),
                        Artwork::CHECKPOINT.with(name.trim().to_owned()),
                    ),
                )
            })),
    )
}

/// Naming the current version, beside the history tab. Its name counts
/// the checkpoints there are.
pub fn checkpoint_button(cx: &ViewCx<Artwork>) -> Element {
    let versions = cx.history(LISTED);
    cx.switch(&versions, |cx, versions| {
        let checkpoints = versions
            .iter()
            .flat_map(|version| version.checkpoints.iter())
            .count();
        let name = ui::i18n::I18nManager::format(
            "canvas-checkpoint-name",
            &[("number", checkpoints.saturating_add(1).into())],
        );
        Box::new(icon_button(
            cx,
            Icon::Flag,
            &ui::i18n::I18nManager::lookup_plain("canvas-checkpoint"),
            Artwork::CHECKPOINT.with(name),
        ))
    })
}

/// While a past version is shown: what it is, and restoring it.
fn viewing(cx: &ViewCx<Artwork>, tools: &Tools) -> Element {
    cx.switch(&tools.preview, |cx, preview| {
        let Some(version) = preview else {
            return Box::new(Spacer::token(ui::tokens::SpacingStep::S0));
        };
        Box::new(
            Row::new()
                .with_gap(ui::tokens::Spacing::BASE.s2)
                .no_wrap()
                .with_padding(ui::tokens::Spacing::BASE.s4, ui::tokens::Spacing::BASE.s2)
                .with_align_items(ui::taffy::AlignItems::Center)
                .push(caption(&ui::i18n::I18nManager::lookup_plain(
                    "canvas-viewing-past",
                )))
                .push(Box::new(Spacer::new()))
                .push(Box::new(
                    cx.button("canvas-restore", Artwork::RESTORE.with(version.clone()))
                        .role(ButtonRole::Primary),
                )),
        )
    })
}

/// The live artwork, at the top of the list.
fn now_row(tools: &Tools) -> Element {
    let content = Row::new()
        .with_padding(ui::tokens::Spacing::BASE.s2, ui::tokens::Spacing::BASE.s2)
        .with_gap(ui::tokens::Spacing::BASE.s3)
        .no_wrap()
        .with_align_items(ui::taffy::AlignItems::Center)
        .push(Box::new(
            Text::new("canvas-history-now").with_weight(ui::text_render::TextWeight::Semibold),
        ));
    Box::new(
        ControlRow::new(content)
            .choosing(&tools.preview, None)
            .with_accessible_name(ui::i18n::I18nManager::lookup_plain("canvas-history-now")),
    )
}

fn entry_row(cx: &ViewCx<Artwork>, tools: &Tools, entry: &Entry) -> Element {
    let by_agent = entry.run.is_some();
    let action = if entry.action.is_empty() {
        ui::i18n::I18nManager::lookup_plain("canvas-history-created")
    } else {
        entry.action.clone()
    };
    let title = Text::new(&action);
    let title = if entry.undone { title.dimmed() } else { title };
    // A raw identity key says nothing to a reader: the person icon does.
    let mut byline: Vec<String> = Some(entry.author.clone())
        .filter(|author| !is_key(author))
        .into_iter()
        .collect();
    if entry.steps > 1 {
        byline.push(ui::i18n::I18nManager::format(
            "canvas-history-steps",
            &[("steps", entry.steps.into())],
        ));
    }
    let when = ago(entry.time_ms);
    if !when.is_empty() {
        byline.push(when);
    }
    let byline = byline.join(", ");
    let mut text = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(0.0)
        .push(Box::new(title))
        .push(caption(&byline));
    if !entry.checkpoints.is_empty() {
        text = text.push(Box::new(
            Row::new()
                .with_gap(ui::tokens::Spacing::BASE.s1)
                .no_wrap()
                .with_align_items(ui::taffy::AlignItems::Center)
                .push(Box::new(
                    ui::widget::IconWidget::new(Icon::Flag)
                        .sized(ui::widget::IconSize::Small)
                        .dim(),
                ))
                .push(caption(&entry.checkpoints.join(", "))),
        ));
    }
    let who =
        Box::new(ui::widget::IconWidget::new(if by_agent { Icon::Bot } else { Icon::User }).dim());
    let mut content = Row::new()
        .with_padding(ui::tokens::Spacing::BASE.s2, ui::tokens::Spacing::BASE.s2)
        .with_gap(ui::tokens::Spacing::BASE.s3)
        .no_wrap()
        .with_align_items(ui::taffy::AlignItems::Center)
        .push(who)
        .push(Box::new(
            Container::new()
                .with_no_padding()
                .with_flex_grow()
                .with_min_width(0.0)
                .with_child(Box::new(text)),
        ));
    if let Some(run) = &entry.run
        && !entry.undone
    {
        content = content.push(Box::new(icon_button(
            cx,
            Icon::Undo2,
            &ui::i18n::I18nManager::format(
                "canvas-undo-run",
                &[("run", entry.author.clone().into())],
            ),
            Artwork::UNDO_RUN.with(run.clone()),
        )));
    }
    Box::new(
        ControlRow::new(content)
            .choosing(&tools.preview, Some(entry.change.clone()))
            .with_id(format!("canvas-version-{}", entry.change))
            .with_accessible_name(format!("{action}, {byline}")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(change: &str, action: &str, run: Option<&str>) -> HistoryVersion {
        HistoryVersion {
            change: change.into(),
            action: action.into(),
            author: run.unwrap_or("quinten").into(),
            run: run.map(Into::into),
            ..HistoryVersion::default()
        }
    }

    #[test]
    fn an_agent_runs_steps_read_as_one_entry_and_a_persons_each_their_own() {
        let versions = [
            version("a", "Paint strokes", None),
            version("b", "Draw shapes", Some("run-1")),
            version("c", "Draw shapes", Some("run-1")),
            HistoryVersion {
                housekeeping: true,
                ..version("d", "Checkpoint", None)
            },
            version("e", "Paint strokes", None),
            version("f", "Paint strokes", None),
        ];
        let entries = entries(&versions);
        let summary: Vec<(&str, usize)> = entries
            .iter()
            .map(|entry| (entry.change.as_str(), entry.steps))
            .collect();
        assert_eq!(summary, [("f", 1), ("e", 1), ("c", 2), ("a", 1)]);
        assert_eq!(entries[2].run.as_deref(), Some("run-1"));
    }
}
