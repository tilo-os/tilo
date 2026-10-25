use super::{NativeTaskActionRequirement, TrackerRun};
use automerge::{AutoCommit, ROOT, ReadDoc, transaction::Transactable};
use ed25519_dalek::SigningKey;
use tilo_action_policy::{Answer, Grant, KindSet, Subject, TreePath, roles::AgentPreset};
use tilo_automerge::ApplyIncoming;
use tilo_peer_state::{SeedState, StatePeer, seed_room};
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
    };
    sign_device_cert(&mut cert, &root).unwrap();
    cert
}

#[test]
fn approved_run_cannot_exceed_its_starters_live_action_permission() {
    let room = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let (mut peer, starter, key, now) = policy_fixture(room);
    let requirement = approve_task(&starter);
    assert_exact_binding(&peer, room, now, &requirement);
    assert_independent_task_grants(&mut peer, room, &key, now, &requirement);
    assert_live_starter_revocation(&mut peer, room, &starter, &key, now, &requirement);
}

/// An action the agent's grants answer "ask" for pauses the run for its
/// person, naming the grant, apart from one they deny.
#[test]
fn a_run_is_told_an_action_asks_first() {
    let room = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    let (mut peer, starter, key, now) = policy_fixture(room);
    let requirement = approve_task(&starter);
    let grant = |id: &str, answer| {
        Grant::new(
            id,
            Subject::agent("scribe"),
            action_node(room, &requirement),
            KindSet::UNDOABLE,
            answer,
            Subject::person("governor"),
        )
    };
    peer.governance_mut()
        .put_governed_grants(&[grant("agent-asks", Answer::Ask)], &key, now)
        .unwrap();
    let asks = requirement.check_run(&peer, room, now).unwrap_err();
    assert!(asks.contains("asks a person first"), "{asks}");
    assert!(asks.contains("agent-asks"), "{asks}");
    assert!(
        NativeTaskActionRequirement::awaits_confirmation(&asks),
        "{asks}"
    );
    peer.governance_mut()
        .put_governed_grants(&[grant("agent-denied", Answer::Deny)], &key, now)
        .unwrap();
    let denied = requirement.check_run(&peer, room, now).unwrap_err();
    assert!(denied.contains("not allowed"), "{denied}");
}

fn policy_fixture(room: &str) -> (StatePeer, DeviceCert, SigningKey, std::time::SystemTime) {
    admitted_fixture(room, collab_types::Role::Editor, None)
}

