//! The now-playing row on the controls page: the cover, what plays and the
//! controls that steer it, and what another of the person's devices hands
//! over. The song's name is the way back to the player that plays it, and a
//! picker appears only where there is something to pick: another player to
//! steer, or another place to hear it. Every control is one of the panel's
//! effects, so a person, an agent and a script reach the player through the
//! same door.

use ui::LocalState;
use ui::TextRole;
use ui::framework::ViewCx;
use ui::icons::Icon;
use ui::tokens::Rhythm;
use ui::widget::{
    Button, ButtonRole, Column, Container, Dropdown, Element, IconWidget, Image, ImageFit, Row,
    Slider, Text, TransportControl,
};

use crate::NowPlaying;
use crate::effect::QsEffect;
use crate::i18n::{t, t_args};
use crate::resource::QuickSettings;

use crate::readings::Readings;

/// The cover's side, and the radius its corners take: the height of the two
/// lines beside it.
const COVER: f32 = 40.0;
const COVER_RADIUS: f32 = 6.0;

/// What the row reads: the song and the choices around it, which build it
/// again when they change.
#[derive(Clone)]
pub(crate) struct Parts {
    choices: LocalState<Option<Choices>>,
}

/// One press of the row's transport: the way it is painted, and the effect
/// it asks of the device.
#[derive(Clone, Debug, PartialEq)]
struct Step {
    id: &'static str,
    /// What the button is called, in the panel's language.
    name: &'static str,
    face: TransportControl,
    effect: QsEffect,
}

/// What the row offers besides its transport.
#[derive(Clone, Debug, PartialEq)]
struct Choices {
    title: String,
    subtitle: String,
    art: ui::widget::TransportArt,
    /// The transport, in the order it stands: only what the player can do.
    steps: Vec<Step>,
    /// The app that plays it, by id, when the device knows it: the row opens
    /// it.
    app: Option<String>,
    /// The app that plays it, named under the row while there is no player to
    /// choose from.
    source: String,
    players: Vec<(String, String)>,
    player: String,
    outputs: Vec<(String, String)>,
    output: String,
    /// Where the song is, from 0 to 1, while its player says how long it is:
    /// the row's progress, which the person drags to another point.
    progress: Option<f64>,
    /// Where the song stands and what is left of it, beside the bar's ends:
    /// the labels the card's bar carries.
    times: Option<(String, String)>,
    /// The handoff to continue here: its request, and the line that says
    /// what it is.
    offer: Option<(String, String)>,
}

impl Choices {
    fn of(playing: &NowPlaying) -> Self {
        let pick = |items: Vec<(String, String, bool)>| {
            let current = items
                .iter()
                .find(|(_, _, current)| *current)
                .map(|(id, _, _)| id.clone())
                .unwrap_or_default();
            let items = items
                .into_iter()
                .map(|(id, label, _)| (id, label))
                .collect();
            (items, current)
        };
        let (players, player) = pick(
            playing
                .players
                .iter()
                .map(|p| (p.id.clone(), p.name.clone(), p.active))
                .collect(),
        );
        let (outputs, output) = pick(
            playing
                .outputs
                .iter()
                .map(|o| (o.token.clone(), o.label.clone(), o.current))
                .collect(),
        );
        let mut steps = Vec::with_capacity(3);
        if playing.can_previous {
            steps.push(Step {
                id: "qs-now-playing-previous",
                name: "qs-now-playing-previous",
                face: TransportControl::Previous,
                effect: QsEffect::PreviousTrack,
            });
        }
        if playing.can_toggle {
            steps.push(Step {
                id: "qs-now-playing-toggle",
                name: if playing.playing {
                    "qs-now-playing-pause"
                } else {
                    "qs-now-playing-play"
                },
                face: TransportControl::PlayPause {
                    playing: playing.playing,
                },
                effect: QsEffect::PlayPause,
            });
        }
        if playing.can_next {
            steps.push(Step {
                id: "qs-now-playing-next",
                name: "qs-now-playing-next",
                face: TransportControl::Next,
                effect: QsEffect::NextTrack,
            });
        }
        Self {
            title: playing.title.clone(),
            subtitle: playing.subtitle.clone(),
            art: playing.art.clone(),
            steps,
            app: playing.app.clone(),
            source: playing.player.clone(),
            players,
            player,
            outputs,
            output,
            progress: playing.progress(),
            times: playing.times().map(|(elapsed, remaining)| {
                (
                    ui::format::timecode(elapsed),
                    ui::format::timecode(remaining),
                )
            }),
            offer: playing.offer.as_ref().map(|offer| {
                let line = t_args(
                    "qs-now-playing-offer",
                    &[
                        ("device", offer.source_label.clone().into()),
                        ("title", offer.title.clone().into()),
                        ("position", playhead(offer.position_ms).into()),
                    ],
                );
                (offer.request_id.clone(), line)
            }),
        }
    }
}

