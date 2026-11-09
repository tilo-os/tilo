use super::{NativeTaskActionRequirement, TrackerRun};
use automerge::{AutoCommit, ROOT, ReadDoc, transaction::Transactable};
use ed25519_dalek::SigningKey;
use tilo_action_policy::turn::Looked;
use tilo_action_policy::{Answer, Grant, KindSet, Subject, TreePath};
use tilo_automerge::ApplyIncoming;
use tilo_peer_state::{SeedState, StatePeer, seed_workspace};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};

fn certificate(root: u8, device: u8) -> DeviceCert {
    let root = SigningKey::from_bytes(&[root; 32]);
    let device = SigningKey::from_bytes(&[device; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "Task policy fixture".into(),
        capabilities: DeviceCapabilities::primary(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, &root).unwrap();
    cert
}

#[test]
fn approved_run_cannot_exceed_its_starters_live_action_permission() {
    let workspace = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let (mut peer, starter, key, now) = policy_fixture(workspace);
    let requirement = approve_task(&starter);
    assert_exact_binding(&peer, workspace, now, &requirement);
    assert_independent_task_grants(&mut peer, workspace, &key, now, &requirement);
    assert_live_starter_revocation(&mut peer, workspace, &starter, &key, now, &requirement);
}

/// An action the agent's grants answer "ask" for pauses the run for its
/// person, naming the grant, apart from one they deny.
#[test]
fn a_run_is_told_an_action_asks_first() {
    let workspace = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    let (mut peer, starter, key, now) = policy_fixture(workspace);
    let requirement = approve_task(&starter);
    let grant = |id: &str, answer| {
        Grant::new(
            id,
            Subject::agent("scribe"),
            action_node(workspace, &requirement),
            KindSet::UNDOABLE,
            answer,
            Subject::person("governor"),
        )
    };
    peer.governance_mut()
        .put_governed_grants(&[grant("agent-asks", Answer::Ask)], &key, now)
        .unwrap();
    let asks = requirement.check_run(&peer, workspace, now).unwrap_err();
    assert!(asks.contains("asks a person first"), "{asks}");
    assert!(asks.contains("agent-asks"), "{asks}");
    assert!(
        NativeTaskActionRequirement::awaits_confirmation(&asks),
        "{asks}"
    );
    peer.governance_mut()
        .put_governed_grants(&[grant("agent-denied", Answer::Deny)], &key, now)
        .unwrap();
    let denied = requirement.check_run(&peer, workspace, now).unwrap_err();
    assert!(denied.contains("not allowed"), "{denied}");
}

/// A change the run can take back needs no approval: a person's grant that
/// asks them when they press the action never pauses a run they started,
/// while a change that cannot be undone still does.
#[test]
fn a_change_the_run_can_take_back_just_happens() {
    let workspace = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
    let (mut peer, starter, key, now) = policy_fixture(workspace);
    let requirement = approve_task(&starter);
    let grant = |id: &str, kinds| {
        Grant::new(
            id,
            Subject::person(hex::encode(starter.user_public_key)),
            action_node(workspace, &requirement),
            kinds,
            Answer::Ask,
            Subject::person("governor"),
        )
    };
    peer.governance_mut()
        .put_governed_grants(&[grant("starter-asks-notes", KindSet::ALL)], &key, now)
        .unwrap();
    requirement.check_run(&peer, workspace, now).unwrap();

    // What cannot be taken back still pauses for the person.
    let mut sends = approve_task(&starter);
    sends.need.kind = tilo_action_policy::ActionKind::Outside;
    let asks = sends.check_run(&peer, workspace, now).unwrap_err();
    assert!(
        NativeTaskActionRequirement::awaits_confirmation(&asks),
        "{asks}"
    );

    // And an answer asking the agent first still asks: the run does not
    // undo the agent's own grants by taking them back either.
    peer.governance_mut()
        .put_governed_grants(
            &[
                grant("starter-asks-notes", KindSet::ALL),
                Grant::new(
                    "agent-asks-notes",
                    Subject::agent("scribe"),
                    action_node(workspace, &requirement),
                    KindSet::UNDOABLE,
                    Answer::Ask,
                    Subject::person("governor"),
                ),
            ],
            &key,
            now,
        )
        .unwrap();
    let asks = requirement.check_run(&peer, workspace, now).unwrap_err();
    assert!(
        NativeTaskActionRequirement::awaits_confirmation(&asks),
        "{asks}"
    );
}

fn policy_fixture(workspace: &str) -> (StatePeer, DeviceCert, SigningKey, std::time::SystemTime) {
    admitted_fixture(workspace, collab_types::Role::Editor, None)
}

/// A workspace where the starter holds `role` and the agent holds a
/// workspace-wide grant of `kinds`, or, without one, of every kind. An agent
/// that reaches anywhere is asked first, as a person's delegate is.
fn admitted_fixture(
    workspace: &str,
    role: collab_types::Role,
    kinds: Option<KindSet>,
) -> (StatePeer, DeviceCert, SigningKey, std::time::SystemTime) {
    let governor = certificate(1, 2);
    let starter = certificate(3, 4);
    let key = SigningKey::from_bytes(&[2; 32]);
    let now = std::time::SystemTime::now();
    let roster = vec![governor.clone(), starter.clone()];
    let mut doc = AutoCommit::new();
    doc.apply_incoming(
        seed_workspace(workspace, &governor, &roster)
            .unwrap()
            .into_iter()
            .map(|bytes| automerge::Change::from_bytes(bytes).unwrap()),
    )
    .unwrap();
    let members = doc.get(ROOT, "members").unwrap().unwrap().1;
    doc.put(
        &members,
        hex::encode(starter.user_public_key),
        automerge::ScalarValue::Bytes(serde_json::to_vec(&role).unwrap()),
    )
    .unwrap();
    doc.commit();
    let mut peer = StatePeer::new(workspace, governor, roster, RevocationList::new(), now).unwrap();
    peer.install_seed(
        SeedState {
            workspace_changes: doc
                .get_changes(&[])
                .into_iter()
                .map(|change| change.raw_bytes().to_vec())
                .collect(),
            documents: vec![],
        },
        &key,
        now,
    )
    .unwrap();
    let kinds = kinds.unwrap_or(KindSet::ALL);
    let admission = [
        Grant::new(
            "agent",
            Subject::agent("scribe"),
            TreePath::workspace(workspace),
            kinds,
            Answer::Allow,
            Subject::person("governor"),
        ),
        Grant::new(
            "agent:outside",
            Subject::agent("scribe"),
            TreePath::workspace(workspace),
            KindSet::OUTSIDE,
            Answer::Ask,
            Subject::person("governor"),
        ),
    ];
    let admission = admission.to_vec();
    peer.governance_mut()
        .put_governed_grants(&admission, &key, now)
        .unwrap();

    (peer, starter, key, now)
}

/// The issue whose run the fixtures check.
const ISSUE: &str = "tasks.tracker/issues/1";

/// The requirement of an action the agent's run of [`ISSUE`] takes, the
/// run approved by `starter`.
fn approve_task(starter: &DeviceCert) -> NativeTaskActionRequirement {
    NativeTaskActionRequirement {
        binding: super::NativeTaskBinding {
            task_id: ISSUE.into(),
            agent_id: "scribe".into(),
            epoch: 1,
            revision: 0,
            starter: Some(hex::encode(starter.user_public_key)),
            turn: None,
        },
        app: "so.tilo.notes".into(),
        resource_kind: "document".into(),
        resource: "notes:release".into(),
        action: "append".into(),
        need: tilo_actions::ActionNeed {
            kind: tilo_action_policy::ActionKind::Undoable,
            field: None,
            privileged: false,
        },
        consequential: false,
        attended: false,
        confirmed: false,
    }
}

/// Checking a requirement against the going run its binding names.
trait CheckRun {
    fn check_run(
        &self,
        peer: &StatePeer,
        workspace: &str,
        now: std::time::SystemTime,
    ) -> Result<(), String>;
}

impl CheckRun for NativeTaskActionRequirement {
    fn check_run(
        &self,
        peer: &StatePeer,
        workspace: &str,
        now: std::time::SystemTime,
    ) -> Result<(), String> {
        self.check_turn(peer, workspace, None, now)
    }
}

impl NativeTaskActionRequirement {
    /// [`CheckRun::check_run`] with what this device looked up about the run's
    /// turn: `looked` is what the message that caused it says, or `None` when
    /// its cause has not arrived here yet.
    fn check_turn(
        &self,
        peer: &StatePeer,
        workspace: &str,
        looked: Option<&tilo_action_policy::turn::Looked>,
        now: std::time::SystemTime,
    ) -> Result<(), String> {
        let doc = peer
            .admitted_workspace()
            .ok_or("tracker action policy unavailable")?;
        let policy = tilo_desktop_model::projection::workspace_policy_of(doc, workspace);
        let binding = &self.binding;
        let run = TrackerRun {
            issue: binding.task_id.clone(),
            title: "Inspect notes".into(),
            instruction: "Inspect notes".into(),
            agent: "scribe".into(),
            epoch: 1,
            starter: binding.starter.clone().unwrap_or_default(),
            active: true,
            finished_by_agent: false,
            paused_through: None,
            budget: None,
            subject: None,
            model: None,
            directions: Vec::new(),
            turn: binding.turn.clone(),
            question: None,
            delegation: None,
        };
        self.check_tracker_policy(&policy, workspace, &run, looked, now)
    }
}

fn assert_exact_binding(
    peer: &StatePeer,
    workspace: &str,
    now: std::time::SystemTime,
    requirement: &NativeTaskActionRequirement,
) {
    requirement.check_run(peer, workspace, now).unwrap();
    let mut stale = requirement.clone();
    stale.binding.epoch = 2;
    assert!(stale.check_run(peer, workspace, now).is_err());
    let mut privileged = requirement.clone();
    privileged.need.privileged = true;
    assert!(privileged.check_run(peer, workspace, now).is_err());
}

fn action_node(workspace: &str, requirement: &NativeTaskActionRequirement) -> TreePath {
    tilo_actions::action_node(
        &TreePath::workspace(workspace),
        &requirement.app,
        &requirement.resource_kind,
        &requirement.resource,
        None,
        &requirement.action,
    )
}

fn assert_independent_task_grants(
    peer: &mut StatePeer,
    workspace: &str,
    key: &SigningKey,
    now: std::time::SystemTime,
    requirement: &NativeTaskActionRequirement,
) {
    // Another issue run by the same agent has an independent grant set.
    let other = "tasks.tracker/issues/2".to_owned();
    peer.governance_mut()
        .put_governed_grants(
            &[Grant::new(
                "other-task-deny",
                Subject::task(&other),
                TreePath::workspace(workspace),
                KindSet::ALL,
                Answer::Deny,
                Subject::person("governor"),
            )],
            key,
            now,
        )
        .unwrap();
    requirement.check_run(peer, workspace, now).unwrap();
    let mut other_requirement = requirement.clone();
    other_requirement.binding.task_id = other;
    assert!(other_requirement.check_run(peer, workspace, now).is_err());
}

fn assert_live_starter_revocation(
    peer: &mut StatePeer,
    workspace: &str,
    starter: &DeviceCert,
    key: &SigningKey,
    now: std::time::SystemTime,
    requirement: &NativeTaskActionRequirement,
) {
    peer.governance_mut()
        .put_governed_grants(
            &[Grant::new(
                "starter-no-append",
                Subject::person(hex::encode(starter.user_public_key)),
                action_node(workspace, requirement),
                KindSet::UNDOABLE,
                Answer::Deny,
                Subject::person("governor"),
            )],
            key,
            now,
        )
        .unwrap();
    // Workspace write and the agent's own grant remain valid: only this exact
    // operation is revoked from the person who consciously started the run.
    assert!(
        peer.permissions(starter.device_public_key, now)
            .unwrap()
            .allows(
                &TreePath::workspace(workspace),
                tilo_action_policy::ActionKind::Undoable,
                now
            )
    );
    assert!(
        requirement
            .check_run(peer, workspace, now)
            .unwrap_err()
            .contains("starter")
    );
}

#[test]
fn an_action_asking_first_pauses_until_confirmed_and_deny_still_refuses() {
    use tilo_action_policy::agent_answers::{self, AnswerScope};
    let workspace = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    let (mut peer, starter, key, now) = policy_fixture(workspace);
    let requirement = approve_task(&starter);
    let awaits = |requirement: &NativeTaskActionRequirement, peer: &StatePeer| {
        let error = requirement.check_run(peer, workspace, now).unwrap_err();
        NativeTaskActionRequirement::awaits_confirmation(&error)
    };

    // An action whose contract confirms (it deletes for good) asks first,
    // though the agent's grants allow its kind.
    let mut purge = requirement.clone();
    purge.consequential = true;
    assert!(awaits(&purge, &peer));
    purge.confirmed = true;
    purge.check_run(&peer, workspace, now).unwrap();

    // The person tells the agent Notes asks first.
    let scope = AnswerScope::app("so.tilo.notes");
    let set = |answer: Answer, peer: &mut StatePeer| {
        peer.governance_mut()
            .put_governed_grants(
                &[agent_answers::grant(
                    workspace,
                    "scribe",
                    (&scope, tilo_action_policy::ActionKind::Undoable),
                    answer,
                    Subject::person("governor"),
                )],
                &key,
                now,
            )
            .unwrap();
    };
    set(Answer::Ask, &mut peer);
    assert!(awaits(&requirement, &peer));
    let mut confirmed = requirement.clone();
    confirmed.confirmed = true;
    confirmed.check_run(&peer, workspace, now).unwrap();

    // Not allowed: refused outright, confirmed or not.
    set(Answer::Deny, &mut peer);
    let error = confirmed.check_run(&peer, workspace, now).unwrap_err();
    assert!(
        !NativeTaskActionRequirement::awaits_confirmation(&error),
        "{error}"
    );

    // Always allowing the one action lifts its contract's confirmation.
    let always = AnswerScope::action("so.tilo.notes", "document", "append");
    peer.governance_mut()
        .put_governed_grants(
            &[agent_answers::grant(
                workspace,
                "scribe",
                (&always, tilo_action_policy::ActionKind::Undoable),
                Answer::Allow,
                Subject::person("governor"),
            )],
            &key,
            now,
        )
        .unwrap();
    let mut purge = requirement;
    purge.consequential = true;
    purge.check_run(&peer, workspace, now).unwrap();
}

/// An action is decided by its kind and the person's answers alone: an agent
/// reads where reading is allowed, and an effect reaching outside the
/// document (writing a file, say) asks first. An attended action (a chooser,
/// a sign-in, a secret shown) asks every time: even "Always allow this"
/// does not let the agent run it unasked, and Deny still refuses it.
#[test]
fn an_attended_action_asks_every_time_and_ordinary_kinds_follow_the_answers() {
    use tilo_action_policy::agent_answers::{self, AnswerScope};
    let workspace = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";
    let (mut peer, starter, key, now) = policy_fixture(workspace);
    let requirement = approve_task(&starter);
    let awaits = |requirement: &NativeTaskActionRequirement, peer: &StatePeer| {
        requirement
            .check_run(peer, workspace, now)
            .err()
            .is_some_and(|error| NativeTaskActionRequirement::awaits_confirmation(&error))
    };

    let mut read = requirement.clone();
    read.need.kind = tilo_action_policy::ActionKind::Read;
    read.check_run(&peer, workspace, now).unwrap();

    let mut save = requirement.clone();
    save.action = "save".into();
    save.need.kind = tilo_action_policy::ActionKind::Outside;
    assert!(awaits(&save, &peer), "an outside effect asks first");
    save.confirmed = true;
    save.check_run(&peer, workspace, now).unwrap();

    let mut choose = requirement;
    choose.action = "open-file".into();
    choose.need.kind = tilo_action_policy::ActionKind::Read;
    choose.consequential = true;
    choose.attended = true;
    let answer = |answer: Answer, peer: &mut StatePeer| {
        peer.governance_mut()
            .put_governed_grants(
                &[agent_answers::grant(
                    workspace,
                    "scribe",
                    (
                        &AnswerScope::action("so.tilo.notes", "document", "open-file"),
                        tilo_action_policy::ActionKind::Read,
                    ),
                    answer,
                    Subject::person("governor"),
                )],
                &key,
                now,
            )
            .unwrap();
    };
    answer(Answer::Allow, &mut peer);
    assert!(awaits(&choose, &peer), "always allowing it still asks");
    choose.confirmed = true;
    choose.check_run(&peer, workspace, now).unwrap();
    answer(Answer::Deny, &mut peer);
    assert!(choose.check_run(&peer, workspace, now).is_err());
    assert!(!awaits(&choose, &peer), "deny refuses outright");
}

/// A task a person starts runs on their behalf: an agent that may change
/// nothing of its own takes the changes its starter may take, held to the
/// answers the workspace keeps for it.
#[test]
fn a_run_takes_its_starters_changes_whatever_its_agent_holds() {
    use tilo_action_policy::agent_answers::{self, AnswerScope};
    for kinds in [KindSet::READ, KindSet::READ_UNDOABLE] {
        let workspace = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
        let (mut peer, starter, key, now) =
            admitted_fixture(workspace, collab_types::Role::Editor, Some(kinds));
        let requirement = approve_task(&starter);
        requirement
            .check_run(&peer, workspace, now)
            .unwrap_or_else(|error| panic!("{kinds:?}: {error}"));

        // Outside effects still ask first by default.
        let mut outside = requirement.clone();
        outside.need.kind = tilo_action_policy::ActionKind::Outside;
        let asks = outside.check_run(&peer, workspace, now).unwrap_err();
        assert!(
            NativeTaskActionRequirement::awaits_confirmation(&asks),
            "{asks}"
        );

        peer.governance_mut()
            .put_governed_grants(
                &[agent_answers::grant(
                    workspace,
                    "scribe",
                    (
                        &AnswerScope::app("so.tilo.notes"),
                        tilo_action_policy::ActionKind::Undoable,
                    ),
                    Answer::Deny,
                    Subject::person("governor"),
                )],
                &key,
                now,
            )
            .unwrap();
        let denied = requirement.check_run(&peer, workspace, now).unwrap_err();
        assert!(denied.contains("not allowed"), "{denied}");

        let policy = tilo_desktop_model::projection::workspace_policy_of(
            peer.admitted_workspace().unwrap(),
            workspace,
        );
        let own = tilo_action_policy::Authority::person(policy.party_of(Subject::agent("scribe")))
            .decide(
                &action_node(workspace, &requirement),
                tilo_action_policy::ActionKind::Undoable,
                &tilo_action_policy::Context::at_time(now),
            );
        assert_eq!(own.answer, Answer::Deny);
    }
}

/// A grant naming the issue decides for its runs as the agent's own do: an
/// outside action the agent's delegation asks about runs once the issue is
/// allowed it in that app, in that issue's runs alone.
#[test]
fn a_grant_naming_the_issue_lets_its_runs_act_without_asking() {
    let workspace = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee";
    let (mut peer, starter, key, now) = admitted_fixture(
        workspace,
        collab_types::Role::Editor,
        Some(KindSet::READ_UNDOABLE),
    );
    let notes = TreePath::workspace(workspace).app("so.tilo.notes");
    peer.governance_mut()
        .put_governed_grants(
            &[Grant::new(
                "starter-sends-notes",
                Subject::person(hex::encode(starter.user_public_key)),
                notes.clone(),
                KindSet::OUTSIDE,
                Answer::Allow,
                Subject::person("governor"),
            )],
            &key,
            now,
        )
        .unwrap();
    let mut outside = approve_task(&starter);
    outside.need.kind = tilo_action_policy::ActionKind::Outside;
    let asks = outside.check_run(&peer, workspace, now).unwrap_err();
    assert!(asks.contains("agent's grants"), "{asks}");
    assert!(
        NativeTaskActionRequirement::awaits_confirmation(&asks),
        "{asks}"
    );
    peer.governance_mut()
        .put_governed_grants(
            &[Grant::new(
                "issue-allows-notes",
                Subject::task(ISSUE),
                notes,
                KindSet::OUTSIDE,
                Answer::Allow,
                Subject::person("governor"),
            )],
            &key,
            now,
        )
        .unwrap();
    outside.check_run(&peer, workspace, now).unwrap();
    let mut other = outside;
    other.binding.task_id = "tasks.tracker/issues/2".into();
    let asks = other.check_run(&peer, workspace, now).unwrap_err();
    assert!(
        NativeTaskActionRequirement::awaits_confirmation(&asks),
        "{asks}"
    );

    // "Allow for this issue" answers the one action, so an action whose
    // contract confirms runs unasked too, in that issue's runs alone.
    let mut purge = approve_task(&starter);
    purge.consequential = true;
    let asks = purge.check_run(&peer, workspace, now).unwrap_err();
    assert!(
        NativeTaskActionRequirement::awaits_confirmation(&asks),
        "{asks}"
    );
    peer.governance_mut()
        .put_governed_grants(
            &[tilo_action_policy::agent_answers::answer_grant(
                workspace,
                Subject::task(ISSUE),
                (
                    &tilo_action_policy::agent_answers::AnswerScope::action(
                        "so.tilo.notes",
                        "document",
                        "append",
                    ),
                    tilo_action_policy::ActionKind::Undoable,
                ),
                Answer::Allow,
                Subject::person("governor"),
            )],
            &key,
            now,
        )
        .unwrap();
    purge.check_run(&peer, workspace, now).unwrap();
    purge.binding.task_id = "tasks.tracker/issues/2".into();
    let asks = purge.check_run(&peer, workspace, now).unwrap_err();
    assert!(
        NativeTaskActionRequirement::awaits_confirmation(&asks),
        "{asks}"
    );
}

/// The starter's own rights bound the run, whatever the agent was admitted
/// with: a person who may not change a note starts no run that changes it,
/// and one who may not change the workspace starts no run at all.
#[test]
fn a_run_never_exceeds_a_starter_who_may_not_change_the_note() {
    let workspace = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";
    let (mut peer, starter, key, now) = admitted_fixture(
        workspace,
        collab_types::Role::Editor,
        Some(KindSet::READ_UNDOABLE),
    );
    peer.governance_mut()
        .put_governed_grants(
            &[Grant::new(
                "starter-reads-notes",
                Subject::person(hex::encode(starter.user_public_key)),
                TreePath::workspace(workspace).app("so.tilo.notes"),
                KindSet::UNDOABLE,
                Answer::Deny,
                Subject::person("governor"),
            )],
            &key,
            now,
        )
        .unwrap();
    let requirement = approve_task(&starter);
    let refused = requirement.check_run(&peer, workspace, now).unwrap_err();
    assert!(refused.contains("starter"), "{refused}");

    let (peer, commenter, _key, now) = admitted_fixture(
        workspace,
        collab_types::Role::Commenter,
        Some(KindSet::READ_UNDOABLE),
    );
    let refused = approve_task(&commenter)
        .check_run(&peer, workspace, now)
        .unwrap_err();
    assert!(refused.contains("starter"), "{refused}");
}

/// "Allow for the issue", given on a run's first read of a Browser page,
/// answers its other reads of Browser pages too: the person is asked once,
/// not on every page. It answers nothing outside the issue, nothing in
/// another kind of resource, and never an action whose contract confirms,
/// which keeps asking unless answered alone.
#[test]
fn allowing_one_page_read_for_the_issue_answers_the_runs_other_page_reads() {
    use crate::collab_app_broker::run_question::issue_answer_scope;
    let workspace = "ffffffff-ffff-4fff-8fff-ffffffffffff";
    let (mut peer, starter, key, now) = admitted_fixture(
        workspace,
        collab_types::Role::Editor,
        Some(KindSet::READ_UNDOABLE),
    );
    peer.governance_mut()
        .put_governed_grants(
            &[Grant::new(
                "starter-browses",
                Subject::person(hex::encode(starter.user_public_key)),
                TreePath::workspace(workspace).app("so.tilo.browser"),
                KindSet::OUTSIDE,
                Answer::Allow,
                Subject::person("governor"),
            )],
            &key,
            now,
        )
        .unwrap();
    let page = |action: &str| {
        let mut read = approve_task(&starter);
        read.app = "so.tilo.browser".into();
        read.resource_kind = "browser.tab".into();
        read.resource = "browser.tab:1".into();
        read.action = action.into();
        read.need.kind = tilo_action_policy::ActionKind::Outside;
        read
    };
    let asks = |requirement: &NativeTaskActionRequirement, peer: &StatePeer| {
        requirement
            .check_run(peer, workspace, now)
            .is_err_and(|error| NativeTaskActionRequirement::awaits_confirmation(&error))
    };
    let first = page("read-page");
    assert!(asks(&first, &peer), "the first read asks");
    // The one answer the sheet records for it.
    peer.governance_mut()
        .put_governed_grants(
            &[tilo_action_policy::agent_answers::answer_grant(
                workspace,
                Subject::task(ISSUE),
                (
                    &issue_answer_scope("so.tilo.browser", ("browser.tab", "read-page"), false),
                    tilo_action_policy::ActionKind::Outside,
                ),
                Answer::Allow,
                Subject::person("governor"),
            )],
            &key,
            now,
        )
        .unwrap();
    for _ in 0..8 {
        for action in ["read-page", "capture-page", "find-page-element"] {
            page(action).check_run(&peer, workspace, now).unwrap();
        }
    }
    let mut elsewhere = page("read-page");
    elsewhere.binding.task_id = "tasks.tracker/issues/2".into();
    assert!(asks(&elsewhere, &peer), "another issue's run still asks");
    let mut submit = page("submit-page");
    submit.consequential = true;
    assert!(
        asks(&submit, &peer),
        "an action whose contract confirms still asks"
    );
    let mut bookmarks = page("add-bookmark");
    bookmarks.resource_kind = "browser.bookmarks".into();
    assert!(
        asks(&bookmarks, &peer),
        "another kind of resource still asks"
    );
    assert_eq!(
        issue_answer_scope("so.tilo.mail", ("mail.message", "send-message"), true),
        tilo_action_policy::agent_answers::AnswerScope::action(
            "so.tilo.mail",
            "mail.message",
            "send-message"
        ),
        "an action whose contract confirms is answered alone"
    );
}

/// A run asked inside a turn acts as the person whose message caused it: the
/// same grants as them, the record's bound narrowing them, and no step once
/// the turn has ended. A cause this device does not hold yet holds the step
/// instead of refusing it; a cause someone else wrote, or one the agent takes
/// no part in, refuses it.
#[test]
fn a_run_inside_a_turn_acts_as_the_person_whose_message_caused_it() {
    let workspace = "ffffffff-ffff-4fff-8fff-ffffffffffff";
    let (mut peer, starter, key, now) = policy_fixture(workspace);
    let mut requirement = approve_task(&starter);
    let starter_id = requirement.binding.starter.clone().unwrap();
    requirement.binding.turn = Some(tilo_action_policy::turn::Turn::new(
        "chat/workspace/channels/1",
        "m1",
    ));

    // The message that caused it reached this device: the step runs as its
    // author.
    requirement
        .check_turn(&peer, workspace, Some(&Looked::by(&starter_id)), now)
        .unwrap();

    // Its cause has not arrived: the step waits rather than running unasked.
    let held = requirement
        .check_turn(&peer, workspace, None, now)
        .unwrap_err();
    assert!(NativeTaskActionRequirement::awaits_cause(&held), "{held}");

    // A message someone else wrote, or one in a conversation the agent takes
    // no part in, is not this run's cause.
    let elsewhere = requirement
        .check_turn(
            &peer,
            workspace,
            Some(&Looked::by(hex::encode([9u8; 32]))),
            now,
        )
        .unwrap_err();
    assert!(
        !NativeTaskActionRequirement::awaits_cause(&elsewhere),
        "{elsewhere}"
    );
    let apart = requirement
        .check_turn(
            &peer,
            workspace,
            Some(&Looked::by(&starter_id).apart()),
            now,
        )
        .unwrap_err();
    assert!(
        !NativeTaskActionRequirement::awaits_cause(&apart),
        "{apart}"
    );

    // A turn that ended takes no further step.
    let ended = requirement
        .check_turn(
            &peer,
            workspace,
            Some(&Looked::by(&starter_id).ended()),
            now,
        )
        .unwrap_err();
    assert!(ended.contains("ended"), "{ended}");

    // The record's bound narrows what the turn allows.
    let bound = Looked::by(&starter_id).bounded_by(vec![tilo_action_policy::Rule::new(
        action_node(workspace, &requirement),
        KindSet::UNDOABLE,
        Answer::Deny,
    )]);
    let narrowed = requirement
        .check_turn(&peer, workspace, Some(&bound), now)
        .unwrap_err();
    assert!(narrowed.contains("not allowed"), "{narrowed}");

    // The person's own grants still hold: what they may not do, the run may
    // not do for them.
    peer.governance_mut()
        .put_governed_grants(
            &[Grant::new(
                "starter-no-notes",
                Subject::person(starter_id.clone()),
                TreePath::workspace(workspace).app("so.tilo.notes"),
                KindSet::ALL,
                Answer::Deny,
                Subject::person("governor"),
            )],
            &key,
            now,
        )
        .unwrap();
    assert!(
        requirement
            .check_turn(&peer, workspace, Some(&Looked::by(&starter_id)), now)
            .is_err(),
        "the starter's own refusal reaches the run"
    );
}

/// A run that answers a person's message in Chat's conversation is a turn of
/// that conversation, not a task of the Tasks tracker: the run's task id is
/// the message's address there. The journal accepts it as a native task's
/// owner beside a tracker issue, so an agent's own invocations on the turn are
/// admitted.
#[test]
fn an_agent_door_connections_run_is_one_of_its_own() {
    assert!(super::is_door_run(
        "so.tilo.session/agent-door/8f2c0b6e4a1d4f3c9b7e5a2d1c0f9e8d"
    ));
    for refused in [
        "so.tilo.session/agent-door/",
        "so.tilo.session/agent-door/a/b",
        "so.tilo.session/agent-door/a b",
        "tasks.tracker/issues/i_1",
    ] {
        assert!(!super::is_door_run(refused), "{refused}");
    }
}

#[test]
fn a_conversation_turns_message_addresses_a_run() {
    assert!(super::is_conversation_turn(
        "chat.workspace/channels/c_780ce7269827cf52/messages/d76dff0868974728"
    ));
    assert!(super::is_conversation_turn(
        "chat.workspace/channels/c_1/messages/m_1"
    ));
    for refused in [
        "chat.workspace/channels/c_1",
        "chat.workspace/channels//messages/m_1",
        "chat.workspace/channels/c_1/messages/",
        "chat.workspace/channels/c_1/messages/a/b",
        "tasks.tracker/issues/i_1",
        "chat.workspace/other/c_1/messages/m_1",
    ] {
        assert!(!super::is_conversation_turn(refused), "{refused}");
    }
}
