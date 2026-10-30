//! The now-playing card on the controls page: the song and the transport
//! that steers it (the toolkit's [`MediaTransport`]), the speaker group it
//! plays on, and, where there is a choice, the player it steers and where
//! its sound goes. Every control is one of the panel's effects, so a person,
//! an agent and a script reach the player through the same door.

use ui::LocalState;
use ui::TextRole;
use ui::framework::ViewCx;
use ui::tokens::Rhythm;
use ui::widget::{
    Column, Container, Dropdown, Element, MediaTransport, Text, TransportActions, TransportFeed,
    TransportState,
};

use crate::effect::QsEffect;
use crate::i18n::t;
use crate::now_playing::NowPlaying;
use crate::resource::QuickSettings;

use crate::host::Readings;

/// What the card reads: the transport's feed, followed on every reading so
/// the playhead moves without building the card again, and the choices
/// around it, which do build it again.
#[derive(Clone)]
pub(crate) struct Parts {
    transport: TransportFeed,
    choices: LocalState<Option<Choices>>,
}

/// What the card offers besides the transport.
#[derive(Clone, Debug, PartialEq)]
struct Choices {
    players: Vec<(String, String)>,
    player: String,
    outputs: Vec<(String, String)>,
    output: String,
    speakers: Option<String>,
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
        Self {
            players,
            player,
            outputs,
            output,
            speakers: playing.speakers.clone(),
        }
    }
}

fn transport_state(playing: &NowPlaying) -> TransportState {
    TransportState {
        title: playing.title.clone(),
        subtitle: playing.subtitle.clone(),
        art: playing.art.clone(),
        art_name: playing.title.clone(),
        playing: playing.playing,
        idle: !playing.can_toggle && !playing.can_next && !playing.can_previous,
        position_ms: playing.position_ms,
        duration_ms: playing.duration_ms,
        ..TransportState::default()
    }
}

impl Parts {
    pub(crate) fn new(readings: &Readings) -> Self {
        let transport = TransportFeed::new();
        let shown = transport.clone();
        readings.watch(move |state| {
            if let Some(playing) = &state.now_playing {
                shown.show(transport_state(playing));
            }
        });
        Self {
            transport,
            choices: readings.project(|state| state.now_playing.as_ref().map(Choices::of)),
        }
    }
}

/// The card, while something plays or is paused; nothing otherwise.
pub(super) fn card(cx: &ViewCx<QuickSettings>, parts: &Parts) -> Element {
    let transport = parts.transport.clone();
    cx.switch(&parts.choices, move |cx, choices| {
        let Some(choices) = choices else {
            return Box::new(Container::new().with_no_padding());
        };
        let actions = cx.actions();
        let controls = TransportActions {
            play_pause: actions.effect(&QsEffect::PlayPause),
            previous: actions.effect(&QsEffect::PreviousTrack),
            next: actions.effect(&QsEffect::NextTrack),
            seek: actions.effect_binding(QsEffect::SEEK),
            ..TransportActions::default()
        };
        let mut card = Column::new()
            .with_intrinsic_height()
            .with_no_padding()
            .with_rhythm(Rhythm::Related)
            .with_id("qs-now-playing")
            .push(Box::new(MediaTransport::new(&transport, controls)));
        if let Some(speakers) = &choices.speakers {
            card = card.push(Box::new(
                Text::new(speakers)
                    .with_role(TextRole::Caption)
                    .with_id("qs-now-playing-speakers"),
            ));
        }
        if choices.players.len() > 1 {
            let mut players =
                Dropdown::from_state(Vec::new(), &LocalState::new(choices.player.clone()))
                    .with_id("qs-now-playing-player")
                    .with_accessible_name(ui::i18n::I18nManager::lookup_plain(
                        "now-playing-choose-player",
                    ))
                    .caption(t("qs-now-playing-player"))
                    .fill();
            players.set_value_options(choices.players.clone());
            card = card.push(Box::new(
                actions.bind_effect(players, QsEffect::CHOOSE_PLAYER),
            ));
        }
        if !choices.outputs.is_empty() {
            let mut outputs =
                Dropdown::from_state(Vec::new(), &LocalState::new(choices.output.clone()))
                    .with_id("qs-now-playing-output")
                    .with_accessible_name(ui::i18n::I18nManager::lookup_plain(
                        "now-playing-choose-output",
                    ))
                    .caption(t("qs-now-playing-output"))
                    .fill();
            outputs.set_value_options(choices.outputs.clone());
            card = card.push(Box::new(
                actions.bind_effect(outputs, QsEffect::CHOOSE_OUTPUT),
            ));
        }
        Box::new(card)
    })
}