/// The playhead as the row's copy says it: `1:23`.
fn playhead(position_ms: f64) -> String {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a playhead in a recording is far inside u64's seconds"
    )]
    let seconds = (position_ms.max(0.0) / 1_000.0).round() as u64;
    ui::format::timecode(seconds)
}

impl Parts {
    pub(crate) fn new(readings: &Readings) -> Self {
        Self {
            choices: readings.project(|state| state.now_playing.as_ref().map(Choices::of)),
        }
    }
}

/// The row, while something plays or is paused; nothing otherwise.
pub(super) fn card(cx: &ViewCx<QuickSettings>, parts: &Parts) -> Element {
    cx.switch(&parts.choices, |cx, choices| {
        let Some(choices) = choices else {
            return Box::new(Container::new().with_no_padding());
        };
        // The song and its bar are one block: the bar reads under the cover,
        // the lines and the transport it belongs to, apart from the pickers
        // that follow it.
        let mut media = Column::new()
            .with_intrinsic_height()
            .with_no_padding()
            .with_rhythm(Rhythm::Tight)
            .push(Box::new(song(cx, choices)));
        if let (Some(progress), Some(times)) = (choices.progress, &choices.times) {
            media = media.push(seek(cx, progress, times));
        }
        let mut card = Column::new()
            .with_intrinsic_height()
            .with_no_padding()
            .with_rhythm(Rhythm::Related)
            .push(media.into_widget());
        for below in choices_below(cx, choices) {
            card = card.push(below);
        }
        Box::new(card)
    })
}

/// The song's row: its cover, what plays, and the controls that steer it.
fn song(cx: &ViewCx<QuickSettings>, choices: &Choices) -> Row {
    // What plays, and the app that plays it: the row's lines are the way
    // back to the player itself.
    let mut lines = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(Rhythm::Tight);
    match &choices.app {
        Some(app) => {
            lines = lines.push(Box::new(
                cx.actions().bind_effect_invocation(
                    // The song's own name is the button's name, so the row reads as
                    // the song it steers; the tooltip says where the press goes.
                    Button::new("qs-now-playing-open", &choices.title)
                        .role(ButtonRole::Plain)
                        .with_accessible_name(&choices.title)
                        .tooltip(t_args(
                            "qs-now-playing-open",
                            &[("app", choices.source.clone().into())],
                        )),
                    &QsEffect::OpenPlaying { app: app.clone() },
                ),
            ));
        }
        None => {
            lines = lines.push(Box::new(
                Text::new(&choices.title)
                    .with_id("qs-now-playing-title")
                    .with_role(TextRole::Body),
            ));
        }
    }
    if !choices.subtitle.is_empty() {
        lines = lines.push(Box::new(
            Text::new(&choices.subtitle)
                .with_id("qs-now-playing-artist")
                .with_role(TextRole::Caption)
                .with_spoken(&choices.subtitle),
        ));
    }
    Row::new()
        .no_wrap()
        .with_rhythm(Rhythm::Related)
        .with_align_items(ui::taffy::AlignItems::Center)
        .with_id("qs-now-playing")
        .push(cover(&choices.art))
        .push(Box::new(
            Container::new()
                .with_no_padding()
                .with_flex_grow()
                .with_child(lines.into_widget()),
        ))
        .push(Box::new(transport(cx, &choices.steps)))
}

