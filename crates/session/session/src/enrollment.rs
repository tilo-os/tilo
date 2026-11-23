//! Machines that ask to join, as the person's devices answer them.
//!
//! The session serves its device's enrollment endpoint for as long as it
//! is up: a machine anywhere that was handed a token dials it, and the session
//! puts the ask to the person. Every ask is written to a resource of the
//! person's own workspace ([`ENROLLMENT_KIND`]) before it is put to anyone, so
//! it shows on all their devices at once and any of them answers it; the
//! device whose endpoint the machine dialled is the one that carries the answer
//! out. Nothing is served only while a sheet is up, and no answer is mirrored
//! between devices: their own workspace is the one place an ask and its answer
//! live.
//!
//! An ask is `request:<ask>` in the resource's document, and an answer is
//! `answer:<ask>`, written by whichever of the person's devices answered.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ReadDoc as _};
use collab_types::CommitNamed as _;
use serde::{Deserialize, Serialize};
use tilo_enrollment::approval::ApprovalOptions;
use tilo_enrollment::{Decision, Person, WaitingMachine, local_link::Issuer};
use tilo_lock::MutexRecover as _;
use tilo_peer_state::PERSON_RECORDS_APP;
use tilo_service::{Answer, Question};

use crate::i18n::{t, t_args};
use crate::platform::notice::Notice;

/// How long one machine's ask is put to the person: a token is valid a day,
/// and the machine's own window is minutes.
const ASK_FOR: Duration = Duration::from_secs(600);

/// How often the ask waits for an answer the person's other devices write.
const LOOK_EVERY: Duration = Duration::from_millis(50);

/// The kind of the person's resource that holds the machines waiting for an answer, one entry
/// under `request:<ask>`, and the answers their devices gave, under `answer:<ask>`.
pub(crate) const ENROLLMENT_KIND: &str = "identity.enrollment";

/// The kind the entries of the resource's document are committed as.
const CHANGE_KIND: &str = "enrollment";

/// One machine asking to join, as the person's store holds it and every
/// device of theirs shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Waiting {
    pub ask: String,
    pub device: String,
    pub service: String,
    pub label: String,
    #[serde(default)]
    pub hardware: String,
    pub code: String,
    /// The token's nonce: what a planned machine's entry is known by.
    #[serde(default)]
    pub nonce: String,
}

impl Waiting {
    fn of(machine: &WaitingMachine) -> Self {
        Self {
            ask: machine.ask.clone(),
            device: machine.device.clone(),
            service: machine.service.clone(),
            label: machine.label.clone(),
            hardware: machine.hardware.clone(),
            code: machine.code.clone(),
            nonce: machine.nonce.clone(),
        }
    }
}

/// What the person's own workspace holds: the machines waiting for an answer, and the
/// answers their devices gave.
pub(crate) struct Held {
    pub waiting: Vec<Waiting>,
    pub answers: BTreeMap<String, bool>,
}

/// The document of the person's own workspace the asks and answers are kept in.
fn document() -> String {
    collab_types::person_document_id(ENROLLMENT_KIND)
}

/// Read the person's own workspace: every ask no device answered yet, and every
/// answer.
pub(crate) fn held(store: &crate::PersonResources) -> Held {
    let Some(bytes) = store.document((PERSON_RECORDS_APP, &document())) else {
        return Held {
            waiting: Vec::new(),
            answers: BTreeMap::new(),
        };
    };
    let Ok(doc) = AutoCommit::load(&bytes) else {
        tracing::warn!(
            "the machines waiting for an answer in the person's own workspace do not read"
        );
        return Held {
            waiting: Vec::new(),
            answers: BTreeMap::new(),
        };
    };
    let mut waiting = Vec::new();
    let mut answers = BTreeMap::new();
    for key in doc.keys(automerge::ROOT) {
        let Some(value) = text_of(&doc, &key) else {
            continue;
        };
        if let Some(ask) = key.strip_prefix("request:") {
            let Some(machine): Option<Waiting> = tilo_failure::ignored!(
                "an ask this build cannot read is no machine waiting",
                serde_json::from_str(&value).ok()
            ) else {
                continue;
            };
            if machine.ask == ask {
                waiting.push(machine);
            }
        } else if let Some(ask) = key.strip_prefix("answer:") {
            answers.insert(ask.to_owned(), value == "approve");
        }
    }
    waiting.sort_by(|left, right| left.ask.cmp(&right.ask));
    Held { waiting, answers }
}

fn text_of(doc: &AutoCommit, key: &str) -> Option<String> {
    let (value, _) = doc.get(automerge::ROOT, key).ok()??;
    value.into_string().ok()
}

