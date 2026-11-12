//! The surface the desk shows over its windows, as the person left it: at rest, or one of the
//! shell's overlays (`overview`, `switcher`, `launcher`, `centre`, `workspaces`, `composer`,
//! `quick-settings`, `conversation`).
//! The session keeps it, so the desk's reading says which one is up (`session.desk.here`) and
//! the shell's views re-run only when it changes.
//!
//! A press the shell hands the session changes it: `show-surface` puts one up (or closes it,
//! when it is the one up), `close-surface` takes one down. The overlays themselves are the
//! Rust chrome's while it still draws them (the overview, the workspace menu); the reading
//! names the one up either way, so a shell's views and the chrome agree.

use std::cell::{Cell, RefCell};
use std::time::Duration;
use tilo_clock::time::Instant;

/// How long Alt has to stay down after Tab before the switcher's panel shows: shorter than a
/// deliberate look, longer than a practised Alt+Tab, so the common "back to the last window"
/// flick switches windows and flashes nothing.
pub const REVEAL_DELAY: Duration = Duration::from_millis(120);

/// Which windows a walk of the switcher crosses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SwitcherScope {
    /// Every window of the workspace in front: Alt+Tab.
    #[default]
    Workspace,
    /// The windows of the app the window in front shows: Alt and the grave key.
    App,
}

impl SwitcherScope {
    /// The scope `name` names in the walk's action: `workspace`, or `app` for the one in front.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        Some(match name {
            "workspace" | "all" => Self::Workspace,
            "app" => Self::App,
            _ => return None,
        })
    }

    /// Its name in the walk's action and in the desk's reading.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::App => "app",
        }
    }
}

/// The surface the desk shows over its windows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Overlay {
    /// The desk at rest, with its bar.
    #[default]
    Rest,
    /// Every strip and window of the workspace, to search and move.
    Overview,
    /// The windows of the workspace, as Alt+Tab walks them.
    Switcher,
    /// Search and launching over the workspace.
    Launcher,
    /// The notifications.
    Centre,
    /// The workspaces the person is in.
    Workspaces,
    /// The agent composer.
    Composer,
    /// The device's quick settings: its connections, its sound and light, the appearance,
    /// do not disturb, the battery and power.
    QuickSettings,
    /// The conversation with the session's agent: its latest run, steps and words, and the
    /// one input the person types a program or a sentence into (shell.md §8.1).
    Conversation,
}

impl Overlay {
    /// The surface `name` names, from the desk's reading; `None` for one this build has not.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        Some(match name {
            "" | "none" | "desk" => Self::Rest,
            "overview" => Self::Overview,
            "switcher" => Self::Switcher,
            "launcher" => Self::Launcher,
            "centre" => Self::Centre,
            "workspaces" => Self::Workspaces,
            "composer" => Self::Composer,
            "quick-settings" => Self::QuickSettings,
            "conversation" => Self::Conversation,
            _ => return None,
        })
    }

    /// Its name in the desk's reading, and in the actions that change it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Rest => "none",
            Self::Overview => "overview",
            Self::Switcher => "switcher",
            Self::Launcher => "launcher",
            Self::Centre => "centre",
            Self::Workspaces => "workspaces",
            Self::Composer => "composer",
            Self::QuickSettings => "quick-settings",
            Self::Conversation => "conversation",
        }
    }
}

/// The surface the desk shows, and what the person typed in the shell's search.
#[derive(Debug, Default)]
pub struct Overlays {
    here: Cell<Overlay>,
    /// Moves whenever the surface changes, so the reading is remade.
    revision: Cell<u64>,
    /// What the person typed in the shell's search: the launcher opens where they left it.
    spotlight: RefCell<String>,
    /// How far Alt+Tab has walked from the window in front: the switcher's choice.
    walk: Cell<i64>,
    /// Which windows that walk crosses.
    scope: Cell<SwitcherScope>,
    /// When the walk began, which decides when its panel shows ([`REVEAL_DELAY`]).
    opened: Cell<Option<Instant>>,
}

impl Overlays {
    /// Nothing up: the desk at rest.
    #[must_use]
    pub fn new() -> Self {
        Self {
            here: Cell::new(Overlay::Rest),
            revision: Cell::new(0),
            spotlight: RefCell::new(String::new()),
            walk: Cell::new(0),
            scope: Cell::new(SwitcherScope::default()),
            opened: Cell::new(None),
        }
    }

    /// The walk starts again from the window in front, whichever surface was up.
    pub fn forget_walk(&self) {
        self.walk.set(0);
        self.opened.set(None);
    }

    /// The surface up now, by the name the desk's reading uses.
    #[must_use]
    pub fn here(&self) -> String {
        self.here.get().name().to_owned()
    }

