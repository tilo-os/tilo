//! The Mail app's actions an agent takes for its person: once that person narrowed it with an
//! answer of theirs, it takes only the action an answer names, and the others are refused.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion"
)]

#[cfg(feature = "agents")]
#[test]
fn a_narrowed_agent_needs_a_rule_that_names_the_mail_action() {
    use tilo_action_policy::ActingPolicy as _;
    use tilo_action_policy::agent_answers::{self, AnswerScope};
    use tilo_action_policy::{
        ActionKind, Answer, Authority, Context, Grantee, TreePath, WorkspacePolicy,
    };
    use tilo_member_types::Acting;

    const WORKSPACE: &str = "0b7c7a52-6f7e-4a55-9a51-1c2f4b3f7e10";
    const APP: &str = "so.tilo.mail";
    const KIND: &str = "mail.mailbox";
    const AGENT: &str = "scout";
    const PERSON: &str = "ada";
    let now = || Context::at(tilo_clock::unix_now_ms());
    let action = |name: &str| {
        TreePath::workspace(WORKSPACE)
            .app(APP)
            .kind(KIND)
            .resource("inbox")
            .action(name)
    };
    let answer_of = |action: &str, answer: Answer| {
        agent_answers::grant(
            WORKSPACE,
            (AGENT, PERSON),
            (&AnswerScope::action(APP, KIND, action), ActionKind::Read),
            answer,
            Grantee::person(PERSON),
        )
    };
    let mut policy = WorkspacePolicy::new(WORKSPACE, PERSON);
    // One answer of the person's names a read of one action: it narrows their agent, and names
    // no other.
    let listing = answer_of(agent_actions::LIST, Answer::Allow);
    policy.grants.push(listing);
    assert!(
        policy.answers_narrow(AGENT, PERSON, &now()),
        "the person's own answer narrows their agent"
    );
    let acting = Acting::handed_agent(AGENT, PERSON);
    let preset = acting.handed_preset(WORKSPACE);
    let decided = |policy: &WorkspacePolicy, name: &str| {
        Authority::of_acting(&acting, policy, &preset)
            .decide(&action(name), ActionKind::Read, &now())
            .answer
    };
    assert_eq!(
        decided(&policy, agent_actions::READ),
        Answer::Deny,
        "a narrowed agent takes no action no answer of its person names"
    );
    assert_eq!(
        decided(&policy, agent_actions::SEARCH),
        Answer::Deny,
        "nor a search"
    );

    // A rule that names the read admits it, and still nothing else.
    policy
        .grants
        .push(answer_of(agent_actions::READ, Answer::Allow));
    assert_eq!(decided(&policy, agent_actions::READ), Answer::Allow);
    assert_eq!(
        decided(&policy, agent_actions::SEARCH),
        Answer::Deny,
        "the search is still named by no answer"
    );

    // A rule that names the search admits that one too.
    policy
        .grants
        .push(answer_of(agent_actions::SEARCH, Answer::Allow));
    assert_eq!(decided(&policy, agent_actions::READ), Answer::Allow);
    assert_eq!(decided(&policy, agent_actions::SEARCH), Answer::Allow);
}

/// The Mail app's own actions this test names, as its types declare them.
#[cfg(feature = "agents")]
mod agent_actions {
    /// Reading a mailbox's messages.
    pub(super) const READ: &str = "read";
    /// Listing what a mailbox holds.
    pub(super) const LIST: &str = "list";
    /// Searching a mailbox.
    pub(super) const SEARCH: &str = "search";
}
