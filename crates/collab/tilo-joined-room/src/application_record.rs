//! An application's first state onto the empty record a room's host created
//! for it, as the creating member's own signed change.
//!
//! No app document write and no arbitrary reader can create ownership: a
//! record's genesis is its room's, never a member's, and never a plaintext
//! document a host signs on a member's behalf.

use crate::{NativeWorkspaceOwner, RoomId};
use ed25519_dalek::SigningKey;
use std::time::SystemTime;
use tilo_user_identity::DeviceCert;

/// A route checked against the installed package's signed resource declarations.
/// The host supplies those declarations from verified package evidence, never
/// from channel arguments. This value itself is not launch authority.
pub struct ApplicationRecord {
    app: String,
    record: String,
}

impl ApplicationRecord {
    /// Bind the SDK's exact kind to a host-verified installed declaration.
    pub fn declared(
        app: &str,
        record: &str,
        kind: &str,
        declared: &[String],
    ) -> Result<Self, String> {
        if app.is_empty()
            || app.len() > 256
            || record.len() > 4096
            || kind.is_empty()
            || kind.len() > 256
            || [app, record, kind]
                .iter()
                .any(|value| value.chars().any(char::is_control))
            || !declared.iter().any(|value| value == kind)
        {
            return Err("application record has no valid installed resource declaration".into());
        }
        Ok(Self {
            app: app.into(),
            record: record.into(),
        })
    }

    /// A record the host keeps itself, which no installed package declares:
    /// a workspace's shared desktop. Only host code names one; nothing an
    /// app sends reaches this.
    ///
    /// # Errors
    /// The route is not a valid record route.
    pub fn host(app: &str, record: &str) -> Result<Self, String> {
        if app.is_empty()
            || app.len() > 256
            || record.len() > 4096
            || [app, record]
                .iter()
                .any(|value| value.chars().any(char::is_control))
        {
            return Err("host record has no valid route".into());
        }
        Ok(Self {
            app: app.into(),
            record: record.into(),
        })
    }
}

impl NativeWorkspaceOwner {
    /// Complete the original creator's bootstrap in a room another device
    /// governs (a host that created the empty record for it): the bootstrap
    /// becomes this member's own signed change onto the record's exact
    /// revision, admitted and published like any other write of it. The
    /// governor's signature never covers member-supplied bytes. Answers the
    /// revision this device's committed view then holds.
    ///
    /// # Errors
    /// Refuses governor or changed custody, a changed revision, malformed or
    /// oversized data, a change the room does not let this member make, or
    /// failed publication.
    pub fn initialize_application_record_as_member(
        &self,
        room: RoomId,
        creator: &DeviceCert,
        key: &SigningKey,
        initialization: ApplicationInitialization<'_>,
        now: SystemTime,
    ) -> Result<[u8; 32], String> {
        use tilo_owner_commit::{CommitOperation, OwnerCommitRequest};
        if initialization.document.is_empty() || initialization.document.len() > 64 * 1024 {
            return Err("application initialization exceeds bootstrap limit".into());
        }
        let route = initialization.route;
        let adds_state = self.with_current(room, |view| {
            member_bootstrap(view, creator, key, initialization, now)
                .map(|changes| changes.is_some())
        })?;
        if adds_state {
            self.commit_document_with_current(
                room,
                |view| {
                    let changes = member_bootstrap(view, creator, key, initialization, now)?
                        .ok_or("application initialization revision changed")?;
                    let message = view.peer().clone().publication_mut().publish_incremental(
                        &route.app,
                        &route.record,
                        changes,
                        key,
                        now,
                    )?;
                    let neighbour_wire::NeighbourMsg::AppDocument { envelope, .. } = message else {
                        return Err("application initialization is not a document write".into());
                    };
                    Ok(OwnerCommitRequest {
                        room: room.to_string(),
                        operation: CommitOperation::Document {
                            app: route.app.clone(),
                            record: route.record.clone(),
                            envelope,
                        },
                    })
                },
                now,
            )?;
        }
        self.with_current(room, |view| {
            match view.read_document(
                creator.device_public_key,
                &route.app,
                &route.record,
                None,
                64 * 1024,
                now,
            )? {
                crate::DocumentRead::Captured(committed) => Ok(committed.revision),
                _ => Err("initialized application capture unavailable".into()),
            }
        })
    }
}

/// The changes `initialization` adds to the record `view` holds, as this
/// member (`creator`, signing with `key`) would sign them: `None` when the
/// bootstrap adds nothing. Checks member custody (another device governs)
/// and that the record is still at the revision the creator read.
fn member_bootstrap(
    view: &crate::NativeCommittedView,
    creator: &DeviceCert,
    key: &SigningKey,
    initialization: ApplicationInitialization<'_>,
    now: SystemTime,
) -> Result<Option<Vec<u8>>, String> {
    if creator != view.device()
        || creator == view.governor()
        || key.verifying_key().to_bytes() != creator.device_public_key
    {
        return Err("application initialization lacks current member custody".into());
    }
    let route = initialization.route;
    match view.read_document(
        creator.device_public_key,
        &route.app,
        &route.record,
        Some(initialization.revision),
        64 * 1024,
        now,
    )? {
        crate::DocumentRead::Unchanged(_) => {}
        crate::DocumentRead::CheckpointPending => {
            return Err("application initialization waits for a checkpoint".into());
        }
        crate::DocumentRead::Captured(_) => {
            return Err("application initialization revision changed".into());
        }
    }
    let held = view
        .peer()
        .document(&route.app, &route.record)
        .ok_or("application record is not in the room")?;
    let mut record = automerge::AutoCommit::load(&held).map_err(|error| error.to_string())?;
    let heads = record.get_heads();
    // The app's bytes are untrusted: taken in by the isolated admission,
    // onto the record's graph, adding only what the bootstrap brings and
    // never replacing what the room holds.
    match tilo_automerge_isolate::apply(
        &mut record,
        initialization.document,
        tilo_automerge_isolate::Dependencies::RequireComplete,
    )
    .map_err(|error| error.to_string())?
    {
        tilo_automerge_isolate::Applied::Changed(_) => {}
        tilo_automerge_isolate::Applied::MissingDependencies => {
            return Err("application initialization does not build on the record".into());
        }
    }
    let changes = record.save_after(&heads);
    if record.get_heads() == heads || changes.is_empty() {
        return Ok(None);
    }
    if changes.len() > 64 * 1024 {
        return Err("application initialization exceeds bootstrap limit".into());
    }
    Ok(Some(changes))
}

#[derive(Clone, Copy)]
pub struct ApplicationInitialization<'a> {
    pub route: &'a ApplicationRecord,
    pub revision: [u8; 32],
    pub document: &'a [u8],
}

#[cfg(test)]
#[path = "application_record_tests.rs"]
pub(crate) mod tests;
