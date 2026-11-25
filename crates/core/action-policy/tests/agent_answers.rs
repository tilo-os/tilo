//! A person's Allow, Ask first and Deny for one of their agents.

use tilo_action_policy::{
    ActionKind, Answer, Authority, Context, Grant, Grantee, KindSet, TreePath, WorkspacePolicy,
    agent_answers::{self, AnswerScope},
};

const WORKSPACE: &str = "r";
const AGENT: &str = "helper";
const PERSON: &str = "a11ce";

fn cx() -> Context<'static> {
    Context::at(1_000)
}

/// A workspace granting the agent what it may change and reach, plus `extra`.
fn grants(extra: Vec<Grant>) -> Vec<Grant> {
    let grant = |id: &str, kinds, answer| {
        Grant::new(
            id.to_owned(),
            Grantee::agent(AGENT, PERSON),
            TreePath::workspace(WORKSPACE),
            kinds,
            answer,
            Grantee::person(PERSON),
        )
    };
    let mut grants = vec![
        grant("agent-member", KindSet::READ_UNDOABLE, Answer::Allow),
        grant("agent-member:outside", KindSet::OUTSIDE, Answer::Ask),
    ];
    grants.extend(extra);
    grants
}

fn decide_at(grants: &[Grant], node: &TreePath, kind: ActionKind, consequential: bool) -> Answer {
    let policy = WorkspacePolicy {
        workspace: WORKSPACE.into(),
        owner: Some(PERSON.into()),
        members: std::collections::BTreeMap::default(),
        joined: std::collections::BTreeMap::default(),
        grants: grants.to_vec(),
        custom_roles: std::collections::BTreeMap::default(),
        assigned_roles: std::collections::BTreeMap::default(),
    };
    let verdict = Authority::person(policy.party_of_agent(AGENT, PERSON)).decide(node, kind, &cx());
    verdict.parts.first().map_or(Answer::Deny, |decision| {
        agent_answers::held_to_contract(decision, consequential)
    })
}

fn mail_action(action: &str) -> TreePath {
    TreePath::workspace(WORKSPACE)
        .app("so.tilo.mail")
        .kind("mail.mailbox")
        .resource("inbox")
        .action(action)
}

fn answer(scope: &AnswerScope, kind: ActionKind, answer: Answer) -> Grant {
    agent_answers::grant(
        WORKSPACE,
        (AGENT, PERSON),
        (scope, kind),
        answer,
        Grantee::person(PERSON),
    )
}

