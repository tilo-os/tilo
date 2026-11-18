//! Machines that ask to join over the internet, as this session answers them.
//!
//! The session starts the enrollment helper (`tilo-enroll approve --iroh --ask`)
//! while the person is adding a device: the helper answers machines over iroh
//! and prints one JSON line per machine that wants to join. This module keeps
//! those asks, shows each as a notification ([`notice`]) that opens the sheet,
//! and writes the person's answer back to the helper. Approving admits the
//! machine; a `tilo://enroll?…` link a machine with no token printed is
//! approved by pasting it in through [`scan`].
//!
//! The helper does the admitting with the person's own key, so the session
//! never holds a signing capability of its own: it only carries the person's
//! answer to what the machine asked.

use std::collections::BTreeMap;
use std::io::{BufRead as _, BufReader, Write as _};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tilo_lock::MutexRecover as _;

use crate::i18n::{t, t_args};
use crate::platform::notice::Notice;

/// The enrollment helper the image installs.
const ENROLL: &str = tilo_identity::device_admin::ENROLL;

/// How long the helper serves one window: the person answers a machine that
/// wants in, and the window closes on its own.
const WINDOW_SECONDS: u64 = 600;

/// How long the helper may take to reach a machine whose link was pasted: the
/// machine's own window, and the exchange after it answers.
const DIAL_SECONDS: u64 = 270;

/// One machine asking to join, as the helper reported it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Waiting {
    pub(crate) ask: String,
    pub(crate) device: String,
    pub(crate) service: String,
    pub(crate) label: String,
    #[serde(default)]
    pub(crate) hardware: String,
    pub(crate) code: String,
}

/// The helper a session starts to answer machines, and the machines it has
/// put to the person.
pub(crate) struct Serving {
    child: Child,
    stdin: ChildStdin,
    waiting: Arc<Mutex<BTreeMap<String, Waiting>>>,
}

