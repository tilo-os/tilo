//! Replay: the workspace's layout at each recent change.
//!
//! The slider and the transport step through the changes; playing steps
//! one a beat until the last ([`ui::widget::Pacer`]). Each step shows the
//! change, who made it and when, every workspace's window groups with the
//! tab each showed, and who was present.

use ui::prelude::*;
use ui::tilo_actions::remote::workspace_activity as wire;
use ui::time::Duration;
use ui::widget::{Card, ListRow, Pacer, Section, TransportControl};

use crate::{Filter, Nav, Provider, ago, page, tr, trf, unanswered};

/// How long each change shows while playing.
const BEAT: Duration = Duration::from_millis(850);

/// The slider's position of change `index`.
fn place_of(index: usize) -> f64 {
    f64::from(u32::try_from(index).unwrap_or(u32::MAX))
}

/// The change at the slider's position `at`, of `total`.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the position is clamped to the frames' indices first"
)]
fn index_of(at: f64, total: usize) -> usize {
    let last = total.saturating_sub(1);
    (at.round().clamp(0.0, place_of(last)) as usize).min(last)
}

/// The place's content and its header's commands.
pub(crate) fn view(cx: &ViewCx<Filter>, nav: &Nav) -> (Element, Vec<Element>) {
    let read = Provider::of(cx).follow::<wire::Replay>(wire::REPLAY, None::<&()>);
    let answer = read.answer();
    let frames = move || answer.get().ready().map_or(0, |replay| replay.frames.len());
    let position = nav.replay_at.clone();
    let playing = nav.replay_playing.clone();

    let step = |forward: bool| {
        let (position, playing, frames) = (position.clone(), playing.clone(), frames.clone());
        move || {
            playing.set(false);
            let last = place_of(frames().saturating_sub(1));
            position.update(|at| {
                *at = if forward {
                    (*at + 1.0).min(last)
                } else {
                    (*at - 1.0).max(0.0)
                };
            });
        }
    };
    let back =
        Button::transport("activity-replay-back", TransportControl::Previous).on_view(step(false));
    let forward =
        Button::transport("activity-replay-forward", TransportControl::Next).on_view(step(true));
    let toggled = (position.clone(), playing.clone(), frames.clone());
    // The control shows what it does now: play while paused, pause while
    // playing.
    let play = cx.switch(&playing, move |_, now| {
        let toggled = toggled.clone();
        Box::new(
            Button::transport(
                "activity-replay-play",
                TransportControl::PlayPause { playing: *now },
            )
            .on_view(move || {
                let (position, playing, frames) = &toggled;
                if !playing.get() && position.get() + 1.0 >= place_of(frames()) {
                    position.set(0.0);
                }
                playing.update(|on| *on = !*on);
            }),
        )
    });

    let shown = nav.clone();
    let content = cx.switch(&read.answer(), move |cx, answer| match answer {
        Answer::Ready(replay) => frames_view(cx, &shown, replay),
        unread => unanswered(unread, "activity-replay-unread").unwrap_or_else(|| page(Vec::new())),
    });
    let (paced, last) = (position, frames);
    let content = Pacer::new(content, &playing, BEAT, move || {
        let next = paced.get() + 1.0;
        let end = place_of(last().saturating_sub(1));
        paced.set(next.min(end));
        next < end
    });
    (
        Box::new(content),
        vec![Box::new(back), play, Box::new(forward)],
    )
}

