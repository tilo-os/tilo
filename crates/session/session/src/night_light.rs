//! The night light: how warm the displays go, and when.
//! The person chooses a warmth (`display.night_light_temperature`) and either the switch Quick
//! Settings shows or the Appearance day/night schedule: on from sunset, off from sunrise, in the
//! times Settings says and at the location the system time zone names. The compositor takes the
//! warmth as a gamma ramp on every output; this module decides what it shows now, and writes the
//! person's own choices back to their settings.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Map, Value};
use tilo_lock::MutexRecover as _;

use crate::Session;
use tilo_compositor::Error;

/// The warmth the displays take when the person chose none, in kelvin.
pub const DEFAULT_TEMPERATURE: u32 = 3_700;

/// The warmest and coolest the displays go, in kelvin: what the compositor's gamma ramp
/// interpolates between.
pub const RANGE: std::ops::RangeInclusive<u32> = 2_000..=6_500;

/// How often a scheduled night light looks at the clock. Sunrise and sunset move with the date and
/// the wall clock can be set right at any moment, so the phase is read rather than slept to; a
/// minute's lag at a boundary is not something a person watching the screen can see.
const SCHEDULE_LOOK: Duration = Duration::from_secs(60);

/// The night light as the person's settings leave it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Preference {
    /// The manual switch, which Quick Settings' tile turns.
    pub on: bool,
    /// The correlated colour temperature the displays warm to, in kelvin.
    pub temperature: u32,
    /// Whether it follows the Appearance day/night schedule instead: on from sunset, off from
    /// sunrise.
    pub scheduled: bool,
}

impl Default for Preference {
    fn default() -> Self {
        Self {
            on: false,
            temperature: DEFAULT_TEMPERATURE,
            scheduled: false,
        }
    }
}

impl Preference {
    /// The person's choice as stored. A warmth outside [`RANGE`] reads as the nearest the displays
    /// can show, so a value written by another tool never leaves them at a colour this session
    /// cannot take back.
    #[must_use]
    pub fn load() -> Self {
        Self::from_document(&settings_store::load())
    }

    /// The choice `document` holds.
    #[must_use]
    pub fn from_document(document: &Map<String, Value>) -> Self {
        Self {
            on: settings_store::hardware::NIGHT_LIGHT.from_document(document),
            temperature: settings_store::hardware::NIGHT_LIGHT_TEMPERATURE
                .from_document(document)
                .clamp(*RANGE.start(), *RANGE.end()),
            scheduled: settings_store::hardware::NIGHT_LIGHT_SCHEDULE.from_document(document),
        }
    }

    /// Whether it shows at `night` , the day/night phase: the schedule's phase when it follows one,
    /// the switch otherwise.
    #[must_use]
    pub const fn shows(&self, night: bool) -> bool {
        if self.scheduled { night } else { self.on }
    }

    /// What the displays warm to at `night` , in kelvin, if anything.
    #[must_use]
    pub fn kelvin(&self, night: bool) -> Option<u32> {
        self.shows(night).then_some(self.temperature)
    }
}

/// What the settings watch fills while the session runs: the choice, and the schedule it follows,
/// both resolved when the settings last changed.
#[derive(Clone, Default)]
pub struct Followed(Arc<Mutex<Follow>>);

#[derive(Default)]
struct Follow {
    preference: Preference,
    schedule: Option<settings_store::day_schedule::Scheduled>,
}

impl Followed {
    /// Take `document` , the settings as they now are.
    pub fn read(&self, document: &Map<String, Value>) {
        let preference = Preference::from_document(document);
        let schedule = preference
            .scheduled
            .then(|| settings_store::day_schedule::Scheduled::from_settings(document));
        let mut follow = self.0.lock_recover();
        follow.preference = preference;
        follow.schedule = schedule;
    }
}

/// The session's night light: the person's choice, what shows now, and what the displays were last
/// told.
pub(crate) struct NightLight {
    followed: Followed,
    /// Whether the person's choices are kept in their settings: a test or a screenshot session
    /// keeps nothing.
    kept: bool,
    /// Whether the displays were told anything yet.
    applied: bool,
    /// The warmth the displays were last told, so a pass that changes nothing writes no ramp.
    shown: Option<u32>,
}

impl NightLight {
    /// The night light of a session whose settings hold `preference` .
    pub(crate) fn new(preference: Preference, kept: bool) -> Self {
        let followed = Followed::default();
        followed.0.lock_recover().preference = preference;
        if preference.scheduled {
            self::retain_schedule(&followed);
        }
        Self {
            followed,
            kept,
            applied: false,
            shown: None,
        }
    }

    /// The night light of a session whose settings hold `preference` , and the handle the settings
    /// watch fills as they change.
    pub(crate) fn watched(preference: Preference, kept: bool) -> (Self, Followed) {
        let night_light = Self::new(preference, kept);
        let followed = night_light.followed();
        (night_light, followed)
    }

