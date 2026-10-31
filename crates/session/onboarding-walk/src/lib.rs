//! Onboarding's first-login controls in the order a person goes through
//! them, and the walk that presses them by name.
//!
//! Tools that need a session past its first login (tilo-vmd's
//! `--identity device`, the legacy stream's headless shell) take the same
//! path a person does instead of writing identity files themselves. They
//! share this one walk, so a renamed or added onboarding control is fixed
//! once; `tests/names.rs` checks every name against the onboarding view.

use std::time::{Duration, Instant};

/// The prefix every onboarding control's name starts with.
pub const PREFIX: &str = "onboarding.";

/// One control the walk presses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Press {
    /// The control's name in the session's target list.
    pub name: &'static str,
    /// A checkbox row: a press toggles it, so it is pressed once.
    pub checkbox: bool,
}

const fn press(name: &'static str) -> Press {
    Press {
        name,
        checkbox: false,
    }
}

const fn checkbox(name: &'static str) -> Press {
    Press {
        name,
        checkbox: true,
    }
}

/// Onboarding's controls in the order a person goes through them: begin,
/// accept that the identity is theirs to protect, create it, skip the
/// backup (acknowledged), skip the optional codecs and the tour, enter.
/// Whichever of them a page shows, the earliest is the one to press.
pub const PRESSES: [Press; 9] = [
    press("onboarding.begin.card"),
    checkbox("onboarding.understood.row"),
    press("onboarding.create.button"),
    checkbox("onboarding.backup.skip-acknowledged.row"),
    press("onboarding.backup.continue.button"),
    press("onboarding.codecs.skip.button"),
    press("onboarding.tour.skip.button"),
    press("onboarding.tour.next.button"),
    press("onboarding.finish.button"),
];

/// The next control to press among those `shown`, given those `pressed`
/// so far: the earliest shown one, skipping a checkbox already pressed.
#[must_use]
pub fn next_press<S: AsRef<str>>(shown: &[S], pressed: &[&str]) -> Option<&'static str> {
    PRESSES
        .iter()
        .filter(|press| shown.iter().any(|name| name.as_ref() == press.name))
        .find(|press| !press.checkbox || !pressed.contains(&press.name))
        .map(|press| press.name)
}

/// A session the walk drives.
pub trait Screen {
    type Error;

    /// The names of the controls the session shows now.
    fn controls(&mut self) -> Result<Vec<String>, Self::Error>;

    /// Press the control `name`, the way a person's click reaches it.
    fn press(&mut self, name: &'static str) -> Result<(), Self::Error>;

    /// Whether the session is past onboarding, given the controls it shows.
    /// By default: it shows controls, none of them onboarding's.
    fn finished(&mut self, controls: &[String]) -> bool {
        !controls.is_empty() && !controls.iter().any(|name| name.starts_with(PREFIX))
    }

    /// Whether the desktop has settled, so a press reaches its control: the
    /// shell refuses one while the desktop moves between two arrangements
    /// (`desktop_in_transition`), as onboarding's pages do when they change.
    /// By default: always.
    fn settled(&mut self) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

/// How the walk paces itself.
#[derive(Clone, Copy, Debug)]
pub struct Pace {
    /// How long onboarding may take, from the walk's start.
    pub timeout: Duration,
    /// How long to wait between looks at the session.
    pub poll: Duration,
    /// How long a pressed control may stay the next one before it is
    /// pressed again: a press can land while a page is still changing.
    pub repress: Duration,
}

impl Default for Pace {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(180),
            poll: Duration::from_millis(300),
            repress: Duration::from_secs(5),
        }
    }
}

/// Why a walk stopped short.
#[derive(Debug, PartialEq, Eq)]
pub enum WalkError<E> {
    /// The session could not be looked at or pressed.
    Screen(E),
    /// Onboarding was still shown when the walk's time ran out.
    Unfinished {
        timeout: Duration,
        shown: Vec<String>,
        pressed: Vec<&'static str>,
    },
}

impl<E: std::fmt::Display> std::fmt::Display for WalkError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Screen(error) => error.fmt(f),
            Self::Unfinished {
                timeout,
                shown,
                pressed,
            } => write!(
                f,
                "onboarding did not finish within {}s; it still shows {shown:?} after pressing {pressed:?}",
                timeout.as_secs()
            ),
        }
    }
}

impl<E: std::fmt::Debug + std::fmt::Display> std::error::Error for WalkError<E> {}

