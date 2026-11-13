//! The runs the agent door's connections work in.
//!
//! An agent acts only inside a run its person started: a turn
//! of a conversation it takes part in ([`crate::conversations`]), or — for a client the person
//! admitted at the agent door — the run that admission starts. Each admitted connection (a
//! program of this login, one inside an environment, an agent from another of the person's
//! devices) works in a run of its own: the desktop mints a task agent for it, its device signs
//! the task delegation, and the run is held here, beside the conversations' turns, for exactly
//! as long as the connection lasts. The connection then joins the broker as that task agent and
//! acts through its run ([`tilo_action_broker::collab_app_broker::TaskRunHost`]): each action is
//! the run's, decided by the person's live grants capped at what an agent may do, and its
//! history is the run's.
//!
//! A step the person's grants ask first about is asked on this desk's sheet rather than on an
//! issue ([`crate::run_questions`]): the run keeps the question while it is up, and the person's
//! "always" for as long as it goes.
//!
//! Nothing is written anywhere: the run is this device's, it ends when the connection does (the
//! person's Stop on its seat ends both), and the broker stops what it still runs.

use std::sync::{Arc, Mutex};

use ed25519_dalek::{Signer as _, SigningKey};
use tilo_broker_core::services::TrackerRun;
use tilo_lock::MutexRecover as _;
use tilo_peer_state::join::WorkspaceId;
use tilo_peer_state::join::task_delegation::{MAX_TASK_DELEGATION_NS, TaskDelegation};

/// The address every door connection's run is named under: no record holds it.
pub(crate) const DOOR_RUNS: &str = tilo_broker_core::services::DOOR_RUN_PREFIX;

/// One door connection's run as it goes: its workspace, the run, and the answers its person gave
/// it for every later step like the one it asked about.
struct Going {
    workspace: String,
    run: TrackerRun,
    answers: Vec<tilo_action_policy::Grant>,
}

/// The door's runs going now, each with its workspace.
#[derive(Default)]
pub(crate) struct DoorRuns {
    held: Mutex<Vec<Going>>,
}

impl DoorRuns {
    /// The runs going in `workspace`.
    pub(crate) fn in_workspace(&self, workspace: &str) -> Vec<TrackerRun> {
        self.held
            .lock_recover()
            .iter()
            .filter(|going| going.workspace == workspace)
            .map(|going| going.run.clone())
            .collect()
    }

    /// The run of issue `issue` for `agent` in `workspace`, while it goes, with the answers its
    /// person gave it.
    pub(crate) fn find(
        &self,
        workspace: &str,
        issue: &str,
        agent: &str,
    ) -> Option<(TrackerRun, Vec<tilo_action_policy::Grant>)> {
        self.held
            .lock_recover()
            .iter()
            .find(|going| {
                going.workspace == workspace && going.run.issue == issue && going.run.agent == agent
            })
            .map(|going| (going.run.clone(), going.answers.clone()))
    }

    /// Keep `question` as the question of the run of `issue` in `workspace`, or none.
    ///
    /// # Errors
    /// No such run goes.
    pub(crate) fn keep_question(
        &self,
        workspace: &str,
        issue: &str,
        question: Option<tilo_broker_core::services::TrackerQuestion>,
    ) -> Result<(), String> {
        self.with_run(workspace, issue, |going| going.run.question = question)
    }

    /// Keep `grant`, the answer the person gave the run of `issue` in `workspace` for its later
    /// steps like the one it asked about, for as long as the run goes.
    ///
    /// # Errors
    /// No such run goes.
    pub(crate) fn keep_answer(
        &self,
        workspace: &str,
        issue: &str,
        grant: tilo_action_policy::Grant,
    ) -> Result<(), String> {
        self.with_run(workspace, issue, |going| {
            going.answers.retain(|kept| kept.id != grant.id);
            going.answers.push(grant);
        })
    }

    /// `change` the run of `issue` in `workspace`.
    fn with_run(
        &self,
        workspace: &str,
        issue: &str,
        change: impl FnOnce(&mut Going),
    ) -> Result<(), String> {
        self.held
            .lock_recover()
            .iter_mut()
            .find(|going| going.workspace == workspace && going.run.issue == issue)
            .map(change)
            .ok_or_else(|| "the agent's run has ended".to_owned())
    }

    /// The answers the person kept for the run of `issue` in `workspace`, while it goes.
    pub(crate) fn answers(&self, workspace: &str, issue: &str) -> Vec<tilo_action_policy::Grant> {
        self.held
            .lock_recover()
            .iter()
            .find(|going| going.workspace == workspace && going.run.issue == issue)
            .map(|going| going.answers.clone())
            .unwrap_or_default()
    }