fn frames_view(cx: &ViewCx<Filter>, nav: &Nav, replay: &wire::Replay) -> Element {
    if replay.frames.is_empty() {
        let (title, message) = if replay.pending {
            ("activity-replay-pending", None)
        } else if replay.unavailable > 0 {
            ("activity-replay-unavailable", replay.note.clone())
        } else {
            ("activity-replay-empty", None)
        };
        let mut state = EmptyState::new(tr(title))
            .with_icon(ui::icons::Icon::Film)
            .with_id("activity-replay-empty");
        if let Some(message) = message {
            state = state.with_message(message);
        }
        return Box::new(state);
    }
    let total = replay.frames.len();
    let slider = Slider::from_state(&nav.replay_at)
        .range(0.0, place_of(total.saturating_sub(1)))
        .step(1.0)
        .with_id("activity-replay-position")
        .with_accessible_name(&tr("activity-replay-scrub"));
    let frames = replay.frames.clone();
    let note = replay.note.clone();
    let frame = cx.switch(&nav.replay_at, move |_, at| {
        let index = index_of(*at, frames.len());
        frames.get(index).map_or_else(
            || page(Vec::new()),
            |frame| frame_view(frame, index, frames.len(), note.as_deref()),
        )
    });
    Box::new(
        Column::new()
            .with_rhythm(ui::tokens::Rhythm::Group)
            .with_fill_remaining_height()
            .push(Box::new(slider))
            .push(frame),
    )
}

/// A workspace's windows as they stood side by side, each as wide as its
/// share of the strip: the window's tab in front names it, and the others
/// it holds follow.
fn arrangement(workspace: &wire::ReplayWorkspace) -> Element {
    let mut row = Row::new()
        .no_wrap()
        .with_rhythm(ui::tokens::Rhythm::Related);
    for window in &workspace.groups {
        let share = u16::try_from(window.width_permille.clamp(1, 1000)).unwrap_or(1000);
        let title = window
            .tabs
            .get(window.active)
            .cloned()
            .unwrap_or_else(|| tr("activity-replay-empty-window"));
        let others: Vec<&str> = window
            .tabs
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != window.active)
            .map(|(_, tab)| tab.as_str())
            .collect();
        let body: Element = if others.is_empty() {
            Box::new(Text::new(&tr("activity-replay-one-tab")).dimmed())
        } else {
            Box::new(Text::new(&others.join(", ")).dimmed())
        };
        let name = trf(
            "activity-replay-window",
            &[
                ("window", title.clone().into()),
                ("share", share.div_ceil(10).into()),
            ],
        );
        row = row.push(Box::new(
            Container::new()
                .with_no_padding()
                .with_share(f32::from(share))
                .with_child(Box::new(
                    Card::new(body).with_title(title).with_accessible_name(name),
                )),
        ));
    }
    Box::new(row)
}

fn frame_view(frame: &wire::Frame, index: usize, total: usize, note: Option<&str>) -> Element {
    let position = trf(
        "activity-replay-position",
        &[
            ("position", index.saturating_add(1).into()),
            ("total", total.into()),
            ("when", ago(frame.at_ms).into()),
        ],
    );
    let mut parts: Vec<Element> = vec![Box::new(Section::titled(
        position,
        vec![Box::new(
            ListRow::new("activity-replay-change", frame.summary.clone())
                .subtitle(frame.author.name.clone())
                .avatar(frame.author.name.clone(), frame.author.identity.clone()),
        )],
    ))];
    if frame.workspaces.is_empty() {
        parts.push(Box::new(
            EmptyState::new(tr("activity-replay-no-workspace")).with_id("activity-replay-gone"),
        ));
    }
    for workspace in &frame.workspaces {
        parts.push(Box::new(Section::titled(
            workspace.name.clone(),
            vec![arrangement(workspace)],
        )));
    }
    if !frame.pointers.is_empty() {
        let rows = frame
            .pointers
            .iter()
            .enumerate()
            .map(|(index, pointer)| {
                Box::new(
                    ListRow::new(
                        format!("activity-replay-present-{index}"),
                        pointer.name.clone(),
                    )
                    .avatar(pointer.name.clone(), pointer.name.clone()),
                ) as Element
            })
            .collect();
        parts.push(Box::new(Section::titled(
            tr("activity-presence-trail"),
            rows,
        )));
    }
    if let Some(note) = note {
        parts.push(Box::new(Text::new(note).dimmed()));
    }
    page(parts)
}
