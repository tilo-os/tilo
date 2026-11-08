//! Window warmth: the temperature of the workspace a window is in and the colour of the person at
//! this machine.
//! The desk paints its own chrome in the active workspace's appearance and sends each app window
//! the appearance of the workspace that window is in
//! ([`ui::floating_surface::FloatingSurfaceHostMessage::Appearance`]), so a window takes the same
//! temperature as the workspace around it and the same focus ring, caret and selection colour as
//! the desk.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use tilo_desktop_model::DesktopTree;
use tilo_desktop_model::ids::WorkspaceSlot;
use ui::Theme;
use ui::WindowAppearance;
use ui::tokens::PresenceHue;

/// How often every workspace's temperature is looked at again while the active workspace stays put,
/// so a new wallpaper retints its workspace.
pub const RECHECK: Duration = Duration::from_secs(3);

/// Every workspace's temperature as last looked at, and the person's colour. [`Self::revision`]
/// moves whenever either changes.
#[derive(Debug)]
pub struct WindowWarmth {
    temperatures: HashMap<WorkspaceSlot, Option<f32>>,
    active: Option<WorkspaceSlot>,
    your_colour: PresenceHue,
    checked: Option<Instant>,
    revision: u64,
}

impl WindowWarmth {
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

    /// Moves whenever a workspace's temperature or the person's colour changes, so a window's
    /// appearance only needs asking again then.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// The person's colour changed (a choice in Settings, or the identity it derives from). Returns
    /// whether it differs from before.
    pub fn set_your_colour(&mut self, hue: PresenceHue) -> bool {
        if self.your_colour == hue {
            return false;
        }
        self.your_colour = hue;
        self.revision = self.revision.wrapping_add(1);
        true
    }

    /// Whether the temperatures are due a look: the active workspace changed, or [`RECHECK`] passed
    /// since the last look.
    #[must_use]
    pub fn due(&self, active: Option<WorkspaceSlot>, now: Instant) -> bool {
        self.active != active
            || self
                .checked
                .is_none_or(|at| now.saturating_duration_since(at) >= RECHECK)
    }

    /// Look at the temperature of every workspace in `workspaces` , forget the ones that are gone
    /// and note which is `active` . Returns whether the active workspace's appearance changed.
    pub fn update(
        &mut self,
        workspaces: &[WorkspaceSlot],
        active: Option<WorkspaceSlot>,
        now: Instant,
        temperature_of: &mut dyn FnMut(WorkspaceSlot) -> Option<f32>,
    ) -> bool {
        let before = self.active();
        let mut moved = false;
        for id in workspaces {
            let hue = temperature_of(*id);
            let same = self
                .temperatures
                .get(id)
                .is_some_and(|known| match (known, hue) {
                    (Some(a), Some(b)) => (a - b).abs() < 0.5,
                    (None, None) => true,
                    _ => false,
                });
            if !same {
                self.temperatures.insert(*id, hue);
                moved = true;
            }
        }
        let present: HashSet<WorkspaceSlot> = workspaces.iter().copied().collect();
        self.temperatures.retain(|id, _| present.contains(id));
        self.active = active;
        self.checked = Some(now);
        if moved {
            self.revision = self.revision.wrapping_add(1);
        }
        self.active() != before
    }

    /// The appearance of a window in workspace `workspace` , or in the active one when the window
    /// has not been placed yet.
    #[must_use]
    pub fn in_workspace(&self, workspace: Option<WorkspaceSlot>) -> WindowAppearance {
        let workspace = workspace.or(self.active);
        WindowAppearance {
            workspace_hue: workspace
                .and_then(|id| self.temperatures.get(&id).copied())
                .flatten(),
            your_colour: Some(self.your_colour),
            // The shape is the window's own, said where its tile is known
            // ([`Self::appearance_of`]).
            corner_radius: None,
        }
    }

    /// The appearance of the active workspace: what the desk's own chrome wears.
    #[must_use]
    pub fn active(&self) -> WindowAppearance {
        self.in_workspace(None)
    }

