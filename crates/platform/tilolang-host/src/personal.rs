//! The workspace's record of each person's own state, as an instance holds it: the view state
//! they set in the app's views and what they have seen ([`tilolang_tilo::personal`]).
//!
//! It is the record every app of the workspace opens beside its own
//! (`collab_types::WORKSPACE_RECORDS`), kept by the language's host app: the instance joins its
//! route on the instance's own connection, keeps a copy of it on disk beside the app's record,
//! writes the person's changes into it and sends them on its route, and takes in what the
//! person's other windows and devices wrote. So the place a person picked is still theirs after a
//! restart and on their other devices.

use std::rc::Rc;

use app_state::DocWriter;
use automerge::ChangeHash;
use collaboration::document_routes::{decode_routed_mutation, validated_route_payload_with};
use collaboration::record::RecordId;
use collaboration::record::save::{open_stored, record_document};
use collaboration::{
    CollabEvent, CollabRequest, CollaborationManager, DocumentMutation, RouteSpec,
};
use tilo_actions::{Action as _, ActionContext, ActionInvoker};
use tilo_automerge::ApplyIncoming as _;
use tilo_resource::{Doc, Resource as _};
use tilolang_tilo::personal::{self, Keep, Personal, PersonalState};

/// The record of each person's own state of one workspace, held by an instance of it.
pub struct PersonalRecord {
    route: RouteSpec,
    doc: Doc,
    /// The on-disk copy; `None` keeps it in memory only.
    writer: Option<DocWriter>,
    /// The heads of what the workspace was handed: a send sends what follows them.
    sent: Vec<ChangeHash>,
    /// What the record holds, read again whenever it moved.
    state: PersonalState,
    /// Whether the record moved since the instance was last given what it holds.
    moved: bool,
    /// The app whose window changes it, as each change names it.
    app: String,
}

impl PersonalRecord {
    /// The record of `workspace`, its copy kept in `documents`, joined on `manager`: what this
    /// device kept of it before, and what the workspace sends from now on.
    pub fn open(
        workspace: uuid::Uuid,
        app: &str,
        documents: &Rc<dyn durable_store::DurableStore>,
        manager: &mut CollaborationManager,
    ) -> Self {
        let id = collab_types::workspace_record_id(personal::KIND);
        let route = RouteSpec::record(personal::OWNER, workspace, id.clone());
        manager.join_route(
            route.clone(),
            collaboration::record::RecordKind::new(personal::KIND),
        );
        let stored = RecordId::parse(&id)
            .ok_or_else(|| format!("`{id}` is no record id"))
            .and_then(|id| {
                record_document(
                    documents,
                    personal::OWNER,
                    &workspace.to_string(),
                    &id.storage_key(),
                )
                .map_err(|error| error.to_string())
            });
        let (doc, writer) = match stored {
            Ok(store) => {
                let (doc, writer) = open_stored(store);
                (doc, Some(writer))
            }
            Err(error) => {
                tracing::warn!(%error, "personal: the person's own state is kept in memory only");
                (Doc::new(), None)
            }
        };
        let mut record = Self {
            route,
            doc,
            writer,
            // What this device kept and never handed over goes with the first send.
            sent: Vec::new(),
            state: PersonalState::default(),
            moved: true,
            app: app.to_owned(),
        };
        record.read();
        record
    }

    /// What the record holds.
    #[must_use]
    pub fn state(&self) -> &PersonalState {
        &self.state
    }

    /// Whether the record moved since the last call: the instance is given what it holds then.
    pub fn take_moved(&mut self) -> bool {
        std::mem::take(&mut self.moved)
    }

    /// Have the instance given what the record holds on the next turn again: it had no views to
    /// give it to yet.
    pub fn moved_again(&mut self) {
        self.moved = true;
    }

    /// Keep `keeps`, the changes `person` made of their own state, in the record as theirs, and
    /// send them on its route. A change the record does not take stays out of it, told once.
    pub fn keep(&mut self, person: &str, keeps: Vec<Keep>, manager: &mut CollaborationManager) {
        if keeps.is_empty() {
            return;
        }
        for keep in keeps {
            if let Err(error) = self.apply(person, &keep) {
                tracing::warn!(%error, "personal: the person's own state stays on this device");
            }
        }
        self.read();
        self.save();
        self.send(manager);
    }

