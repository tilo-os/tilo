//! Host-owned empty application records, before an authenticated creator joins.
//! Neither an app document write nor an arbitrary reader can create ownership.

use crate::{NativeWorkspaceOwner, RoomId, publication};
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
    ///
    /// # Errors
    /// The route is malformed or the installed package does not declare the kind.
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
}

impl NativeWorkspaceOwner {
    /// Complete the original creator's bootstrap against its exact revision.
    /// The broker retains the source-bound one-time receipt; this owner checks
    /// current signing custody and commits before any initialized view is Ready.
    ///
    /// # Errors
    /// Refuses changed custody, revision, malformed data, or failed publication.
    pub fn initialize_application_record(
        &self,
        room: RoomId,
        creator: &DeviceCert,
        key: &SigningKey,
        initialization: ApplicationInitialization<'_>,
        now: SystemTime,
    ) -> Result<[u8; 32], String> {
        let _turn = self.0.mutation.lock();
        let mut candidate = self.with_current(room, |view| Ok(view.clone()))?;
        if creator != candidate.device()
            || creator != candidate.governor()
            || key.verifying_key().to_bytes() != creator.device_public_key
        {
            return Err("application initialization lacks current governor custody".into());
        }
        if initialization.document.is_empty() || initialization.document.len() > 64 * 1024 {
            return Err("application initialization exceeds bootstrap limit".into());
        }
        let route = initialization.route;
        match candidate.read_document(
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
        let mut document = automerge::AutoCommit::load(initialization.document)
            .map_err(|error| error.to_string())?;
        // StatePeer's normal admitted snapshot merge preserves the original
        // graph, including fields this SDK does not know; never replace it.
        let message = candidate.peer.publish_document(
            &route.app,
            &route.record,
            document.save(),
            key,
            now,
        )?;
        let outgoing = publication::prepare(&publication::request(&message)?)?;
        publication::reserve(&candidate, &outgoing)?;
        publication::append_local(&mut candidate, outgoing)?;
        candidate.record.peer = candidate.peer.durable()?;
        candidate.record.generation = candidate
            .record
            .generation
            .checked_add(1)
            .ok_or("native application generation exhausted")?;
        let crate::DocumentRead::Captured(committed) = candidate.read_document(
            creator.device_public_key,
            &route.app,
            &route.record,
            None,
            64 * 1024,
            now,
        )?
        else {
            return Err("initialized application capture unavailable".into());
        };
        self.publish_room(room, candidate, None)?;
        Ok(committed.revision)
    }
    /// Create an empty, canonical record with actual current governor custody.
    /// Existing records remain byte-identical. Publication is durable before
    /// this returns; the caller may only then admit the creator's channel.
    ///
    /// # Errors
    /// Custody is not the selected governing device, creation is refused, or
    /// the same native owner publication barrier cannot commit the candidate.
    pub fn prepare_application_record(
        &self,
        room: RoomId,
        creator: &DeviceCert,
        key: &SigningKey,
        route: &ApplicationRecord,
        now: SystemTime,
    ) -> Result<bool, String> {
        let _turn = self.0.mutation.lock();
        let mut candidate = self.with_current(room, |view| Ok(view.clone()))?;
        if creator != candidate.device()
            || creator != candidate.governor()
            || key.verifying_key().to_bytes() != creator.device_public_key
        {
            return Err("application creator lacks exact current governor signing custody".into());
        }
        candidate.peer.permissions(creator.device_public_key, now)?;
        if candidate.peer.document(&route.app, &route.record).is_some() {
            return Ok(false);
        }
        let message = candidate.peer.publish_document(
            &route.app,
            &route.record,
            automerge::AutoCommit::new().save(),
            key,
            now,
        )?;
        let outgoing = publication::prepare(&publication::request(&message)?)?;
        publication::reserve(&candidate, &outgoing)?;
        publication::append_local(&mut candidate, outgoing)?;
        candidate.record.peer = candidate.peer.durable()?;
        candidate.record.generation = candidate
            .record
            .generation
            .checked_add(1)
            .ok_or("native application generation exhausted")?;
        self.publish_room(room, candidate, None)?;
        Ok(true)
    }
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