    /// Hold `run` in `workspace` until the returned guard drops.
    pub(crate) fn open(self: &Arc<Self>, workspace: &str, run: TrackerRun) -> Held {
        let issue = run.issue.clone();
        self.held.lock_recover().push(Going {
            workspace: workspace.to_owned(),
            run,
            answers: Vec::new(),
        });
        Held {
            runs: Arc::clone(self),
            workspace: workspace.to_owned(),
            issue,
        }
    }
}

/// One door connection's run, going while this is held.
pub(crate) struct Held {
    runs: Arc<DoorRuns>,
    workspace: String,
    issue: String,
}

impl Held {
    /// The issue that names its run: what every change the connection makes is stamped with, and
    /// what taking its work back names.
    pub(crate) fn issue(&self) -> &str {
        &self.issue
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        self.runs
            .held
            .lock_recover()
            .retain(|going| !(going.workspace == self.workspace && going.run.issue == self.issue));
        // The broker re-reads its runs when a decision it reads changes: the run it delivered is
        // gone, so what it still runs of it is stopped.
        tilo_identity_host::policy_change::announce();
    }
}

/// A door connection's run as this device mints it: the run the broker reads, and the task
/// agent's presentation the connection joins with.
pub(crate) struct Minted {
    pub(crate) run: TrackerRun,
    pub(crate) presentation: Vec<u8>,
}

/// What a run is minted for: the workspace the connection works in, the person who admitted it
/// (whom the agent acts for), and the agent it is seated as (`claude-code@host#2`), the name its
/// run, and so its history, is listed under.
pub(crate) struct Admitted<'a> {
    pub(crate) workspace: &'a str,
    pub(crate) person: &'a str,
    pub(crate) seated: &'a str,
}

/// Mint the run of one admitted connection: a task agent of its own (`agent_key`), delegated by
/// this device (`device`, certified as `certificate`) for a day at most from `now_ns`, on an issue
/// no other connection shares.
///
/// # Errors
/// The device's certificate is another person's, or the delegation does not sign.
pub(crate) fn mint(
    (device, certificate): (&SigningKey, &tilo_user_identity::DeviceCert),
    agent_key: &SigningKey,
    admitted: &Admitted<'_>,
    now_ns: u64,
) -> Result<Minted, String> {
    let starter = tilo_broker_core::principal_id_for_root(certificate.user_public_key);
    if starter != admitted.person {
        return Err("this device's identity is not the person's who admitted the agent".into());
    }
    let issue = format!("{DOOR_RUNS}{}", uuid::Uuid::new_v4().simple());
    let delegation = TaskDelegation::sign(
        device,
        certificate.user_public_key,
        agent_key.verifying_key().to_bytes(),
        issue.clone(),
        WorkspaceId::parse(admitted.workspace).ok_or("the admission names no workspace")?,
        now_ns,
        now_ns.saturating_add(MAX_TASK_DELEGATION_NS),
    )
    .map_err(|error| format!("the agent's delegation does not sign: {error}"))?;
    let presentation = runtime_collab::TaskAgentJoinIdentity {
        workspace_id: admitted.workspace.to_owned(),
        device_certificate: serde_json::to_vec(certificate).map_err(|error| error.to_string())?,
        delegation: delegation.encode(),
        proof: agent_key
            .sign(&runtime_identity::collaboration_join_signing_bytes(
                admitted.workspace,
            ))
            .to_bytes()
            .to_vec(),
    }
    .encode()
    .map_err(|error| error.to_string())?;
    let run = TrackerRun {
        issue,
        title: admitted.seated.to_owned(),
        instruction: String::new(),
        agent: delegation.agent_id.clone(),
        epoch: 1,
        starter,
        active: true,
        finished_by_agent: false,
        paused_through: None,
        budget: None,
        subject: None,
        model: None,
        directions: Vec::new(),
        question: None,
        // The person's admission at the door is its cause, not a message: the run acts on the
        // person's grants capped at an agent's, as every run without a turn does.
        turn: None,
        delegation: Some(Ok(delegation.encode())),
    };
    Ok(Minted { run, presentation })
}

