//! What answers a window's questions where no device runs: a capture, a headless drive, a profile
//! opened on its own, a test, or a shell surface whose record only its own process keeps. Never a
//! window over a workspace a person's device keeps, whose own device answers. A device decides
//! what its person may do with the grants of the workspace it keeps; this stands in for one
//! keeping the harness's workspace, deciding with the
//! same function ([`policy::answer_at`]) over the person's access there, which the harness may
//! change while the window is open (a role given or taken back).

use std::cell::RefCell;
use std::rc::Rc;

use crate::shown_actions::{ActionDecision, ActionVerdict, REFUSED, ShownAction, UNAVAILABLE};
use tilo_ui_base::permissions::{MemberId, Role, policy};

/// The stand-in device of a harness's window. Clones share the access it decides by.
#[derive(Clone)]
pub struct HarnessDevice(Rc<RefCell<policy::MemberAccess>>);

impl HarnessDevice {
    /// A device keeping `workspace`, where `person` has `role`.
    #[must_use]
    pub fn member_of(workspace: &str, person: &MemberId, role: Role) -> Self {
        Self(Rc::new(RefCell::new(access_of(
            workspace,
            person,
            Some(role),
        ))))
    }

    /// A device keeping `workspace`, which `person` owns, acting through their agent `agent`,
    /// granted exactly `kinds` at each of `nodes` (paths below the workspace).
    #[must_use]
    pub fn agent_granted(
        workspace: &str,
        person: &MemberId,
        agent: &str,
        nodes: &[policy::TreePath],
        kinds: tilo_action_types::KindSet,
    ) -> Self {
        let subject = policy::Grantee::agent(agent, person.id());
        let grants = nodes.iter().map(|node| {
            policy::Grant::new(
                "stand-in-grant",
                subject.clone(),
                policy::TreePath::workspace(workspace).join(node),
                kinds,
                policy::Answer::Allow,
                policy::Grantee::person(person.id()),
            )
        });
        let access = access_of(workspace, person, Some(Role::Owner)).acting_as(
            tilo_member_types::Actor::Member(tilo_member_types::MemberId::agent(
                agent,
                person.id(),
            )),
            grants,
        );
        Self(Rc::new(RefCell::new(access)))
    }

    /// The person now has `role` in the workspace, as when its owner changes their access.
    pub fn set_role(&self, person: &MemberId, role: Role) {
        let workspace = self.0.borrow().workspace().to_owned();
        *self.0.borrow_mut() = access_of(&workspace, person, Some(role));
    }

    /// The person no longer belongs to the workspace: no role and no grant left.
    pub fn remove(&self, person: &MemberId) {
        let workspace = self.0.borrow().workspace().to_owned();
        *self.0.borrow_mut() = access_of(&workspace, person, None);
    }

    /// Decide by `access` from now on, as when the workspace's grants change.
    pub fn set_access(&self, access: policy::MemberAccess) {
        *self.0.borrow_mut() = access;
    }

    /// The access it decides by now.
    #[must_use]
    pub fn access(&self) -> policy::MemberAccess {
        self.0.borrow().clone()
    }

    /// What the device says of `actions` now. An action of a workspace it does not keep is
    /// decided by nothing here.
    #[must_use]
    pub fn decide(&self, actions: &[ShownAction]) -> Vec<ActionDecision> {
        let access = self.0.borrow();
        let now = crate::time::unix_now();
        // As a device answers: each action, and around it the action as declared and at each node
        // below that a grant names.
        let authority = access.authority();
        let nodes: Vec<_> = authority
            .nodes()
            .into_iter()
            .map(policy::TreePath::within_workspace)
            .collect();
        let mut around: Vec<ShownAction> = actions
            .iter()
            .flat_map(|action| {
                std::iter::once(action.declared())
                    .chain(nodes.iter().filter_map(|node| action.at_node(node)))
            })
            .filter(|around| !actions.contains(around))
            .collect();
        around.sort();
        around.dedup();
        let asked: Vec<ShownAction> = actions.iter().cloned().chain(around).collect();
        crate::shown_actions::decide(&asked, |action| {
            // An answer besides those asked stands for a record the window did not ask about.
            let action = &if actions.contains(action) {
                action.clone()
            } else {
                action.on_any_record()
            };
            if action
                .workspace
                .as_deref()
                .is_some_and(|workspace| workspace != access.workspace())
            {
                return ActionVerdict::Refused(UNAVAILABLE.into());
            }
            let context = policy::Context::at_secs(now);
            // Whether the app reaches outside is the workspace's say over the app.
            let grants = access.policy.grants.iter().chain(&access.granted);
            if let Some(verdict) =
                crate::shown_actions::reach_verdict(action, access.workspace(), grants, &context)
            {
                return verdict;
            }
            let node = access.node(&action.node);
            match policy::answer_at(
                &access.authority(),
                (&node, action.kind_needed, action.privileged),
                None,
                &context,
            ) {
                (policy::Answer::Allow, spent) if spent.is_empty() => ActionVerdict::Allowed,
                (policy::Answer::Allow | policy::Answer::Ask, _) => ActionVerdict::AsksFirst,
                (policy::Answer::Deny, _) => ActionVerdict::Refused(REFUSED.into()),
            }
        })
    }

    /// This device as what a headless driver answers its window's questions with (its
    /// `decide_shown_actions`): for the person whose access it decides by, whoever asks.
    #[must_use]
    pub fn host(
        &self,
    ) -> Box<dyn Fn(&tilo_member_types::Acting, &[ShownAction]) -> Vec<ActionDecision>> {
        let device = self.clone();
        Box::new(move |_, actions: &[ShownAction]| device.decide(actions))
    }

