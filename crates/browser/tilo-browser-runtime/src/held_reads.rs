//! What a page holds of its workspace's resources, as its person may read them.
//!
//! A window reads another app's resource through its session, and the page answers from what it
//! holds (`browser::session_asks`): for a desktop's windows, and for the window of a page that
//! shows one app alone. What it holds is a resource one of its windows has open, one its
//! workspace's replication took in, or, on a page that joined no workspace, one its own store
//! keeps ([`Beyond`]).
//!
//! A page that joined a workspace holds only what the workspace sent it, and a workspace sends
//! some resources only to a device that asked for them: every resource of a workspace anyone may
//! walk into, a part of a chain behind its head, an on-demand resource. A window that opens such a
//! resource asks for it. A read asks the same way ([`Beyond::ask`]): a resource the page holds none
//! of is asked of the workspace when it is first read, so it arrives and its changes follow,
//! whether or not any window ever shows it. Until the workspace answered, the resource is still
//! arriving; once it said it holds none of it, the resource is absent; and another person's own
//! resource is refused, as the workspace refuses it.
//!
//! Who may read is decided before anything is read or asked ([`crate::read_bounds`]).
//!
//! The page's session asks answer with this for every mount; it is here, apart from the page's
//! thread-locals, so it is tested natively.

use std::cell::RefCell;

use tilo_actions::remote::{
    ActionCallError, Caller, RESOURCE_READ_REFUSED, ResourceAddress, ResourceCopy,
};
use tilo_desktop_surface::session::HeldRecords;
use ui::automerge::transaction::Transactable as _;
use ui::automerge::{AutoCommit, ChangeHash};
use ui::permissions::SharedPermissions;
use ui::resource::{ResourceStore, WorkspaceAnswer};

/// What a page holds of its workspace's resources beyond the ones its windows hold open.
pub(crate) trait Beyond {
    /// Whether the page joined a workspace, whose replication then holds its resources.
    fn joined(&self) -> bool;

    /// The resource `address` as the workspace's replication holds it now, a window's open copy of
    /// it included. Asked of a page that joined a workspace.
    fn replicated(&self, address: &ResourceAddress) -> Option<AutoCommit>;

    /// The heads the workspace last delivered the resource `address` at.
    fn delivered(&self, address: &ResourceAddress) -> Option<Vec<ChangeHash>>;

    /// Ask the workspace for the resource `address`, which this page holds none of, as a window
    /// that opens it asks: once per resource, and from then on the workspace sends what it holds
    /// of it and every change to it. What the workspace answered of a resource it sent none of;
    /// `None` while the resource or the answer is still on its way.
    fn ask(&self, address: &ResourceAddress) -> Option<WorkspaceAnswer>;

    /// What the workspace answered of the resource `address` when it sent none of it. Asks it
    /// nothing.
    fn answered(&self, address: &ResourceAddress) -> Option<WorkspaceAnswer>;

    /// The resource `address` as its app last committed it where a page that joined no workspace
    /// keeps it, when no window holds it open.
    fn stored(&self, address: &ResourceAddress) -> Option<AutoCommit>;

    /// Whether a page that joined no workspace keeps the resource `address` no window has open.
    fn keeps(&self, address: &ResourceAddress) -> bool;

    /// A count that moves whenever a resource the workspace sent or took may have changed.
    fn moved(&self) -> u64;
}

/// What a page holds of its workspace's resources, as its person may read them: the same for a
/// desktop and for a mount of one app, so one policy decides both.
pub(crate) struct Held<'a, B> {
    /// The person at this page: a read is theirs or nobody's.
    pub(crate) person: &'a str,
    pub(crate) workspace: &'a str,
    /// The role the workspace answered this person holds, as `desktop::SharedRole::code` counts
    /// it: 0 until it has.
    pub(crate) role: u8,
    pub(crate) permissions: &'a SharedPermissions,
    /// The stores the page's windows hold open.
    pub(crate) stores: &'a RefCell<Vec<ResourceStore>>,
    /// What the page holds beyond those.
    pub(crate) beyond: B,
}