/// A key no other task agent holds.
///
/// # Errors
/// The system has no randomness to give.
pub(crate) fn agent_key() -> Result<SigningKey, String> {
    let mut seed = [0_u8; 32];
    getrandom::fill(&mut seed).map_err(|error| error.to_string())?;
    Ok(SigningKey::from_bytes(&seed))
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKSPACE: &str = "6f1c1b8e-3c55-4c1e-9d2f-0a4b5c6d7e8f";

    fn device() -> (SigningKey, tilo_user_identity::DeviceCert) {
        let root = SigningKey::from_bytes(&[0x41; 32]);
        let key = SigningKey::from_bytes(&[0x42; 32]);
        let mut cert = tilo_user_identity::DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "This box".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
        (key, cert)
    }

    fn question() -> tilo_broker_core::services::TrackerQuestion {
        tilo_broker_core::services::TrackerQuestion {
            id: "q1".into(),
            app: "so.tilo.terminal".into(),
            resource: "terminal".into(),
            action: "run".into(),
            arguments_digest: "00".into(),
            draft: serde_json::Value::Null,
            before: None,
            answer: tilo_broker_core::services::TrackerAnswer::Waiting,
        }
    }

    fn person(cert: &tilo_user_identity::DeviceCert) -> String {
        tilo_broker_core::principal_id_for_root(cert.user_public_key)
    }

    /// The run a connection is minted is a task agent's of its own, delegated by this device for
    /// the person who admitted it, and its presentation is what the broker's run host joins: the
    /// same delegation, proven by the agent's own key, in that workspace.
    #[test]
    fn a_connections_run_is_a_task_agent_of_its_own_delegated_by_this_device() {
        let (key, cert) = device();
        let person = person(&cert);
        let admitted = Admitted {
            workspace: WORKSPACE,
            person: &person,
            seated: "claude-code@box#1",
        };
        let first = mint((&key, &cert), &agent_key().unwrap(), &admitted, 1_000).unwrap();
        let second = mint((&key, &cert), &agent_key().unwrap(), &admitted, 1_000).unwrap();
        assert_ne!(first.run.issue, second.run.issue, "a run of its own");
        assert_ne!(first.run.agent, second.run.agent, "a task agent of its own");
        assert!(first.run.agent.starts_with("task-"), "{}", first.run.agent);
        assert!(
            tilo_broker_core::services::is_door_run(&first.run.issue),
            "the broker keeps its invocations as the run's: {}",
            first.run.issue
        );
        assert!(
            first.run.issue.starts_with(DOOR_RUNS),
            "{}",
            first.run.issue
        );
        assert_eq!(
            first.run.starter, person,
            "acting for the person who admitted it"
        );
        assert!(first.run.active && first.run.turn.is_none());
        let joined = runtime_collab::TaskAgentJoinIdentity::decode(&first.presentation)
            .unwrap()
            .expect("a task agent's presentation");
        assert_eq!(joined.workspace_id, WORKSPACE);
        assert_eq!(
            first.run.delegation(),
            Ok(Some(joined.delegation.as_slice())),
            "the run carries the delegation the connection joins with"
        );
        let delegation = TaskDelegation::decode(&joined.delegation).unwrap();
        assert_eq!(delegation.agent_id, first.run.agent);
        assert_eq!(delegation.work_group, first.run.issue);
        assert_eq!(delegation.device_public_key, cert.device_public_key);
    }

    /// A device whose identity is not the admitting person's mints nobody a run.
    #[test]
    fn another_persons_device_mints_no_run() {
        let (key, cert) = device();
        let admitted = Admitted {
            workspace: WORKSPACE,
            person: "someone-else",
            seated: "claude-code@box#1",
        };
        assert!(mint((&key, &cert), &agent_key().unwrap(), &admitted, 1_000).is_err());
    }

    /// A run goes while its connection holds it, in its workspace alone.
    #[test]
    fn a_run_goes_while_its_connection_holds_it() {
        let (key, cert) = device();
        let person = person(&cert);
        let admitted = Admitted {
            workspace: WORKSPACE,
            person: &person,
            seated: "claude-code@box#1",
        };
        let minted = mint((&key, &cert), &agent_key().unwrap(), &admitted, 1_000).unwrap();
        let runs = Arc::new(DoorRuns::default());
        let held = runs.open(WORKSPACE, minted.run.clone());
        assert_eq!(runs.in_workspace(WORKSPACE), vec![minted.run.clone()]);
        assert!(runs.in_workspace("another").is_empty());
        assert!(
            runs.find(WORKSPACE, &minted.run.issue, &minted.run.agent)
                .is_some()
        );
        let mut asked = minted.run.clone();
        asked.question = Some(question());
        runs.keep_question(WORKSPACE, &minted.run.issue, asked.question.clone())
            .unwrap();
        assert_eq!(
            runs.in_workspace(WORKSPACE),
            vec![asked],
            "its question is the run's while it asks"
        );
        drop(held);
        assert!(
            runs.in_workspace(WORKSPACE).is_empty(),
            "it ends with the connection"
        );
    }
}
