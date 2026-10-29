//! The greeter as the boot's sign-in screen (docs/security/sign-in.md §3.4).
//!
//! The greeter runs as a child in boot mode, with no capabilities, on the
//! display and input devices this process opens for it, and talks to it over
//! a socket pair ([`crate::link`]). Everything that decides stays here: the
//! greeter shows what it is told and answers with the person picked and the
//! secret typed. When it cannot start, or stops answering, the sign-in goes
//! on on the console ([`ConsoleScreen`]).

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd as _, BorrowedFd, OwnedFd, RawFd};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use serde::Deserialize;
use zeroize::Zeroizing;

use crate::flow::{Entry, Pick, Prompt, Screen};
use crate::link::{FromGreeter, Link, Power, ToGreeter};
use crate::people::Person;
use crate::screen::ConsoleScreen;

/// The uid the greeter runs as: nobody, which owns nothing in the initramfs.
const NOBODY: u32 = 65534;

/// Where the greeter is started from and what it is given.
pub struct Launch {
    pub program: PathBuf,
    /// `/dev/dri`, whose first card with a connected display is shown on.
    pub dri: PathBuf,
    /// `/sys/class/drm`, which says which cards have a display connected.
    pub drm_class: PathBuf,
    /// `/dev/input`, whose event devices the greeter reads.
    pub input: PathBuf,
    /// `/etc/group`, for the group that may play sound.
    pub group: PathBuf,
    /// A directory the greeter may write: its home and runtime directory.
    pub runtime: PathBuf,
    /// The volume's `tilo-comforts` token, or null.
    pub comforts: serde_json::Value,
}

/// The greeter while it runs.
struct Running {
    link: Link,
    /// `None` for a greeter in a test, which is not a process.
    child: Option<Child>,
}

