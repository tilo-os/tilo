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
            Grantee::agent(AGENT),
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
        agents: std::collections::BTreeMap::default(),
    };
    let verdict =
        Authority::person(policy.party_of(Grantee::agent(AGENT))).decide(node, kind, &cx());
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
        AGENT,
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
            AnswerScope::app("a"),
            AnswerScope::kind("a", "k"),
            AnswerScope::action("a", "k", "x"),
        ] {
            assert_eq!(AnswerScope::of_node(&scope.node(WORKSPACE)), Some(scope));
        }
        assert_eq!(AnswerScope::of_node(&TreePath::workspace(WORKSPACE)), None);
    }

    phase scopes_read_back_from_their_keys;
    {
        for scope in [
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
            .bounded_by(Party::new(vec![Grantee::agent(AGENT)], &all));
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
        AGENT,
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
