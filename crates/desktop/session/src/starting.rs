//! The wait for this computer's daemon to answer who this account is to it, in front of the
//! shell: an account that signed in with no identity of its own has its person made and taken up
//! here, and a daemon that is still starting answers that later than a screen would like.
//!
//! The wait is the daemon's to give, never a fixed one: a start that waits out what the network
//! answers — a relay's net report among it — answers `account#who` when it can, and the person the
//! account acts as is taken up then. A daemon that answers a failure with the system unit's words
//! stands in place of the shell, as a login it did not start does; one of this account's own that
//! could not answer leaves the session with the setup the device still needs, as it did before.
//!
//! Nothing here polls. The asking runs on a worker, which wakes the session's loop with what it
//! found.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::task::Waker;

use tilo_onboarding_session::ceremony::Ceremony;
use ui::widget::{Container, EmptyState};

use crate::i18n::t;
use crate::startup;

/// What the worker asked of the daemon found.
enum Answer {
    /// The person this account acts as was taken up here (`taken`: made or joined now, so first
    /// use is due), or there was none to take up.
    Settled { taken: bool },
    /// The daemon answered a failure, saying why.
    Failed(String),
}

/// What [`StartingWait::pump`] found this pass.
pub(crate) enum StartingStep {
    /// Still waiting for the daemon's answer.
    Waiting,
    /// The person was taken up: `Some` is the setup this device still needs, and `None` opens the
    /// shell.
    Settled(Option<Box<Ceremony>>),
    /// The daemon answered a failure with the system unit's words: they stand where the shell
    /// would, rather than a desktop that keeps nothing.
    Failed(String),
}

/// The wait for the daemon to answer who this account is to it.
pub struct StartingWait {
    /// The directory the identity the person is taken up into lives in.
    identity_dir: PathBuf,
    /// Whether this session is an operating system Tilo does not control, which chooses the setup
    /// the person is taken up into.
    host_desktop: bool,
    /// Wakes the session's loop when the daemon's answer changes what the wait shows.
    waiter: Waker,
    /// Where [`Self::pump`] learns that the daemon answered.
    answer: Option<Receiver<Answer>>,
    /// The answer the wait already has, when no worker could be started to bring one.
    answered: Option<Answer>,
    /// The shell takes over: the person was taken up and no setup is due.
    done: bool,
}

impl StartingWait {
    /// Wait for the daemon to answer who this account is to it, taking the person up into
    /// `identity_dir`. Nothing asks yet: [`Self::listening`] arms the wait, which happens as the
    /// surface showing it is mounted.
    pub(crate) fn new(identity_dir: PathBuf, host_desktop: bool) -> Self {
        Self {
            identity_dir,
            host_desktop,
            waiter: Waker::noop().clone(),
            answer: None,
            answered: None,
            done: false,
        }
    }

    /// Arm the wait: ask the daemon on a worker, which wakes `waiter` with what it found.
    pub(crate) fn listening(mut self, waiter: Waker) -> Self {
        self.waiter = waiter;
        self.ask();
        self
    }

    /// Whether the shell takes the wait's place.
    pub(crate) fn done(&self) -> bool {
        self.done
    }

    /// Advance the wait: take the daemon's answer, when it came.
    pub(crate) fn pump(&mut self) -> StartingStep {
        let answered = match self.answered.take() {
            Some(answered) => answered,
            None => match self.answer.as_ref().map(Receiver::try_recv) {
                None | Some(Err(TryRecvError::Empty)) => return StartingStep::Waiting,
                Some(Ok(answer)) => answer,
                Some(Err(TryRecvError::Disconnected)) => {
                    Answer::Failed("the daemon's answer was lost".to_owned())
                }
            },
        };
        self.answer = None;
        match answered {
            Answer::Settled { taken } => StartingStep::Settled(
                startup::setup_due_now(self.host_desktop, taken).map(Box::new),
            ),
            Answer::Failed(why) => {
                // A daemon of this account's own that could not make the person leaves the
                // session with the setup the device still needs, as it always did; the system
                // unit's starts every login's shell, and the person reads its words instead of a
                // desktop that would keep nothing.
                if startup::stands_in_place_of_the_shell() {
                    return StartingStep::Failed(why);
                }
                tracing::warn!(%why, "this account's person is not taken up here");
                StartingStep::Settled(
                    startup::setup_due_now(self.host_desktop, false).map(Box::new),
                )
            }
        }
    }

    /// Show the shell in the wait's place.
    pub(crate) fn finish(&mut self) {
        self.done = true;
    }