    fn apply(&mut self, person: &str, keep: &Keep) -> Result<(), tilo_actions::ActionError> {
        let encoded = tilo_resource::encode::<Personal>(keep)?;
        let context = ActionContext::for_resource(personal::KIND, personal::KIND, Personal::LABEL)
            .with_app(self.app.clone(), self.app.clone())
            .with_invoker(ActionInvoker::person(person))
            .with_change(keep.change_kind());
        tilo_actions::apply_local_with_context(
            &mut self.doc,
            keep,
            encoded,
            Some(person.as_bytes()),
            context,
        )?;
        self.doc.commit();
        Ok(())
    }

    /// Take in `event` when it is a change of this record. `None` for an event of anything
    /// else; else whether the record gained a change.
    pub fn take(
        &mut self,
        event: &CollabEvent,
        manager: &mut CollaborationManager,
    ) -> Option<bool> {
        let before = self.doc.committed_heads();
        let applied = match event {
            CollabEvent::Document {
                envelope,
                decrypted_payload,
                ..
            } => {
                let payload = validated_route_payload_with(
                    &self.route,
                    envelope,
                    decrypted_payload.as_deref(),
                )?;
                decode_routed_mutation(payload, decrypted_payload.as_deref())
                    .map_err(|error| format!("{error:?}"))
                    .and_then(|mutation| {
                        load(&mut self.doc, &mutation).map_err(|error| error.to_string())
                    })
                    .unwrap_or_else(|error| {
                        tracing::warn!(%error, "personal: a change was not taken in");
                        0
                    })
            }
            CollabEvent::CommittedDocument { route, read } if *route == self.route => {
                read.take_into(&mut self.doc, load).unwrap_or(0)
            }
            _ => return None,
        };
        if applied == 0 {
            return Some(false);
        }
        // What arrived is the workspace's already: only what this device made past what it
        // handed over goes back.
        if heads_equal(&before, &self.sent) {
            self.sent = self.doc.committed_heads();
        }
        self.read();
        self.save();
        self.send(manager);
        Some(true)
    }

    /// Send what the workspace was not handed yet on the record's route: the whole record when
    /// it was handed nothing, so the workspace founds it from all of it.
    fn send(&mut self, manager: &mut CollaborationManager) {
        let request = if self.sent.is_empty() {
            if self.doc.committed_heads().is_empty() {
                return;
            }
            CollabRequest::document_snapshot(self.doc.save_committed())
        } else {
            let change = self.doc.committed_document().save_after(&self.sent);
            if change.is_empty() {
                return;
            }
            CollabRequest::document_incremental(change)
        };
        match manager.try_send_to_route(self.route.clone(), request) {
            Ok(()) => self.sent = self.doc.committed_heads(),
            Err(error) => {
                tracing::debug!(%error, "personal: the change goes with the next send");
            }
        }
    }

    fn read(&mut self) {
        match Personal::read(&self.doc) {
            Ok(state) => {
                if state != self.state {
                    self.state = state;
                    self.moved = true;
                }
            }
            Err(error) => tracing::warn!(%error, "personal: the record is unreadable"),
        }
    }

    /// Write what the record holds to its on-disk copy now, as the instance ends.
    pub fn flush(&mut self) {
        if let Some(writer) = &mut self.writer {
            writer.request(&self.doc);
            writer.flush();
        }
    }

    fn save(&mut self) {
        if let Some(writer) = &mut self.writer {
            writer.request(&self.doc);
        }
    }
}

fn load(doc: &mut Doc, mutation: &DocumentMutation) -> Result<usize, automerge::AutomergeError> {
    match mutation {
        DocumentMutation::Incremental(bytes) => doc.load_incoming(bytes),
        DocumentMutation::Snapshot(bytes) => doc.load_incoming_whole(bytes),
    }
}

/// Whether `a` and `b` name the same heads, in whatever order.
fn heads_equal(a: &[ChangeHash], b: &[ChangeHash]) -> bool {
    a.len() == b.len() && a.iter().all(|head| b.contains(head))
}
