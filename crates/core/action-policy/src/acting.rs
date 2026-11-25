//! Who made a change: one tree, and the one admission every action API takes.

use crate::{
    Actor, Admission, Answer, Asker, Authority, Context, Grant, GrantId, Grantee, Invoked,
    MemberId, Party, Reach, Refused, TreePath, WorkspacePolicy, admit,
};

/// [`crate::Acting::asker`] for an acting with software or a device between
/// the person and the change when `delegated`.
#[must_use]
pub const fn asker_of(delegated: bool, host_reaches_person: bool) -> Asker {
    Asker {
        reachable: !delegated || host_reaches_person,
        delegated,
    }
}

impl crate::Acting {
    /// Who an invocation of this acting asks: the person, who can answer when
    /// they act themselves or when the host running it reaches them
    /// (`host_reaches_person`); and whether someone acts for them, who is
    /// asked before an action the person attends.
    #[must_use]
    pub fn asker(&self, host_reaches_person: bool) -> Asker {
        asker_of(self.delegated(), host_reaches_person)
    }

    /// The preset every handed agent of the tree carries in `workspace`
    /// ([`crate::Acting::handed`], [`crate::roles::agent_preset`]), for the
    /// person it acts for: the one place a device's delegation to its agent
    /// becomes grants, so the device that admitted the agent and every
    /// receiver of its changes decide it alike.
    #[must_use]
    pub fn handed_preset(&self, workspace: &str) -> Vec<Grant> {
        self.nodes()
            .into_iter()
            .filter(|node| node.handed)
            .filter_map(|node| Some((node.member_id()?.as_agent()?, node.acted_for_person()?)))
            .flat_map(|((agent, own), person)| {
                crate::roles::agent_preset(workspace, (agent, own.as_str()), person)
            })
            .collect()
    }
}

impl<'a> Authority<'a> {
    /// The authority of `acting` under `policy`: an action is allowed only when
    /// every node of its tree allows it.
    ///
    /// A person is decided by their grants and their role's; from a device that
    /// is not theirs, by what such a device may do at most
    /// ([`crate::Cap::FOREIGN_DEVICE`]). An agent or bot by the grants naming
    /// it in the workspace and the ones its person gave it (`granted`). An
    /// automation by what the people it runs for gave it.
    #[must_use]
    pub fn of_acting(
        acting: &'a crate::Acting,
        policy: &'a WorkspacePolicy,
        granted: &'a [Grant],
    ) -> Self {
        let mut people: usize = 0;
        let mut person_parts = Vec::new();
        let mut others = Vec::new();
        // An automation acts with its owner's party: the tree names it for the
        // person it runs for, and its steps are that person's grants. One that
        // runs for nobody allows nothing.
        let owned = !acting.acted_for.is_empty();
        // The answers a person gave the agent session that caused the acting decide for the
        // whole tree, as the agent's own do.
        let answers: Vec<&Grant> = match &acting.cause {
            Some(cause) => crate::session_answers(&policy.grants, cause.as_str()).collect(),
            None => Vec::new(),
        };
        for node in acting.nodes() {
            match &node.by {
                Actor::Member(member) => match member {
                    MemberId::Person(person) => {
                        people = people.saturating_add(1);
                        let party = policy
                            .party(person.as_str())
                            .with_grants(granted)
                            .with_grants(answers.iter().copied());
                        person_parts.push(if node.foreign_device {
                            party.capped(crate::Cap::FOREIGN_DEVICE)
                        } else {
                            party
                        });
                    }
                    MemberId::Agent { .. } | MemberId::Bot(_) => {
                        others.push(
                            software_party(member, policy, granted)
                                .with_grants(answers.iter().copied()),
                        );
                    }
                },
                // A visitor holds what the workspace opens to any key, and nothing else.
                Actor::Visitor(_) => others.push(policy.party_of(Grantee::AnyKey)),
                Actor::Automation(_) if owned => {}
                // An app acting by itself holds the grants naming it, never a person's.
                Actor::Automation(_) => others.push(Party::default()),
                // An app acting by itself, in the process its device launched as it, holds the
                // grants naming it, with those its device hands it from its signed manifest.
                Actor::App(app) => others.push(
                    policy
                        .party_of(Grantee::app(app.as_str()))
                        .with_grants(granted),
                ),
            }
        }
        let mut parts = person_parts.into_iter().chain(others);
        Self {
            person: parts.next().unwrap_or_default(),
            bounds: parts.collect(),
            people,
            app: acting.app.as_ref(),
            app_kinds: None,
        }
    }
}

