//! The appearance the shell gives every window: the temperature of the
//! workspace it is shown in and the colour of the person at this machine.
//!
//! The shell paints its own chrome in the active workspace's appearance and
//! sends each app window the appearance of the workspace that window is in
//! (`ui::floating_surface::FloatingSurfaceHostMessage::Appearance`), so an
//! app tile takes the same temperature as the room around it and the same
//! focus ring, caret and selection colour as the shell.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use ui::WindowAppearance;
use ui::tokens::PresenceHue;

/// How often every workspace's temperature is looked at again while the
/// active workspace stays put, so a new wallpaper retints its room.
pub const RECHECK: Duration = Duration::from_secs(3);

/// Every workspace's temperature as last looked at, and the person's
/// colour. [`Self::revision`] moves whenever either changes.
#[derive(Debug)]
pub struct WindowAppearances {
    temperatures: HashMap<String, Option<f32>>,
    active: Option<String>,
    your_colour: PresenceHue,
    checked: Option<Instant>,
    revision: u64,
}

impl WindowAppearances {
    #[must_use]
    pub fn new(your_colour: PresenceHue) -> Self {
        Self {
            temperatures: HashMap::new(),
            active: None,
            your_colour,
            checked: None,
            revision: 0,
        }
    }

    /// Moves whenever a workspace's temperature or the person's colour
    /// changes, so a window's appearance only needs asking again then.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// The person's colour changed (a choice in Settings, or the identity it
    /// derives from). Returns whether it differs from before.
    pub fn set_your_colour(&mut self, hue: PresenceHue) -> bool {
        if self.your_colour == hue {
            return false;
        }
        self.your_colour = hue;
        self.revision = self.revision.wrapping_add(1);
        true
    }

    /// Whether the temperatures are due a look: the active workspace
    /// changed, or [`RECHECK`] passed since the last look.
    #[must_use]
    pub fn due(&self, active: Option<&str>, now: Instant) -> bool {
        self.active.as_deref() != active
            || self
                .checked
                .is_none_or(|at| now.saturating_duration_since(at) >= RECHECK)
    }

    /// Look at the temperature of every workspace in `workspaces`, forget
    /// the ones that are gone and note which is `active`. Returns whether
    /// the active workspace's appearance changed.
    pub fn update(
        &mut self,
        workspaces: &[String],
        active: Option<&str>,
        now: Instant,
        temperature_of: &mut dyn FnMut(&str) -> Option<f32>,
    ) -> bool {
        let before = self.active();
        let mut moved = false;
        for id in workspaces {
            let hue = temperature_of(id);
            let same = self
                .temperatures
                .get(id)
                .is_some_and(|known| match (known, hue) {
                    (Some(a), Some(b)) => (a - b).abs() < 0.5,
                    (None, None) => true,
                    _ => false,
                });
            if !same {
                self.temperatures.insert(id.clone(), hue);
                moved = true;
            }
        }
        let present: HashSet<&str> = workspaces.iter().map(String::as_str).collect();
        self.temperatures
            .retain(|id, _| present.contains(id.as_str()));
        self.active = active.map(str::to_owned);
        self.checked = Some(now);
        if moved {
            self.revision = self.revision.wrapping_add(1);
        }
        self.active() != before
    }

    /// The appearance of a window in workspace `workspace`, or in the active
    /// one when the window has not been placed yet.
    #[must_use]
    pub fn in_workspace(&self, workspace: Option<&str>) -> WindowAppearance {
        let workspace = workspace.or(self.active.as_deref());
        WindowAppearance {
            workspace_hue: workspace
                .and_then(|id| self.temperatures.get(id).copied())
                .flatten(),
            your_colour: Some(self.your_colour),
        }
    }

    /// The appearance of the active workspace: what the shell's own chrome
    /// wears.
    #[must_use]
    pub fn active(&self) -> WindowAppearance {
        self.in_workspace(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rooms() -> Vec<String> {
        vec!["warm".to_owned(), "grey".to_owned()]
    }

    fn temperature(id: &str) -> Option<f32> {
        (id == "warm").then_some(40.0)
    }

    #[test]
    fn each_window_takes_its_own_workspace_and_your_colour() {
        let mut windows = WindowAppearances::new(PresenceHue::Teal);
        let now = Instant::now();
        assert!(windows.due(Some("warm"), now), "nothing looked at yet");
        assert!(windows.update(&rooms(), Some("warm"), now, &mut temperature));
        assert_eq!(
            windows.active(),
            WindowAppearance {
                workspace_hue: Some(40.0),
                your_colour: Some(PresenceHue::Teal),
            }
        );
        assert_eq!(
            windows.in_workspace(Some("grey")).workspace_hue,
            None,
            "a grey wallpaper leaves its windows neutral"
        );
        assert_eq!(
            windows.in_workspace(Some("gone")).workspace_hue,
            None,
            "an unknown workspace has no temperature"
        );
        assert!(!windows.due(Some("warm"), now));
        assert!(windows.due(Some("grey"), now), "a switch is looked at now");
        assert!(windows.due(Some("warm"), now + RECHECK));
    }

    #[test]
    fn the_revision_moves_only_when_an_appearance_does() {
        let mut windows = WindowAppearances::new(PresenceHue::Blue);
        let now = Instant::now();
        windows.update(&rooms(), Some("warm"), now, &mut temperature);
        let first = windows.revision();
        assert!(!windows.update(&rooms(), Some("warm"), now, &mut temperature));
        assert_eq!(windows.revision(), first, "the same temperatures again");
        assert!(
            windows.update(&rooms(), Some("grey"), now, &mut temperature),
            "switching rooms changes what the chrome wears"
        );
        assert_eq!(windows.revision(), first, "but no window's own room moved");
        let mut cooler = |id: &str| (id == "warm").then_some(200.0);
        windows.update(&rooms(), Some("grey"), now, &mut cooler);
        assert_ne!(
            windows.revision(),
            first,
            "a new wallpaper retints its room"
        );
        let second = windows.revision();
        assert!(!windows.set_your_colour(PresenceHue::Blue));
        assert!(windows.set_your_colour(PresenceHue::Violet));
        assert_ne!(windows.revision(), second);
        assert_eq!(windows.active().your_colour, Some(PresenceHue::Violet));
    }
}
