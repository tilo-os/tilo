//! What is playing, as actions.
//!
//! The now-playing card's transport is reachable from the palette, the
//! control socket, an agent or a screen reader, not only by clicking the
//! card: play/pause, next, previous and seek for each running player. Moving
//! playback to another output is the continuity provider's `audio.route.*`
//! and `media.handoff.*` actions, which the card's output picker also uses.
//!
//! Ids: `play-pause:<player>`, `next:<player>`, `previous:<player>` and
//! `seek:<player>`, where `<player>` is an opaque digest of the player's bus
//! name. Each is re-resolved against the live player list when invoked.

use std::rc::Rc;
use std::time::Instant;

use crate::shell::chrome_state::Chrome;
use crate::shell::i18n::{t, t_args};
use crate::shell::now_playing::{
    NowPlayingSource, NowPlayingView, PlaybackStatus, PlayerCommand, PlayerSnapshot,
};
use crate::shell::search::action_api::{ActionError, ActionParam, ParamType};
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};

pub const PROVIDER_ID: &str = "media.now-playing";

pub struct NowPlayingProvider {
    source: Rc<dyn NowPlayingSource>,
}

impl NowPlayingProvider {
    pub fn new(source: Rc<dyn NowPlayingSource>) -> Self {
        Self { source }
    }
}

/// The opaque name of one player in action ids.
fn player_token(bus_name: &str) -> String {
    let digest = blake3::derive_key("so.tilo.media.now-playing-player.v1", bus_name.as_bytes());
    hex::encode(digest.get(..12).unwrap_or(&digest))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verb {
    PlayPause,
    Next,
    Previous,
    Seek,
}

impl Verb {
    const ALL: [Self; 4] = [Self::PlayPause, Self::Next, Self::Previous, Self::Seek];

    fn prefix(self) -> &'static str {
        match self {
            Self::PlayPause => "play-pause",
            Self::Next => "next",
            Self::Previous => "previous",
            Self::Seek => "seek",
        }
    }

    fn available(self, player: &PlayerSnapshot) -> bool {
        match self {
            Self::PlayPause => player.can_toggle(),
            Self::Next => player.can_go_next,
            Self::Previous => player.can_go_previous,
            Self::Seek => player.seekable(),
        }
    }

    fn title(self, player: &PlayerSnapshot) -> String {
        let key = match self {
            Self::PlayPause if player.status == PlaybackStatus::Playing => "search-media-pause",
            Self::PlayPause => "search-media-play",
            Self::Next => "search-media-next",
            Self::Previous => "search-media-previous",
            Self::Seek => "search-media-seek",
        };
        t_args(key, &[("player", player.identity.as_str().into())])
    }
}

fn id(verb: Verb, player: &PlayerSnapshot) -> String {
    format!("{}:{}", verb.prefix(), player_token(&player.bus_name))
}

fn matches(needle: &str, haystacks: &[&str]) -> Option<i32> {
    if needle.is_empty() {
        return Some(0);
    }
    haystacks
        .iter()
        .any(|text| text.to_lowercase().contains(needle))
        .then_some(60)
}

/// The command `verb` sends to the player with `token`, resolved against the
/// players running now. `position_s` is the seek target in seconds.
fn resolve(
    source: &dyn NowPlayingSource,
    verb: Verb,
    token: &str,
    position_s: Option<f64>,
) -> Result<(String, PlayerCommand), ActionError> {
    let players = source.players();
    let player = players
        .iter()
        .find(|player| player_token(&player.bus_name) == token)
        .ok_or_else(|| ActionError::Refused("that player is no longer running".into()))?;
    if !verb.available(player) {
        return Err(ActionError::Refused(
            "the player does not offer that control now".into(),
        ));
    }
    let command = match verb {
        Verb::PlayPause => PlayerCommand::PlayPause,
        Verb::Next => PlayerCommand::Next,
        Verb::Previous => PlayerCommand::Previous,
        Verb::Seek => {
            let seconds = position_s
                .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
                .ok_or_else(|| {
                    ActionError::Refused("position_s must be a time in seconds".into())
                })?;
            let view = NowPlayingView::new(vec![player.clone()], None, Vec::new(), Instant::now())
                .ok_or_else(|| ActionError::Refused("that player is no longer running".into()))?;
            #[allow(
                clippy::cast_precision_loss,
                reason = "track lengths are far below f64's exact-integer range"
            )]
            let length_s = player.length_us.unwrap_or(0) as f64 / 1_000_000.0;
            view.seek_command(seconds / length_s.max(f64::EPSILON))
                .ok_or_else(|| ActionError::Refused("the player cannot seek".into()))?
        }
    };
    Ok((player.bus_name.clone(), command))
}

