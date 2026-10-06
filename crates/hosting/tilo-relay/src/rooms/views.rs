//! Exact-source discovery using the governor's already-owned channel state.
use super::{GovernedRoom, Rooms, SystemTime};
use tilo_actions::remote::{Caller, CallerPrincipal};
use tilo_agent_participant::room_actions::{ViewDocument, ViewReadPolicy};
use tilo_browser_peer::DocumentRoute;

const MAX_VIEW_DOCUMENT_BYTES: usize = 16 * 1024 * 1024;

impl GovernedRoom {
    /// Called while holding this governed room's lock, including before keys.
    fn view_read_policy(&self, caller: &Caller, app: &str, record: &str) -> Option<ViewReadPolicy> {
        if !matches!(caller.principal(), CallerPrincipal::Person(_)) {
            return None;
        }
        let device = *caller.device_public_key()?;
        let user = caller.user_public_key()?;
        let peer = self.peer();
        if !peer
            .certificates()
            .iter()
            .any(|cert| cert.device_public_key == device && &cert.user_public_key == user)
        {
            return None;
        }
        let now = SystemTime::now();
        peer.check_document_read(device, app, record, now).ok()?;
        let (role, caps, person) = peer.permissions(device, now).ok()?;
        Some(ViewReadPolicy {
            room: self.room().to_owned(),
            role,
            caps,
            person,
            unix_time: now.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_secs(),
        })
    }
}

impl Rooms {
    pub(crate) async fn resource_view_policy(
        &self,
        workspace: u64,
        caller: &Caller,
        app: &str,
        record: &str,
    ) -> Option<ViewReadPolicy> {
        let rooms = self
            .inner
            .rooms
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for governed in rooms {
            let room = governed.lock().await;
            if room.workspace_id() == workspace {
                return room.view_read_policy(caller, app, record);
            }
        }
        None
    }

    pub(crate) async fn resource_view_document(
        &self,
        workspace: u64,
        caller: &Caller,
        source: &DocumentRoute,
    ) -> Result<ViewDocument, String> {
        if source.workspace_id().ok() != Some(workspace) {
            return Err("source room mismatch".into());
        }
        let state = self
            .room(&source.cluster_id)
            .await
            .map_err(|error| error.to_string())?;
        let room = state.lock().await;
        if room.workspace_id() != workspace {
            return Err("source room mismatch".into());
        }
        // This guard is under the same lock as key access and replay. Revocation
        // cannot slip between a prior policy lookup and the first key access.
        room.view_read_policy(caller, &source.app_id, &source.record)
            .ok_or("resource read not permitted")?;
        let channel =
            collab_types::document_channel_id(&source.app_id, &source.cluster_id, &source.record);
        let keys = room
            .stewardship
            .as_ref()
            .ok_or("source stewardship unavailable")?
            .existing_keys(channel)?;
        let retained = self
            .inner
            .host
            .records
            .view_channel(
                workspace,
                tilo_peer_state::transport::app_document_channel(&source.app_id, &source.record),
                MAX_VIEW_DOCUMENT_BYTES,
            )
            .await?;
        let bytes = replay_source(source, keys, retained)?;
        room.view_read_policy(caller, &source.app_id, &source.record)
            .ok_or("resource read not permitted")?;
        drop(room);
        drop(state);
        Ok(ViewDocument {
            revision: blake3::hash(&bytes).to_hex().to_string(),
            bytes,
        })
    }
}

fn replay_source(
    source: &DocumentRoute,
    keys: channel_crypto::ChannelContentKeys,
    retained: Vec<crate::room_records::RetainedBody>,
) -> Result<Vec<u8>, String> {
    let channel =
        collab_types::document_channel_id(&source.app_id, &source.cluster_id, &source.record);
    let mut ciphers = tilo_browser_peer::document::Ciphers::new();
    ciphers.insert(
        source.clone(),
        tilo_browser_peer::document::content_key_reader(keys),
    );
    let mut reassembly = tilo_browser_peer::document::Reassembly::default();
    let mut doc = automerge::AutoCommit::new();
    let mut bytes = 0usize;
    let mut found = false;
    for retained in retained {
        let Ok(tilo_peer_state::NeighbourMsg::AppDocument {
            app_id,
            cluster_id,
            record_id,
            envelope,
            ..
        }) = postcard::from_bytes(&retained.body)
        else {
            continue;
        };
        if app_id != source.app_id || cluster_id != source.cluster_id || record_id != source.record
        {
            continue;
        }
        if envelope
            .encryption
            .is_none_or(|metadata| metadata.channel != channel)
        {
            return Err("source encrypted channel mismatch".into());
        }
        bytes = bytes
            .checked_add(retained.body.len())
            .ok_or("source replay exceeds read budget")?;
        if bytes > MAX_VIEW_DOCUMENT_BYTES {
            return Err("source replay exceeds read budget".into());
        }
        let frame =
            tilo_browser_peer::document::open_frame(&retained.body, retained.author, &mut ciphers)
                .map_err(|error| error.to_string())?
                .ok_or("source frame missing")?;
        if let Some(frame) = reassembly
            .accept(frame)
            .map_err(|error| error.to_string())?
        {
            let mutation = frame.mutation().ok_or("source mutation missing")?;
            let (collab_types::DocumentMutation::Snapshot(change)
            | collab_types::DocumentMutation::Incremental(change)) = mutation;
            tilo_automerge::ApplyIncoming::load_incoming(&mut doc, &change)
                .map_err(|error| error.to_string())?;
            found = true;
        }
    }
    if !found || reassembly.buffered_bytes() != 0 || !doc.get_missing_deps(&[]).is_empty() {
        return Err("existing source is not completely retained".into());
    }
    Ok(doc.save())
}