/// A member of `member` as a grant names it.
#[must_use]
pub fn granted_to(member: &MemberId) -> Grantee {
    Grantee::Member(member.clone())
}

/// An agent's or bot's own grants: the workspace's naming it, and those its
/// acting carries (`granted`, what its certificate's delegation gives it). An
/// agent is the pair of its id and the person it acts for, whom its
/// certificate chains to: only the grants and answers naming that pair reach
/// it, and once its person answered it ([`WorkspacePolicy::answers_narrow`]) what
/// `granted` hands it decides nothing.
fn software_party<'a>(
    member: &MemberId,
    policy: &'a WorkspacePolicy,
    granted: &'a [Grant],
) -> Party<'a> {
    match member {
        // A person's answer for their agent narrows it to what their answers name, whatever its
        // delegation hands it.
        MemberId::Agent { id, person } => policy.party_of_agent(id, person.as_str()).narrowed_by(
            granted,
            policy
                .grants_of_agent(id, person.as_str())
                .filter(|grant| crate::agent_answers::is_answer(grant)),
        ),
        member => policy.party_of(granted_to(member)).with_grants(granted),
    }
}

/// One invocation, as admission weighs it.
#[derive(Clone, Copy, Debug)]
pub struct Invocation<'a> {
    /// The action's node.
    pub node: &'a TreePath,
    /// The kind of action it is.
    pub kind: crate::ActionKind,
    /// Whether it exercises authority over the workspace or others in it
    /// ([`crate::Verdict::privileged`]).
    pub privileged: bool,
    /// Whether it needs its person attending.
    pub attended: bool,
    /// The node of the resource it changes, decided as well where it is one:
    /// so a person restricted on one resource cannot change it through an
    /// action they may otherwise take.
    pub resource: Option<&'a TreePath>,
    /// How far the invocation it continues was admitted to reach, when it
    /// continues one: what goes further asks first.
    pub ceiling: Option<Reach>,
}

/// The grants' answer for an action of `kind`, privileged when
/// `privileged`, at `node`, for `authority` at `cx`, and the `Once` grants
/// relying on it spends. A step going further than `ceiling` (the
/// invocation it continues) is asked for as its own request.
#[must_use]
pub fn answer_at(
    authority: &Authority<'_>,
    (node, kind, privileged): (&TreePath, crate::ActionKind, bool),
    ceiling: Option<Reach>,
    cx: &Context<'_>,
) -> (Answer, Vec<GrantId>) {
    let verdict = verdict_at(authority, (node, kind, privileged), ceiling, cx);
    (verdict.answer, verdict.spent())
}

/// The verdict of `answer_at`, kept whole: what it was decided by, so admission can tell the
/// answer a person gave in advance ([`Authority::answered_in_advance`]) from one asking them
/// now.
fn verdict_at<'a>(
    authority: &'a Authority<'_>,
    (node, kind, privileged): (&TreePath, crate::ActionKind, bool),
    ceiling: Option<Reach>,
    cx: &Context<'_>,
) -> crate::Verdict<'a> {
    let mut verdict = authority.decide(node, kind, cx);
    if privileged {
        verdict = verdict.privileged();
    }
    let exceeds = ceiling.is_some_and(|ceiling| !ceiling.covers(kind, privileged));
    if verdict.answer == Answer::Allow && exceeds {
        verdict.answer = Answer::Ask;
    }
    verdict
}

