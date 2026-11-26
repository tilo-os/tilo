//! Onboarding's first-login controls in the order a person goes through
//! them, and the walk that presses them by name.
//!
//! Tools that need a session past its first login (tilo-device-step's
//! `--identity device`, the legacy stream's headless shell) take the same
//! path a person does instead of writing identity files themselves. They
//! share this one walk, so a renamed or added onboarding control is fixed
//! once; `tests/names.rs` checks every name against setup's app (`apps/tl/onboarding`).
//!
//! A control is named as assistive technology hears it, in the session's language: the walk
//! drives a session whose language is English, as a person reading the page would.

use std::time::{Duration, Instant};

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
/// backup (acknowledged), skip the optional codecs (or skip past their failure) and the tour,
/// enter. Whichever of them a page shows, the earliest is the one to press.
pub const PRESSES: [Press; 10] = [
    press("Create a new identity"),
    checkbox("I understand that losing this device without a backup means losing this identity."),
    press("Create local identity"),
    checkbox(
        "I understand that without a recovery key or backup, losing this device means losing this identity.",
    ),
    press("Continue without backup"),
    press("Not now"),
    press("Skip"),
    press("Skip tour"),
    press("Enter Tilo"),
    press("Continue to desktop"),
];

/// Setup's controls the walk never presses, which only setup shows: the ways to bring an
/// existing identity over. A page of only these is still setup. Words a desktop's apps show too
/// ("Back", "Continue", "Try again", "Join", "Install") are left out, so an app open after setup
/// never reads as setup.
pub const SETUP_ONLY: [&str; 8] = [
    "Use my existing Tilo identity",
    "Use a code from another device",
    "Enter your recovery key",
    "Restore from a backup",
    "Restore identity",
    "I wrote it down",
    "Choose location and save backup…",
    "Save another copy",
];

/// Whether `name` is one of the controls only setup shows: while setup stands, the session offers
/// no control but its own.
#[must_use]
pub fn setup_control(name: &str) -> bool {
    PRESSES.iter().any(|press| press.name == name) || SETUP_ONLY.contains(&name)
}

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
    /// By default: it shows controls, none of them setup's.
    fn finished(&mut self, controls: &[String]) -> bool {
        !controls.is_empty() && !controls.iter().any(|name| setup_control(name))
    }

    /// Whether the desktop has settled, so a press reaches its control: the
    /// shell refuses one while the desktop moves between two layouts
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

    const BEGIN: &str = PRESSES[0].name;
    const UNDERSTOOD: &str = PRESSES[1].name;
    const CREATE: &str = PRESSES[2].name;
    const SKIP_BACKUP: &str = PRESSES[3].name;
    const CONTINUE: &str = PRESSES[4].name;
    const NOT_NOW: &str = PRESSES[5].name;
    const SKIP_TOUR: &str = PRESSES[7].name;
    const ENTER: &str = PRESSES[8].name;
    const FINISH: &str = PRESSES[9].name;

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
                || vec!["Search".to_owned()],
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
        let page = [UNDERSTOOD, CREATE];
        assert_eq!(next_press(&page, &[]), Some(UNDERSTOOD));
        assert_eq!(next_press(&page, &[UNDERSTOOD]), Some(CREATE));
        assert_eq!(next_press(&["Restore from a backup"], &[]), None);
    }

    #[test]
    fn a_walk_presses_each_page_through_to_the_desktop() {
        let mut screen = Onboarding::new(&[
            &[BEGIN, "Restore from a backup"],
            &[UNDERSTOOD, CREATE],
            &[SKIP_BACKUP, CONTINUE],
            &["Install", NOT_NOW],
            &[SKIP_TOUR, ENTER],
            &[FINISH],
        ]);
        let pressed = walk(&mut screen, quick()).unwrap();
        assert_eq!(
            pressed,
            [
                BEGIN,
                UNDERSTOOD,
                CREATE,
                SKIP_BACKUP,
                CONTINUE,
                NOT_NOW,
                SKIP_TOUR,
                FINISH,
            ]
        );
        assert_eq!(screen.presses, pressed);
    }

    #[test]
    fn a_pressed_control_is_not_pressed_again_until_the_page_had_time_to_move() {
        let mut screen = Onboarding::new(&[&[BEGIN], &[FINISH]]);
        screen.lose_next = true;
        let pace = Pace {
            repress: Duration::from_millis(50),
            ..quick()
        };
        let pressed = walk(&mut screen, pace).unwrap();
        // The lost press is repeated once, after `repress`, not every poll.
        assert_eq!(pressed, [BEGIN, BEGIN, FINISH]);
    }

    #[test]
    fn a_walk_that_runs_out_of_time_says_where_it_stopped() {
        let mut screen = Onboarding::new(&[&["Restore from a backup"]]);
        let pace = Pace {
            timeout: Duration::ZERO,
            ..quick()
        };
        assert_eq!(
            walk(&mut screen, pace),
            Err(WalkError::Unfinished {
                timeout: Duration::ZERO,
                shown: vec!["Restore from a backup".to_owned()],
                pressed: Vec::new(),
            })
        );
    }

    #[test]
    fn a_session_showing_nothing_yet_is_not_past_onboarding() {
        let mut screen = Onboarding::new(&[]);
        assert!(!screen.finished(&[]));
        assert!(screen.finished(&["shell.search".to_owned()]));
        assert!(!screen.finished(&[BEGIN.to_owned()]));
        assert!(
            !screen.finished(&["Back".to_owned(), "Restore identity".to_owned()]),
            "a page of setup's that the walk never presses is still setup"
        );
        assert!(
            screen.finished(&["Back".to_owned(), "Try again".to_owned()]),
            "an app's own words after setup are not setup's"
        );
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
                vec![BEGIN.to_owned()]
            } else {
                vec!["Search".to_owned()]
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
        assert_eq!(walk(&mut screen, quick()), Ok(vec![BEGIN]));
        assert_eq!(screen.presses, [BEGIN]);
    }
}
