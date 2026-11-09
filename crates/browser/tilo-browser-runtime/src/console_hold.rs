//! Holding Console in front of the page's desktop while the page shows the
//! console layout (`shell::console`): what each frame asks of the desktop,
//! from what is in front of it. Kept apart from the shell, which only the
//! page builds, so its tests run on the host too.

/// The Console app.
pub(crate) const CONSOLE_APP: &str = "so.tilo.console";

/// Where the page is in holding Console in front.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ConsoleHold {
    /// Console was not asked for yet.
    #[default]
    Opening,
    /// Console was asked for and is on its way: its window opens once its
    /// code is here, and whatever is in front until then stays.
    Showing,
    /// Console is in front, and held there.
    Holding,
    /// The person opened something else: the page shows the desktop.
    Released,
}

/// What is in front of the desktop, as the hold reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Front<G> {
    /// No window.
    Nothing,
    /// Console's window, in group `G`, and whether that group fills the
    /// display.
    Console { group: G, fullscreen: bool },
    /// Another app's window, and the group filling the display, if any.
    Other { fullscreen: Option<G> },
}

/// What holding Console asks of the desktop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step<G> {
    /// Nothing to do.
    Rest,
    /// Show Console: its window, or a new one.
    Show,
    /// Fill the display with Console's group.
    Fill(G),
    /// The person opened something else: let the group filling the display
    /// go, if one does, and show the desktop.
    Release(Option<G>),
}

impl ConsoleHold {
    /// The step `front` asks for, and the hold after it.
    pub(crate) fn step<G: Copy>(self, front: Front<G>) -> (Self, Step<G>) {
        match (self, front) {
            (Self::Released, _) => (Self::Released, Step::Rest),
            (_, Front::Console { group, fullscreen }) => (
                Self::Holding,
                if fullscreen {
                    Step::Rest
                } else {
                    Step::Fill(group)
                },
            ),
            // On opening, whatever the desktop restored is behind Console.
            (Self::Opening, _) => (Self::Showing, Step::Show),
            (Self::Showing, _) => (Self::Showing, Step::Rest),
            // Console was closed: the desktop is what is left.
            (Self::Holding, Front::Nothing) => (Self::Released, Step::Release(None)),
            (Self::Holding, Front::Other { fullscreen }) => {
                (Self::Released, Step::Release(fullscreen))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_console_opens_over_what_the_desktop_restored_and_fills_the_page() {
        let (hold, step) = ConsoleHold::Opening.step(Front::<u8>::Other { fullscreen: None });
        assert_eq!((hold, step), (ConsoleHold::Showing, Step::Show));
        // Its window is not up yet: what was in front stays, and nothing is
        // let go of.
        let (hold, step) = hold.step(Front::<u8>::Other { fullscreen: None });
        assert_eq!((hold, step), (ConsoleHold::Showing, Step::Rest));
        let (hold, step) = hold.step(Front::Console {
            group: 3,
            fullscreen: false,
        });
        assert_eq!((hold, step), (ConsoleHold::Holding, Step::Fill(3)));
        let (hold, step) = hold.step(Front::Console {
            group: 3,
            fullscreen: true,
        });
        assert_eq!((hold, step), (ConsoleHold::Holding, Step::Rest));
    }

    #[test]
    fn opening_something_from_the_console_leaves_it_for_the_desktop_for_good() {
        let (hold, step) = ConsoleHold::Holding.step(Front::Other {
            fullscreen: Some(3_u8),
        });
        assert_eq!(
            (hold, step),
            (ConsoleHold::Released, Step::Release(Some(3)))
        );
        // Back on Console's window, the page stays the desktop the person chose.
        let (hold, step) = hold.step(Front::Console {
            group: 3,
            fullscreen: false,
        });
        assert_eq!((hold, step), (ConsoleHold::Released, Step::Rest));
    }

    #[test]
    fn closing_the_console_leaves_the_desktop() {
        let (hold, step) = ConsoleHold::Holding.step(Front::<u8>::Nothing);
        assert_eq!((hold, step), (ConsoleHold::Released, Step::Release(None)));
    }
}