/// Admit `invocation` for `authority`, asked of `asker`, at `cx`: whether
/// it runs or asks first, and the `Once` grants admitting it spends.
///
/// # Errors
/// Whose answer refused it.
pub fn admit_invocation(
    authority: &Authority<'_>,
    asker: Asker,
    invocation: Invocation<'_>,
    cx: &Context<'_>,
) -> Result<(Admission, Vec<GrantId>), Refused> {
    let verdict = verdict_at(
        authority,
        (invocation.node, invocation.kind, invocation.privileged),
        invocation.ceiling,
        cx,
    );
    // What its person already answered for this very action or resource was not theirs to ask
    // again, though it reaches outside the system: asking would put to them what they answered.
    let answered_in_advance = authority.answered_in_advance(&verdict);
    let answer = verdict.answer;
    let spent = verdict.spent();
    let resource = invocation.resource.map(|resource| {
        answer_at(
            authority,
            (resource, invocation.kind, false),
            invocation.ceiling,
            cx,
        )
        .0
    });
    let admission = admit(
        Invoked {
            answer,
            attended: invocation.attended,
            outside: invocation.kind == crate::ActionKind::Outside,
            answered_in_advance,
            resource,
        },
        asker,
    )?;
    Ok((admission, spent))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Acting, ActionKind, Answer, Context, Grant, Grantee, KindSet, Role, SESSION_ANSWER_PREFIX,
        Verdict, roles,
    };

    const WORKSPACE: &str = "w";

    fn policy() -> WorkspacePolicy {
        let mut policy = WorkspacePolicy::new(WORKSPACE, "owner");
        policy.members.insert("editor".into(), Role::Editor);
        policy.members.insert("viewer".into(), Role::Viewer);
        policy
    }

    fn app() -> TreePath {
        TreePath::workspace(WORKSPACE).app("so.tilo.notes")
    }

    fn answer(
        acting: &Acting,
        policy: &WorkspacePolicy,
        granted: &[Grant],
        kind: ActionKind,
    ) -> Answer {
        Authority::of_acting(acting, policy, granted)
            .decide(&app(), kind, &Context::at_secs(1))
            .answer
    }

    tilo_test_env::scenario! {
        fn whoever_acts_is_decided_by_every_party_it_acts_for;

        phase a_person_is_decided_by_their_grants_and_their_roles;
        {
            let policy = policy();
            assert_eq!(
                answer(
                    &Acting::person("editor"),
                    &policy,
                    &[],
                    ActionKind::Undoable
                ),
                Answer::Allow
            );
            assert_eq!(
                answer(
                    &Acting::person("viewer"),
                    &policy,
                    &[],
                    ActionKind::Undoable
                ),
                Answer::Deny
            );
        }

        phase every_person_in_the_tree_has_to_allow_it;
        {
            let policy = policy();
            // What the agent's turn gives it: reading and changing the workspace.
            let turn = [Grant::new(
                "turn:scribe:0",
                Grantee::agent("scribe", "editor"),
                TreePath::workspace(WORKSPACE),
                KindSet::READ_UNDOABLE,
                Answer::Allow,
                Grantee::person("editor"),
            )];
            let agent_for_editor_and_owner = Acting::agent("scribe", "editor")
                .acting_for(Acting::person("editor"))
                .acting_for(Acting::person("owner"));
            assert_eq!(
                answer(
                    &agent_for_editor_and_owner,
                    &policy,
                    &turn,
                    ActionKind::Undoable
                ),
                Answer::Allow
            );
            let agent_for_a_viewer = agent_for_editor_and_owner.acting_for(Acting::person("viewer"));
            assert_eq!(
                answer(&agent_for_a_viewer, &policy, &turn, ActionKind::Undoable),
                Answer::Deny,
                "the viewer's grants decide too"
            );
        }

        phase an_app_acting_by_itself_is_decided_by_the_grants_naming_it;
        {
            let app = Acting::by_app("so.tilo.notes");
            assert_eq!(
                answer(&app, &policy(), &[], ActionKind::Undoable),
                Answer::Deny,
                "an app holds no person's grants, the owner's included"
            );
            // Nor does the owner's device lend it theirs by signing what it wrote.
            let own = TreePath::workspace(WORKSPACE).app("so.tilo.notes");
            let signed = crate::MemberAccess::person(policy(), "owner")
                .invoked_by(&app)
                .expect("the owner's device writes for an app it runs");
            assert_eq!(
                signed.answer(&own, ActionKind::Undoable, &Context::at_secs(1)),
                Answer::Deny,
                "the device that signs an app's write bounds it, and grants it nothing"
            );
            // Installing Notes gave it its own records, and nothing beyond them.
            let mut policy = policy();
            policy.grants.push(crate::roles::app_records_grant(
                WORKSPACE,
                "so.tilo.notes",
                "owner",
            ));
            assert_eq!(
                answer(&app, &policy, &[], ActionKind::Undoable),
                Answer::Allow
            );
            let elsewhere = TreePath::workspace(WORKSPACE)
                .app("so.tilo.chat")
                .kind("chat.messages");
            assert_eq!(
                Authority::of_acting(&app, &policy, &[])
                    .decide(&elsewhere, ActionKind::Undoable, &Context::at_secs(1))
                    .answer,
                Answer::Deny,
                "another app's records are no part of what installing gave"
            );
        }

        phase an_agent_needs_its_own_grants_as_well_as_its_persons;
        {
            let mut policy = policy();
            policy.grants.push(Grant::new(
                "scribe-may-edit",
                Grantee::agent("scribe", "editor"),
                TreePath::workspace(WORKSPACE),
                KindSet::READ_UNDOABLE,
                Answer::Allow,
                Grantee::person("editor"),
            ));
            let acting = Acting::agent("scribe", "editor").acting_for(Acting::person("editor"));
            assert_eq!(
                answer(&acting, &policy, &[], ActionKind::Undoable),
                Answer::Allow
            );
            let unbounded = Acting::agent("other", "editor").acting_for(Acting::person("editor"));
            assert_eq!(
                answer(&unbounded, &policy, &[], ActionKind::Undoable),
                Answer::Deny,
                "an agent with no grants of its own does nothing"
            );
        }

        phase an_agent_answering_someone_else_keeps_its_own_persons_refusal;
        {
            // The agent's certificate gives it the workspace; its own person, the owner, answers
            // no for it on the whole workspace. Its turn answers the editor, through the owner.
            let turn = [Grant::new(
                "turn:scribe:0",
                Grantee::agent("scribe", "owner"),
                TreePath::workspace(WORKSPACE),
                KindSet::READ_UNDOABLE,
                Answer::Allow,
                Grantee::person("owner"),
            )];
            let answering_the_editor = Acting::agent("scribe", "owner")
                .acting_for(Acting::person("editor"))
                .agent_of("owner");
            assert_eq!(
                answer(&answering_the_editor, &policy(), &turn, ActionKind::Undoable),
                Answer::Allow,
                "both people and the agent's certificate allow it"
            );
            let mut refusing = policy();
            refusing.grants.push(crate::agent_answers::grant(
                WORKSPACE,
                ("scribe", "owner"),
                (&crate::agent_answers::AnswerScope::Workspace, ActionKind::Undoable),
                Answer::Deny,
                Grantee::person("owner"),
            ));
            assert_eq!(
                answer(&answering_the_editor, &refusing, &turn, ActionKind::Undoable),
                Answer::Deny,
                "what its own person answered for it refuses it, whoever's turn it answers"
            );
            let mut widening = refusing.clone();
            // The editor answers only for an agent of their own: `scribe` of the editor is
            // another agent.
            widening.grants.push(crate::agent_answers::grant(
                WORKSPACE,
                ("scribe", "editor"),
                (
                    &crate::agent_answers::AnswerScope::app("so.tilo.notes"),
                    ActionKind::Undoable,
                ),
                Answer::Allow,
                Grantee::person("editor"),
            ));
            assert_eq!(
                answer(&answering_the_editor, &widening, &turn, ActionKind::Undoable),
                Answer::Deny,
                "the more specific yes of the person a turn answers widens nothing the agent's own \
                 person refused"
            );
        }

        phase a_sessions_answers_decide_its_steps_and_no_other;
        {
            let session = "so.tilo.session/agent-session/release";
            let mut policy = policy();
            policy.grants.push(Grant::new(
                "scribe-asks",
                Grantee::agent("scribe", "editor"),
                TreePath::workspace(WORKSPACE),
                KindSet::READ_UNDOABLE,
                Answer::Ask,
                Grantee::person("editor"),
            ));
            policy.grants.push(Grant::new(
                format!("{SESSION_ANSWER_PREFIX}{session}:app:so.tilo.notes:undoable"),
                Grantee::agent("scribe", "editor"),
                app(),
                KindSet::UNDOABLE,
                Answer::Allow,
                Grantee::person("editor"),
            ));
            let run = Acting::agent("scribe", "editor")
                .acting_for(Acting::person("editor"))
                .caused_by(session);
            assert_eq!(
                answer(&run, &policy, &[], ActionKind::Undoable),
                Answer::Allow,
                "the person's answer for the session decides its steps"
            );
            let another = Acting::agent("scribe", "editor")
                .acting_for(Acting::person("editor"))
                .caused_by("so.tilo.session/agent-session/other");
            assert_eq!(
                answer(&another, &policy, &[], ActionKind::Undoable),
                Answer::Ask,
                "another session's answers decide nothing here"
            );
        }

        phase an_automation_acts_with_its_owners_party_bounded_by_its_steps;
        {
            let policy = policy();
            let automation = Acting::automation("1")
                .acting_for(Acting::agent("scribe", "editor").acting_for(Acting::person("editor")));
            let turn = [Grant::new(
                "turn:scribe:0",
                Grantee::agent("scribe", "editor"),
                TreePath::workspace(WORKSPACE),
                KindSet::READ_UNDOABLE,
                Answer::Allow,
                Grantee::person("editor"),
            )];
            assert_eq!(
                answer(&automation, &policy, &turn, ActionKind::Undoable),
                Answer::Allow,
                "the tree's people and software decide; the automation is its owner's"
            );
            // A step it was given may narrow what it does there.
            let mut granted = turn.to_vec();
            granted.push(Grant::new(
                "automation-1:outside",
                Grantee::person("editor"),
                TreePath::workspace(WORKSPACE),
                KindSet::OUTSIDE,
                Answer::Deny,
                Grantee::person("editor"),
            ));
            assert_eq!(
                answer(&automation, &policy, &granted, ActionKind::Outside),
                Answer::Deny
            );
            let homeless = Acting::automation("1");
            assert_eq!(
                answer(&homeless, &policy, &turn, ActionKind::Undoable),
                Answer::Deny,
                "an automation that runs for nobody allows nothing"
            );
        }

        phase a_device_that_is_not_theirs_governs_nothing_and_asks_before_reaching_out;
        {
            let policy = policy();
            let foreign = Acting::person("owner").on_foreign_device(true);
            assert_eq!(
                answer(&foreign, &policy, &[], ActionKind::Outside),
                Answer::Ask
            );
            let invite = roles::governance_action(WORKSPACE, roles::governance::ADD_MEMBER);
            let verdict: Verdict<'_> = Authority::of_acting(&foreign, &policy, &[]).decide(
                &invite,
                roles::GOVERNANCE_KIND,
                &Context::at_secs(1),
            );
            assert_eq!(verdict.answer, Answer::Deny, "never governs a workspace");
            assert_eq!(
                answer(&Acting::person("owner"), &policy, &[], ActionKind::Undoable),
                Answer::Allow
            );
        }

        phase an_app_reaches_only_the_kinds_it_declares_in_its_own_node;
        {
            let policy = policy();
            let acting = Acting::person("editor").in_app("so.tilo.notes");
            let kinds = ["notes.note".to_owned()];
            let answer = |node: TreePath| {
                Authority::of_acting(&acting, &policy, &[])
                    .in_app(&kinds)
                    .decide(&node, ActionKind::Undoable, &Context::at_secs(1))
                    .answer
            };
            let own = TreePath::workspace(WORKSPACE).app("so.tilo.notes");
            assert_eq!(answer(own.clone().kind("notes.note")), Answer::Allow);
            assert_eq!(
                answer(own.kind("notes.book")),
                Answer::Deny,
                "a kind the app does not declare is out of its reach"
            );
            assert_eq!(
                answer(
                    TreePath::workspace(WORKSPACE)
                        .app("so.tilo.chat")
                        .kind("chat.messages")
                ),
                Answer::Allow,
                "in another app's node the person's own grants decide"
            );
        }

        phase an_apps_own_connection_holds_the_grants_naming_it;
        {
            // An app's held connection acts as no person: it holds the grants that name the
            // app, and an empty person's grants are nothing.
            let app = "so.tilo.calendar";
            let node = TreePath::workspace(WORKSPACE).app("tilo.accounts");
            let granted = [Grant::new(
                "manifest",
                Grantee::app(app),
                node.clone(),
                crate::KindSet::READ,
                Answer::Allow,
                Grantee::person("owner"),
            )];
            let holding = Acting::by_app(app).in_app(app);
            assert_eq!(
                Authority::of_acting(&holding, &policy(), &granted)
                    .decide(&node, ActionKind::Read, &Context::at_secs(1))
                    .answer,
                Answer::Allow,
                "the grant naming the app decides for its own connection"
            );
            // Someone acting through the app for nobody is not the app: it holds nothing.
            let nobody = Acting::member(MemberId::person("")).in_app(app);
            assert_eq!(
                Authority::of_acting(&nobody, &policy(), &granted)
                    .decide(&node, ActionKind::Read, &Context::at_secs(1))
                    .answer,
                Answer::Deny,
                "nobody acting through the app borrows its grants"
            );
            assert_eq!(
                Authority::of_acting(&holding, &policy(), &[])
                    .decide(&node, ActionKind::Read, &Context::at_secs(1))
                    .answer,
                Answer::Deny,
                "and nothing reaches it that names no one"
            );
        }

        phase a_tree_asks_the_person_unless_someone_acts_for_them;
        {
            let person = Acting::person("editor");
            assert!(person.asker(true).reachable);
            assert!(!person.asker(true).delegated);
            let agent = Acting::agent("scribe", "editor").acting_for(Acting::person("editor"));
            assert!(agent.asker(true).delegated);
            assert!(
                !agent.asker(false).reachable,
                "with nobody to ask, an ask refuses"
            );
        }
    }

    tilo_test_env::scenario! {
        fn a_persons_answer_in_advance_lifts_the_ask_of_what_their_agent_sends;

        phase an_agents_send_asks_its_person;
        {
            let policy = policy();
            let turn = [
                Grant::new(
                    "turn:scribe:0",
                    Grantee::agent("scribe", "editor"),
                    TreePath::workspace(WORKSPACE),
                    KindSet::READ_UNDOABLE,
                    Answer::Allow,
                    Grantee::person("editor"),
                ),
                Grant::new(
                    "turn:scribe:1",
                    Grantee::agent("scribe", "editor"),
                    TreePath::workspace(WORKSPACE),
                    KindSet::OUTSIDE,
                    Answer::Ask,
                    Grantee::person("editor"),
                ),
            ];
            let acting = Acting::agent("scribe", "editor").acting_for(Acting::person("editor"));
            let send = TreePath::workspace(WORKSPACE)
                .app("so.tilo.notes")
                .kind("note")
                .resource("n1")
                .action("send");
            let asked = |turn: &[Grant]| {
                admit_invocation(
                    &Authority::of_acting(&acting, &policy, turn),
                    Asker {
                        reachable: true,
                        delegated: true,
                    },
                    Invocation {
                        node: &send,
                        kind: ActionKind::Outside,
                        privileged: false,
                        attended: false,
                        resource: None,
                        ceiling: None,
                    },
                    &Context::at_secs(1),
                )
                .expect("it is admitted, or asks")
                .0
            };
            assert_eq!(
                asked(&turn),
                Admission::AsksFirst,
                "an agent's send waits for its person"
            );

            // The person's own "Always allow this" for that very action lifts the ask.
            let scope = crate::agent_answers::AnswerScope::action("so.tilo.notes", "note", "send");
            let mut answered = turn.to_vec();
            answered.push(crate::agent_answers::grant(
                WORKSPACE,
                ("scribe", "editor"),
                (&scope, ActionKind::Outside),
                Answer::Allow,
                Grantee::person("editor"),
            ));
            assert_eq!(
                asked(&answered),
                Admission::Runs,
                "what its person answered in advance runs unasked"
            );

            // An answer for another action is no answer for this one.
            let elsewhere =
                crate::agent_answers::AnswerScope::action("so.tilo.notes", "note", "archive");
            let mut other = turn.to_vec();
            other.push(crate::agent_answers::grant(
                WORKSPACE,
                ("scribe", "editor"),
                (&elsewhere, ActionKind::Outside),
                Answer::Allow,
                Grantee::person("editor"),
            ));
            assert_eq!(
                asked(&other),
                Admission::AsksFirst,
                "only an answer for that very action lifts its ask"
            );
        }

        phase an_attended_action_its_person_answered_in_advance_runs_unasked_too;
        {
            let policy = policy();
            let mut turn = vec![Grant::new(
                "turn:scribe:0",
                Grantee::agent("scribe", "editor"),
                TreePath::workspace(WORKSPACE),
                KindSet::READ_UNDOABLE,
                Answer::Allow,
                Grantee::person("editor"),
            )];
            let acting = Acting::agent("scribe", "editor").acting_for(Acting::person("editor"));
            let act = TreePath::workspace(WORKSPACE)
                .app("so.tilo.notes")
                .kind("note")
                .resource("n1")
                .action("edit");
            let admission = |turn: &[Grant]| {
                admit_invocation(
                    &Authority::of_acting(&acting, &policy, turn),
                    Asker {
                        reachable: true,
                        delegated: true,
                    },
                    Invocation {
                        node: &act,
                        kind: ActionKind::Undoable,
                        privileged: false,
                        attended: true,
                        resource: None,
                        ceiling: None,
                    },
                    &Context::at_secs(1),
                )
                .expect("it is admitted, or asks")
                .0
            };
            assert_eq!(admission(&turn), Admission::AsksFirst);
            let scope = crate::agent_answers::AnswerScope::action("so.tilo.notes", "note", "edit");
            turn.push(crate::agent_answers::grant(
                WORKSPACE,
                ("scribe", "editor"),
                (&scope, ActionKind::Undoable),
                Answer::Allow,
                Grantee::person("editor"),
            ));
            assert_eq!(
                admission(&turn),
                Admission::Runs,
                "an attended action its person answered in advance runs unasked"
            );
        }
    }
}