impl Serving {
    /// Start answering machines with the enrollment helper, as the person
    /// whose identity `root` and `dir` are: the helper prints each machine
    /// that wants to join and waits for the person's answer here.
    ///
    /// # Errors
    ///
    /// The helper cannot be started, or its answers cannot be read.
    pub(crate) fn start(root: &std::path::Path, dir: &std::path::Path) -> Result<Self, String> {
        let mut command = Command::new(ENROLL);
        command
            .args(["approve", "--iroh", "--ask"])
            .arg("--identity-root")
            .arg(root)
            .arg("--identity-dir")
            .arg(dir)
            .args(["--timeout", &WINDOW_SECONDS.to_string()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        // A machine behind a network address translation reaches this device
        // only through the relay it names in the token.
        if let Some(relay) = own_relay() {
            command.arg("--relay").arg(relay);
        }
        Self::over(command)
    }

    /// [`Self::start`] with the program and arguments a test builds.
    fn over(mut command: Command) -> Result<Self, String> {
        let mut child = command
            .spawn()
            .map_err(|error| format!("this session cannot answer machines: {error}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "the enrollment helper takes no answer".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "the enrollment helper says nothing".to_string())?;
        let waiting: Arc<Mutex<BTreeMap<String, Waiting>>> = Arc::default();
        let reading = Arc::clone(&waiting);
        std::thread::Builder::new()
            .name("tilo-enrollment-answers".into())
            .spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else {
                        return;
                    };
                    let Some(value): Option<serde_json::Value> = tilo_failure::ignored!(
                        "a helper line that is not JSON reports no machine",
                        serde_json::from_str(&line).ok()
                    ) else {
                        continue;
                    };
                    // The helper answers each ask it takes, and reports the next
                    // machine; the machine's own line has no `approved`.
                    if value.get("approved").is_some() || value.get("decline").is_some() {
                        if let Some(ask) = value.get("ask").and_then(|ask| ask.as_str()) {
                            reading.lock_recover().remove(ask);
                        }
                        continue;
                    }
                    let Some(machine): Option<Waiting> = tilo_failure::ignored!(
                        "a helper line that is not a machine reports none",
                        serde_json::from_value(value).ok()
                    ) else {
                        continue;
                    };
                    reading.lock_recover().insert(machine.ask.clone(), machine);
                }
            })
            .map_err(|error| format!("this session cannot answer machines: {error}"))?;
        Ok(Self {
            child,
            stdin,
            waiting,
        })
    }

    /// The machines waiting for the person's answer now.
    pub(crate) fn waiting(&self) -> Vec<Waiting> {
        self.waiting.lock_recover().values().cloned().collect()
    }

    /// Answer one waiting machine: `approve` admits it, `false` turns it away.
    ///
    /// # Errors
    ///
    /// The helper stopped taking answers.
    pub(crate) fn answer(&mut self, ask: &str, approve: bool) -> Result<(), String> {
        let answer = if approve { "approve" } else { "decline" };
        writeln!(self.stdin, "{answer} {ask}")
            .and_then(|()| self.stdin.flush())
            .map_err(|error| format!("the enrollment helper stopped taking answers: {error}"))?;
        self.waiting.lock_recover().remove(ask);
        Ok(())
    }
}

impl Drop for Serving {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Let a machine with no token in, by its own `tilo://enroll?…` line: the
/// helper dials it and approves it there. Nothing is served on this device.
///
/// # Errors
///
/// The helper cannot be started, or the line names no machine.
pub(crate) fn scan(
    line: &str,
    root: &std::path::Path,
    dir: &std::path::Path,
) -> Result<std::process::Child, String> {
    if !line.trim().starts_with("tilo://enroll?") {
        return Err("paste the machine's link, which starts with tilo://enroll?".into());
    }
    let mut command = Command::new(ENROLL);
    command
        .args(["approve", "--iroh", "--dial", line.trim()])
        .arg("--identity-root")
        .arg(root)
        .arg("--identity-dir")
        .arg(dir)
        .args(["--timeout", &DIAL_SECONDS.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
        .spawn()
        .map_err(|error| format!("this session cannot approve that machine: {error}"))
}

/// The relay this device's endpoints are reached through, when it names one.
fn own_relay() -> Option<String> {
    tilo_endpoint_reachability::relays()
        .relays
        .urls::<Vec<_>>()
        .first()
        .map(ToString::to_string)
}

/// The tag the ask about `ask` is told and withdrawn under.
pub(crate) fn tag(ask: &str) -> String {
    format!("enroll:{ask}")
}

/// The action a notice offers to admit the machine whose ask is `ask`.
pub(crate) fn approve_key(ask: &str) -> String {
    format!("enroll-answer:{ask}:approve")
}

/// The action a notice offers to turn the machine whose ask is `ask` away.
pub(crate) fn decline_key(ask: &str) -> String {
    format!("enroll-answer:{ask}:decline")
}

/// The action a notice offers to open the sheet for the machine whose ask is `ask`.
pub(crate) fn review_key(ask: &str) -> String {
    format!("enroll-answer:{ask}:review")
}

/// What an action the person chose from a notice means for a waiting machine: its ask
/// and whether it is admitted, or `None` when the action is not about one.
pub(crate) fn answer_of(action: &str) -> Option<(String, bool)> {
    let rest = action.strip_prefix("enroll-answer:")?;
    let (ask, answer) = rest.rsplit_once(':')?;
    match answer {
        "approve" => Some((ask.to_owned(), true)),
        "decline" => Some((ask.to_owned(), false)),
        _ => None,
    }
}

/// The ask a review action is about, or `None` for another action.
pub(crate) fn review_of(action: &str) -> Option<String> {
    let rest = action.strip_prefix("enroll-answer:")?;
    let (ask, answer) = rest.rsplit_once(':')?;
    (answer == "review").then(|| ask.to_owned())
}

/// What a machine waiting to join is called in the person's words: its name,
/// what it is, and the code both sides show.
pub(crate) fn described(machine: &Waiting) -> String {
    let details = if machine.hardware.is_empty() {
        machine.label.clone()
    } else {
        format!("{} · {}", machine.label, machine.hardware)
    };
    t_args(
        "machines-waiting-body",
        &[
            ("machine", details.into()),
            ("code", machine.code.clone().into()),
        ],
    )
}

/// The notice telling the person `machine` wants to join: what it is and the
/// code both sides show, with the answers the person may give.
pub(crate) fn notice(machine: &Waiting) -> Notice {
    Notice::new(
        t("machines-waiting-source"),
        1,
        t("machines-waiting-title"),
        described(machine),
    )
    .choosing(vec![
        (review_key(&machine.ask), t("machines-waiting-review")),
        (
            approve_key(&machine.ask),
            t_args(
                "machines-waiting-approve",
                &[("code", machine.code.clone().into())],
            ),
        ),
        (decline_key(&machine.ask), t("machines-waiting-decline")),
    ])
    // It stays until the person answers it: waiting decides nothing.
    .staying(Some(Duration::ZERO))
    .tagged(tag(&machine.ask))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine() -> Waiting {
        Waiting {
            ask: "ab".repeat(16),
            device: "cd".repeat(32),
            service: "ef".repeat(32),
            label: "Home server".into(),
            hardware: "8 cores · 16 GB memory · Fedora Linux 44".into(),
            code: "482913".into(),
        }
    }

    tilo_test_env::scenario! {
        fn machines_that_ask_to_join_are_shown_and_answered;

        phase a_machine_the_helper_reports_waits_with_what_it_is;
        // The helper's line is the ask the person sees: what the machine calls
        // itself, what it is and the code both sides show.
        {
            let line = serde_json::to_string(&machine()).expect("a line");
            let read: Waiting = serde_json::from_str(&line).expect("parsed");
            assert_eq!(read, machine());
            let said = described(&read);
            assert!(said.contains("Home server"), "{said}");
            assert!(said.contains("Fedora Linux 44"), "{said}");
            assert!(said.contains("482913"), "{said}");
        }

        phase the_notice_offers_the_persons_three_answers;
        {
            let waiting = machine();
            let told = notice(&waiting);
            assert_eq!(told.tag, tag(&waiting.ask));
            let keys: Vec<&str> = told
                .actions
                .iter()
                .map(|(key, _)| key.as_str())
                .collect();
            assert_eq!(
                keys,
                [
                    review_key(&waiting.ask),
                    approve_key(&waiting.ask),
                    decline_key(&waiting.ask)
                ]
            );
            assert_eq!(
                answer_of(&approve_key(&waiting.ask)),
                Some((waiting.ask.clone(), true))
            );
            assert_eq!(
                answer_of(&decline_key(&waiting.ask)),
                Some((waiting.ask.clone(), false))
            );
            assert_eq!(review_of(&review_key(&waiting.ask)), Some(waiting.ask));
            assert_eq!(answer_of("something-else"), None);
        }

        phase a_helper_that_prints_a_machine_and_takes_the_answer;
        // The session starts the helper: the machine it prints is
        // put to the person, and the person's answer is the line it reads.
        {
            let mut command = Command::new("/bin/sh");
            command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .arg("-c")
                .arg(
                    "printf '%s\\n' '{\"ask\":\"aabb\",\"device\":\"cc\",\"service\":\"dd\",\
                     \"label\":\"Home server\",\"hardware\":\"8 cores\",\"code\":\"482913\"}'; \
                     read answer; printf '{\"ask\":\"aabb\",\"approved\":true}\\n'; \
                     printf '%s' \"$answer\" > \"$1\"",
                )
                .arg("sh")
                .arg(std::env::temp_dir().join(format!(
                    "tilo-enrollment-answer-{}.txt",
                    std::process::id()
                )));
            let path = std::env::temp_dir().join(format!(
                "tilo-enrollment-answer-{}.txt",
                std::process::id()
            ));
            let mut serving = Serving::over(command).expect("the helper starts");
            let machine = wait_for(|| {
                serving
                    .waiting()
                    .into_iter()
                    .next()
                    .ok_or_else(|| "the machine is reported".to_string())
            });
            assert_eq!(machine.label, "Home server");
            serving.answer(&machine.ask, true).expect("answered");
            assert!(
                serving.waiting().is_empty(),
                "an answered machine waits no more"
            );
            wait_for(|| {
                std::fs::read_to_string(&path)
                    .ok()
                    .filter(|answer| !answer.trim().is_empty())
                    .ok_or_else(|| "the helper took the answer".to_string())
            });
            assert_eq!(
                std::fs::read_to_string(&path).expect("the answer").trim(),
                "approve aabb"
            );
            let _ = std::fs::remove_file(&path);
        }
    }

    /// What `read` answers, once it does, within the test's hang bound.
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "test arithmetic: the bound is a small duration"
    )]
    fn wait_for<T>(mut read: impl FnMut() -> Result<T, String>) -> T {
        let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
        loop {
            match read() {
                Ok(value) => return value,
                Err(what) if std::time::Instant::now() < deadline => {
                    let _ = what;
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(what) => panic!("{what} did not happen in time"),
            }
        }
    }
}