    /// What the person typed in the shell's search.
    #[must_use]
    pub fn spotlight(&self) -> String {
        self.spotlight.borrow().clone()
    }

    /// Show `surface`: the desk's reading moves, and the shell's views re-run. Alt+Tab opens
    /// the switcher on the window before the one in front, as a quick Alt+Tab does.
    pub fn show(&self, surface: Overlay) {
        if self.here.get() == surface {
            return;
        }
        self.here.set(surface);
        if surface == Overlay::Switcher {
            self.opened.set(Some(tilo_clock::now()));
            self.scope.set(SwitcherScope::Workspace);
            self.walk.set(1);
        } else {
            self.opened.set(None);
        }
        self.revision.set(self.revision.get().wrapping_add(1));
    }

    /// Alt+Tab's press: open the switcher in `scope` on the window before the one in front, or
    /// walk it `steps` windows on while it is already up. A press in the other scope starts the
    /// walk again, in that scope, from the window in front.
    pub fn walk(&self, scope: Option<&str>, steps: i64) {
        let scope = scope.and_then(SwitcherScope::named).unwrap_or_default();
        let walking = self.here.get() == Overlay::Switcher && self.scope.get() == scope;
        self.here.set(Overlay::Switcher);
        self.scope.set(scope);
        if walking {
            self.walk.set(self.walk.get().saturating_add(steps));
        } else {
            self.opened.set(Some(tilo_clock::now()));
            self.walk.set(steps);
        }
        self.revision.set(self.revision.get().wrapping_add(1));
    }

    /// Which windows the walk crosses.
    #[must_use]
    pub fn scope(&self) -> SwitcherScope {
        self.scope.get()
    }

    /// Whether the switcher's panel shows: Alt has been held past [`REVEAL_DELAY`] since the
    /// walk began, so a quick flick switched windows without a panel.
    #[must_use]
    pub fn revealed(&self, now: Instant) -> bool {
        self.here.get() == Overlay::Switcher
            && self
                .opened
                .get()
                .is_some_and(|at| now.saturating_duration_since(at) >= REVEAL_DELAY)
    }

    /// When the switcher's panel comes up, while Alt is held on it and it has not yet: the one
    /// instant the session sleeps to, so nothing draws in between.
    #[must_use]
    pub fn deadline(&self, now: Instant) -> Option<Instant> {
        let opened = self.opened.get()?;
        (self.here.get() == Overlay::Switcher && !self.revealed(now))
            .then(|| opened.checked_add(REVEAL_DELAY))
            .flatten()
    }

    /// Whether the switcher's walk is up: Alt went down with Tab and has not come up since. Its
    /// panel need not show yet (see [`Self::revealed`]): a quick flick walks and commits with
    /// nothing drawn, and what holds the keyboard until Alt comes up.
    #[must_use]
    pub fn is_up(&self) -> bool {
        self.here.get() == Overlay::Switcher
    }

    /// Choose the window `at` windows past the one in front, in the scope the walk is crossing:
    /// Home, End and a card under the pointer choose by place instead of crossing the windows
    /// one at a time. It goes to the window at once, as a press on its card does.
    pub fn select(&self, at: i64) {
        if self.here.get() != Overlay::Switcher {
            return;
        }
        self.walk.set(at);
        self.revision.set(self.revision.get().wrapping_add(1));
    }

    /// How far the walk stands from the window in front. The reading turns it into the one
    /// window the switcher has reached, of `count`.
    #[must_use]
    pub fn walked(&self) -> i64 {
        self.walk.get()
    }

    /// Where the walk has reached of `count` windows, `front` being the window in front's
    /// place among them: `None` while the switcher is not up.
    #[must_use]
    pub fn reached(&self, count: usize, front: Option<usize>) -> Option<usize> {
        if count == 0 || !self.is_up() {
            return None;
        }
        let front = i64::try_from(front.unwrap_or(0)).unwrap_or(0);
        let count = i64::try_from(count).unwrap_or(1).max(1);
        let at = front.saturating_add(self.walk.get()).rem_euclid(count);
        usize::try_from(at).ok()
    }

    /// Show the surface `name` names, else close the one up (`show-surface`).
    pub fn toggle(&self, name: &str) {
        let Some(surface) = Overlay::named(name) else {
            return;
        };
        if self.here.get() == surface {
            self.show(Overlay::Rest);
        } else {
            self.show(surface);
        }
    }

    /// Show the surface `name` names (`show-surface`), whether or not it is up.
    pub fn open(&self, name: &str) {
        if let Some(surface) = Overlay::named(name) {
            self.show(surface);
        }
    }