    /// The handle the settings watch fills as the person's settings change.
    fn followed(&self) -> Followed {
        self.followed.clone()
    }

    /// Whether the night light shows now: what the controls' reading and the Quick Settings tile
    /// say.
    pub(crate) fn showing(&self) -> bool {
        self.preference().shows(self.night_now())
    }

    /// What the displays warm to now, in kelvin, if anything.
    pub(crate) fn kelvin(&self) -> Option<u32> {
        self.preference().kelvin(self.night_now())
    }

    /// Whether the displays show what the night light shows now: whether this pass has a ramp to
    /// write.
    fn unapplied(&self) -> bool {
        !self.applied || self.shown != self.kelvin()
    }

    /// Warm the displays to what shows now. A pass that changes nothing leaves the compositor's
    /// ramp alone.
    pub(crate) fn follow(
        &mut self,
        compositor: &mut tilo_compositor::Compositor,
    ) -> Result<(), Error> {
        if !self.unapplied() {
            return Ok(());
        }
        let kelvin = self.kelvin();
        compositor.set_night_light(kelvin)?;
        self.applied = true;
        self.shown = kelvin;
        Ok(())
    }

    /// The person asked for it on (`Some(true)`), off (`Some(false)`), or switched (`None`): what
    /// the switch becomes. An explicit choice leaves the schedule, since they asked for this state
    /// now rather than for the sun's, and both are kept in their settings.
    pub(crate) fn toggle(&self, on: Option<bool>) -> bool {
        let night = self.night_now();
        let mut follow = self.followed.0.lock_recover();
        let on = on.unwrap_or_else(|| !follow.preference.shows(night));
        follow.preference.on = on;
        follow.preference.scheduled = false;
        follow.schedule = None;
        drop(follow);
        if self.kept {
            if let Err(error) = settings_store::hardware::NIGHT_LIGHT.set(on) {
                tracing::warn!(%error, "the night light setting was not kept");
            }
            if let Err(error) = settings_store::hardware::NIGHT_LIGHT_SCHEDULE.set(false) {
                tracing::warn!(%error, "the night light schedule setting was not kept");
            }
        }
        on
    }

    /// How long until what shows can change by itself, so the loop wakes for a schedule rather than
    /// waiting for someone to touch something.
    pub(crate) fn wait(&self) -> Option<Duration> {
        self.followed
            .0
            .lock_recover()
            .schedule
            .is_some()
            .then_some(SCHEDULE_LOOK)
    }

    fn preference(&self) -> Preference {
        self.followed.0.lock_recover().preference
    }

    /// Whether it is night where this machine is, as the schedule says.
    fn night_now(&self) -> bool {
        let follow = self.followed.0.lock_recover();
        follow
            .schedule
            .is_some_and(|schedule| schedule.is_night(ui::theme::local_moment()))
    }
}

impl Session {
    /// The person asked for the night light on, off, or switched: the displays take it now and the
    /// switch says what it became.
    pub(crate) fn set_night_light(&mut self, on: Option<bool>) -> Result<(), Error> {
        let on = self.night_light.toggle(on);
        self.night_light.follow(&mut self.compositor)?;
        self.hint(tilo_desktop::osd::OsdKind::NightLight, on);
        Ok(())
    }
}

/// A session that starts with the schedule on follows it: the schedule is resolved from the
/// settings it started with.
fn retain_schedule(followed: &Followed) {
    let document = settings_store::load();
    let mut follow = followed.0.lock_recover();
    follow.schedule = Some(settings_store::day_schedule::Scheduled::from_settings(
        &document,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_person_chooses_the_warmth_and_the_schedule_decides_when() {
        let preference = Preference {
            on: true,
            temperature: 2_400,
            scheduled: false,
        };
        assert_eq!(preference.kelvin(false), Some(2_400));
        assert_eq!(
            preference.kelvin(true),
            Some(2_400),
            "the switch shows in the day too"
        );

        let scheduled = Preference {
            scheduled: true,
            ..preference
        };
        assert_eq!(scheduled.kelvin(false), None, "no warmth by day");
        assert_eq!(
            scheduled.kelvin(true),
            Some(2_400),
            "the person's warmth by night"
        );

        // A warmth another tool wrote outside what the displays show reads as the nearest they can.
        let document = Map::from_iter([(
            "display_person".to_owned(),
            serde_json::json!({ "night_light": true, "night_light_temperature": 9_000 }),
        )]);
        assert_eq!(Preference::from_document(&document).temperature, 6_500);
    }

    #[test]
    fn the_schedule_follows_the_appearance_day_and_night() {
        let document = Map::from_iter([(
            "display_person".to_owned(),
            serde_json::json!({ "night_light": true, "night_light_schedule": true }),
        )]);
        let preference = Preference::from_document(&document);
        assert!(preference.scheduled);
        assert_eq!(preference.kelvin(false), None);
        assert_eq!(preference.kelvin(true), Some(DEFAULT_TEMPERATURE));
    }
}
