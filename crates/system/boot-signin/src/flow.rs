//! What the boot sign-in does, independent of how it draws or checks:
//! the person picks themselves (when several may open the volume), gives
//! their secret until it opens the volume, or asks for another way in.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::people::{Person, SecretKind};

/// How the sign-in ended, as the process exit status the caller reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    /// The volume's key file is in place and the handoff to the session made.
    Unlocked,
    /// Nobody can sign in here, or their secret could not be tried: the
    /// existing prompts take over.
    NothingToTry,
    /// The person asked for another way in.
    OtherWays,
}

impl Exit {
    pub fn code(self) -> u8 {
        match self {
            Self::Unlocked => 0,
            Self::NothingToTry => 2,
            Self::OtherWays => 3,
        }
    }
}

/// The person's choice on the screen listing everyone who may sign in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    Person(usize),
    OtherWays,
}

/// What came back from the secret field.
pub enum Entry {
    Secret(Zeroizing<String>),
    /// Enter on an empty field: back to the list, or another way in.
    Empty,
    /// The field could not be shown or was cancelled.
    Unavailable,
}

/// The limit the TPM reported after a refused secret.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Limit {
    pub tries_left: Option<u32>,
    pub wait_seconds: Option<u64>,
}

pub enum Unlock {
    Opened,
    Wrong(Limit),
    CouldNotTry,
}

/// The line under the field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Note {
    None,
    Wrong {
        secret: SecretKind,
        tries_left: Option<u32>,
    },
    Wait {
        seconds: u64,
    },
    /// The entry is one already refused: it was not tried again.
    Repeated {
        secret: SecretKind,
    },
}

impl Note {
    fn after(limit: Limit, secret: SecretKind) -> Self {
        match limit.wait_seconds {
            Some(seconds) if seconds > 0 => Self::Wait { seconds },
            _ => Self::Wrong {
                secret,
                tries_left: limit.tries_left,
            },
        }
    }

    /// The class the Plymouth theme keys its drawn fallback on.
    pub fn class(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Wrong { .. } | Self::Repeated { .. } => "wrong",
            Self::Wait { .. } => "wait",
        }
    }

    pub fn text(self) -> String {
        match self {
            Self::None => String::new(),
            Self::Wrong {
                secret,
                tries_left: Some(1),
            } => format!("Incorrect {}. 1 try left.", secret.noun()),
            Self::Wrong {
                secret,
                tries_left: Some(left),
            } => format!("Incorrect {}. {left} tries left.", secret.noun()),
            Self::Wrong {
                secret,
                tries_left: None,
            } => format!("Incorrect {}. Try again.", secret.noun()),
            Self::Wait { seconds } => {
                format!("Too many failed attempts. Try again in {}.", wait(seconds))
            }
            Self::Repeated { secret } => {
                format!("You already tried that {}. Try another.", secret.noun())
            }
        }
    }
}

fn wait(seconds: u64) -> String {
    let minutes = seconds.div_ceil(60).max(1);
    if minutes < 90 {
        return if minutes == 1 {
            "1 minute".to_owned()
        } else {
            format!("{minutes} minutes")
        };
    }
    let hours = minutes.div_ceil(60);
    format!("{hours} hours")
}

/// One ask of the secret field.
pub struct Prompt<'a> {
    pub person: &'a Person,
    pub note: Note,
    /// Several people may sign in, so an empty entry goes back to the list.
    pub several: bool,
}

pub trait Screen {
    /// Let the person pick themselves, starting from `highlighted`.
    fn pick(&mut self, people: &[Person], highlighted: usize) -> Pick;
    fn ask(&mut self, prompt: &Prompt<'_>) -> Entry;
    /// The secret is being checked.
    fn checking(&mut self, person: &Person);
    /// The sign-in screen is done, opened or not.
    fn finish(&mut self, unlocked: bool);
}

pub trait Auth {
    /// Everyone who may open the volume; empty when nobody can sign in here.
    fn people(&mut self) -> Vec<Person>;
    fn unlock(&mut self, person: &Person, secret: &str) -> Unlock;
}

/// Wrong entries already tried this boot. Someone unsure whether an entry
/// registered and typing it again spends no second try (sign-in.md §4.2).
///
/// tilo-authd keeps this rule after boot; in the initramfs this process is
/// the one that lives across entries. It keeps only an HMAC of each refused
/// entry, under a key it makes when it starts. The key and the HMACs live in
/// memory alone and are wiped when the volume opens and when it exits.
struct Tried {
    /// `None` when no key could be made: then nothing is remembered, and
    /// every entry is tried.
    key: Option<Zeroizing<[u8; 32]>>,
    entries: Vec<(u32, Zeroizing<[u8; 32]>)>,
}

impl Tried {
    const LIMIT: usize = 16;