/// A room where the starter holds `role` and the agent is admitted with
/// `preset`, or, without one, holds a room-wide grant of every kind.
fn admitted_fixture(
    room: &str,
    role: collab_types::Role,
    preset: Option<AgentPreset>,
) -> (StatePeer, DeviceCert, SigningKey, std::time::SystemTime) {
    let governor = certificate(1, 2);
    let starter = certificate(3, 4);
    let key = SigningKey::from_bytes(&[2; 32]);
    let now = std::time::SystemTime::now();
    let roster = vec![governor.clone(), starter.clone()];
    let mut doc = AutoCommit::new();
    doc.apply_incoming(
        seed_room(room, &governor, &roster)
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
    let mut peer = StatePeer::new(room, governor, roster, RevocationList::new(), now).unwrap();
    peer.install_seed(
        SeedState {
            room_changes: doc
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
    let admission = preset.map_or_else(
        || {
            vec![Grant::new(
                "agent",
                Subject::agent("scribe"),
                TreePath::room(room),
                KindSet::ALL,
                Answer::Allow,
                Subject::person("governor"),
            )]
        },
        |preset| preset.grants(room, "scribe", &Subject::person("governor")),
    );
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
        room: &str,
        now: std::time::SystemTime,
    ) -> Result<(), String>;
}

impl CheckRun for NativeTaskActionRequirement {
    fn check_run(
        &self,
        peer: &StatePeer,
        room: &str,
        now: std::time::SystemTime,
    ) -> Result<(), String> {
        let binding = &self.binding;
        let run = TrackerRun {
            issue: binding.task_id.clone(),
            title: "Inspect notes".into(),
            instruction: "Inspect notes".into(),
            agent: "scribe".into(),
            epoch: 1,
            starter: binding.starter.clone().unwrap_or_default(),
            active: true,
            budget: None,
            subject: None,
            model: None,
            directions: Vec::new(),
            question: None,
        };
        self.check_tracker(peer, room, &run, now)
    }
}

fn assert_exact_binding(
    peer: &StatePeer,
    room: &str,
    now: std::time::SystemTime,
    requirement: &NativeTaskActionRequirement,
) {
    requirement.check_run(peer, room, now).unwrap();
    let mut stale = requirement.clone();
    stale.binding.epoch = 2;
    assert!(stale.check_run(peer, room, now).is_err());
    let mut privileged = requirement.clone();
    privileged.need.privileged = true;
    assert!(privileged.check_run(peer, room, now).is_err());
}

fn action_node(room: &str, requirement: &NativeTaskActionRequirement) -> TreePath {
    tilo_actions::action_node(
        &TreePath::room(room),
        &requirement.app,
        &requirement.resource_kind,
        &requirement.resource,
        None,
        &requirement.action,
    )
}

fn assert_independent_task_grants(
    peer: &mut StatePeer,
    room: &str,
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
                TreePath::room(room),
                KindSet::ALL,
                Answer::Deny,
                Subject::person("governor"),
            )],
            key,
            now,
        )
        .unwrap();
    requirement.check_run(peer, room, now).unwrap();
    let mut other_requirement = requirement.clone();
    other_requirement.binding.task_id = other;
    assert!(other_requirement.check_run(peer, room, now).is_err());
}

fn assert_live_starter_revocation(
    peer: &mut StatePeer,
    room: &str,
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
                action_node(room, requirement),
                KindSet::UNDOABLE,
                Answer::Deny,
                Subject::person("governor"),
            )],
            key,
            now,
        )
        .unwrap();
    // Room write and the agent's own grant remain valid: only this exact
    // operation is revoked from the person who consciously started the run.
    assert!(
        peer.permissions(starter.device_public_key, now)
            .unwrap()
            .allows(
                &TreePath::room(room),
                tilo_action_policy::ActionKind::Undoable,
                now
            )
    );
    assert!(
        requirement
            .check_run(peer, room, now)
            .unwrap_err()
            .contains("starter")
    );
}

