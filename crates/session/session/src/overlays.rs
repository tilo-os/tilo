//! The surface the desk shows over its windows, as the person left it: at rest, or one of the
//! shell's overlays (`overview`, `switcher`, `launcher`, `centre`, `workspaces`, `composer`,
//! `conversation`).
//! The session keeps it, so the desk's reading says which one is up (`session.desk.here`) and
//! the shell's views re-run only when it changes.
//!
//! A press the shell hands the session changes it: `show-surface` puts one up (or closes it,
//! when it is the one up), `close-surface` takes one down. The overlays themselves are the
//! Rust chrome's while it still draws them (the overview, the workspace menu); the reading
//! names the one up either way, so a shell's views and the chrome agree.

use std::cell::{Cell, RefCell};

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
        }
    }

    /// The walk starts again from the window in front, whichever surface was up.
    pub fn forget_walk(&self) {
        self.walk.set(0);
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
        if surface == Overlay::Switcher && self.walk.get() == 0 {
            self.walk.set(1);
        }
        self.revision.set(self.revision.get().wrapping_add(1));
    }

    /// Walk the switcher `steps` windows on from where it is, wrapping: Alt+Tab's press, and
    /// Shift+Alt+Tab's back.
    pub fn step(&self, steps: i64) {
        self.walk.set(self.walk.get().saturating_add(steps));
        self.revision.set(self.revision.get().wrapping_add(1));
    }

    /// How far the walk stands from the window in front. The reading turns it into the one
    /// window the switcher has reached, of `count`.
    #[must_use]
    pub fn walk(&self) -> i64 {
        self.walk.get()
    }

    /// Where the walk has reached of `count` windows, `front` being the window in front's
    /// place among them: `None` when the walk is not up (`walk() == 0`).
    #[must_use]
    pub fn reached(&self, count: usize, front: Option<usize>) -> Option<usize> {
        if self.walk.get() == 0 || count == 0 || self.here.get() != Overlay::Switcher {
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
            "conversation",
        ] {
            let surface = Overlay::named(name).unwrap_or_else(|| panic!("{name} is a surface"));
            assert!(Overlay::named(surface.name()).is_some());
        }
        assert!(Overlay::named("gallery").is_none());
    }
}