impl<B: Beyond> Held<'_, B> {
    /// `read` of the resource `address` as a window of its app holds it open, if one does.
    fn open<R>(&self, address: &ResourceAddress, read: impl FnOnce(&AutoCommit) -> R) -> Option<R> {
        let id = ui::resource::DocumentId::from_route(&address.record)?;
        let stores = self.stores.borrow();
        stores
            .iter()
            .find(|store| {
                let (app, home) = store.app_and_home();
                app == address.app && home == self.workspace
            })?
            .read(&id, read)
    }

    /// The resource `address` as this page holds it: what the workspace's replication holds, on a
    /// page that joined a workspace, and else what a window has open or the page's store keeps.
    /// Without what is still being typed, as a read answers committed content.
    fn committed(&self, address: &ResourceAddress) -> Option<Vec<u8>> {
        let mut held = if self.beyond.joined() {
            self.beyond.replicated(address)?
        } else {
            self.open(address, Clone::clone)
                .or_else(|| self.beyond.stored(address))?
        };
        if held.pending_ops() > 0 {
            held.rollback();
        }
        Some(held.save())
    }
}

impl<B: Beyond> HeldRecords for Held<'_, B> {
    fn read(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
    ) -> Result<ResourceCopy, ActionCallError> {
        let joined = self.beyond.joined();
        // Who may read is decided before anything is read or asked ([`crate::read_bounds`]).
        crate::read_bounds::Reader {
            person: self.person,
            workspace: self.workspace,
            joined,
            role: self.role,
            permissions: self.permissions,
        }
        .may_read(caller, address)?;
        if let Some(held) = self.committed(address) {
            return Ok(ResourceCopy::Document(held));
        }
        if !joined {
            return Ok(ResourceCopy::Absent);
        }
        // A workspace sends some resources only to a device that asked: it is asked for this one,
        // as a window that opens it asks, and says what it holds of it.
        match self.beyond.ask(address) {
            None => Err(ActionCallError::Unavailable {
                reason: "the record is still arriving from the workspace".into(),
            }),
            Some(WorkspaceAnswer::HoldsNone) => Ok(ResourceCopy::Absent),
            Some(WorkspaceAnswer::NotYours) => Err(ActionCallError::Refused {
                reason: RESOURCE_READ_REFUSED.into(),
            }),
        }
    }

    fn mark(&self, address: &ResourceAddress) -> Option<Vec<u8>> {
        // A resource a window holds open moves with its committed heads, whoever wrote it; one no
        // window holds moves only as the workspace delivers it.
        let heads = self
            .open(address, AutoCommit::base_heads)
            .or_else(|| self.beyond.delivered(address));
        if let Some(heads) = heads {
            return Some(heads.iter().flat_map(|head| head.0).collect());
        }
        if self.beyond.joined() {
            // What the workspace answered of a resource it sent none of marks it too: a read held
            // while the resource was arriving is answered on the pass after the answer.
            return self.beyond.answered(address).map(|answer| match answer {
                WorkspaceAnswer::HoldsNone => vec![0],
                WorkspaceAnswer::NotYours => vec![1],
            });
        }
        self.beyond.keeps(address).then(Vec::new)
    }

    fn authority(&self) -> u64 {
        ui::permissions::shared_revision()
            .wrapping_mul(8)
            .wrapping_add(u64::from(self.role))
    }

    fn moved(&self) -> u64 {
        use std::hash::{Hash as _, Hasher as _};
        let mut moved = std::collections::hash_map::DefaultHasher::new();
        for store in self.stores.borrow().iter() {
            store.kept_revision().hash(&mut moved);
            store.content_revision().hash(&mut moved);
        }
        self.beyond.moved().hash(&mut moved);
        moved.finish()
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    use tilo_actions::remote::{
        ActionCallError, Caller, RESOURCE_READ_REFUSED, ResourceAddress, ResourceCopy,
        ResourceReadRequest,
    };
    use tilo_desktop_surface::session::HeldRecords as _;
    use ui::RemoteActions;
    use ui::automerge::transaction::Transactable as _;
    use ui::automerge::{AutoCommit, ChangeHash};
    use ui::permissions::{
        Acting, Grantee, MemberAccess, MemberId, PermissionContext, Role, SharedPermissions, policy,
    };
    use ui::resource::{ResourceStore, WorkspaceAnswer};
    use ui::time::Duration;

    use super::{Beyond, Held};

    const ADA: &str = "ada";
    const HOME: &str = "w1";

    /// An app and the id of one of its documents there.
    type Key = (String, String);

    fn key(address: &ResourceAddress) -> Key {
        (address.app.clone(), address.record.clone())
    }

    /// A workspace that sends a page a document only once the page asked for it, as the server
    /// of a workspace anyone may walk into does for every document, and every server for a part
    /// of a chain behind its head: what the workspace holds, what it sent this page, and what
    /// the page asked it for.
    #[derive(Default)]
    struct Workspace {
        /// What the workspace holds where it is kept.
        there: RefCell<HashMap<Key, AutoCommit>>,
        /// The documents there that are another person's own.
        anothers: RefCell<HashSet<Key>>,
        /// What the workspace sent this page.
        here: RefCell<HashMap<Key, AutoCommit>>,
        /// What this page asked the workspace for, in order.
        asked: RefCell<Vec<Key>>,
        /// What the workspace answered of a document it sent none of.
        answers: RefCell<HashMap<Key, WorkspaceAnswer>>,
        moved: Cell<u64>,
    }

    impl Workspace {
        /// Someone writes `body` into `address` where the workspace is kept: an agent's turn
        /// there, a member's other device.
        fn write(&self, address: &ResourceAddress, body: &str) {
            let mut there = self.there.borrow_mut();
            let document = there.entry(key(address)).or_default();
            document.put(ui::automerge::ROOT, "body", body).unwrap();
            let _ = document.commit();
        }

        /// What the page's session brings from the workspace: each document the page asked for,
        /// as the workspace holds it now, or its answer for one it sends none of. Nothing of a
        /// document the page never asked for.
        fn deliver(&self) {
            for asked in self.asked.borrow().iter() {
                if self.anothers.borrow().contains(asked) {
                    self.answers
                        .borrow_mut()
                        .insert(asked.clone(), WorkspaceAnswer::NotYours);
                } else if let Some(document) = self.there.borrow().get(asked) {
                    self.answers.borrow_mut().remove(asked);
                    self.here
                        .borrow_mut()
                        .insert(asked.clone(), document.clone());
                } else {
                    self.answers
                        .borrow_mut()
                        .insert(asked.clone(), WorkspaceAnswer::HoldsNone);
                }
            }
            self.moved.set(self.moved.get().saturating_add(1));
        }
    }

    impl Beyond for &Workspace {
        fn joined(&self) -> bool {
            true
        }

        fn replicated(&self, address: &ResourceAddress) -> Option<AutoCommit> {
            self.here.borrow().get(&key(address)).cloned()
        }

        fn delivered(&self, address: &ResourceAddress) -> Option<Vec<ChangeHash>> {
            self.here
                .borrow()
                .get(&key(address))
                .map(AutoCommit::base_heads)
        }

        fn ask(&self, address: &ResourceAddress) -> Option<WorkspaceAnswer> {
            let asked = key(address);
            if !self.asked.borrow().contains(&asked) {
                self.asked.borrow_mut().push(asked.clone());
            }
            self.answers.borrow().get(&asked).copied()
        }

        fn answered(&self, address: &ResourceAddress) -> Option<WorkspaceAnswer> {
            self.answers.borrow().get(&key(address)).copied()
        }

        fn stored(&self, _: &ResourceAddress) -> Option<AutoCommit> {
            None
        }

        fn keeps(&self, _: &ResourceAddress) -> bool {
            false
        }

        fn moved(&self) -> u64 {
            self.moved.get()
        }
    }

    /// A document of the word processor that no window of the page holds open.
    fn poem() -> ResourceAddress {
        address(
            "so.tilo.word-processor",
            "uuid:0b6f4c2e-3a51-4b7e-9d43-5c0e8a1f2d6b",
        )
    }

    /// A document of Notes that is another person's own.
    fn diary() -> ResourceAddress {
        address("so.tilo.notes", "uuid:7d0a53c1-9e2f-4c68-8b15-2f4e6a9c0d37")
    }

    /// The document `id` of `app`, as a read names it.
    fn address(app: &str, id: &str) -> ResourceAddress {
        ResourceAddress {
            app: app.into(),
            record: id.into(),
        }
    }

    /// What `person` may do as a member holding `role`.
    fn member(person: &str, role: Role) -> SharedPermissions {
        SharedPermissions::new(PermissionContext::member(&MemberId::person(person), role))
    }

    /// What `person` may do in a workspace they are no member of.
    fn no_member(person: &str) -> SharedPermissions {
        let policy = policy::WorkspacePolicy::unowned(HOME, &Grantee::role(Role::Owner));
        SharedPermissions::new(PermissionContext::new(
            Acting::person(person.to_owned()),
            Vec::new(),
            MemberAccess::person(policy, person.to_owned()),
        ))
    }

    /// Ada's page in `workspace`, none of whose windows holds a document open, told she holds
    /// the role `role` counts there.
    fn held<'a>(
        permissions: &'a SharedPermissions,
        stores: &'a RefCell<Vec<ResourceStore>>,
        role: u8,
        workspace: &'a Workspace,
    ) -> Held<'a, &'a Workspace> {
        Held {
            person: ADA,
            workspace: HOME,
            role,
            permissions,
            stores,
            beyond: workspace,
        }
    }

    fn read_of(address: &ResourceAddress) -> ResourceReadRequest {
        ResourceReadRequest {
            app: address.app.clone(),
            record: address.record.clone(),
            joined: false,
            after: None,
            wait_ms: None,
        }
    }

    /// What the window's reading in `call` shows of the document: its body's words, or how the
    /// document stands.
    fn shown(call: &ui::ResourceReadCall) -> String {
        call.with(|arrival| {
            let Some(first) = arrival
                .ready()
                .and_then(|readings| readings.pointer("/records/0"))
            else {
                return "no reading".to_owned();
            };
            if first.get("arriving").is_some() {
                return "arriving".to_owned();
            }
            if first.get("absent").is_some() {
                return "absent".to_owned();
            }
            first
                .pointer("/value/body")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("no body")
                .to_owned()
        })
    }

    fn refused() -> Result<ResourceCopy, ActionCallError> {
        Err(ActionCallError::Refused {
            reason: RESOURCE_READ_REFUSED.into(),
        })
    }

    /// A view in a message names a document that no window of its page holds open, in a
    /// workspace that sends a document only to a device that asked for it. The page asks the
    /// workspace for it when the view first reads it; the view shows the document once it
    /// arrived, and each later change of it, though no window ever opens it.
    #[test]
    fn a_read_of_a_document_no_window_holds_open_asks_the_workspace_for_it_and_follows_it() {
        let workspace = Workspace::default();
        workspace.write(&poem(), "The sea at dawn");
        let permissions = member(ADA, Role::Viewer);
        let stores = RefCell::new(Vec::new());
        let held = held(&permissions, &stores, 1, &workspace);
        // The session of Ada's page, one of its windows, and the page's answers to what the
        // window reads: what a view in a message reads through.
        let (session, openings) = tilo_desktop_surface::session::serve(HOME, Arc::default());
        let mut answers = openings.answer_records();
        let window =
            RemoteActions::with_provider(session, Caller::local_person(ADA).in_workspace(HOME));
        let now = ui::time::now();

        // The view asks for the document, and the page holds none of it yet.
        let view = window.follow_resource(None, read_of(&poem()));
        answers.answer(&held, now);
        assert_eq!(shown(view.call()), "arriving");
        assert_eq!(
            *workspace.asked.borrow(),
            [key(&poem())],
            "the page asks its workspace for the document the view named, once"
        );

        // The workspace sends it: the read held meanwhile is answered on the next pass.
        workspace.deliver();
        answers.answer(&held, now + Duration::from_secs(1));
        assert_eq!(shown(view.call()), "The sea at dawn");

        // Someone writes it again where the workspace is kept: the change follows.
        workspace.write(&poem(), "The sea at dusk");
        workspace.deliver();
        answers.answer(&held, now + Duration::from_secs(2));
        assert_eq!(shown(view.call()), "The sea at dusk");
        assert_eq!(
            workspace.asked.borrow().len(),
            1,
            "asked once: the workspace sends its changes from then on"
        );
        assert!(answers.waits(), "the read is held for the next write");
    }

    /// Who may read is decided before the workspace is asked for anything: for a reader the
    /// page refuses, and for one it cannot decide yet, nothing is asked and nothing arrives.
    #[test]
    fn nothing_is_asked_of_the_workspace_for_a_reader_who_may_not_read() {
        let workspace = Workspace::default();
        workspace.write(&poem(), "The sea at dawn");
        let stores = RefCell::new(Vec::new());
        let ada = Caller::local_person(ADA).in_workspace(HOME);

        // The workspace's permissions hold nothing for her, whatever role the page was told.
        let outside = no_member(ADA);
        assert_eq!(
            held(&outside, &stores, 3, &workspace).read(&ada, &poem()),
            refused()
        );
        // Where she may read: another person, another workspace and an agent acting for her.
        let inside = member(ADA, Role::Viewer);
        let hers = held(&inside, &stores, 1, &workspace);
        let another = Caller::local_person("bo").in_workspace(HOME);
        assert_eq!(hers.read(&another, &poem()), refused());
        let elsewhere = Caller::local_person(ADA).in_workspace("w2");
        assert_eq!(hers.read(&elsewhere, &poem()), refused());
        let agent = Caller::local_agent("helper")
            .acting_for(ADA)
            .in_workspace(HOME);
        assert!(matches!(
            hers.read(&agent, &poem()),
            Err(ActionCallError::Refused { .. })
        ));
        // Before the workspace said what she may do, nothing is read or asked on a guess.
        assert!(matches!(
            held(&inside, &stores, 0, &workspace).read(&ada, &poem()),
            Err(ActionCallError::Unavailable { .. })
        ));

        assert!(
            workspace.asked.borrow().is_empty(),
            "the workspace was asked for nothing: {:?}",
            workspace.asked.borrow()
        );
        workspace.deliver();
        assert!(workspace.here.borrow().is_empty(), "and sent nothing");
    }

    /// What the workspace answers of a document it sends none of is what its reader is
    /// answered: one it holds none of is absent, not arriving for good, and another person's
    /// own is refused. One written after all arrives, since the workspace was asked for it.
    #[test]
    fn a_document_the_workspace_holds_none_of_is_absent_and_another_persons_own_is_refused() {
        let workspace = Workspace::default();
        workspace.write(&diary(), "Dear diary");
        workspace.anothers.borrow_mut().insert(key(&diary()));
        let permissions = member(ADA, Role::Viewer);
        let stores = RefCell::new(Vec::new());
        let held = held(&permissions, &stores, 1, &workspace);
        let ada = Caller::local_person(ADA).in_workspace(HOME);

        // Asked and not answered yet: both are still arriving, and nothing marks them.
        assert!(matches!(
            held.read(&ada, &poem()),
            Err(ActionCallError::Unavailable { .. })
        ));
        assert!(matches!(
            held.read(&ada, &diary()),
            Err(ActionCallError::Unavailable { .. })
        ));
        assert_eq!(held.mark(&poem()), None);

        // The workspace answers: it holds none of the one, and the other is someone else's own.
        workspace.deliver();
        assert_eq!(held.read(&ada, &poem()), Ok(ResourceCopy::Absent));
        assert_eq!(held.read(&ada, &diary()), refused());
        assert!(
            held.mark(&poem()).is_some(),
            "the answer moves the mark: a read held for the document is answered on the next pass"
        );

        // Someone writes it after all: the workspace was asked for it, so it arrives.
        workspace.write(&poem(), "The sea at dawn");
        workspace.deliver();
        assert!(matches!(
            held.read(&ada, &poem()),
            Ok(ResourceCopy::Document(_))
        ));
    }
}
