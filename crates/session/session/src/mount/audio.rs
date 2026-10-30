//! The panel's sound, carried between quick settings and the audio service.
//!
//! One process owns the audio graph and publishes it as the `sound`
//! resource: which outputs and microphones there are, which play and
//! record, how loud, and whether they are muted. The session lends the
//! panel what the service reads ([`land`]) and hands the panel's sound
//! effects to the service's actions ([`follow`]), so the panel is a view of
//! the graph and never a second copy of it with a mind of its own.
//!
//! Nothing here decides what a change means: "raising the volume out of
//! silence unmutes it" is the service's rule, and the reading it publishes
//! afterwards is what the panel and the readout show.

use serde_json::{Map, Value};
use tilo_actions::ActionAddress;
use tilo_quick_settings::QsEffect;
use tilo_quick_settings::device::SessionRequest;

use crate::Shared;

/// The sound resource the audio service publishes.
const SOUND: &str = tilo_audio_service::sound::SOUND;

/// Land what the audio service reads of the sound, if it publishes it and
/// this session has a device to show it on. Returns whether the person's
/// panel has that reading now.
pub fn land(shared: &Shared) -> bool {
    let Some(device) = &shared.device else {
        return false;
    };
    let Some(reading) = shared.services.borrow().read(SOUND) else {
        return false;
    };
    match serde_json::from_value::<tilo_audio_service::sound::Sound>(reading) {
        Ok(sound) => {
            device.sound(&sound);
            true
        }
        Err(error) => {
            tracing::warn!(%error, "the sound reading did not parse");
            false
        }
    }
}

/// Carry out a sound effect quick settings asked of the desktop through the
/// audio service. Returns whether this was one; `false` leaves it to the
/// other things the session follows.
pub fn follow(shared: &Shared, request: &SessionRequest) -> bool {
    let SessionRequest::Effect(effect) = request else {
        return false;
    };
    let Some((action, arguments)) = sound_action(effect) else {
        return false;
    };
    let address = ActionAddress::new(SOUND, action);
    match shared.doors.invoke(&address, &arguments) {
        Ok(_) => true,
        Err(error) => {
            // The service reads the graph again after every change, so a
            // refusal is answered on screen by the reading, not by a
            // notification: the panel's controls go back to where they were.
            tracing::debug!(%error, %action, "the sound did not change");
            true
        }
    }
}

/// The action of the `sound` resource that carries out `effect`, with its
/// arguments; `None` for an effect that does not change the sound.
fn sound_action(effect: &QsEffect) -> Option<(&'static str, Map<String, Value>)> {
    let mut arguments = Map::new();
    let action = match effect {
        QsEffect::SetVolume { level } => {
            arguments.insert("level".into(), Value::from(*level));
            "set-volume"
        }
        QsEffect::VolumeUp => "volume-up",
        QsEffect::VolumeDown => "volume-down",
        QsEffect::SetMuted { muted } => {
            arguments.insert("muted".into(), Value::from(*muted));
            "set-muted"
        }
        QsEffect::ToggleMuted => "toggle-muted",
        QsEffect::RouteSound { node } => {
            arguments.insert("node".into(), Value::from(*node));
            "route-output"
        }
        _ => return None,
    };
    Some((action, arguments))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_sound_effect_is_one_of_the_services_actions() {
        let cases = [
            (QsEffect::SetVolume { level: 0.4 }, "set-volume"),
            (QsEffect::VolumeUp, "volume-up"),
            (QsEffect::VolumeDown, "volume-down"),
            (QsEffect::SetMuted { muted: true }, "set-muted"),
            (QsEffect::ToggleMuted, "toggle-muted"),
            (QsEffect::RouteSound { node: 41 }, "route-output"),
        ];
        for (effect, expected) in cases {
            let (action, _) = sound_action(&effect).expect("a sound effect");
            assert_eq!(action, expected, "{effect:?}");
        }
        assert!(
            sound_action(&QsEffect::LockScreen).is_none(),
            "locking the screen is not a sound"
        );
    }

    #[test]
    fn the_arguments_are_what_the_action_takes() {
        let (_, arguments) = sound_action(&QsEffect::SetVolume { level: 0.4 }).unwrap();
        assert_eq!(arguments.get("level"), Some(&Value::from(0.4)));
        let (_, arguments) = sound_action(&QsEffect::RouteSound { node: 41 }).unwrap();
        assert_eq!(arguments.get("node"), Some(&Value::from(41)));
        let (_, arguments) = sound_action(&QsEffect::VolumeUp).unwrap();
        assert!(arguments.is_empty(), "a step takes nothing");
    }
}