impl Running {
    fn stop(self) {
        // It holds nothing to save: what it showed is over.
        drop(self.link);
        if let Some(mut child) = self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// An answer the greeter gave before the flow asked for it: the flow asks
/// for the person and the secret one after the other, the greeter's screen
/// takes both at once.
enum Answer {
    Secret {
        person: usize,
        secret: Zeroizing<String>,
    },
    CantSignIn,
}

pub struct GreeterScreen {
    greeter: Option<Running>,
    /// The console, when the greeter goes away. It holds Plymouth, which
    /// gives the display up to the greeter and takes it back after.
    fallback: ConsoleScreen,
    /// The people the greeter was shown, whose indices it answers with.
    listed: Vec<Person>,
    answer: Option<Answer>,
    systemctl: PathBuf,
}

impl GreeterScreen {
    /// Start the greeter on the display, or give `fallback` back when there
    /// is no display to show it on or it cannot start; the console then asks.
    ///
    /// Plymouth gives up the display while the greeter has it.
    pub fn start(
        launch: &Launch,
        fallback: ConsoleScreen,
        systemctl: PathBuf,
    ) -> Result<Self, ConsoleScreen> {
        if let Some(plymouth) = &fallback.plymouth {
            plymouth.deactivate();
        }
        match spawn(launch).and_then(|greeter| hello(greeter, &launch.comforts)) {
            Ok(greeter) => {
                eprintln!("tilo-boot-signin: the greeter asks");
                Ok(Self {
                    greeter: Some(greeter),
                    fallback,
                    listed: Vec::new(),
                    answer: None,
                    systemctl,
                })
            }
            Err(error) => {
                eprintln!(
                    "tilo-boot-signin: the greeter cannot start ({error}); asking on the console"
                );
                // The splash comes back until the console asks, and stays when
                // nothing is asked here.
                if let Some(plymouth) = &fallback.plymouth {
                    plymouth.reactivate();
                }
                Err(fallback)
            }
        }
    }

    /// Hand the sign-in to the console. The splash comes back until the
    /// console asks, and stays when nothing more is asked here.
    fn fall_back(&mut self, error: &io::Error) {
        if let Some(greeter) = self.greeter.take() {
            eprintln!(
                "tilo-boot-signin: the greeter stopped answering ({error}); asking on the console"
            );
            greeter.stop();
            if let Some(plymouth) = &self.fallback.plymouth {
                plymouth.reactivate();
            }
        }
        self.answer = None;
    }

    fn tell(&mut self, message: &ToGreeter) -> bool {
        let Some(greeter) = &self.greeter else {
            return false;
        };
        match greeter.link.send_to_greeter(message) {
            Ok(()) => true,
            Err(error) => {
                self.fall_back(&error);
                false
            }
        }
    }

    /// The greeter's next answer about who signs in and with what. The power
    /// menu is acted on here, since the greeter may not.
    fn answer(&mut self) -> Option<Answer> {
        loop {
            let greeter = self.greeter.as_ref()?;
            match greeter.link.recv_from_greeter() {
                Ok(FromGreeter::Secret { person, secret }) if person < self.listed.len() => {
                    return Some(Answer::Secret { person, secret });
                }
                Ok(FromGreeter::Secret { .. }) => {
                    self.fall_back(&io::Error::new(
                        io::ErrorKind::InvalidData,
                        "it answered for someone it was not shown",
                    ));
                    return None;
                }
                Ok(FromGreeter::CantSignIn) => return Some(Answer::CantSignIn),
                Ok(FromGreeter::Power(power)) => self.power(power),
                Err(error) => {
                    self.fall_back(&error);
                    return None;
                }
            }
        }
    }

    fn power(&self, power: Power) {
        let verb = match power {
            Power::PowerOff => "poweroff",
            Power::Reboot => "reboot",
        };
        eprintln!("tilo-boot-signin: {verb} asked at the sign-in screen");
        let _ = Command::new(&self.systemctl)
            .arg(verb)
            .stdin(Stdio::null())
            .status();
    }
}

impl Screen for GreeterScreen {
    fn pick(&mut self, people: &[Person], highlighted: usize) -> Pick {
        let answer = match self.answer.take() {
            Some(answer) => Some(answer),
            None if self.tell(&ToGreeter::People {
                people: people.to_vec(),
                highlighted,
            }) =>
            {
                self.listed = people.to_vec();
                self.answer()
            }
            None => None,
        };
        match answer {
            Some(Answer::Secret { person, secret }) => {
                self.answer = Some(Answer::Secret { person, secret });
                Pick::Person(person)
            }
            Some(Answer::CantSignIn) => Pick::OtherWays,
            None => self.fallback.pick(people, highlighted),
        }
    }

    fn ask(&mut self, prompt: &Prompt<'_>) -> Entry {
        if self.greeter.is_none() {
            return self.fallback.ask(prompt);
        }
        // With one person there is no list, so nothing told the greeter who
        // signs in yet.
        if self.listed.is_empty() {
            if !self.tell(&ToGreeter::People {
                people: vec![prompt.person.clone()],
                highlighted: 0,
            }) {
                return self.fallback.ask(prompt);
            }
            self.listed = vec![prompt.person.clone()];
        }
        let Some(asked) = self
            .listed
            .iter()
            .position(|person| person == prompt.person)
        else {
            self.fall_back(&io::Error::new(
                io::ErrorKind::InvalidInput,
                "asked for someone it was not shown",
            ));
            return self.fallback.ask(prompt);
        };
        let answer = match self.answer.take() {
            Some(answer) => Some(answer),
            None if self.tell(&ToGreeter::Ask {
                person: asked,
                note: prompt.note,
                several: prompt.several,
            }) =>
            {
                self.answer()
            }
            None => None,
        };
        match answer {
            Some(Answer::Secret { person, secret }) if person == asked => Entry::Secret(secret),
            // Someone else on the list typed theirs, or "Can't sign in" was
            // chosen: back to the list, which takes the answer.
            Some(answer) if prompt.several => {
                self.answer = Some(answer);
                Entry::Empty
            }
            Some(_) => Entry::Empty,
            None => self.fallback.ask(prompt),
        }
    }

    fn checking(&mut self, person: &Person) {
        if self.greeter.is_some() {
            self.tell(&ToGreeter::Checking);
        } else {
            self.fallback.checking(person);
        }
    }

    fn finish(&mut self, unlocked: bool) {
        match self.greeter.take() {
            Some(greeter) => {
                let _ = greeter
                    .link
                    .send_to_greeter(&ToGreeter::Finish { unlocked });
                greeter.stop();
                if let Some(plymouth) = &self.fallback.plymouth {
                    plymouth.reactivate();
                }
            }
            None => self.fallback.finish(unlocked),
        }
    }
}

impl Drop for GreeterScreen {
    fn drop(&mut self) {
        if let Some(greeter) = self.greeter.take() {
            greeter.stop();
            if let Some(plymouth) = &self.fallback.plymouth {
                plymouth.reactivate();
            }
        }
    }
}

/// Tell the greeter the device's comforts, which it reads before anything
/// else: its language, keymap and voice come from them.
fn hello(greeter: Running, comforts: &serde_json::Value) -> io::Result<Running> {
    let told = greeter.link.send_to_greeter(&ToGreeter::Hello {
        comforts: comforts.clone(),
    });
    match told {
        Ok(()) => Ok(greeter),
        Err(error) => {
            greeter.stop();
            Err(error)
        }
    }
}

/// The descriptors the greeter is handed.
struct Handed {
    card: OwnedFd,
    inputs: Vec<(PathBuf, OwnedFd)>,
}

fn spawn(launch: &Launch) -> io::Result<Running> {
    let handed = Handed {
        card: open_card(&launch.dri, &launch.drm_class)?,
        inputs: open_inputs(&launch.input),
    };
    let gid = audio_group(&launch.group).unwrap_or(NOBODY);
    let runtime = &launch.runtime;
    match fs::DirBuilder::new().mode(0o700).create(runtime) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    rustix::fs::chown(runtime, Some(uid(NOBODY)), Some(gid_of(gid)))?;
    let (ours, theirs) = Link::pair()?;
    let theirs = theirs.into_fd();
    let mut command = Command::new(&launch.program);
    command
        .arg("--boot")
        .arg("--link-fd")
        .arg(theirs.as_raw_fd().to_string())
        .arg("--card-fd")
        .arg(handed.card.as_raw_fd().to_string());
    for (path, fd) in &handed.inputs {
        command
            .arg("--input")
            .arg(format!("{}:{}", fd.as_raw_fd(), path.display()));
    }
    let kept: Vec<RawFd> = std::iter::once(theirs.as_raw_fd())
        .chain(std::iter::once(handed.card.as_raw_fd()))
        .chain(handed.inputs.iter().map(|(_, fd)| fd.as_raw_fd()))
        .collect();
    command
        .env_clear()
        .env("PATH", "/usr/bin:/usr/sbin")
        .env("HOME", runtime)
        .env("XDG_RUNTIME_DIR", runtime)
        .env("XDG_CONFIG_HOME", runtime.join("config"))
        .env("XDG_CACHE_HOME", runtime.join("cache"))
        .env("XDG_DATA_HOME", runtime.join("data"))
        .stdin(Stdio::null())
        .uid(NOBODY)
        .gid(gid);
    if let Some(log) = std::env::var_os("RUST_LOG") {
        command.env("RUST_LOG", log);
    }
    // SAFETY: between fork and exec the closure only makes system calls,
    // which allocate nothing and take no lock.
    unsafe {
        command.pre_exec(move || {
            for fd in &kept {
                rustix::io::fcntl_setfd(BorrowedFd::borrow_raw(*fd), rustix::io::FdFlags::empty())?;
            }
            // Nothing it runs may gain what it lacks.
            rustix::thread::set_no_new_privs(true)?;
            Ok(())
        });
    }
    let child = command.spawn()?;
    // The descriptors are the greeter's now; this process keeps none of
    // them, so the display's master goes with the greeter when it ends.
    drop(handed);
    drop(theirs);
    Ok(Running {
        link: ours,
        child: Some(child),
    })
}

fn uid(raw: u32) -> rustix::fs::Uid {
    rustix::fs::Uid::from_raw(raw)
}

fn gid_of(raw: u32) -> rustix::fs::Gid {
    rustix::fs::Gid::from_raw(raw)
}

/// The first card that drives a connected display, opened as its master.
fn open_card(dri: &Path, drm_class: &Path) -> io::Result<OwnedFd> {
    let mut cards: Vec<String> = fs::read_dir(dri)?
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| {
            name.strip_prefix("card")
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        })
        .collect();
    cards.sort_by_key(|name| {
        name.trim_start_matches("card")
            .parse::<u32>()
            .unwrap_or(u32::MAX)
    });
    for card in cards {
        if !connected(drm_class, &card) {
            continue;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(rustix::fs::OFlags::CLOEXEC.bits().cast_signed())
            .open(dri.join(&card))?;
        let fd = OwnedFd::from(file);
        set_master(&fd)?;
        return Ok(fd);
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "no display is connected",
    ))
}

/// Whether one of `card`'s connectors has a display on it, as sysfs says.
fn connected(drm_class: &Path, card: &str) -> bool {
    let prefix = format!("{card}-");
    fs::read_dir(drm_class).is_ok_and(|entries| {
        entries.filter_map(Result::ok).any(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(&prefix))
                && fs::read_to_string(entry.path().join("status"))
                    .is_ok_and(|status| status.trim() == "connected")
        })
    })
}