    /// This device as what a window's driver answers its questions with.
    #[must_use]
    pub fn shown(&self) -> crate::application::ShownDevice {
        let device = self.clone();
        Rc::new(move |actions: &[ShownAction]| device.decide(actions))
    }
}

/// Verdicts `device` gives of every action `resources` (and the resources inside them) publish as
/// `app`: the questions a window's root asks its device answered before it shows them.
#[must_use]
pub fn decided(
    app: &str,
    resources: &[tilo_actions::ResourceSpec],
    device: &HarnessDevice,
) -> tilo_ui_base::permissions::SharedPermissions {
    let shared = tilo_ui_base::permissions::SharedPermissions::default();
    answer(&shared, app, resources, device);
    shared
}

/// [`decided`] answered into `shared`, for a binding whose permissions were made before the
/// resources it publishes were known.
pub fn answer(
    shared: &tilo_ui_base::permissions::SharedPermissions,
    app: &str,
    resources: &[tilo_actions::ResourceSpec],
    device: &HarnessDevice,
) {
    fn questions(app: &str, resource: &tilo_actions::ResourceSpec, out: &mut Vec<ShownAction>) {
        out.extend(
            resource
                .actions
                .iter()
                .map(|action| ShownAction::published(app, resource, action)),
        );
        for child in &resource.children {
            questions(app, child, out);
        }
    }
    let mut asked = Vec::new();
    for resource in resources {
        questions(app, resource, &mut asked);
    }
    answer_actions(shared, asked, device);
}

/// Verdicts `device` gives of `actions`, answered into `shared`: for the questions a root asks
/// beside what its resources publish (whether the person may change the app's records at all).
pub fn answer_actions(
    shared: &tilo_ui_base::permissions::SharedPermissions,
    actions: impl IntoIterator<Item = ShownAction>,
    device: &HarnessDevice,
) {
    let asked: Vec<ShownAction> = actions.into_iter().collect();
    shared.update(|verdicts| {
        verdicts.ask(asked.clone());
        verdicts.decide(&device.decide(&asked));
    });
}

/// `person`'s access in `workspace`, having `role` there (its owner, or a member of an unowned
/// workspace), or none.
fn access_of(workspace: &str, person: &MemberId, role: Option<Role>) -> policy::MemberAccess {
    let person = person.id().to_owned();
    let policy = if role == Some(Role::Owner) {
        policy::WorkspacePolicy::new(workspace, person.clone())
    } else {
        let mut policy =
            policy::WorkspacePolicy::unowned(workspace, &policy::Grantee::role(Role::Owner));
        if let Some(role) = role {
            policy.members.insert(person.clone(), role.into());
        }
        policy
    };
    policy::MemberAccess::person(policy, person)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shown_actions::ShownVerdicts;
    use tilo_actions::ActionKind;

    /// An agent its person let write one field of every memo (a grant on `memo/**/title`) may
    /// retitle a memo the window never asked about, as the device admits it there, and may not
    /// write another field of it: what the device answers as declared is decided on such a
    /// record, which the grant reaches as it reaches every memo.
    #[test]
    fn a_record_no_one_asked_about_takes_what_a_grant_on_every_record_says() {
        let memo = || policy::TreePath::root().app("memo").kind("test.memo");
        let device = HarnessDevice::agent_granted(
            "ws",
            &MemberId::person("ada"),
            "scribe",
            &[policy::TreePath::root().app("memo").rest().field("title")],
            tilo_action_types::KindSet::READ_UNDOABLE,
        );
        let edit = |record: &str, field: &str, action: &str| {
            ShownAction::at(
                memo().resource(record).field(field).action(action),
                ActionKind::Undoable,
            )
        };
        let apply = |record: &str| {
            ShownAction::at(
                memo().resource(record).action("apply"),
                ActionKind::Undoable,
            )
        };
        let asked = [
            edit("first", "title", "retitle"),
            edit("first", "lines", "add-line"),
            apply("first"),
            ShownAction::app("memo", ActionKind::Undoable),
        ];
        let mut shown = ShownVerdicts::default().asking(asked.clone());
        shown.decide(&device.decide(&asked));
        let said = |action: &ShownAction| {
            device.decide(std::slice::from_ref(action))[0]
                .verdict
                .clone()
        };

        let retitle = edit("second", "title", "retitle");
        assert_eq!(
            said(&retitle),
            ActionVerdict::Allowed,
            "the device admits it"
        );
        assert_eq!(
            shown.answer(&retitle),
            policy::Answer::Allow,
            "and the window shows it so before it asked"
        );
        let add_line = edit("second", "lines", "add-line");
        assert!(matches!(said(&add_line), ActionVerdict::Refused(_)));
        assert_eq!(
            shown.answer(&add_line),
            policy::Answer::Deny,
            "another field stays refused"
        );
        assert_eq!(shown.refusal(&add_line), Some(REFUSED));

        // A typed edit carries its field's value: one of the title is decided as the title is,
        // one of another field as any change the agent's grants do not name.
        let carrying = |field: &str| {
            ShownAction::at(
                memo().resource("second").field(field).action("apply"),
                ActionKind::Undoable,
            )
        };
        assert_eq!(shown.answer(&carrying("title")), policy::Answer::Allow);
        assert_eq!(said(&carrying("title")), ActionVerdict::Allowed);
        assert_eq!(shown.answer(&carrying("lines")), policy::Answer::Deny);
        assert!(matches!(
            said(&carrying("lines")),
            ActionVerdict::Refused(_)
        ));
    }
}