tilo_test_env::scenario! {
    fn a_persons_answers_decide_their_agent;

    phase defaults_allow_reading_and_changes_and_ask_before_outside_or_consequential_actions;
    {
        use agent_answers::default_answer;
        assert_eq!(default_answer(ActionKind::Read, false), Answer::Allow);
        assert_eq!(default_answer(ActionKind::Undoable, false), Answer::Allow);
        assert_eq!(default_answer(ActionKind::Outside, false), Answer::Ask);
        // Deleting for good is an undoable-kind action whose contract confirms.
        assert_eq!(default_answer(ActionKind::Undoable, true), Answer::Ask);
        assert_eq!(default_answer(ActionKind::Read, true), Answer::Ask);
    }

    phase an_operator_agent_reads_changes_and_asks_before_sending;
    {
        let grants = grants(Vec::new());
        let read = decide_at(&grants, &mail_action("list"), ActionKind::Read, false);
        let change = decide_at(
            &grants,
            &mail_action("archive"),
            ActionKind::Undoable,
            false,
        );
        let send = decide_at(
            &grants,
            &mail_action("send_message"),
            ActionKind::Outside,
            true,
        );
        let purge = decide_at(&grants, &mail_action("purge"), ActionKind::Undoable, true);
        assert_eq!(
            [read, change, send, purge],
            [Answer::Allow, Answer::Allow, Answer::Ask, Answer::Ask]
        );
    }

    phase an_app_answer_overrides_the_admission_for_that_app_only;
    {
        let grants = grants(vec![answer(
            &AnswerScope::app("so.tilo.mail"),
            ActionKind::Undoable,
            Answer::Deny,
        )]);
        assert_eq!(
            decide_at(
                &grants,
                &mail_action("archive"),
                ActionKind::Undoable,
                false
            ),
            Answer::Deny
        );
        let other = TreePath::workspace(WORKSPACE)
            .app("so.tilo.notes")
            .kind("note")
            .resource("n1")
            .action("edit");
        assert_eq!(
            decide_at(&grants, &other, ActionKind::Undoable, false),
            Answer::Allow
        );
    }

    phase a_kind_answer_beats_the_app_answer;
    {
        let grants = grants(vec![
            answer(
                &AnswerScope::app("so.tilo.mail"),
                ActionKind::Outside,
                Answer::Deny,
            ),
            answer(
                &AnswerScope::kind("so.tilo.mail", "mail.mailbox"),
                ActionKind::Outside,
                Answer::Ask,
            ),
        ]);
        assert_eq!(
            decide_at(
                &grants,
                &mail_action("send_message"),
                ActionKind::Outside,
                false
            ),
            Answer::Ask
        );
    }

    phase allowing_a_kind_wide_never_lifts_a_contract_confirmation;
    {
        let grants = grants(vec![answer(
            &AnswerScope::app("so.tilo.mail"),
            ActionKind::Outside,
            Answer::Allow,
        )]);
        assert_eq!(
            decide_at(
                &grants,
                &mail_action("send_message"),
                ActionKind::Outside,
                true
            ),
            Answer::Ask
        );
        // An outside action that confirms nothing is allowed by it.
        assert_eq!(
            decide_at(&grants, &mail_action("sync"), ActionKind::Outside, false),
            Answer::Allow
        );
    }

    phase always_allowing_one_action_allows_it_and_nothing_beside_it;
    {
        let scope = AnswerScope::action("so.tilo.mail", "mail.mailbox", "send_message");
        let grants = grants(vec![answer(&scope, ActionKind::Outside, Answer::Allow)]);
        assert_eq!(
            decide_at(
                &grants,
                &mail_action("send_message"),
                ActionKind::Outside,
                true
            ),
            Answer::Allow
        );
        // The action's node may pass through a field of its resource.
        let through_field = TreePath::workspace(WORKSPACE)
            .app("so.tilo.mail")
            .kind("mail.mailbox")
            .resource("inbox")
            .field("outbox")
            .action("send_message");
        assert_eq!(
            decide_at(&grants, &through_field, ActionKind::Outside, true),
            Answer::Allow
        );
        assert_eq!(
            decide_at(&grants, &mail_action("purge"), ActionKind::Undoable, true),
            Answer::Ask
        );
    }

    phase a_deny_for_one_action_holds_under_a_broader_allow;
    {
        let grants = grants(vec![
            answer(
                &AnswerScope::app("so.tilo.mail"),
                ActionKind::Outside,
                Answer::Allow,
            ),
            answer(
                &AnswerScope::action("so.tilo.mail", "mail.mailbox", "send_message"),
                ActionKind::Outside,
                Answer::Deny,
            ),
        ]);
        assert_eq!(
            decide_at(
                &grants,
                &mail_action("send_message"),
                ActionKind::Outside,
                false
            ),
            Answer::Deny
        );
    }

    phase scopes_read_back_from_their_nodes;
    {
        for scope in [
            AnswerScope::Workspace,
            AnswerScope::app("a"),
            AnswerScope::kind("a", "k"),
            AnswerScope::action("a", "k", "x"),
        ] {
            assert_eq!(AnswerScope::of_node(&scope.node(WORKSPACE)), Some(scope));
        }
    }

    phase scopes_read_back_from_their_keys;
    {
        for scope in [
            AnswerScope::Workspace,
            AnswerScope::app("so.tilo.notes"),
            AnswerScope::kind("so.tilo.notes", "note"),
            AnswerScope::action("so.tilo.notes", "note", "send"),
        ] {
            assert_eq!(AnswerScope::parse(&scope.key()), Some(scope));
        }
        for wrong in ["", "app:", "app", "note:so.tilo.notes", "kind:a", "app:a:b"] {
            assert_eq!(AnswerScope::parse(wrong), None, "{wrong}");
        }
    }
}