impl SearchProvider for NowPlayingProvider {
    fn id(&self) -> &str {
        PROVIDER_ID
    }

    fn name(&self) -> &'static str {
        "Now playing"
    }

    fn display_name(&self) -> String {
        t("search-provider-now-playing")
    }

    fn resource_kinds(&self) -> &[ResourceKind] {
        &[ResourceKind::Action]
    }

    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        if !ctx.wanted.contains(&ResourceKind::Action) {
            return Vec::new();
        }
        let needle = ctx.needle.trim().to_lowercase();
        let mut out = Vec::new();
        for player in self.source.players() {
            for verb in Verb::ALL {
                if !verb.available(&player) {
                    continue;
                }
                let action_id = id(verb, &player);
                let title = verb.title(&player);
                let Some(score) = matches(
                    &needle,
                    &[&title, player.headline(), &action_id, "media", "music"],
                ) else {
                    continue;
                };
                let source = Rc::clone(&self.source);
                let token = player_token(&player.bus_name);
                out.push(SearchResult {
                    kind: ResourceKind::Action,
                    operation_kind: tilo_actions::OperationKind::Effect,
                    provider_id: PROVIDER_ID.into(),
                    id: action_id,
                    title,
                    subtitle: Some(player.headline().to_string()),
                    badge: Some(t("search-badge-media")),
                    score,
                    params: if verb == Verb::Seek {
                        vec![ActionParam::required(
                            "position_s",
                            ParamType::Number,
                            "Where to move the playhead, in seconds from the start",
                        )]
                    } else {
                        Vec::new()
                    },
                    contract: Some(tilo_actions::ActionContract {
                        tags: vec!["desktop".into(), "media".into()],
                        ..tilo_actions::ActionContract::default()
                    }),
                    on_open: Rc::new(move |_doc, _root, _chrome: &Chrome, args| {
                        let (bus_name, command) =
                            resolve(source.as_ref(), verb, &token, args.number("position_s"))?;
                        if source.send(&bus_name, command) {
                            Ok(())
                        } else {
                            Err(ActionError::Refused(
                                "the command could not reach the player".into(),
                            ))
                        }
                    }),
                    on_invoke: None,
                    on_begin_invoke: None,
                });
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::now_playing::tests::{FakeSource, player};

    #[test]
    fn transport_resolves_to_exactly_the_named_player() {
        let source = FakeSource {
            players: vec![
                player("music", "Music", PlaybackStatus::Playing),
                player("video", "Video", PlaybackStatus::Paused),
            ],
            ..FakeSource::default()
        };
        let token = player_token("org.mpris.MediaPlayer2.video");
        assert!(!token.contains("video"), "ids never carry the bus name");
        assert_eq!(
            resolve(&source, Verb::Next, &token, None),
            Ok(("org.mpris.MediaPlayer2.video".into(), PlayerCommand::Next))
        );
        assert_eq!(
            resolve(&source, Verb::Seek, &token, Some(100.0)),
            Ok((
                "org.mpris.MediaPlayer2.video".into(),
                PlayerCommand::SetPosition {
                    track_id: "/org/tilo/track/1".into(),
                    position_us: 100_000_000,
                }
            ))
        );
        assert!(resolve(&source, Verb::Seek, &token, None).is_err());
        assert!(resolve(&source, Verb::Next, "gone", None).is_err());
    }

    #[test]
    fn unavailable_controls_are_refused() {
        let mut radio = player("radio", "Radio", PlaybackStatus::Playing);
        radio.can_go_next = false;
        radio.can_seek = false;
        let source = FakeSource {
            players: vec![radio],
            ..FakeSource::default()
        };
        let token = player_token("org.mpris.MediaPlayer2.radio");
        assert!(resolve(&source, Verb::Next, &token, None).is_err());
        assert!(resolve(&source, Verb::Seek, &token, Some(1.0)).is_err());
        assert!(resolve(&source, Verb::PlayPause, &token, None).is_ok());
    }
}