/// `DRM_IOCTL_SET_MASTER`: Plymouth has let go of the display, so this
/// process (root) takes it, and the greeter inherits it with the descriptor.
fn set_master(fd: &OwnedFd) -> io::Result<()> {
    const SET_MASTER: rustix::ioctl::Opcode = rustix::ioctl::opcode::none(b'd', 0x1e);
    // SAFETY: SET_MASTER takes no argument and only acts on the descriptor.
    unsafe { rustix::ioctl::ioctl(fd, rustix::ioctl::NoArg::<SET_MASTER>::new()) }?;
    Ok(())
}

/// Every event device, each with its path. A device that does not open is
/// left out; the greeter shows its on-screen keyboard if no keyboard is left.
fn open_inputs(input: &Path) -> Vec<(PathBuf, OwnedFd)> {
    let Ok(entries) = fs::read_dir(input) else {
        return Vec::new();
    };
    let mut inputs: Vec<(PathBuf, OwnedFd)> = entries
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("event"))
        })
        .filter_map(|entry| {
            let path = entry.path();
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(
                    (rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NONBLOCK)
                        .bits()
                        .cast_signed(),
                )
                .open(&path)
                .ok()?;
            Some((path, OwnedFd::from(file)))
        })
        .collect();
    inputs.sort_by(|a, b| a.0.cmp(&b.0));
    inputs
}