    /// The appearance of the window showing `app` : the temperature of the workspace that window is
    /// in (the active one while the tree does not hold it yet), the person's colour, and the
    /// window's own corner radius from `theme` .
    #[must_use]
    pub fn appearance_of(&self, tree: &DesktopTree, app: &str, theme: &Theme) -> WindowAppearance {
        let mut appearance = self.in_workspace(
            tree.workspace_holding_app(app)
                .map(|workspace| workspace.slot),
        );
        appearance.corner_radius = Some(theme.tokens.radii.tile);
        appearance
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspaces() -> Vec<WorkspaceSlot> {
        vec![WorkspaceSlot(1), WorkspaceSlot(2)]
    }

    fn temperature(id: WorkspaceSlot) -> Option<f32> {
        (id == WorkspaceSlot(1)).then_some(40.0)
    }

    #[test]
    fn each_window_takes_its_own_workspace_and_your_colour() {
        let mut warmth = WindowWarmth::new(PresenceHue::Teal);
        let now = Instant::now();
        assert!(
            warmth.due(Some(WorkspaceSlot(1)), now),
            "nothing looked at yet"
        );
        assert!(warmth.update(&workspaces(), Some(WorkspaceSlot(1)), now, &mut temperature));
        assert_eq!(
            warmth.active(),
            WindowAppearance {
                workspace_hue: Some(40.0),
                your_colour: Some(PresenceHue::Teal),
                corner_radius: None,
            }
        );
        assert_eq!(
            warmth.in_workspace(Some(WorkspaceSlot(2))).workspace_hue,
            None,
            "a grey wallpaper leaves its windows neutral"
        );
        assert_eq!(
            warmth.in_workspace(Some(WorkspaceSlot(9))).workspace_hue,
            None,
            "an unknown workspace has no temperature"
        );
        assert!(!warmth.due(Some(WorkspaceSlot(1)), now));
        assert!(
            warmth.due(Some(WorkspaceSlot(2)), now),
            "a switch is looked at now"
        );
        assert!(warmth.due(Some(WorkspaceSlot(1)), now + RECHECK));
    }

    #[test]
    fn the_revision_moves_only_when_an_appearance_does() {
        let mut warmth = WindowWarmth::new(PresenceHue::Blue);
        let now = Instant::now();
        warmth.update(&workspaces(), Some(WorkspaceSlot(1)), now, &mut temperature);
        let first = warmth.revision();
        assert!(!warmth.update(&workspaces(), Some(WorkspaceSlot(1)), now, &mut temperature));
        assert_eq!(warmth.revision(), first, "the same temperatures again");
        assert!(
            warmth.update(&workspaces(), Some(WorkspaceSlot(2)), now, &mut temperature),
            "switching workspaces changes what the chrome wears"
        );
        assert_eq!(
            warmth.revision(),
            first,
            "but no window's own workspace moved"
        );
        let mut cooler = |id: WorkspaceSlot| (id == WorkspaceSlot(1)).then_some(200.0);
        warmth.update(&workspaces(), Some(WorkspaceSlot(2)), now, &mut cooler);
        assert_ne!(
            warmth.revision(),
            first,
            "a new wallpaper retints its workspace"
        );
        let second = warmth.revision();
        assert!(!warmth.set_your_colour(PresenceHue::Blue));
        assert!(warmth.set_your_colour(PresenceHue::Violet));
        assert_ne!(warmth.revision(), second);
        assert_eq!(warmth.active().your_colour, Some(PresenceHue::Violet));
    }

    /// A wallpaper whose hue moves less than half a degree is the same wallpaper, so a re-look does
    /// not retint the workspace every 3 s.
    #[test]
    fn a_temperature_within_half_a_degree_is_the_same_one() {
        let mut warmth = WindowWarmth::new(PresenceHue::Teal);
        let now = Instant::now();
        warmth.update(&workspaces(), Some(WorkspaceSlot(1)), now, &mut temperature);
        let first = warmth.revision();
        let mut barely = |id: WorkspaceSlot| (id == WorkspaceSlot(1)).then_some(40.4);
        assert!(!warmth.update(&workspaces(), Some(WorkspaceSlot(1)), now, &mut barely));
        assert_eq!(warmth.revision(), first);
        let mut justover = |id: WorkspaceSlot| (id == WorkspaceSlot(1)).then_some(40.5);
        assert!(warmth.update(&workspaces(), Some(WorkspaceSlot(1)), now, &mut justover));
        assert_ne!(warmth.revision(), first);
    }
}