    /// Search from `query`: the launcher opens with what the person asked for, whether it was
    /// already up or not.
    pub fn search(&self, query: &str) {
        query.clone_into(&mut self.spotlight.borrow_mut());
        self.show(Overlay::Launcher);
        self.revision.set(self.revision.get().wrapping_add(1));
    }

    /// Close the surface `name` names, when it is the one up (`close-surface`).
    pub fn close(&self, name: &str) {
        if Overlay::named(name) == Some(self.here.get()) {
            self.show(Overlay::Rest);
        }
    }

    /// A change of the surface, as the reading sees it.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_surface_opens_toggles_and_closes_by_name() {
        let surfaces = Overlays::new();
        assert_eq!(surfaces.here(), "none");
        surfaces.open("overview");
        assert_eq!(surfaces.here(), "overview");
        surfaces.toggle("overview");
        assert_eq!(surfaces.here(), "none", "toggling the one up closes it");
        surfaces.toggle("launcher");
        assert_eq!(surfaces.here(), "launcher");
        surfaces.close("overview");
        assert_eq!(surfaces.here(), "launcher", "another one stays up");
        surfaces.close("launcher");
        assert_eq!(surfaces.here(), "none");
    }

    #[test]
    fn every_surface_the_reading_names_reads_back() {
        for name in [
            "none",
            "desk",
            "overview",
            "switcher",
            "launcher",
            "centre",
            "workspaces",
            "composer",
            "quick-settings",
            "conversation",
        ] {
            let surface = Overlay::named(name).unwrap_or_else(|| panic!("{name} is a surface"));
            assert!(Overlay::named(surface.name()).is_some());
        }
        assert!(Overlay::named("gallery").is_none());
    }

    #[test]
    fn a_flick_shows_nothing_and_holding_reveals_the_panel() {
        let surfaces = Overlays::new();
        let opened = tilo_clock::now();
        surfaces.walk(None, 1);
        assert_eq!(surfaces.here(), "switcher", "the walk is going");
        assert!(
            !surfaces.revealed(opened + Duration::from_millis(100)),
            "a flick shorter than the delay flashes no panel"
        );
        let deadline = surfaces
            .deadline(opened + Duration::from_millis(100))
            .expect("one wake is due when the delay is up");
        assert!(
            deadline >= opened && deadline <= opened + Duration::from_millis(200),
            "the wake is the end of the delay: {deadline:?}"
        );
        assert!(
            surfaces.revealed(opened + Duration::from_millis(200)),
            "holding Alt past the delay shows it"
        );
        assert_eq!(
            surfaces.deadline(opened + Duration::from_millis(200)),
            None,
            "and nothing else is due"
        );
        surfaces.forget_walk();
        surfaces.show(Overlay::Rest);
        assert!(!surfaces.revealed(opened + Duration::from_secs(60)));
    }

    #[test]
    fn the_walk_carries_its_scope_and_a_press_in_the_other_one_starts_it_again() {
        let surfaces = Overlays::new();
        surfaces.walk(Some("app"), 1);
        assert_eq!(surfaces.scope(), SwitcherScope::App);
        assert_eq!(surfaces.walked(), 1);
        surfaces.walk(Some("app"), 1);
        assert_eq!(surfaces.walked(), 2, "the same scope walks on");
        surfaces.walk(Some("workspace"), 1);
        assert_eq!(surfaces.scope(), SwitcherScope::Workspace);
        assert_eq!(
            surfaces.walked(),
            1,
            "the other scope walks from the window in front again"
        );
        surfaces.walk(None, -1);
        assert_eq!(
            surfaces.scope(),
            SwitcherScope::Workspace,
            "Alt+Tab's scope"
        );
        assert_eq!(
            surfaces.walked(),
            0,
            "stepping back past it walks to the window in front"
        );
        assert_eq!(
            surfaces.reached(3, Some(1)),
            Some(1),
            "the window in front is a window the walk can rest on, as Shift+Tab reaches it"
        );
    }

    #[test]
    fn choosing_by_place_reaches_that_window_and_a_surface_down_is_no_walk() {
        let surfaces = Overlays::new();
        surfaces.select(2);
        assert_eq!(
            surfaces.reached(4, Some(1)),
            None,
            "nothing walks while the switcher is down"
        );
        surfaces.walk(Some("app"), 1);
        surfaces.select(2);
        assert_eq!(surfaces.reached(4, Some(1)), Some(3));
        surfaces.select(0);
        assert_eq!(
            surfaces.reached(4, Some(1)),
            Some(1),
            "and choosing the first of them puts the walk back on the window in front"
        );
        surfaces.show(Overlay::Rest);
        assert!(!surfaces.is_up());
    }
}