/// Walk `screen` through onboarding: press the next control it shows,
/// wait until the page moves on, and repeat until it is
/// [`finished`](Screen::finished). Returns the controls pressed, in order.
///
/// # Errors
///
/// The screen's own error, or [`WalkError::Unfinished`] once `pace.timeout`
/// has passed with onboarding still shown.
pub fn walk<S: Screen>(
    screen: &mut S,
    pace: Pace,
) -> Result<Vec<&'static str>, WalkError<S::Error>> {
    let started = Instant::now();
    let mut pressed: Vec<&'static str> = Vec::new();
    let mut last: Option<(&'static str, Instant)> = None;
    loop {
        let shown = screen.controls().map_err(WalkError::Screen)?;
        if screen.finished(&shown) {
            return Ok(pressed);
        }
        if started.elapsed() >= pace.timeout {
            return Err(WalkError::Unfinished {
                timeout: pace.timeout,
                shown,
                pressed,
            });
        }
        if let Some(next) = next_press(&shown, &pressed)
            && screen.settled().map_err(WalkError::Screen)?
        {
            let waiting =
                last.is_some_and(|(name, at)| name == next && at.elapsed() < pace.repress);
            if !waiting {
                screen.press(next).map_err(WalkError::Screen)?;
                pressed.push(next);
                last = Some((next, Instant::now()));
            }
        }
        std::thread::sleep(pace.poll);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Onboarding's pages, each shown until one of its controls is pressed.
    struct Onboarding {
        pages: Vec<Vec<&'static str>>,
        page: usize,
        presses: Vec<&'static str>,
        /// The next press changes nothing, as one landing mid-transition.
        lose_next: bool,
    }

    impl Onboarding {
        fn new(pages: &[&[&'static str]]) -> Self {
            Self {
                pages: pages.iter().map(|page| page.to_vec()).collect(),
                page: 0,
                presses: Vec::new(),
                lose_next: false,
            }
        }
    }

    impl Screen for Onboarding {
        type Error = String;

        fn controls(&mut self) -> Result<Vec<String>, String> {
            Ok(self.pages.get(self.page).map_or_else(
                || vec!["shell.search".to_owned()],
                |page| page.iter().map(|name| (*name).to_owned()).collect(),
            ))
        }

        fn press(&mut self, name: &'static str) -> Result<(), String> {
            self.presses.push(name);
            if std::mem::take(&mut self.lose_next) {
                return Ok(());
            }
            // A checkbox only ticks; anything else moves to the next page.
            if !PRESSES
                .iter()
                .any(|press| press.name == name && press.checkbox)
            {
                self.page = self.page.saturating_add(1);
            }
            Ok(())
        }
    }

    fn quick() -> Pace {
        Pace {
            timeout: Duration::from_secs(10),
            poll: Duration::ZERO,
            repress: Duration::ZERO,
        }
    }

    #[test]
    fn the_earliest_shown_control_is_next_and_a_checkbox_is_pressed_once() {
        let page = ["onboarding.understood.row", "onboarding.create.button"];
        assert_eq!(next_press(&page, &[]), Some("onboarding.understood.row"));
        assert_eq!(
            next_press(&page, &["onboarding.understood.row"]),
            Some("onboarding.create.button")
        );
        assert_eq!(next_press(&["onboarding.import.button"], &[]), None);
    }

    #[test]
    fn a_walk_presses_each_page_through_to_the_desktop() {
        let mut screen = Onboarding::new(&[
            &["onboarding.begin.card", "onboarding.import.button"],
            &["onboarding.understood.row", "onboarding.create.button"],
            &[
                "onboarding.backup.skip-acknowledged.row",
                "onboarding.backup.continue.button",
            ],
            &["onboarding.tour.skip.button", "onboarding.tour.next.button"],
            &["onboarding.finish.button"],
        ]);
        let pressed = walk(&mut screen, quick()).unwrap();
        assert_eq!(
            pressed,
            [
                "onboarding.begin.card",
                "onboarding.understood.row",
                "onboarding.create.button",
                "onboarding.backup.skip-acknowledged.row",
                "onboarding.backup.continue.button",
                "onboarding.tour.skip.button",
                "onboarding.finish.button",
            ]
        );
        assert_eq!(screen.presses, pressed);
    }

    #[test]
    fn a_pressed_control_is_not_pressed_again_until_the_page_had_time_to_move() {
        let mut screen =
            Onboarding::new(&[&["onboarding.begin.card"], &["onboarding.finish.button"]]);
        screen.lose_next = true;
        let pace = Pace {
            repress: Duration::from_millis(50),
            ..quick()
        };
        let pressed = walk(&mut screen, pace).unwrap();
        // The lost press is repeated once, after `repress`, not every poll.
        assert_eq!(
            pressed,
            [
                "onboarding.begin.card",
                "onboarding.begin.card",
                "onboarding.finish.button"
            ]
        );
    }

    #[test]
    fn a_walk_that_runs_out_of_time_says_where_it_stopped() {
        let mut screen = Onboarding::new(&[&["onboarding.import.button"]]);
        let pace = Pace {
            timeout: Duration::ZERO,
            ..quick()
        };
        assert_eq!(
            walk(&mut screen, pace),
            Err(WalkError::Unfinished {
                timeout: Duration::ZERO,
                shown: vec!["onboarding.import.button".to_owned()],
                pressed: Vec::new(),
            })
        );
    }

    #[test]
    fn a_session_showing_nothing_yet_is_not_past_onboarding() {
        let mut screen = Onboarding::new(&[]);
        assert!(!screen.finished(&[]));
        assert!(screen.finished(&["shell.search".to_owned()]));
        assert!(!screen.finished(&["onboarding.begin.card".to_owned()]));
    }

    /// One page whose control is refused while the desktop moves.
    struct Moving {
        moving_for: usize,
        presses: Vec<&'static str>,
    }

    impl Screen for Moving {
        type Error = String;

        fn controls(&mut self) -> Result<Vec<String>, String> {
            Ok(if self.presses.is_empty() {
                vec!["onboarding.begin.card".to_owned()]
            } else {
                vec!["shell.search".to_owned()]
            })
        }

        fn press(&mut self, name: &'static str) -> Result<(), String> {
            if self.moving_for > 0 {
                return Err(format!("{name}: desktop_in_transition"));
            }
            self.presses.push(name);
            Ok(())
        }

        fn settled(&mut self) -> Result<bool, String> {
            let settled = self.moving_for == 0;
            self.moving_for = self.moving_for.saturating_sub(1);
            Ok(settled)
        }
    }

    #[test]
    fn a_control_is_pressed_once_the_desktop_settles() {
        let mut screen = Moving {
            moving_for: 3,
            presses: Vec::new(),
        };
        assert_eq!(
            walk(&mut screen, quick()),
            Ok(vec!["onboarding.begin.card"])
        );
        assert_eq!(screen.presses, ["onboarding.begin.card"]);
    }
}
