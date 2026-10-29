//! Idle dimming and blanking, and outputs turned off and on.
//!
//! [`crate::idle::SessionManager`] decides; this applies its decisions:
//! the dim lowers the session's backlight or darkens the scene
//! ([`crate::dim`]), a blank switches every panel off through
//! [`crate::output_power`], and locking or suspending is handed to the
//! session ([`crate::Compositor::take_session_events`]), which starts its
//! lock client or asks logind.

use std::time::Instant;

use smithay::output::Output;
use smithay::utils::Point;

use crate::dim::ScreenDimmer;
use crate::idle::{Holds, IdleConfig, SessionManager, SessionTransition};
use crate::state::State;

/// The idle state machine, the dim, and what the session has to act on.
pub(crate) struct Idle {
    pub(crate) session: SessionManager,
    pub(crate) dimmer: ScreenDimmer,
    /// Lock and suspend transitions for the session, oldest first.
    pub(crate) events: Vec<SessionTransition>,
    /// Something outside the compositor holds the machine awake.
    pub(crate) suspend_held: bool,
}

impl Idle {
    pub(crate) fn new() -> Self {
        Self {
            session: SessionManager::new(IdleConfig::default(), Instant::now()),
            dimmer: ScreenDimmer::default(),
            events: Vec::new(),
            suspend_held: false,
        }
    }
}

impl State {
    /// The person touched something: wake and undim the displays.
    pub(crate) fn note_input(&mut self) {
        let transitions = self.idle.session.note_activity(Instant::now());
        self.apply_idle(transitions);
    }

    /// Advance the idle timers; an idle-inhibit surface or a suspend hold
    /// keeps them from firing.
    pub(crate) fn tick_idle(&mut self) {
        let holds = Holds {
            idle: self.idle_inhibited(),
            suspend: self.idle.suspend_held,
        };
        let transitions = self.idle.session.tick_inhibited(Instant::now(), holds);
        self.apply_idle(transitions);
    }

    /// Apply what the idle state machine decided.
    pub(crate) fn apply_idle(&mut self, transitions: Vec<SessionTransition>) {
        if transitions.is_empty() {
            return;
        }
        for transition in transitions {
            match transition {
                SessionTransition::Dimmed => self.idle.dimmer.dim(),
                SessionTransition::Undimmed => self.idle.dimmer.restore(),
                SessionTransition::Blanked => self.output_power.set_blanked(true),
                SessionTransition::Woke => self.output_power.set_blanked(false),
                SessionTransition::Locked
                | SessionTransition::Unlocked
                | SessionTransition::Suspend => self.idle.events.push(transition),
            }
        }
        self.idle
            .dimmer
            .keep_session_truthful(&mut self.idle.session);
    }

    /// A lock client locked the session: idle locking has nothing left to
    /// do, and the lock surface shows at full brightness.
    pub(crate) fn lock_shown(&mut self) {
        let transitions = self.idle.session.request_lock(Instant::now());
        let display: Vec<SessionTransition> = transitions
            .into_iter()
            .filter(|transition| *transition != SessionTransition::Locked)
            .collect();
        self.apply_idle(display);
    }

    /// The lock client unlocked the session after verifying the person.
    pub(crate) fn lock_lifted(&mut self) {
        let transitions = self.idle.session.request_unlock(Instant::now());
        let display: Vec<SessionTransition> = transitions
            .into_iter()
            .filter(|transition| *transition != SessionTransition::Unlocked)
            .collect();
        self.apply_idle(display);
    }

    /// Every known output's name and whether a configuration keeps it on.
    pub(crate) fn output_states(&self) -> Vec<(String, bool)> {
        self.space
            .outputs()
            .map(|output| (output.name(), true))
            .chain(
                self.disabled_outputs
                    .iter()
                    .map(|output| (output.name(), false)),
            )
            .collect()
    }

    /// Turn `name` off: clients no longer see it and windows leave it.
    pub(crate) fn disable_output(&mut self, name: &str) {
        let Some(output) = self
            .space
            .outputs()
            .find(|output| output.name() == name)
            .cloned()
        else {
            return;
        };
        self.space.unmap_output(&output);
        crate::output::withdraw(&output, &self.display);
        self.disabled_outputs.push(output);
    }

    /// Turn `name` on again, at its configured place.
    pub(crate) fn enable_output(&mut self, name: &str) {
        let Some(index) = self
            .disabled_outputs
            .iter()
            .position(|output| output.name() == name)
        else {
            return;
        };
        let output: Output = self.disabled_outputs.remove(index);
        crate::output::advertise(&output, &self.display);
        let location: Point<i32, smithay::utils::Logical> = output.current_location();
        self.space.map_output(&output, location);
    }

    /// The overlay alpha the scene is drawn under while dimmed without a
    /// backlight.
    pub(crate) fn dim_alpha(&self) -> Option<f32> {
        self.idle.dimmer.overlay_alpha()
    }
}