/// The `audio` group's id in `group` (an `/etc/group`), so the greeter may
/// open the sound devices.
fn audio_group(group: &Path) -> Option<u32> {
    fs::read_to_string(group).ok()?.lines().find_map(|line| {
        let mut fields = line.split(':');
        (fields.next()? == "audio").then_some(())?;
        fields.nth(1)?.parse().ok()
    })
}

#[derive(Deserialize)]
struct Metadata {
    #[serde(default)]
    tokens: BTreeMap<String, serde_json::Value>,
}

/// The `tilo-comforts` token in `cryptsetup luksDump --dump-json-metadata`
/// output; of several, the lowest id. Null when there is none. Its values are
/// not read here: the greeter clamps them as it applies them.
pub fn comforts(metadata: &str) -> serde_json::Value {
    let Ok(metadata) = serde_json::from_str::<Metadata>(metadata) else {
        return serde_json::Value::Null;
    };
    let mut tokens: Vec<(u32, serde_json::Value)> = metadata
        .tokens
        .into_iter()
        .filter_map(|(id, token)| Some((id.parse().ok()?, token)))
        .filter(|(_, token)| {
            token.get("type").and_then(serde_json::Value::as_str) == Some("tilo-comforts")
        })
        .collect();
    tokens.sort_by_key(|(id, _)| *id);
    tokens
        .into_iter()
        .next()
        .map_or(serde_json::Value::Null, |(_, token)| token)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::thread::JoinHandle;

    use super::*;
    use crate::flow::{self, Auth, Exit, Note, Unlock};
    use crate::people::SecretKind;

    fn people(count: usize) -> Vec<Person> {
        ["Anna", "Ben"]
            .iter()
            .zip(1000..)
            .take(count)
            .map(|(name, uid)| Person {
                uid,
                name: name.to_lowercase(),
                display_name: (*name).to_owned(),
                secret: SecretKind::Pin,
            })
            .collect()
    }

    struct FakeAuth {
        people: Vec<Person>,
        right: (u32, &'static str),
        tries: Vec<(u32, String)>,
    }

    impl Auth for FakeAuth {
        fn people(&mut self) -> Vec<Person> {
            self.people.clone()
        }

        fn unlock(&mut self, person: &Person, secret: &str) -> Unlock {
            self.tries.push((person.uid, secret.to_owned()));
            if (person.uid, secret) == self.right {
                Unlock::Opened
            } else {
                Unlock::Wrong(flow::Limit {
                    tries_left: Some(4),
                    wait_seconds: None,
                })
            }
        }
    }

    enum Act {
        Secret(usize, &'static str),
        CantSignIn,
        Power(Power),
    }

    /// A greeter that answers each list and each question with the next of
    /// `acts`, and returns everything it was told.
    fn greeter(link: Link, acts: Vec<Act>) -> JoinHandle<Vec<ToGreeter>> {
        let mut acts = VecDeque::from(acts);
        std::thread::spawn(move || {
            let mut told = Vec::new();
            while let Ok(message) = link.recv_from_supervisor() {
                // One person is shown without a list: the question that
                // follows is what takes the answer.
                let answers = match &message {
                    ToGreeter::People { people, .. } => people.len() > 1,
                    ToGreeter::Ask { .. } => true,
                    _ => false,
                };
                told.push(message);
                if !answers {
                    continue;
                }
                loop {
                    let reply = match acts.pop_front() {
                        Some(Act::Secret(person, secret)) => FromGreeter::Secret {
                            person,
                            secret: Zeroizing::new(secret.to_owned()),
                        },
                        Some(Act::CantSignIn) => FromGreeter::CantSignIn,
                        Some(Act::Power(power)) => {
                            link.send_to_supervisor(&FromGreeter::Power(power)).unwrap();
                            continue;
                        }
                        // Out of acts: the greeter goes away.
                        None => return told,
                    };
                    link.send_to_supervisor(&reply).unwrap();
                    break;
                }
            }
            told
        })
    }

    fn fallback() -> ConsoleScreen {
        ConsoleScreen {
            ask_password: PathBuf::from("/nonexistent/ask-password"),
            plymouth: None,
            volume: "root".into(),
        }
    }

    fn screen(link: Link, systemctl: PathBuf) -> GreeterScreen {
        GreeterScreen {
            greeter: Some(Running { link, child: None }),
            fallback: fallback(),
            listed: Vec::new(),
            answer: None,
            systemctl,
        }
    }

    fn run(count: usize, acts: Vec<Act>, systemctl: PathBuf) -> (Exit, FakeAuth, Vec<ToGreeter>) {
        let (ours, theirs) = Link::pair().unwrap();
        let fake = greeter(theirs, acts);
        let mut auth = FakeAuth {
            people: people(count),
            right: (1001, "2468"),
            tries: Vec::new(),
        };
        let exit = flow::run(&mut auth, &mut screen(ours, systemctl));
        (exit, auth, fake.join().unwrap())
    }

    #[test]
    fn the_person_and_secret_typed_on_the_greeter_open_the_volume() {
        let (exit, auth, told) = run(2, vec![Act::Secret(1, "2468")], PathBuf::new());
        assert_eq!(exit, Exit::Unlocked);
        assert_eq!(auth.tries, [(1001, "2468".to_owned())]);
        assert_eq!(
            told,
            [
                ToGreeter::People {
                    people: people(2),
                    highlighted: 0
                },
                ToGreeter::Checking,
                ToGreeter::Finish { unlocked: true },
            ]
        );
    }

    #[test]
    fn a_wrong_secret_is_noted_and_someone_else_may_then_sign_in() {
        let (exit, auth, told) = run(
            2,
            vec![Act::Secret(0, "1111"), Act::Secret(1, "2468")],
            PathBuf::new(),
        );
        assert_eq!(exit, Exit::Unlocked);
        assert_eq!(
            auth.tries,
            [(1000, "1111".to_owned()), (1001, "2468".to_owned())]
        );
        assert_eq!(
            told,
            [
                ToGreeter::People {
                    people: people(2),
                    highlighted: 0
                },
                ToGreeter::Checking,
                ToGreeter::Ask {
                    person: 0,
                    note: Note::Wrong {
                        secret: SecretKind::Pin,
                        tries_left: Some(4)
                    },
                    several: true
                },
                ToGreeter::Checking,
                ToGreeter::Finish { unlocked: true },
            ]
        );
    }

    #[test]
    fn one_person_is_shown_alone_and_cant_sign_in_is_the_other_ways() {
        let (exit, auth, told) = run(1, vec![Act::CantSignIn], PathBuf::new());
        assert_eq!(exit, Exit::OtherWays);
        assert!(auth.tries.is_empty());
        assert_eq!(
            told,
            [
                ToGreeter::People {
                    people: people(1),
                    highlighted: 0
                },
                ToGreeter::Ask {
                    person: 0,
                    note: Note::None,
                    several: false
                },
                ToGreeter::Finish { unlocked: false },
            ]
        );
        let (exit, _, _) = run(
            2,
            vec![Act::Secret(0, "1"), Act::CantSignIn],
            PathBuf::new(),
        );
        assert_eq!(exit, Exit::OtherWays);
    }

    #[test]
    fn the_power_menu_is_carried_out_here() {
        let _serial = crate::test_support::serial();
        let dir = tempfile::tempdir().unwrap();
        let systemctl = dir.path().join("systemctl");
        fs::write(
            &systemctl,
            format!("#!/bin/sh\necho \"$*\" >>{}/asked\n", dir.path().display()),
        )
        .unwrap();
        fs::set_permissions(
            &systemctl,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        let (exit, _, _) = run(
            2,
            vec![Act::Power(Power::Reboot), Act::Secret(1, "2468")],
            systemctl,
        );
        assert_eq!(exit, Exit::Unlocked);
        assert_eq!(
            fs::read_to_string(dir.path().join("asked")).unwrap(),
            "reboot\n"
        );
    }

    #[test]
    fn a_greeter_that_goes_away_leaves_the_screen_to_the_console() {
        // The console's agent is missing here, so its ask is unavailable:
        // the flow ends with the other ways, not with the greeter's answer.
        let (exit, auth, _) = run(2, vec![Act::Secret(0, "1111")], PathBuf::new());
        assert_eq!(auth.tries, [(1000, "1111".to_owned())]);
        assert_eq!(exit, Exit::OtherWays);
    }

    #[test]
    fn an_answer_for_someone_not_shown_ends_the_greeter() {
        let (exit, auth, _) = run(1, vec![Act::Secret(3, "2468")], PathBuf::new());
        assert!(auth.tries.is_empty());
        assert_eq!(exit, Exit::OtherWays);
    }

    #[test]
    fn the_greeter_hears_the_comforts_first() {
        let (ours, theirs) = Link::pair().unwrap();
        let comforts = serde_json::json!({"screen_reader": true});
        let running = hello(
            Running {
                link: ours,
                child: None,
            },
            &comforts,
        )
        .unwrap();
        assert_eq!(
            theirs.recv_from_supervisor().unwrap(),
            ToGreeter::Hello { comforts }
        );
        running.stop();
    }

    #[test]
    fn the_comforts_token_is_found_in_the_header() {
        let metadata = r#"{"keyslots":{},"tokens":{
            "3":{"type":"tilo-comforts","keyslots":[],"screen_reader":false},
            "1":{"type":"tilo-signin","keyslots":["1"]},
            "2":{"type":"tilo-comforts","keyslots":[],"screen_reader":true}
        }}"#;
        assert_eq!(comforts(metadata)["screen_reader"], serde_json::json!(true));
        assert_eq!(comforts("{}"), serde_json::Value::Null);
        assert_eq!(comforts("not json"), serde_json::Value::Null);
    }

    #[test]
    fn the_audio_group_is_read_from_the_group_file() {
        let dir = tempfile::tempdir().unwrap();
        let group = dir.path().join("group");
        fs::write(&group, "root:x:0:\nvideo:x:39:\naudio:x:63:\n").unwrap();
        assert_eq!(audio_group(&group), Some(63));
        fs::write(&group, "root:x:0:\n").unwrap();
        assert_eq!(audio_group(&group), None);
    }
}
