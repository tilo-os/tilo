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
//!
//! Nothing is admitted without the workspace's policy. A mount shown before this device holds
//! it (the first seconds after a login) holds each write until the policy arrives rather than
//! refusing it, so a person's first save does not fail; once a policy is held, a source that
//! cannot be read for a moment answers with the newest one held.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tilo_action_policy::{Answer, Context, MemberAccess, WorkspacePolicy};
use tilo_action_types::ActionKind;
use tilo_lock::MutexRecover as _;
use tilo_member_types::{Acting, MemberId};

/// How long a policy read from its source answers before it is read again: a burst of file
/// operations asks once, and a changed policy answers within a second.
const POLICY_FRESH: Duration = Duration::from_secs(1);

/// How long a write waits for the workspace's policy while this device holds none yet: long
/// enough for a device that just logged in to receive it, after which the write is refused.
const POLICY_WAIT: Duration = Duration::from_secs(120);

/// How often a waiting write asks the source again.
const POLICY_POLL: Duration = Duration::from_millis(50);

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
    /// How long a write waits for a first policy ([`POLICY_WAIT`]).
    wait: Duration,
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
            wait: POLICY_WAIT,
        })
    }

    /// The same admission, a write waiting at most `wait` for a first policy.
    #[must_use]
    pub const fn waiting_at_most(mut self, wait: Duration) -> Self {
        self.wait = wait;
        self
    }

    /// The workspace's policy: the one read within [`POLICY_FRESH`], else the source's now,
    /// else the newest held. With none ever held, waits for the source to have one.
    fn policy(&self) -> Option<Arc<WorkspacePolicy>> {
        let held = self.held.lock_recover().clone();
        if let Some((read, policy)) = &held
            && read.elapsed() < POLICY_FRESH
        {
            return Some(Arc::clone(policy));
        }
        if let Some(policy) = self.read() {
            return Some(policy);
        }
        if let Some((_, policy)) = held {
            return Some(policy);
        }
        let started = Instant::now();
        while started.elapsed() < self.wait {
            std::thread::sleep(POLICY_POLL.min(self.wait.saturating_sub(started.elapsed())));
            if let Some(policy) = self.read() {
                return Some(policy);
            }
        }
        None
    }

    /// The source's policy now, kept as the newest held.
    fn read(&self) -> Option<Arc<WorkspacePolicy>> {
        let policy = Arc::new((self.source)()?);
        *self.held.lock_recover() = Some((Instant::now(), Arc::clone(&policy)));
        Some(policy)
    }

    /// Whether the filesystem may show this workspace's files to the device.
    #[must_use]
    pub fn reads(&self) -> bool {
        self.policy().is_some_and(|policy| {
            let access = MemberAccess::new(policy, self.signer.clone());
            let document = access.node(
                &tilo_action_policy::TreePath::root()
                    .app(fs_protocol::tree::FILES_APP)
                    .resource(fs_protocol::tree::document_id()),
            );
            access.answer(
                &document,
                ActionKind::Read,
                &Context::at_time(tilo_clock::wall()),
            ) == Answer::Allow
        })
    }

    /// Whether `invoker` (the person at this device when `None`) may take the file action
    /// `verb` on the tree's node `node`, or on the tree as a whole with no node named. Waits for
    /// the workspace's policy while this device holds none, and is refused only once that wait
    /// ends without one: no receiver could decide it either.
    #[must_use]
    pub fn admits(&self, invoker: Option<&Acting>, node: Option<&str>, verb: &str) -> bool {
        let Some(policy) = self.policy() else {
            return false;
        };
        let access = MemberAccess::new(policy, self.signer.clone());
        let access = match invoker {
            Some(invoker) => match access.invoked_by(invoker) {
                Some(access) => access,
                None => return false,
            },
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
            .waiting_at_most(Duration::from_millis(200))
    }

    /// An admission whose source has a policy only from its `arrives`-th read on, and none
    /// again from its `leaves`-th.
    fn arriving(arrives: usize, leaves: usize) -> FileAdmission {
        let reads = std::sync::atomic::AtomicUsize::new(0);
        FileAdmission::new(
            EDITOR,
            Box::new(move || {
                let read = reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                (arrives..leaves).contains(&read).then(policy)
            }),
        )
        .expect("a person's key")
        .waiting_at_most(Duration::from_secs(10))
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

        phase a_first_write_waits_for_the_policy_instead_of_failing;
        {
            // The device has no policy for its first few reads, as right after a login.
            let admission = arriving(5, usize::MAX);
            assert!(admission.admits(None, Some("n1"), verbs::WRITE));
        }

        phase a_policy_held_answers_while_the_source_cannot_be_read;
        {
            let admission = arriving(0, 1);
            assert!(admission.admits(None, Some("n1"), verbs::WRITE));
            std::thread::sleep(POLICY_FRESH);
            assert!(
                admission.admits(None, Some("n1"), verbs::WRITE),
                "the newest policy held still answers"
            );
        }

        phase a_handed_agent_writes_for_its_person_within_their_role;
        {
            let agent = |person: &str| {
                Acting::agent("claude", person)
                    .handed()
                    .in_session("s1")
                    .acting_for(Acting::person(person))
            };
            let for_editor = agent(EDITOR);
            assert!(admission(EDITOR, Some(policy())).admits(Some(&for_editor), None, verbs::WRITE));
            let for_viewer = agent(VIEWER);
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
                tilo_action_policy::Grantee::agent("claude", OWNER),
                tilo_action_policy::TreePath::workspace("w")
                    .joined(&fs_protocol::file_action::tree_node()),
                tilo_action_types::KindSet::UNDOABLE,
                Answer::Deny,
                tilo_action_policy::Grantee::person(OWNER),
            ));
            let agent = Acting::agent("claude", OWNER)
                .handed()
                .acting_for(Acting::person(OWNER));
            let admission = admission(OWNER, Some(denied));
            assert!(!admission.admits(Some(&agent), Some("n1"), verbs::WRITE));
            assert!(admission.admits(None, Some("n1"), verbs::WRITE));
        }
    }
}