/// What a person answered their agent for exactly one action is their own answer to it, given
/// before it came: where they would be asked, the agent goes ahead. An answer for a whole app
/// does not stand in for them, one somebody else gave does not, and what they may not do at all
/// stays refused.
#[test]
fn an_answer_for_one_action_is_its_persons_answer_given_in_advance() {
    use tilo_action_policy::Party;
    const TERMINAL: &str = "so.tilo.terminal";
    let command = TreePath::workspace(WORKSPACE)
        .app(TERMINAL)
        .kind("terminal.session")
        .resource("session")
        .action("run");
    let verdict = |person: Answer, given: Vec<Grant>| {
        let mut all = grants(given);
        all.push(Grant::new(
            "role:outside",
            Grantee::person(PERSON),
            TreePath::workspace(WORKSPACE),
            KindSet::OUTSIDE,
            person,
            Grantee::person(PERSON),
        ));
        let authority = Authority::person(Party::new(vec![Grantee::person(PERSON)], &all))
            .bounded_by(Party::new(vec![Grantee::agent(AGENT, PERSON)], &all));
        (
            authority
                .decide(&command, ActionKind::Outside, &cx())
                .answer,
            authority.answer(&command, ActionKind::Outside, false, &cx()),
        )
    };
    let one = AnswerScope::action(TERMINAL, "terminal.session", "run");
    let everything = AnswerScope::app(TERMINAL);
    let allowed = |scope: &AnswerScope| vec![answer(scope, ActionKind::Outside, Answer::Allow)];
    assert_eq!(verdict(Answer::Ask, Vec::new()), (Answer::Ask, Answer::Ask));
    assert_eq!(
        verdict(Answer::Ask, allowed(&one)),
        (Answer::Allow, Answer::Allow)
    );
    assert_eq!(
        verdict(Answer::Ask, allowed(&everything)),
        (Answer::Ask, Answer::Ask)
    );
    assert_eq!(
        verdict(Answer::Deny, allowed(&one)),
        (Answer::Deny, Answer::Deny)
    );
    let by_somebody_else = agent_answers::grant(
        WORKSPACE,
        (AGENT, "b0b"),
        (&one, ActionKind::Outside),
        Answer::Allow,
        Grantee::person("b0b"),
    );
    assert_eq!(
        verdict(Answer::Ask, vec![by_somebody_else]),
        (Answer::Ask, Answer::Ask)
    );
}

/// Allow for an outside effect may be given for one action of any app, and for a whole app
/// only within the person's own account.
#[test]
fn allowing_an_outside_effect_takes_one_action_outside_the_persons_account() {
    use agent_answers::{ACCOUNT_APP, may_give};
    let outside = ActionKind::Outside;
    let terminal = AnswerScope::action("so.tilo.terminal", "terminal.session", "run");
    assert!(may_give(Some(&terminal), outside, Answer::Allow));
    for wide in [
        AnswerScope::app("so.tilo.terminal"),
        AnswerScope::kind("so.tilo.terminal", "terminal.session"),
    ] {
        assert!(!may_give(Some(&wide), outside, Answer::Allow), "{wide:?}");
        assert!(may_give(Some(&wide), outside, Answer::Ask), "{wide:?}");
    }
    assert!(!may_give(None, outside, Answer::Allow));
    assert!(may_give(
        Some(&AnswerScope::app(ACCOUNT_APP)),
        outside,
        Answer::Allow
    ));
    assert!(may_give(None, ActionKind::Undoable, Answer::Allow));
}

/// A workspace with `grants`, and no other state to decide by.
fn policy_with(grants: Vec<Grant>) -> WorkspacePolicy {
    WorkspacePolicy {
        workspace: WORKSPACE.into(),
        owner: Some(PERSON.into()),
        members: std::collections::BTreeMap::default(),
        joined: std::collections::BTreeMap::default(),
        grants,
        custom_roles: std::collections::BTreeMap::default(),
        assigned_roles: std::collections::BTreeMap::default(),
    }
}

/// A person stops their agent: an answer denying it everything on the workspace, which no more
/// specific grant of the agent lifts, until the person takes the answer back.
#[test]
fn a_stop_denies_everything_and_no_grant_of_the_agent_lifts_it() {
    let stop = agent_answers::stop_grant(WORKSPACE, AGENT, PERSON);
    let mut all = grants(vec![
        stop,
        // Its certificate's rights, and an answer its person gave it earlier.
        Grant::new(
            "agent-owned",
            Grantee::agent(AGENT, PERSON),
            mail_action("send"),
            KindSet::ALL,
            Answer::Allow,
            Grantee::person(PERSON),
        ),
        agent_answers::grant(
            WORKSPACE,
            (AGENT, PERSON),
            (&AnswerScope::app("so.tilo.mail"), ActionKind::Outside),
            Answer::Allow,
            Grantee::person(PERSON),
        ),
    ]);
    assert_eq!(
        decide_at(&all, &mail_action("send"), ActionKind::Outside, false),
        Answer::Deny,
        "nothing of a stopped agent runs"
    );
    assert_eq!(
        decide_at(
            &all,
            &TreePath::workspace(WORKSPACE),
            ActionKind::Read,
            false
        ),
        Answer::Deny
    );
    let policy = policy_with(all.clone());
    assert!(policy.stops(AGENT, PERSON, &cx()));
    // Letting it go on takes the answer back: it acts again.
    all.retain(|grant| grant.id != agent_answers::stop_grant_id(AGENT, PERSON));
    assert_eq!(
        decide_at(&all, &mail_action("send"), ActionKind::Outside, false),
        Answer::Allow
    );
}

