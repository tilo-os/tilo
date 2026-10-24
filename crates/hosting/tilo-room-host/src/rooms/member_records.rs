//! An application's empty record, created by this host for a member opening
//! the app in a room it governs for the first time.
//!
//! A room's records exist only once its governor signs their genesis: a
//! member's change to a record the room does not hold waits for it. When a
//! device governs the room it signs the empty genesis itself
//! (`prepare_application_record` in `tilo-joined-room`); when this host
//! governs, the member asks it here, through the member action
//! `application-record.prepare` (`super::actions::member`). The host signs
//! the same empty, canonical document and nothing else: no member-supplied
//! snapshot or factory crosses this action. The app's first state then reaches the record
//! as the member's own change, admitted like any other.

use super::{CollaborationError, Rooms, system_time};
use tilo_user_identity::DeviceCert;

/// The record a member asks for: an app and its record id on the wire
/// (`""` for the app's primary).
pub(crate) struct PrepareApplicationRecordRequest {
    pub(crate) app: String,
    pub(crate) record: String,
}

/// The longest application id and record id the action takes, as the
/// creating device's own declaration check bounds them.
const MAX_APP_BYTES: usize = 256;
const MAX_RECORD_BYTES: usize = 4096;

/// A created or found record, and the room generation that carries it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreparedRecord {
    /// Whether this request signed the genesis.
    pub created: bool,
    /// The committed room generation.
    pub generation: u64,
}

fn valid(request: &PrepareApplicationRecordRequest) -> Result<(), CollaborationError> {
    let PrepareApplicationRecordRequest { app, record } = request;
    if app.is_empty()
        || app.len() > MAX_APP_BYTES
        || record.len() > MAX_RECORD_BYTES
        || [app, record]
            .iter()
            .any(|value| value.chars().any(char::is_control))
    {
        return Err(CollaborationError::new(
            "application record route is malformed",
        ));
    }
    Ok(())
}

/// Whether `caller` may have the record of `request` created in `peer`'s
/// room now: a device in the current roster, of a member the room lets
/// change the record, for an app the room lets author its records.
fn may_create(
    peer: &tilo_peer_state::StatePeer,
    caller: &DeviceCert,
    request: &PrepareApplicationRecordRequest,
    at: web_time::SystemTime,
) -> Result<(), CollaborationError> {
    if !peer.certificates().contains(caller) {
        return Err(CollaborationError::new(
            "application record caller is not current admitted evidence",
        ));
    }
    let member = peer.permissions(caller.device_public_key, at)?;
    if !member
        .policy
        .app_authors(&request.app, &tilo_action_policy::Context::at_time(at))
    {
        return Err(CollaborationError::new(
            "the room does not let this application author its records",
        ));
    }
    let node = collab_types::document_node(peer.room(), &request.app, &request.record);
    if !member.allows(&node, tilo_action_policy::ActionKind::Undoable, at) {
        return Err(CollaborationError::new(
            "application record caller may not change the record",
        ));
    }
    Ok(())
}

impl Rooms {
    /// Create `request`'s record in `room`, empty and signed by this host,
    /// for `caller`, an admitted member's device (the certificate the
    /// member action's caller resolved to). The room must already let the app author
    /// its records, and the caller must be a current member who may change
    /// that record: the same decision that later admits the app's first
    /// state from it. An existing record is left byte-identical, so a retry
    /// answers `created: false`.
    ///
    /// # Errors
    /// Refuses invalid or revoked certificates, devices outside the current
    /// roster, members who may not change the record, apps the room does not
    /// let author, malformed routes, rooms whose records are sealed
    /// channels, and failed signed publication.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the room stays locked through the committed and published genesis"
    )]
    pub(crate) async fn prepare_member_application_record(
        &self,
        room: &str,
        caller: &DeviceCert,
        request: &PrepareApplicationRecordRequest,
    ) -> Result<PreparedRecord, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        tilo_user_identity::verify_device_cert_at(caller, now)
            .map_err(|error| CollaborationError::new(error.to_string()))?;
        if self.revocations().is_revoked(&caller.device_public_key) {
            return Err(CollaborationError::new("device revoked"));
        }
        valid(request)?;
        let state = self.room(room).await?;
        let mut governed = state.lock().await;
        if governed.stewardship.is_some() {
            return Err(CollaborationError::new(
                "this room's records are its members' sealed channels",
            ));
        }
        if governed.peer().authority() != &self.inner.host.cert {
            return Err(CollaborationError::new(
                "application record governor differs from current room authority",
            ));
        }
        let at = system_time(now);
        let peer = governed.peer();
        may_create(peer, caller, request, at)?;
        if peer.document(&request.app, &request.record).is_some() {
            return Ok(PreparedRecord {
                created: false,
                generation: governed.record.generation,
            });
        }
        let mut candidate = peer.clone();
        candidate.publish_document(
            &request.app,
            &request.record,
            automerge::AutoCommit::new().save(),
            &self.inner.host.key,
            at,
        )?;
        let collaboration = governed.collaboration().clone();
        let generation = governed.commit(candidate, &self.inner.host.key, at, |_, _| {
            Ok(collaboration)
        })?;
        self.settle(&mut governed).await?;
        Ok(PreparedRecord {
            created: true,
            generation,
        })
    }
}