    /// Ask the daemon who this account is, on a worker, and wake the loop with what it found.
    fn ask(&mut self) {
        let identity_dir = self.identity_dir.clone();
        let waiter = self.waiter.clone();
        let (sender, answer) = channel();
        let spawned = std::thread::Builder::new()
            .name("tilo-identity-starting".into())
            .spawn(move || {
                let found = startup::found_here(&identity_dir);
                // The receiver may be gone with the session: nothing to tell.
                let _ = sender.send(match found {
                    Ok(taken) => Answer::Settled { taken },
                    Err(why) => Answer::Failed(why),
                });
                waiter.wake_by_ref();
            });
        match spawned {
            Ok(_) => self.answer = Some(answer),
            Err(error) => {
                tracing::warn!(%error, "the wait for this computer's daemon did not start");
                self.answered = Some(Answer::Failed(format!(
                    "the wait for this computer's daemon did not start: {error}"
                )));
            }
        }
    }

    /// What the wait shows: that Tilo is starting and that the desktop opens by itself.
    pub(crate) fn screen() -> Container {
        crate::setup::on_ground(Box::new(
            Container::new()
                .filling()
                .with_centered_max_width(560.0)
                .with_child(Box::new(
                    EmptyState::new(t("session-starting-title"))
                        .with_message(t("session-starting-body")),
                )),
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead as _, Write as _};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use super::*;

    /// The wait holds until this computer's daemon answers who the account is to it — a start that
    /// waits out what the network answers gives its answer tens of seconds later — and takes the
    /// person the daemon names up then. A fixed wait's end is never read as the device's answer.
    #[test]
    fn the_wait_takes_the_person_up_whenever_the_daemon_answers() {
        let mut home = tilo_test_env::TestHome::new().expect("a home");
        tilo_identity::sources::test_trust::install();
        let identity_dir =
            tilo_identity::store::account_identity_dir(&home.config().join("tilo/identity"));
        std::fs::create_dir_all(&identity_dir).expect("the account's directory");
        let root = home.root().join("system");
        home.env().set("TILO_IDENTITY_ROOT", &root);
        // This computer enrolled the person, and the account's own identity is not made yet.
        let _key = tilo_identity::provisioning::provisioned_as_its_daemon(
            &identity_dir,
            &tilo_identity::provisioning::collaboration_identity_paths(),
            tilo_identity::provisioning::IdentityStorage::Files,
        )
        .expect("this computer's identity");
        let paths = tilo_identity::provisioning::collaboration_identity_paths();
        let cert = tilo_device_identity::persist::read_owner_cert(&paths)
            .expect("this device's certificate")
            .expect("the owner's certificate");
        let person = cert.user_public_key;
        std::fs::remove_dir_all(&identity_dir).expect("the account's identity is not made yet");
        // The daemon at a profile this test names, answering who the account is when the test
        // lets it.
        let profile = home.root().join("profile");
        std::fs::create_dir_all(&profile).expect("the daemon's profile");
        home.env().set(tilo_machine::DAEMON_PROFILE_VAR, &profile);
        let place = tilo_api::daemon::this_computer().expect("the daemon's place");
        let socket = place.native();
        if let Some(directory) = socket.parent() {
            std::fs::create_dir_all(directory).expect("the daemon's socket directory");
        }
        let listener =
            std::os::unix::net::UnixListener::bind(&socket).expect("the daemon's socket");
        listener.set_nonblocking(true).expect("a socket looked at");
        let answer = serde_json::json!({
            "device": hex::encode(cert.device_public_key),
            "person": hex::encode(person),
            "claimed": true,
        })
        .to_string();
        let stop = Arc::new(AtomicBool::new(false));
        let answered = Arc::new(AtomicBool::new(false));
        let answering = {
            let stop = Arc::clone(&stop);
            let answered = Arc::clone(&answered);
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let mut request = String::new();
                            let _ = std::io::BufReader::new(
                                stream.try_clone().expect("its reading half"),
                            )
                            .read_line(&mut request);
                            while !answered.load(Ordering::SeqCst) {
                                std::thread::sleep(Duration::from_millis(2));
                            }
                            let _ = writeln!(stream, "{answer}");
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(2));
                        }
                        Err(error) => panic!("the daemon's endpoint failed: {error}"),
                    }
                }
            })
        };
        let mut wait =
            StartingWait::new(identity_dir.clone(), false).listening(Waker::noop().clone());
        assert!(!wait.done(), "the shell waits for the daemon's answer");
        assert!(matches!(wait.pump(), StartingStep::Waiting));
        // The daemon answers, and the person is taken up.
        answered.store(true, Ordering::SeqCst);
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match wait.pump() {
                StartingStep::Waiting => {
                    assert!(
                        Instant::now() < deadline,
                        "the daemon answered but the wait never took it up"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
                StartingStep::Settled(_) => break,
                StartingStep::Failed(why) => panic!("the wait failed: {why}"),
            }
        }
        let taken = tilo_identity::store::resolve_active_identity(&identity_dir)
            .and_then(|path| tilo_identity::store::load_metadata_from_path(&path).ok())
            .expect("the person this account is taken up as");
        assert_eq!(
            taken.group_public_key, person,
            "the account acts as the person this computer's device certificate names"
        );
        stop.store(true, Ordering::SeqCst);
        answering.join().expect("the daemon's endpoint");
    }
}
