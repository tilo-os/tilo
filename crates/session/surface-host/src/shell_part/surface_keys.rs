#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test fixtures")]

use std::collections::BTreeMap;

use super::tests_support::{
    alt, compiled, grave, key, part_of, part_with, reading_with_windows, tab,
};
use super::{Ask, OVERLAYS};
use tilo_session_core::surface::{HostKey, KeyClaim, SurfaceHost as _};

/// What the part makes of a key the compositor offers it.
enum Answer {
    /// The key is the shell's: the part takes it, and asks the desk for this action with these
    /// arguments, once.
    Asks(&'static str, Vec<(&'static str, serde_json::Value)>),
    /// The key belongs to whatever has the keyboard: the part passes it on and asks nothing.
    Passes,
}

/// A key offered to the part, in a session that is locked or not, and the part's answer.
struct Offered {
    key: HostKey,
    locked: bool,
    answer: Answer,
}

/// A desk showing the surface `here` (`""`: at rest), and the keys its part is offered in turn.
struct Case {
    name: &'static str,
    here: &'static str,
    keys: Vec<Offered>,
}

fn asks(
    key: HostKey,
    action: &'static str,
    arguments: Vec<(&'static str, serde_json::Value)>,
) -> Offered {
    Offered {
        key,
        locked: false,
        answer: Answer::Asks(action, arguments),
    }
}

/// `key` walks the switcher `steps` on, over the windows of `scope`.
fn walks(key: HostKey, steps: i32, scope: &'static str) -> Offered {
    asks(
        key,
        "walk-switcher",
        vec![("steps", steps.into()), ("scope", scope.into())],
    )
}

fn passes(key: HostKey) -> Offered {
    Offered {
        key,
        locked: false,
        answer: Answer::Passes,
    }
}

/// `key` offered while the session is locked: no key is the shell's to answer then.
fn locked(key: HostKey) -> Offered {
    Offered {
        key,
        locked: true,
        answer: Answer::Passes,
    }
}

fn escape() -> HostKey {
    key(0xff1b, true)
}

fn enter() -> HostKey {
    key(0xff0d, true)
}

/// A letter typed with nothing held.
fn letter() -> HostKey {
    key(u32::from('a'), true)
}

/// The key above Tab as a layout sends it with Shift held: `~`.
fn tilde() -> HostKey {
    HostKey {
        keysym: 0x007e,
        ..grave(true)
    }
}

/// `Alt_R`, let go.
fn right_alt_let_go() -> HostKey {
    key(0xffea, false)
}

/// `key` as it is let go, with what is held still held.
fn let_go(key: HostKey) -> HostKey {
    HostKey {
        pressed: false,
        ..key
    }
}

fn cases() -> Vec<Case> {
    let mut cases = at_rest();
    cases.extend(with_a_surface_up());
    cases
}

/// The desk at rest: only the keys that open the switcher are the shell's.
fn at_rest() -> Vec<Case> {
    vec![
        Case {
            name: "Alt+Tab from rest",
            here: "",
            keys: vec![
                // Alt+Tab asks the desk for its switcher; with Shift it walks back.
                walks(tab(true, true), -1, "workspace"),
                // Holding Alt and pressing Tab again walks the switcher on.
                walks(tab(true, false), 1, "workspace"),
                // With no surface up, every other key is the window's in front.
                passes(tab(false, false)),
                passes(escape()),
                passes(enter()),
                passes(alt(false)),
                passes(letter()),
                // Tab let go while Alt is still held walks no further.
                passes(let_go(tab(true, false))),
                locked(tab(true, false)),
            ],
        },
        Case {
            name: "Alt+grave from rest",
            here: "",
            keys: vec![
                // Alt and the grave key walk the app in front's windows.
                walks(grave(false), 1, "app"),
                // Shift walks the other way, and stays in the app.
                walks(grave(true), -1, "app"),
                walks(tilde(), -1, "app"),
                passes(let_go(grave(false))),
                locked(grave(false)),
            ],
        },
    ]
}

/// A surface of the desk's up: the keys that answer it are the shell's too.
fn with_a_surface_up() -> Vec<Case> {
    vec![
        Case {
            name: "the switcher up",
            here: "switcher",
            keys: vec![
                // Letting Alt go goes to the window the walk reached.
                asks(alt(false), "commit-switcher", Vec::new()),
                asks(right_alt_let_go(), "commit-switcher", Vec::new()),
                asks(enter(), "commit-switcher", Vec::new()),
                asks(escape(), "close-surface", vec![("slot", "switcher".into())]),
                // Alt going down again, and a letter, are no answer to the switcher.
                passes(alt(true)),
                passes(letter()),
                // Tab let go between two walks, Alt still held, neither commits nor walks.
                passes(let_go(tab(true, false))),
                locked(alt(false)),
                locked(escape()),
            ],
        },
        Case {
            name: "the overview up",
            here: "overview",
            keys: vec![
                asks(escape(), "close-surface", vec![("slot", "overview".into())]),
                // Enter and letting Alt go answer the switcher alone.
                passes(enter()),
                passes(alt(false)),
                passes(letter()),
                // Alt+Tab asks for the switcher whatever surface is up.
                walks(tab(true, false), 1, "workspace"),
                // Escape let go closes nothing a second time.
                passes(let_go(escape())),
                locked(escape()),
            ],
        },
        Case {
            name: "the launcher up",
            here: "launcher",
            keys: vec![
                // What is typed into the launcher is its field's, Enter included: of the keys
                // typed there only Escape is the shell's.
                passes(letter()),
                passes(enter()),
                passes(alt(false)),
                asks(escape(), "close-surface", vec![("slot", "launcher".into())]),
                locked(escape()),
            ],
        },
    ]
}

/// The compositor offers the part every key before the window in front hears it
/// (`claim_key`), and hands it the ones it takes (`key`): Alt+Tab and Alt+grave ask the desk to
/// walk its switcher from anywhere, letting Alt go or Enter commit the switcher while it is up,
/// and Escape closes whatever surface is up. Each key the shell takes asks the desk exactly
/// once; every other key (a key let go that is not Alt on the switcher, what is typed into the
/// launcher), and every key of a locked session, is passed on and asks nothing.
#[test]
fn a_key_is_the_shells_only_where_a_surface_answers_it() {
    let packages = compiled();
    for case in cases() {
        let name = case.name;
        let mut reading = reading_with_windows();
        case.here.clone_into(&mut reading.here);
        reading.revealed = case.here == "switcher";
        let (mut part, link) = part_of(&packages, reading, (1280, 800));
        // What the desk was asked while its switcher or its launcher came up is no key's.
        if matches!(case.here, "switcher" | "launcher") {
            link.asked.borrow_mut().clear();
        }
        for (at, offered) in case.keys.iter().enumerate() {
            let claim = part.claim_key(&offered.key, offered.locked, false);
            match &offered.answer {
                Answer::Asks(action, arguments) => {
                    assert_eq!(claim, KeyClaim::Take, "{name}: key {at} is the shell's");
                    part.key(&offered.key);
                    let asked: Vec<Ask> = link.asked.borrow_mut().drain(..).collect();
                    let expected = Ask {
                        resource: OVERLAYS.to_owned(),
                        action: (*action).to_owned(),
                        arguments: arguments
                            .iter()
                            .map(|(name, value)| ((*name).to_owned(), value.clone()))
                            .collect::<BTreeMap<_, _>>(),
                        from: None,
                    };
                    assert_eq!(asked, vec![expected], "{name}: what key {at} asks");
                }
                Answer::Passes => {
                    assert_eq!(
                        claim,
                        KeyClaim::Pass,
                        "{name}: key {at} is not the shell's (locked: {})",
                        offered.locked
                    );
                    assert!(
                        link.asked.borrow().is_empty(),
                        "{name}: key {at} asked {:?}",
                        link.asked.borrow()
                    );
                }
            }
        }
    }
}

#[test]
fn a_surface_up_makes_the_part_cover_the_outputs() {
    let mut reading = super::tests_support::reading_with_windows();
    "launcher".clone_into(&mut reading.here);
    let (part, _link) = part_with(reading);
    assert_eq!(
        part.widgets.placement().size,
        (1280, 800),
        "the launcher has the desk's room"
    );
    assert!(
        part.accepts_pointer(640.0, 400.0),
        "the launcher takes presses"
    );
}
