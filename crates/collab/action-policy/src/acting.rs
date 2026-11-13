//! Who made a change: one tree, and the one admission every door takes.

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
        for node in acting.nodes() {
            match &node.by {
                Actor::Member(member) => match member {
                    MemberId::Person(person) => {
                        people = people.saturating_add(1);
                        let party = policy.party(person).with_grants(granted);
                        person_parts.push(if node.foreign_device {
                            party.capped(crate::Cap::FOREIGN_DEVICE)
                        } else {
                            party
                        });
                    }
                    MemberId::Agent(_) | MemberId::Bot(_) => {
                        others.push(software_party(member, policy, granted));
                    }
                },
                Actor::Automation(_) if owned => {}
                Actor::Automation(_) => others.push(Party::default()),
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
    match member {
        MemberId::Person(id) => Grantee::person(id.clone()),
        MemberId::Agent(id) => Grantee::agent(id.clone()),
        MemberId::Bot(id) => Grantee::bot(id.clone()),
    }
}

/// An agent's or bot's own grants.
fn software_party<'a>(
    member: &MemberId,
    policy: &'a WorkspacePolicy,
    granted: &'a [Grant],
) -> Party<'a> {
    policy.party_of(granted_to(member)).with_grants(granted)
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
    /// The node of the record it changes, decided as well where it is one:
    /// so a person restricted on one record cannot change it through an
    /// action they may otherwise take.
    pub record: Option<&'a TreePath>,
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
    let mut verdict = authority.decide(node, kind, cx);
    if privileged {
        verdict = verdict.privileged();
    }
    let exceeds = ceiling.is_some_and(|ceiling| !ceiling.covers(kind, privileged));
    let answer = match verdict.answer {
        Answer::Allow if exceeds => Answer::Ask,
        answer => answer,
    };
    (answer, verdict.spent())
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
    let (answer, spent) = answer_at(
        authority,
        (invocation.node, invocation.kind, invocation.privileged),
        invocation.ceiling,
        cx,
    );
    let record = invocation.record.map(|record| {
        answer_at(
            authority,
            (record, invocation.kind, false),
            invocation.ceiling,
            cx,
        )
        .0
    });
    let admission = admit(
        Invoked {
            answer,
            attended: invocation.attended,
            record,
        },
        asker,
    )?;
    Ok((admission, spent))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Acting, ActionKind, Answer, Context, Grant, KindSet, Role, Verdict, roles};

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

    #[test]
    fn a_person_is_decided_by_their_grants_and_their_roles() {
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

    #[test]
    fn every_person_in_the_tree_has_to_allow_it() {
        let policy = policy();
        // What the agent's turn gives it: reading and changing the workspace.
        let turn = [Grant::new(
            "turn:scribe:0",
            Grantee::agent("scribe"),
            TreePath::workspace(WORKSPACE),
            KindSet::READ_UNDOABLE,
            Answer::Allow,
            Grantee::person("editor"),
        )];
        let agent_for_editor_and_owner = Acting::agent("scribe")
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

    #[test]
    fn an_agent_needs_its_own_grants_as_well_as_its_persons() {
        let mut policy = policy();
        policy.grants.push(Grant::new(
            "scribe-may-edit",
            Grantee::agent("scribe"),
            TreePath::workspace(WORKSPACE),
            KindSet::READ_UNDOABLE,
            Answer::Allow,
            Grantee::person("owner"),
        ));
        let acting = Acting::agent("scribe").acting_for(Acting::person("editor"));
        assert_eq!(
            answer(&acting, &policy, &[], ActionKind::Undoable),
            Answer::Allow
        );
        let unbounded = Acting::agent("other").acting_for(Acting::person("editor"));
        assert_eq!(
            answer(&unbounded, &policy, &[], ActionKind::Undoable),
            Answer::Deny,
            "an agent with no grants of its own does nothing"
        );
    }

    #[test]
    fn an_automation_acts_with_its_owners_party_bounded_by_its_steps() {
        let policy = policy();
        let automation = Acting::automation("1")
            .acting_for(Acting::agent("scribe").acting_for(Acting::person("editor")));
        let turn = [Grant::new(
            "turn:scribe:0",
            Grantee::agent("scribe"),
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

    #[test]
    fn a_device_that_is_not_theirs_governs_nothing_and_asks_before_reaching_out() {
        let policy = policy();
        let foreign = Acting::person("owner").on_foreign_device(true);
        assert_eq!(
            answer(&foreign, &policy, &[], ActionKind::Outside),
            Answer::Ask
        );
        let invite = roles::governance_action(WORKSPACE, roles::governance::INVITE);
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

    #[test]
    fn an_app_reaches_only_the_kinds_it_declares_in_its_own_node() {
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

    #[test]
    fn a_tree_asks_the_person_unless_someone_acts_for_them() {
        let person = Acting::person("editor");
        assert!(person.asker(true).reachable);
        assert!(!person.asker(true).delegated);
        let agent = Acting::agent("scribe").acting_for(Acting::person("editor"));
        assert!(agent.asker(true).delegated);
        assert!(
            !agent.asker(false).reachable,
            "with nobody to ask, an ask refuses"
        );
    }
}