#[test]
fn an_action_asking_first_pauses_until_confirmed_and_deny_still_refuses() {
    use tilo_action_policy::agent_answers::{self, AnswerScope};
    let room = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    let (mut peer, starter, key, now) = policy_fixture(room);
    let requirement = approve_task(&starter);
    let awaits = |requirement: &NativeTaskActionRequirement, peer: &StatePeer| {
        let error = requirement.check_run(peer, room, now).unwrap_err();
        NativeTaskActionRequirement::awaits_confirmation(&error)
    };

    // An action whose contract confirms (it deletes for good) asks first,
    // though the agent's grants allow its kind.
    let mut purge = requirement.clone();
    purge.consequential = true;
    assert!(awaits(&purge, &peer));
    purge.confirmed = true;
    purge.check_run(&peer, room, now).unwrap();

    // The person tells the agent Notes asks first.
    let scope = AnswerScope::app("so.tilo.notes");
    let set = |answer: Answer, peer: &mut StatePeer| {
        peer.governance_mut()
            .put_governed_grants(
                &[agent_answers::grant(
                    room,
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
    confirmed.check_run(&peer, room, now).unwrap();

    // Not allowed: refused outright, confirmed or not.
    set(Answer::Deny, &mut peer);
    let error = confirmed.check_run(&peer, room, now).unwrap_err();
    assert!(
        !NativeTaskActionRequirement::awaits_confirmation(&error),
        "{error}"
    );

    // Always allowing the one action lifts its contract's confirmation.
    let always = AnswerScope::action("so.tilo.notes", "document", "append");
    peer.governance_mut()
        .put_governed_grants(
            &[agent_answers::grant(
                room,
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
    purge.check_run(&peer, room, now).unwrap();
}

/// An action is decided by its kind and the person's answers alone: an agent
/// reads where reading is allowed, and an effect reaching outside the
/// document (writing a file, say) asks first. An attended action (a chooser,
/// a sign-in, a secret shown) asks every time: even "Always allow this"
/// does not let the agent run it unasked, and Deny still refuses it.
#[test]
fn an_attended_action_asks_every_time_and_ordinary_kinds_follow_the_answers() {
    use tilo_action_policy::agent_answers::{self, AnswerScope};
    let room = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";
    let (mut peer, starter, key, now) = policy_fixture(room);
    let requirement = approve_task(&starter);
    let awaits = |requirement: &NativeTaskActionRequirement, peer: &StatePeer| {
        requirement
            .check_run(peer, room, now)
            .err()
            .is_some_and(|error| NativeTaskActionRequirement::awaits_confirmation(&error))
    };

    let mut read = requirement.clone();
    read.need.kind = tilo_action_policy::ActionKind::Read;
    read.check_run(&peer, room, now).unwrap();

    let mut save = requirement.clone();
    save.action = "save".into();
    save.need.kind = tilo_action_policy::ActionKind::Outside;
    assert!(awaits(&save, &peer), "an outside effect asks first");
    save.confirmed = true;
    save.check_run(&peer, room, now).unwrap();

    let mut choose = requirement;
    choose.action = "open-file".into();
    choose.need.kind = tilo_action_policy::ActionKind::Read;
    choose.consequential = true;
    choose.attended = true;
    let answer = |answer: Answer, peer: &mut StatePeer| {
        peer.governance_mut()
            .put_governed_grants(
                &[agent_answers::grant(
                    room,
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
    choose.check_run(&peer, room, now).unwrap();
    answer(Answer::Deny, &mut peer);
    assert!(choose.check_run(&peer, room, now).is_err());
    assert!(!awaits(&choose, &peer), "deny refuses outright");
}

/// A task a person starts runs on their behalf: an agent admitted with the
/// default presets, which grant it no changes of its own, takes the changes
/// its starter may take, held to the answers the room keeps for it.
#[test]
fn a_run_takes_its_starters_changes_whatever_preset_admitted_its_agent() {
    use tilo_action_policy::agent_answers::{self, AnswerScope};
    for preset in [AgentPreset::Observer, AgentPreset::Contributor] {
        let room = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
        let (mut peer, starter, key, now) =
            admitted_fixture(room, collab_types::Role::Editor, Some(preset));
        let requirement = approve_task(&starter);
        requirement
            .check_run(&peer, room, now)
            .unwrap_or_else(|error| panic!("{preset:?}: {error}"));

        // Outside effects still ask first by default.
        let mut outside = requirement.clone();
        outside.need.kind = tilo_action_policy::ActionKind::Outside;
        let asks = outside.check_run(&peer, room, now).unwrap_err();
        assert!(
            NativeTaskActionRequirement::awaits_confirmation(&asks),
            "{asks}"
        );

        peer.governance_mut()
            .put_governed_grants(
                &[agent_answers::grant(
                    room,
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
        let denied = requirement.check_run(&peer, room, now).unwrap_err();
        assert!(denied.contains("not allowed"), "{denied}");

        let policy =
            tilo_desktop_model::projection::room_policy_of(peer.admitted_room().unwrap(), room);
        let own = tilo_action_policy::Authority::person(policy.party_of(Subject::agent("scribe")))
            .decide(
                &action_node(room, &requirement),
                tilo_action_policy::ActionKind::Undoable,
                &tilo_action_policy::Context::at_time(now),
            );
        assert_eq!(own.answer, Answer::Deny, "{preset:?}");
    }
}

/// A grant naming the issue decides for its runs as the agent's own do: an
/// outside action the agent's delegation asks about runs once the issue is
/// allowed it in that app, in that issue's runs alone.
#[test]
fn a_grant_naming_the_issue_lets_its_runs_act_without_asking() {
    let room = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee";
    let (mut peer, starter, key, now) = admitted_fixture(
        room,
        collab_types::Role::Editor,
        Some(AgentPreset::Operator),
    );
    let notes = TreePath::room(room).app("so.tilo.notes");
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
    let asks = outside.check_run(&peer, room, now).unwrap_err();
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
    outside.check_run(&peer, room, now).unwrap();
    let mut other = outside;
    other.binding.task_id = "tasks.tracker/issues/2".into();
    let asks = other.check_run(&peer, room, now).unwrap_err();
    assert!(
        NativeTaskActionRequirement::awaits_confirmation(&asks),
        "{asks}"
    );

    // "Allow for this issue" answers the one action, so an action whose
    // contract confirms runs unasked too, in that issue's runs alone.
    let mut purge = approve_task(&starter);
    purge.consequential = true;
    let asks = purge.check_run(&peer, room, now).unwrap_err();
    assert!(
        NativeTaskActionRequirement::awaits_confirmation(&asks),
        "{asks}"
    );
    peer.governance_mut()
        .put_governed_grants(
            &[tilo_action_policy::agent_answers::answer_grant(
                room,
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
    purge.check_run(&peer, room, now).unwrap();
    purge.binding.task_id = "tasks.tracker/issues/2".into();
    let asks = purge.check_run(&peer, room, now).unwrap_err();
    assert!(
        NativeTaskActionRequirement::awaits_confirmation(&asks),
        "{asks}"
    );
}

/// The starter's own rights bound the run, whatever the agent was admitted
/// with: a person who may not change a note starts no run that changes it,
/// and one who may not change the room starts no run at all.
#[test]
fn a_run_never_exceeds_a_starter_who_may_not_change_the_note() {
    let room = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";
    let (mut peer, starter, key, now) = admitted_fixture(
        room,
        collab_types::Role::Editor,
        Some(AgentPreset::Operator),
    );
    peer.governance_mut()
        .put_governed_grants(
            &[Grant::new(
                "starter-reads-notes",
                Subject::person(hex::encode(starter.user_public_key)),
                TreePath::room(room).app("so.tilo.notes"),
                KindSet::UNDOABLE,
                Answer::Deny,
                Subject::person("governor"),
            )],
            &key,
            now,
        )
        .unwrap();
    let requirement = approve_task(&starter);
    let refused = requirement.check_run(&peer, room, now).unwrap_err();
    assert!(refused.contains("starter"), "{refused}");

    let (peer, commenter, _key, now) = admitted_fixture(
        room,
        collab_types::Role::Commenter,
        Some(AgentPreset::Operator),
    );
    let refused = approve_task(&commenter)
        .check_run(&peer, room, now)
        .unwrap_err();
    assert!(refused.contains("starter"), "{refused}");
}

/// A grant letting the issue govern the room never lets a run govern it: the
/// run holds the issue's delegation, which is the person who started it, and a
/// task never governs. Even approved, the run is refused.
#[test]
fn a_grant_letting_the_issue_govern_the_room_still_asks_its_starter() {
    let room = "ffffffff-ffff-4fff-8fff-ffffffffffff";
    let (mut peer, starter, key, now) = admitted_fixture(
        room,
        collab_types::Role::Editor,
        Some(AgentPreset::Operator),
    );
    peer.governance_mut()
        .put_governed_grants(
            &[Grant::new(
                "issue-invites",
                Subject::task(ISSUE),
                tilo_action_policy::roles::governance_action(room, "invite"),
                KindSet::of(tilo_action_policy::roles::GOVERNANCE_KIND),
                Answer::Allow,
                Subject::person("governor"),
            )],
            &key,
            now,
        )
        .unwrap();
    let mut invite = approve_task(&starter);
    invite.need.kind = tilo_action_policy::roles::GOVERNANCE_KIND;
    let refused = invite.check_run(&peer, room, now).unwrap_err();
    assert!(!refused.is_empty(), "{refused}");
}
