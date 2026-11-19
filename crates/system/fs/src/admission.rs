//! Who may change a workspace's files through this filesystem: the workspace's own action
//! grants, asked the way every receiver of the change asks them.
//!
//! A program that writes, renames or removes a file in the mount makes a file action of the
//! tree ([`fs_protocol::FileAction`]), signed by this device and naming who made it
//! ([`crate::writer`]). Every holder of the workspace then decides that change by the author
//! device's access in the workspace, invoked by the change's invoker
//! ([`tilo_action_policy::MemberAccess::invoked_by`]), at the file action's node
//! ([`fs_protocol::file_action::verb_node`]), and merges it only on an outright Allow. The mount
//! asks the same question before it writes: a change no holder would take is refused here, as a
//! read-only file system, instead of being written on this device and never reaching anyone.
//! A role change, a revoked grant or an agent's narrowed delegation answers at the next
//! operation; nothing is decided at mount time.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tilo_action_policy::{
    Acting, ActionKind, Answer, Context, MemberAccess, MemberId, WorkspacePolicy,
};
use tilo_lock::MutexRecover as _;

/// How long a policy read from its source answers before it is read again: a burst of file
/// operations asks once, and a changed policy answers within a second.
const POLICY_FRESH: Duration = Duration::from_secs(1);

/// Where the workspace's policy comes from, as this device holds it now; `None` while it holds
/// none.
pub type PolicySource = dyn Fn() -> Option<WorkspacePolicy> + Send + Sync;

/// The admission of the file actions one mount writes for one workspace.
pub struct FileAdmission {
    /// The device writing the changes, as receivers name their author: the person at it, on
    /// their own device.
    signer: Acting,
    source: Box<PolicySource>,
    held: Mutex<Option<(Instant, Arc<WorkspacePolicy>)>>,
}

impl std::fmt::Debug for FileAdmission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileAdmission")
            .field("signer", &self.signer)
            .finish_non_exhaustive()
    }
}

impl FileAdmission {
    /// The admission of what this device writes for `person` (their root key, hex), deciding by
    /// the policy `source` reads. `None` for a person key that is none.
    #[must_use]
    pub fn new(person: &str, source: Box<PolicySource>) -> Option<Self> {
        Some(Self {
            signer: Acting::member(MemberId::parsed_person(person)?),
            source,
            held: Mutex::new(None),
        })
    }

    fn policy(&self) -> Option<Arc<WorkspacePolicy>> {
        let fresh = self
            .held
            .lock_recover()
            .as_ref()
            .filter(|(read, _)| read.elapsed() < POLICY_FRESH)
            .map(|(_, policy)| Arc::clone(policy));
        if fresh.is_some() {
            return fresh;
        }
        let policy = Arc::new((self.source)()?);
        *self.held.lock_recover() = Some((Instant::now(), Arc::clone(&policy)));
        Some(policy)
    }

    /// Whether `invoker` (the person at this device when `None`) may take the file action
    /// `verb` on the tree's node `node`, or on the tree as a whole with no node named. Refused
    /// while this device holds no policy of the workspace: no receiver could decide it either.
    #[must_use]
    pub fn admits(&self, invoker: Option<&Acting>, node: Option<&str>, verb: &str) -> bool {
        let Some(policy) = self.policy() else {
            return false;
        };
        let access = MemberAccess::new(policy, self.signer.clone());
        let access = match invoker {
            Some(invoker) => access.invoked_by(invoker),
            None => access,
        };
        let at = access.node(&fs_protocol::file_action::verb_node(node, verb));
        let cx = Context::at_time(tilo_clock::wall());
        // An ask is a refusal: nobody answers one for a file operation, and no receiver takes a
        // change on one.
        access.answer(&at, ActionKind::Undoable, &cx) == Answer::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs_protocol::file_action::verbs;
    use tilo_action_policy::Role;

    const OWNER: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const EDITOR: &str = "2222222222222222222222222222222222222222222222222222222222222222";
    const VIEWER: &str = "3333333333333333333333333333333333333333333333333333333333333333";

    fn policy() -> WorkspacePolicy {
        let mut policy = WorkspacePolicy::new("w", OWNER);
        policy.members.insert(EDITOR.into(), Role::Editor);
        policy.members.insert(VIEWER.into(), Role::Viewer);
        policy
    }

    fn admission(person: &str, policy: Option<WorkspacePolicy>) -> FileAdmission {
        let policy = Mutex::new(policy);
        FileAdmission::new(person, Box::new(move || policy.lock_recover().clone()))
            .expect("a person's key")
    }

    tilo_test_env::scenario! {
        fn a_mount_admits_a_file_action_as_the_workspace_decides_it;

        phase the_role_decides_what_the_person_at_this_device_changes;
        {
            for verb in verbs::ALL {
                assert!(admission(OWNER, Some(policy())).admits(None, Some("n1"), verb));
                assert!(admission(EDITOR, Some(policy())).admits(None, Some("n1"), verb));
                assert!(!admission(VIEWER, Some(policy())).admits(None, Some("n1"), verb));
            }
        }

        phase nothing_is_written_while_no_policy_is_held;
        {
            assert!(!admission(OWNER, None).admits(None, None, verbs::WRITE));
        }

        phase a_handed_agent_writes_for_its_person_within_their_role;
        {
            let agent = Acting::agent("claude").handed().in_session("s1");
            let for_editor = agent.clone().acting_for(Acting::person(EDITOR));
            assert!(admission(EDITOR, Some(policy())).admits(Some(&for_editor), None, verbs::WRITE));
            let for_viewer = agent.acting_for(Acting::person(VIEWER));
            assert!(!admission(VIEWER, Some(policy())).admits(Some(&for_viewer), None, verbs::WRITE));
        }

        phase a_program_writes_with_its_person_s_access;
        {
            let editor = crate::writer::Writer::Program {
                id: "org.gnome.TextEditor".into(),
                name: "Text Editor".into(),
            };
            let app = crate::writer::Writer::App { id: "so.tilo.notes".into() };
            for writer in [editor, app] {
                for (person, may) in [(OWNER, true), (EDITOR, true), (VIEWER, false)] {
                    let invoker = writer.invoker(Some(person)).expect("a program names itself");
                    let admits = admission(person, Some(policy())).admits(
                        Some(&invoker.acting),
                        Some("n1"),
                        verbs::WRITE,
                    );
                    assert_eq!(admits, may, "{writer:?} for {person}");
                }
            }
        }

        phase a_deny_on_the_tree_refuses_even_its_owner_s_agent;
        {
            let mut denied = policy();
            denied.grants.push(tilo_action_policy::Grant::new(
                "no-agent-writes",
                tilo_action_policy::Grantee::agent("claude"),
                tilo_action_policy::TreePath::workspace("w")
                    .joined(&fs_protocol::file_action::tree_node()),
                tilo_action_policy::KindSet::UNDOABLE,
                Answer::Deny,
                tilo_action_policy::Grantee::person(OWNER),
            ));
            let agent = Acting::agent("claude")
                .handed()
                .acting_for(Acting::person(OWNER));
            let admission = admission(OWNER, Some(denied));
            assert!(!admission.admits(Some(&agent), Some("n1"), verbs::WRITE));
            assert!(admission.admits(None, Some("n1"), verbs::WRITE));
        }
    }
}