/// Write one entry of the resource's document, taking what it already holds, and carry it to
/// the person's other devices as any workspace's change.
fn write(
    store: &crate::PersonResources,
    write: impl Fn(&mut AutoCommit) -> Result<(), String> + Send + Sync + 'static,
) {
    let document = document();
    store.edit_document((PERSON_RECORDS_APP, &document), write);
}

fn put(doc: &mut AutoCommit, key: &str, value: &str) -> Result<(), String> {
    doc.put(automerge::ROOT, key, value)
        .map_err(|error| error.to_string())?;
    doc.commit_edit(CHANGE_KIND, key);
    Ok(())
}

/// Enter `machine` as asking to join: what every one of the person's devices
/// shows, and what the device that serves its connection waits on.
pub(crate) fn enter(
    store: &crate::PersonResources,
    machine: &WaitingMachine,
) -> Result<(), String> {
    let value = serde_json::to_string(&Waiting::of(machine)).map_err(|error| error.to_string())?;
    let ask = machine.ask.clone();
    write(store, move |doc| {
        put(doc, &format!("request:{ask}"), &value)
    });
    Ok(())
}

/// Answer the machine whose ask is `ask`: `approve` admits it. The answer is
/// the person's whichever of their devices wrote it; the one serving the
/// machine carries it out.
pub(crate) fn answer(store: &crate::PersonResources, ask: &str, approve: bool) {
    let value = if approve { "approve" } else { "decline" };
    let ask = ask.to_owned();
    write(store, move |doc| {
        let ask = ask.as_str();
        let request = format!("request:{ask}");
        if text_of(doc, &request).is_none() {
            return Err("this device request is no longer pending".to_owned());
        }
        doc.delete(automerge::ROOT, request)
            .map_err(|error| error.to_string())?;
        put(doc, &format!("answer:{ask}"), value)
    });
}

/// The answer the person gave the machine whose ask is `ask`, once any of
/// their devices gave one.
fn answered(store: &crate::PersonResources, ask: &str) -> Option<Decision> {
    held(store).answers.get(ask).map(|approve| {
        if *approve {
            Decision::Approve
        } else {
            Decision::Decline
        }
    })
}

/// The person's own workspace, for the endpoint that answers machines: another device answers
/// an ask written there, and this one carries it out here.
pub(crate) struct Asked {
    store: crate::PersonResources,
}

impl Person for Asked {
    fn decide(&mut self, machine: &WaitingMachine) -> anyhow::Result<Decision> {
        enter(&self.store, machine).map_err(|error| anyhow::anyhow!(error))?;
        // The person may answer here or on any other device of theirs: their
        // own workspace carries whichever comes first.
        let Some(deadline) = std::time::Instant::now().checked_add(ASK_FOR) else {
            return Ok(Decision::Decline);
        };
        while std::time::Instant::now() < deadline {
            if let Some(decision) = answered(&self.store, &machine.ask) {
                return Ok(decision);
            }
            std::thread::sleep(LOOK_EVERY);
        }
        Ok(Decision::Decline)
    }
}

/// The issuer is published only after the device has verified its enrollment authority.
#[derive(Default)]
pub(crate) struct LocalLinks(Arc<Mutex<Option<Issuer>>>);

impl LocalLinks {
    pub(crate) fn issue(&self, person: [u8; 32], now_ms: u64) -> Option<(String, u64)> {
        self.0.lock_recover().as_ref()?.issue(person, now_ms).ok()
    }
}

/// Answer machines on this device's enrollment endpoint for as long as the
/// session is up, noting each ask in the person's own workspace. `root` and
/// `directory` are the person's identity as this device holds it.
pub(crate) fn serve(
    store: crate::PersonResources,
    root: std::path::PathBuf,
    directory: std::path::PathBuf,
    wake: tilo_service::Wake,
) -> LocalLinks {
    let links = LocalLinks::default();
    let serving = Arc::clone(&links.0);
    let relays = own_relays();
    let spawned = std::thread::Builder::new()
        .name("tilo-enrollment-serve".into())
        .spawn(move || {
            let mut asked = Asked { store };
            let mut options = Vec::new();
            // The machine that installs a server of the person's, and one of
            // their own devices: the same endpoint answers both.
            for kind in [
                tilo_enrollment::EnrollmentKind::HomePeer,
                tilo_enrollment::EnrollmentKind::PersonalDevice,
            ] {
                options.push(ApprovalOptions {
                    identity_root: &root,
                    identity_dir: Some(&directory),
                    timeout: 600,
                    workspace_cache: std::path::Path::new("/var/cache/quick-fs"),
                    acl: std::path::Path::new("/var/lib/tilo/fs-capabilities.json"),
                    server_workspace_ids: &[],
                    expected_device: None,
                    expected_service: None,
                    kind,
                    port: 0,
                    ready: None,
                    approved: None,
                    person_uid: None,
                });
            }
            match tilo_enrollment::transport::serve_for_lifetime(
                &options,
                relays.as_ref(),
                &mut asked,
                |endpoint, identity| {
                    *serving.lock_recover() = Some(Issuer::new(endpoint, identity));
                    wake();
                },
            ) {
                Ok(()) => {}
                Err(error) => tracing::info!("{error:#}"),
            }
            *serving.lock_recover() = None;
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "this session answers no machines");
    }
    links
}