/// A person writes what their own agent may do, and what it may not, in a workspace they do not
/// administer: their say over their agent needs no governance. An agent is the pair of its id and
/// its person, so what another person writes for an agent of the same id is about their own, and
/// reaches this one not at all: neither stopping it nor holding it back.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one person and another, writing for agents of one id, read as one scenario"
)]
fn a_person_answers_for_their_own_agent_without_governing() {
    use tilo_action_policy::{Acting, Change};
    let mut members = std::collections::BTreeMap::new();
    members.insert(PERSON.to_owned(), tilo_action_policy::Role::Viewer);
    members.insert("b0b".to_owned(), tilo_action_policy::Role::Viewer);
    let mut policy = policy_with(Vec::new());
    policy.owner = Some("0wner".into());
    policy.members = members;
    let person = Acting::person(PERSON);
    let read = policy.clone();
    let authority = Authority::of_acting(&person, &read, &[]);
    let stop = || agent_answers::stop_grant(WORKSPACE, AGENT, PERSON);
    assert!(
        policy
            .apply(PERSON, &authority, Change::Grant(stop()), &cx(), false)
            .is_ok(),
        "a stop is the person's own answer for their agent"
    );
    assert!(policy.stops(AGENT, PERSON, &cx()));
    assert!(
        policy
            .apply(
                PERSON,
                &authority,
                Change::Revoke(agent_answers::stop_grant_id(AGENT, PERSON)),
                &cx(),
                false,
            )
            .is_ok(),
        "and taking it back is too"
    );
    let bob_acting = Acting::person("b0b");
    let bob = Authority::of_acting(&bob_acting, &read, &[]);
    // Bob's stop and an answer keeping the agent from reading mail are his own, for his own
    // agent of that id: admitted, and inert for the agent acting for the person.
    let bobs_stop = agent_answers::stop_grant(WORKSPACE, AGENT, "b0b");
    let bobs_deny = agent_answers::grant(
        WORKSPACE,
        (AGENT, "b0b"),
        (&AnswerScope::app("so.tilo.mail"), ActionKind::Read),
        Answer::Deny,
        Grantee::person("b0b"),
    );
    let before = policy.clone();
    for change in [Change::Grant(bobs_stop), Change::Grant(bobs_deny)] {
        let applied = policy.apply("b0b", &bob, change, &cx(), false);
        assert!(applied.is_ok(), "{applied:?}");
    }
    assert!(policy.stops(AGENT, "b0b", &cx()), "Bob stopped his own");
    assert!(!policy.stops(AGENT, PERSON, &cx()), "and not the person's");
    assert!(
        policy.answers_narrow(AGENT, "b0b", &cx()) && !policy.answers_narrow(AGENT, PERSON, &cx()),
        "Bob's answers narrow his own agent of that id, and leave the person's as it was"
    );
    let theirs = Acting::handed_agent(AGENT, PERSON);
    let preset = theirs.handed_preset(WORKSPACE);
    for (node, kind) in [
        (mail_action("send"), ActionKind::Outside),
        (
            TreePath::workspace(WORKSPACE).app("so.tilo.mail"),
            ActionKind::Read,
        ),
        (TreePath::workspace(WORKSPACE), ActionKind::Read),
    ] {
        let decided = |policy: &WorkspacePolicy| {
            (
                Authority::person(policy.party_of_agent(AGENT, PERSON))
                    .decide(&node, kind, &cx())
                    .answer,
                Authority::of_acting(&theirs, policy, &preset)
                    .decide(&node, kind, &cx())
                    .answer,
            )
        };
        assert_eq!(
            decided(&policy),
            decided(&before),
            "the person's agent is decided as before Bob wrote anything, at {node}"
        );
    }
    let bobs = Acting::handed_agent(AGENT, "b0b");
    assert_eq!(
        Authority::of_acting(&bobs, &policy, &bobs.handed_preset(WORKSPACE))
            .decide(&TreePath::workspace(WORKSPACE), ActionKind::Read, &cx())
            .answer,
        Answer::Deny,
        "while Bob's own agent of that id is stopped"
    );
    // Nor may Bob write under the person's name for their agent: a stop named as theirs but
    // given by him replaces or takes back nothing of theirs.
    let mut posing = agent_answers::stop_grant(WORKSPACE, AGENT, "b0b");
    posing.id = agent_answers::stop_grant_id(AGENT, PERSON);
    assert!(
        policy
            .apply("b0b", &bob, Change::Grant(posing), &cx(), false)
            .is_err(),
        "one person's say is not another's to write"
    );
    let mut stopped = policy.clone();
    stopped
        .apply(PERSON, &authority, Change::Grant(stop()), &cx(), false)
        .expect("the person stops their agent");
    assert!(
        stopped
            .apply(
                "b0b",
                &bob,
                Change::Revoke(agent_answers::stop_grant_id(AGENT, PERSON)),
                &cx(),
                false
            )
            .is_err(),
        "nor take back the person's stop"
    );
}