    fn new() -> Self {
        let mut key = Zeroizing::new([0_u8; 32]);
        let key = getrandom::fill(key.as_mut()).is_ok().then_some(key);
        Self {
            key,
            entries: Vec::new(),
        }
    }

    fn mac(&self, uid: u32, secret: &str) -> Option<Zeroizing<[u8; 32]>> {
        let key = self.key.as_ref()?;
        let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key.as_ref()).ok()?;
        mac.update(&uid.to_le_bytes());
        mac.update(secret.as_bytes());
        let mut out = Zeroizing::new([0_u8; 32]);
        out.copy_from_slice(&mac.finalize().into_bytes());
        Some(out)
    }

    fn already(&self, uid: u32, secret: &str) -> bool {
        self.mac(uid, secret).is_some_and(|mac| {
            self.entries
                .iter()
                .any(|(owner, tried)| *owner == uid && tried.as_ref() == mac.as_ref())
        })
    }

    fn remember(&mut self, uid: u32, secret: &str) {
        let Some(mac) = self.mac(uid, secret) else {
            return;
        };
        if self.entries.len() >= Self::LIMIT {
            self.entries.remove(0);
        }
        self.entries.push((uid, mac));
    }

    fn forget(&mut self) {
        self.entries.clear();
        self.key = None;
    }
}

pub fn run(auth: &mut dyn Auth, screen: &mut dyn Screen) -> Exit {
    let people = auth.people();
    if people.is_empty() {
        return Exit::NothingToTry;
    }
    let several = people.len() > 1;
    let mut tried = Tried::new();
    let mut highlighted = 0;
    loop {
        let index = if several {
            match screen.pick(&people, highlighted) {
                Pick::Person(index) => index,
                Pick::OtherWays => {
                    screen.finish(false);
                    return Exit::OtherWays;
                }
            }
        } else {
            0
        };
        let Some(person) = people.get(index) else {
            continue;
        };
        highlighted = index;
        if let Some(exit) = sign_in(auth, screen, person, several, &mut tried) {
            screen.finish(exit == Exit::Unlocked);
            return exit;
        }
    }
}