/// Where the song stands, dragged to another point, between the two times it
/// runs from and to: the panel's own seek effect, so a click, a script and
/// the floating layer's card all go to the player the same way.
fn seek(cx: &ViewCx<QuickSettings>, progress: f64, times: &(String, String)) -> Element {
    let position = LocalState::new(progress);
    let slider = Slider::from_state(&position)
        .with_id("qs-now-playing-seek")
        .with_accessible_name(&t("qs-now-playing-seek"))
        .step(0.01)
        .commits_on_release();
    let slider: Element = Box::new(cx.actions().bind_effect(slider, QsEffect::SEEK));
    Box::new(
        Row::new()
            .no_wrap()
            .with_rhythm(Rhythm::Tight)
            .with_align_items(ui::taffy::AlignItems::Center)
            .with_id("qs-now-playing-seek-row")
            .push(time("qs-now-playing-elapsed", &times.0))
            .push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_flex_grow()
                    .with_child(slider),
            ))
            .push(time("qs-now-playing-remaining", &times.1)),
    )
}

/// A point in the song beside its bar's end, captioned and quiet as the
/// card's own labels are.
fn time(id: &str, label: &str) -> Element {
    Box::new(
        Text::new(label)
            .with_id(id)
            .with_role(TextRole::Caption)
            .dimmed(),
    )
}

/// The transport, painted the way every media surface paints one: a round
/// play/pause and plain skips beside it, each asking its own effect.
fn transport(cx: &ViewCx<QuickSettings>, steps: &[Step]) -> Row {
    let actions = cx.actions();
    let mut controls = Row::new()
        .no_wrap()
        .with_rhythm(Rhythm::Tight)
        .with_align_items(ui::taffy::AlignItems::Center);
    for step in steps {
        let Some(binding) = actions.effect(&step.effect) else {
            continue;
        };
        let name = t(step.name);
        controls = controls.push(Box::new(
            Button::transport(step.id, step.face)
                .with_accessible_name(&name)
                .tooltip(name)
                .on_action(binding),
        ));
    }
    controls
}

/// What the row offers under the song: the handoff to continue here, and the
/// pickers where there is a choice. One player and one place to hear it need
/// no picker: the row itself is the way to both.
fn choices_below(cx: &ViewCx<QuickSettings>, choices: &Choices) -> Vec<Element> {
    let actions = cx.actions();
    let mut below: Vec<Element> = Vec::new();
    if let Some((request, label)) = &choices.offer {
        below.push(Box::new(actions.bind_effect_invocation(
            Button::new("qs-now-playing-resume", label),
            &QsEffect::ResumePlayback {
                request: request.clone(),
            },
        )));
    }
    if choices.players.len() > 1 {
        let mut players =
            Dropdown::from_state(Vec::new(), &LocalState::new(choices.player.clone()))
                .with_id("qs-now-playing-player")
                .with_accessible_name(t("now-playing-choose-player"))
                .caption(t("qs-now-playing-player"))
                .fill();
        players.set_value_options(choices.players.clone());
        below.push(Box::new(
            actions.bind_effect(players, QsEffect::CHOOSE_PLAYER),
        ));
    }
    if choices.outputs.len() > 1 {
        let mut outputs =
            Dropdown::from_state(Vec::new(), &LocalState::new(choices.output.clone()))
                .with_id("qs-now-playing-output")
                .with_accessible_name(t("now-playing-choose-output"))
                .caption(t("qs-now-playing-output"))
                .fill();
        outputs.set_value_options(choices.outputs.clone());
        below.push(Box::new(
            actions.bind_effect(outputs, QsEffect::CHOOSE_OUTPUT),
        ));
    }
    below
}

/// The song's cover at the row's leading edge, or the kind's glyph while it
/// has none: the same picture the card, the pill and the lock show.
fn cover(art: &ui::widget::TransportArt) -> Element {
    let inside: Element = match art {
        ui::widget::TransportArt::Picture(picture) => Box::new(
            Image::from_picture(ui::Pictures::window(), picture.clone())
                .with_fit(ImageFit::Cover)
                .with_corner_radius(COVER_RADIUS)
                .filling(),
        ),
        ui::widget::TransportArt::Image(image) => Box::new(
            Image::from_rgba(image.data.data().to_vec().into(), image.width, image.height)
                .with_fit(ImageFit::Cover)
                .with_corner_radius(COVER_RADIUS)
                .filling(),
        ),
        ui::widget::TransportArt::None | ui::widget::TransportArt::Placeholder => {
            Box::new(IconWidget::new(Icon::Music).dim())
        }
    };
    Box::new(
        Container::new()
            .with_id("qs-now-playing-cover")
            .with_no_padding()
            .with_size(COVER, COVER)
            .with_child(inside),
    )
}