/// Let a machine with no token in, by its own `tilo://enroll?…` line: this
/// device dials it and approves it there. Answers as both kinds, as the
/// endpoint does.
pub(crate) fn approve_line(
    line: &str,
    root: std::path::PathBuf,
    directory: std::path::PathBuf,
) -> Result<(), String> {
    if !line.trim().starts_with("tilo://enroll?") {
        return Err("paste the machine's link, which starts with tilo://enroll?".into());
    }
    let line = line.to_owned();
    let relays = own_relays();
    std::thread::Builder::new()
        .name("tilo-enrollment-dial".into())
        .spawn(move || {
            for kind in [
                tilo_enrollment::EnrollmentKind::HomePeer,
                tilo_enrollment::EnrollmentKind::PersonalDevice,
            ] {
                let options = ApprovalOptions {
                    identity_root: &root,
                    identity_dir: Some(&directory),
                    timeout: 270,
                    workspace_cache: std::path::Path::new("/var/cache/quick-fs"),
                    acl: std::path::Path::new("/var/lib/tilo/fs-capabilities.json"),
                    server_workspace_ids: &[],
                    expected_device: None,
                    expected_service: None,
                    kind,
                    port: 0,
                    ready: None,
                    approved: None,
                    person_uid: None,
                };
                match tilo_enrollment::transport::approve_line(&options, relays.as_ref(), &line) {
                    Ok(()) => return,
                    Err(error) => tracing::info!("{error:#}"),
                }
            }
        })
        .map_err(|error| format!("this session cannot approve that machine: {error}"))?;
    Ok(())
}

/// The tag the ask about `ask` is told and withdrawn under.
pub(crate) fn tag(ask: &str) -> String {
    format!("enroll:{ask}")
}

/// The kind the session's question sheet reads a machine asking to join as.
pub(crate) const KIND: &str = "network.machine-enrol";

pub(crate) fn question_handle(ask: &str) -> String {
    format!("machine-enrol:{ask}")
}

/// The question resource a machine's ask is published as while it waits: the
/// same ask as its notice, so an agent answers it where it answers a disk
/// unlock, through the question's own `answer` and `cancel` actions.
pub(crate) fn question(machine: &Waiting) -> Question {
    Question {
        handle: question_handle(&machine.ask),
        kind: KIND.to_owned(),
        app_id: None,
        label: machine.label.clone(),
        details: serde_json::to_value(machine).unwrap_or_default(),
    }
}

/// Whether the person's answer to [`question`] admits the machine: answering
/// it does, and its cancel (a declined question) turns the machine away.
pub(crate) const fn admits(answer: Option<&Answer>) -> bool {
    answer.is_some()
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
    t_args(
        "machines-waiting-body",
        &[
            ("machine", machine.label.clone().into()),
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

/// The relays this device's endpoints are reached through: a machine behind
/// a network translation finds its enrollment endpoint only there.
fn own_relays() -> Option<iroh::RelayMap> {
    let relays = tilo_endpoint_reachability::relays().relays;
    if relays.is_empty() {
        return None;
    }
    Some(relays)
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
            nonce: "aa".repeat(16),
        }
    }

    tilo_test_env::scenario! {
        fn machines_that_ask_to_join_are_shown_and_answered;

        phase a_machine_the_store_holds_waits_with_what_it_is;
        // The store's entry is the ask the person sees: what the machine calls
        // itself, what it is and the code both sides show.
        {
            let line = serde_json::to_string(&machine()).expect("an entry");
            let read: Waiting = serde_json::from_str(&line).expect("parsed");
            assert_eq!(read, machine());
            let said = described(&read);
            assert!(said.contains("Home server"), "{said}");
            assert!(!said.contains("Fedora Linux 44"), "{said}");
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
    }
}
