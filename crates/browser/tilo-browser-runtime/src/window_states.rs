//! Where each window of the page's desktop stands, as its status publishes it: what a caller
//! (a test, a tool, an agent driving the page) waits on instead of waiting a while.
//!
//! A window is `arriving` while its app's code or its record is on its way, `landed` once its app
//! runs on its record, `drawn` once that has presented, and `could-not-open` with what the window
//! says instead when it cannot open what it names.

/// Where one window stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WindowState {
    /// Its app's code or its record is on its way.
    Arriving,
    /// Its app runs on its record, and has not drawn yet.
    Landed,
    /// What it shows has presented.
    Drawn,
    /// It cannot open what it names: `reason` is what the window says instead.
    CouldNotOpen { reason: String },
}

impl WindowState {
    /// A window's state from what the desktop knows of it: what it said instead of opening,
    /// whether its app runs on its record, and whether that has drawn.
    pub(crate) fn of(unopened: Option<&str>, landed: bool, drawn: bool) -> Self {
        match unopened {
            Some(reason) => Self::CouldNotOpen {
                reason: reason.to_owned(),
            },
            None if landed && drawn => Self::Drawn,
            None if landed => Self::Landed,
            None => Self::Arriving,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::WindowState;

    #[test]
    fn a_window_moves_from_arriving_to_drawn_or_says_why_it_did_not_open() {
        assert_eq!(WindowState::of(None, false, false), WindowState::Arriving);
        // A notice presents at once, but the window did not open what it names.
        assert_eq!(
            WindowState::of(Some("Notes could not be loaded"), false, true),
            WindowState::CouldNotOpen {
                reason: "Notes could not be loaded".into()
            }
        );
        assert_eq!(WindowState::of(None, true, false), WindowState::Landed);
        assert_eq!(WindowState::of(None, true, true), WindowState::Drawn);
    }
}