/// A rule of the workspace's own that denies an agent keeps it from what the rule names, whoever
/// the agent acts for: the vault and the mail alike are refused, never put to its person. The
/// workspace's Allow for the agent is no person's answer, and leaves the agent's part undecided.
/// The agent's part is its own, the pair's, whichever person its acting names beside it.
#[test]
fn the_workspaces_denial_of_an_agent_reaches_it_for_every_person() {
    const OWNER: &str = "0be7";
    let vault = TreePath::workspace(WORKSPACE)
        .app("so.tilo.passwords")
        .kind(tilo_action_policy::roles::WORKSPACE_VAULT_KIND);
    let mail = TreePath::workspace(WORKSPACE)
        .app("so.tilo.mail")
        .kind("mail.mailbox");
    let files = TreePath::workspace(WORKSPACE).app("so.tilo.files");
    let rule = |id: &str, node: &TreePath, answer| {
        Grant::new(
            id.to_owned(),
            Grantee::agent(AGENT, PERSON),
            node.clone(),
            KindSet::ALL,
            answer,
            Grantee::person(OWNER),
        )
    };
    let policy = WorkspacePolicy {
        workspace: WORKSPACE.into(),
        owner: Some(OWNER.into()),
        members: std::collections::BTreeMap::default(),
        joined: std::collections::BTreeMap::default(),
        grants: vec![
            rule("workspace:helper-kept-from-vault", &vault, Answer::Deny),
            rule("workspace:helper-kept-from-mail", &mail, Answer::Deny),
            rule("workspace:helper-files", &files, Answer::Allow),
        ],
        custom_roles: std::collections::BTreeMap::default(),
        assigned_roles: std::collections::BTreeMap::default(),
    };
    let agent_part = |node: &TreePath, kind| {
        let mut verdict =
            Authority::person(policy.party_of_agent(AGENT, PERSON)).decide(node, kind, &cx());
        verdict.parts.remove(0)
    };
    for node in [vault.clone().action("add"), mail.action("send")] {
        for kind in [ActionKind::Read, ActionKind::Undoable, ActionKind::Outside] {
            let decision = agent_part(&node, kind);
            assert_eq!(decision.answer, Answer::Deny, "at {node} {kind:?}");
            assert!(
                matches!(decision.basis, tilo_action_policy::Basis::Grant(_)),
                "the workspace's rule refuses it, not the want of one: {decision:?}"
            );
        }
    }
    let reading = agent_part(&files, ActionKind::Read);
    assert!(
        matches!(reading.basis, tilo_action_policy::Basis::NoGrant),
        "the workspace's Allow is no answer for the agent: {reading:?}"
    );
    // Another person's agent of the same id is another agent: the rule names none of it.
    let other = Authority::person(policy.party_of_agent(AGENT, "b0b")).decide(
        &vault.action("add"),
        ActionKind::Read,
        &cx(),
    );
    assert!(
        matches!(other.parts[0].basis, tilo_action_policy::Basis::NoGrant),
        "{other:?}"
    );
}