/// Ask `person` for their secret until it opens the volume. `None` goes back
/// to the list.
fn sign_in(
    auth: &mut dyn Auth,
    screen: &mut dyn Screen,
    person: &Person,
    several: bool,
    tried: &mut Tried,
) -> Option<Exit> {
    let mut note = Note::None;
    loop {
        let secret = match screen.ask(&Prompt {
            person,
            note,
            several,
        }) {
            Entry::Secret(secret) => secret,
            Entry::Empty if several => return None,
            Entry::Empty | Entry::Unavailable => return Some(Exit::OtherWays),
        };
        if tried.already(person.uid, &secret) {
            note = Note::Repeated {
                secret: person.secret,
            };
            continue;
        }
        screen.checking(person);
        match auth.unlock(person, &secret) {
            Unlock::Opened => {
                tried.forget();
                return Some(Exit::Unlocked);
            }
            Unlock::Wrong(limit) => {
                note = Note::after(limit, person.secret);
                // A refusal during a wait never reached the check, so the
                // same entry may still be right once the wait is over.
                if limit.wait_seconds.unwrap_or(0) == 0 {
                    tried.remember(person.uid, &secret);
                }
            }
            Unlock::CouldNotTry => return Some(Exit::NothingToTry),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;

    fn person(uid: u32, name: &str, secret: SecretKind) -> Person {
        Person {
            uid,
            name: name.to_lowercase(),
            display_name: name.to_owned(),
            secret,
        }
    }

    #[derive(Default)]
    struct FakeAuth {
        people: Vec<Person>,
        right: Vec<(u32, &'static str)>,
        limits: VecDeque<Limit>,
        cannot_try: bool,
        tries: Vec<(u32, String)>,
    }

    impl Auth for FakeAuth {
        fn people(&mut self) -> Vec<Person> {
            self.people.clone()
        }

        fn unlock(&mut self, person: &Person, secret: &str) -> Unlock {
            self.tries.push((person.uid, secret.to_owned()));
            if self.cannot_try {
                return Unlock::CouldNotTry;
            }
            if self.right.contains(&(person.uid, secret)) {
                return Unlock::Opened;
            }
            Unlock::Wrong(self.limits.pop_front().unwrap_or_default())
        }
    }

    enum Step {
        Pick(Pick),
        Type(&'static str),
        Empty,
        Unavailable,
    }

    #[derive(Default)]
    struct FakeScreen {
        steps: VecDeque<Step>,
        seen: Vec<String>,
        finished: Option<bool>,
    }

    impl FakeScreen {
        fn with(steps: Vec<Step>) -> Self {
            Self {
                steps: steps.into(),
                ..Self::default()
            }
        }
    }

    impl Screen for FakeScreen {
        fn pick(&mut self, people: &[Person], highlighted: usize) -> Pick {
            self.seen
                .push(format!("pick {} from {highlighted}", people.len()));
            match self.steps.pop_front() {
                Some(Step::Pick(pick)) => pick,
                _ => panic!("the screen asked for a pick out of turn"),
            }
        }

        fn ask(&mut self, prompt: &Prompt<'_>) -> Entry {
            self.seen.push(format!(
                "ask {} {} [{}]{}",
                prompt.person.display_name,
                prompt.person.secret.label(),
                prompt.note.text(),
                if prompt.several { " several" } else { "" }
            ));
            match self.steps.pop_front() {
                Some(Step::Type(secret)) => Entry::Secret(Zeroizing::new(secret.to_owned())),
                Some(Step::Empty) => Entry::Empty,
                Some(Step::Unavailable) | None => Entry::Unavailable,
                Some(Step::Pick(_)) => panic!("the screen asked for a secret out of turn"),
            }
        }

        fn checking(&mut self, person: &Person) {
            self.seen.push(format!("checking {}", person.display_name));
        }

        fn finish(&mut self, unlocked: bool) {
            assert!(self.finished.is_none(), "the screen finished twice");
            self.finished = Some(unlocked);
        }
    }

    #[test]
    fn nobody_to_sign_in_leaves_the_screen_untouched() {
        let mut auth = FakeAuth::default();
        let mut screen = FakeScreen::default();
        assert_eq!(run(&mut auth, &mut screen), Exit::NothingToTry);
        assert!(screen.seen.is_empty());
        assert_eq!(screen.finished, None);
    }

    #[test]
    fn one_person_types_their_pin_once() {
        let mut auth = FakeAuth {
            people: vec![person(1000, "Anna", SecretKind::Pin)],
            right: vec![(1000, "123456")],
            ..FakeAuth::default()
        };
        let mut screen = FakeScreen::with(vec![Step::Type("123456")]);
        assert_eq!(run(&mut auth, &mut screen), Exit::Unlocked);
        assert_eq!(screen.seen, ["ask Anna PIN []", "checking Anna"]);
        assert_eq!(screen.finished, Some(true));
        assert_eq!(auth.tries, [(1000, "123456".to_owned())]);
    }

    #[test]
    fn wrong_entries_say_so_with_the_tries_left_or_the_wait() {
        let mut auth = FakeAuth {
            people: vec![person(1000, "Anna", SecretKind::Pin)],
            right: vec![(1000, "123456")],
            limits: VecDeque::from([
                Limit {
                    tries_left: Some(9),
                    wait_seconds: None,
                },
                Limit {
                    tries_left: Some(1),
                    wait_seconds: None,
                },
                Limit::default(),
                Limit {
                    tries_left: Some(0),
                    wait_seconds: Some(7200),
                },
            ]),
            ..FakeAuth::default()
        };
        let mut screen = FakeScreen::with(vec![
            Step::Type("1"),
            Step::Type("2"),
            Step::Type("3"),
            Step::Type("4"),
            Step::Type("123456"),
        ]);
        assert_eq!(run(&mut auth, &mut screen), Exit::Unlocked);
        let asked: Vec<&String> = screen
            .seen
            .iter()
            .filter(|line| line.starts_with("ask"))
            .collect();
        assert_eq!(
            asked,
            [
                "ask Anna PIN []",
                "ask Anna PIN [Incorrect PIN. 9 tries left.]",
                "ask Anna PIN [Incorrect PIN. 1 try left.]",
                "ask Anna PIN [Incorrect PIN. Try again.]",
                "ask Anna PIN [Too many failed attempts. Try again in 2 hours.]",
            ]
        );
    }

    #[test]
    fn an_entry_already_tried_is_not_tried_again() {
        let mut auth = FakeAuth {
            people: vec![person(1000, "Anna", SecretKind::Password)],
            right: vec![(1000, "correct horse")],
            limits: VecDeque::from([Limit {
                tries_left: Some(9),
                wait_seconds: None,
            }]),
            ..FakeAuth::default()
        };
        let mut screen = FakeScreen::with(vec![
            Step::Type("wrong"),
            Step::Type("wrong"),
            Step::Type("correct horse"),
        ]);
        assert_eq!(run(&mut auth, &mut screen), Exit::Unlocked);
        assert_eq!(
            auth.tries,
            [
                (1000, "wrong".to_owned()),
                (1000, "correct horse".to_owned())
            ]
        );
        let asks: Vec<&str> = screen
            .seen
            .iter()
            .map(String::as_str)
            .filter(|line| line.starts_with("ask"))
            .collect();
        assert_eq!(
            asks,
            [
                "ask Anna Password []",
                "ask Anna Password [Incorrect password. 9 tries left.]",
                "ask Anna Password [You already tried that password. Try another.]",
            ]
        );
    }

    #[test]
    fn an_entry_refused_during_a_wait_is_tried_again_after_it() {
        let mut auth = FakeAuth {
            people: vec![person(1000, "Anna", SecretKind::Pin)],
            limits: VecDeque::from([Limit {
                tries_left: None,
                wait_seconds: Some(30),
            }]),
            ..FakeAuth::default()
        };
        let mut screen = FakeScreen::with(vec![Step::Type("123456"), Step::Type("123456")]);
        let exit = run(&mut auth, &mut screen);
        assert_eq!(auth.tries.len(), 2, "the second entry reached the check");
        assert_eq!(exit, Exit::OtherWays);
        assert_eq!(
            screen.seen.get(2).map(String::as_str),
            Some("ask Anna PIN [Too many failed attempts. Try again in 1 minute.]")
        );
    }

    #[test]
    fn several_people_pick_themselves_and_an_empty_entry_goes_back() {
        let mut auth = FakeAuth {
            people: vec![
                person(1000, "Anna", SecretKind::Pin),
                person(1001, "Ben", SecretKind::Password),
            ],
            right: vec![(1001, "hunter22")],
            ..FakeAuth::default()
        };
        let mut screen = FakeScreen::with(vec![
            Step::Pick(Pick::Person(0)),
            Step::Empty,
            Step::Pick(Pick::Person(1)),
            Step::Type("hunter22"),
        ]);
        assert_eq!(run(&mut auth, &mut screen), Exit::Unlocked);
        assert_eq!(
            screen.seen,
            [
                "pick 2 from 0",
                "ask Anna PIN [] several",
                "pick 2 from 0",
                "ask Ben Password [] several",
                "checking Ben",
            ]
        );
        assert_eq!(auth.tries, [(1001, "hunter22".to_owned())]);
    }

    #[test]
    fn other_ways_in_end_the_sign_in() {
        let mut one = FakeAuth {
            people: vec![person(1000, "Anna", SecretKind::Pin)],
            ..FakeAuth::default()
        };
        let mut empty = FakeScreen::with(vec![Step::Empty]);
        assert_eq!(run(&mut one, &mut empty), Exit::OtherWays);
        assert_eq!(empty.finished, Some(false));

        let mut cancelled = FakeScreen::with(vec![Step::Unavailable]);
        assert_eq!(run(&mut one, &mut cancelled), Exit::OtherWays);

        let mut several = FakeAuth {
            people: vec![
                person(1000, "Anna", SecretKind::Pin),
                person(1001, "Ben", SecretKind::Pin),
            ],
            ..FakeAuth::default()
        };
        let mut picked = FakeScreen::with(vec![Step::Pick(Pick::OtherWays)]);
        assert_eq!(run(&mut several, &mut picked), Exit::OtherWays);
        assert_eq!(picked.finished, Some(false));
        assert!(several.tries.is_empty());
    }

    #[test]
    fn a_secret_that_cannot_be_tried_hands_over_to_the_existing_prompts() {
        let mut auth = FakeAuth {
            people: vec![person(1000, "Anna", SecretKind::Pin)],
            cannot_try: true,
            ..FakeAuth::default()
        };
        let mut screen = FakeScreen::with(vec![Step::Type("123456")]);
        assert_eq!(run(&mut auth, &mut screen), Exit::NothingToTry);
        assert_eq!(screen.finished, Some(false));
    }

    #[test]
    fn waits_read_in_minutes_then_hours() {
        assert_eq!(wait(1), "1 minute");
        assert_eq!(wait(61), "2 minutes");
        assert_eq!(wait(89 * 60), "89 minutes");
        assert_eq!(wait(90 * 60), "2 hours");
        assert_eq!(wait(7200), "2 hours");
        assert_eq!(wait(0), "1 minute");
    }
}
