//! Members' writes to a governed room's layout document, decided before the
//! relay keeps them.
//!
//! Whoever may edit the room may write the layout entries minted in their
//! own device's namespace. Rearranging a window another device opened is a
//! request the room retains, judged on that item's `moderation` node with the
//! asker's own grants ([`crate::layout_guard`]); nobody writes into another
//! device's entries. Every door a member's write enters by asks
//! [`Rooms::judge_layout_write`] first; a refused write is neither kept
//! nor forwarded, like any change the room does not grant.

use collab_types::DocumentMutation;
use tilo_action_policy::ActionKind;
use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_RECORD, act_node};

use super::{GovernedRoom, Rooms, SystemTime};
use crate::layout_guard::LayoutGuard;

/// Whether `(app, record)` is the route a room's layout travels on.
pub(crate) fn is_layout(app: &str, record: &str) -> bool {
    app == DESKTOP_APP && record == DESKTOP_RECORD
}

/// Whether `body`, a member's retained write, writes the room's layout: what
/// a door asks before taking the room's lock for it.
pub(crate) fn writes_layout(body: &[u8]) -> bool {
    matches!(
        postcard::from_bytes::<neighbour_wire::NeighbourMsg>(body),
        Ok(neighbour_wire::NeighbourMsg::AppDocument { app_id, record_id, .. })
            if is_layout(&app_id, &record_id)
    )
}

impl GovernedRoom {
    /// The layout as the room's writes are decided against, started from the
    /// governor's copy the first time a write needs it.
    async fn layout_guard(&mut self) -> Result<&mut LayoutGuard, String> {
        if self.layout.is_none() {
            let held = self.held_desktop().await?;
            let doc = match held {
                Some(bytes) => automerge::AutoCommit::load(&bytes).map_err(|e| e.to_string())?,
                None => automerge::AutoCommit::new(),
            };
            self.layout = Some(LayoutGuard::new(doc));
        }
        self.layout
            .as_mut()
            .ok_or_else(|| "the room's layout is not followed".into())
    }

    /// The governor's copy of the layout, sealed or not.
    #[cfg(any(feature = "agents", feature = "web"))]
    async fn held_desktop(&self) -> Result<Option<Vec<u8>>, String> {
        self.readable_document(DESKTOP_APP, DESKTOP_RECORD).await
    }

    /// The governor's copy of the layout, written in the clear.
    #[cfg(not(any(feature = "agents", feature = "web")))]
    #[expect(
        clippy::unused_async,
        reason = "one signature with the build that opens sealed records"
    )]
    async fn held_desktop(&self) -> Result<Option<Vec<u8>>, String> {
        Ok(self.peer.document(DESKTOP_APP, DESKTOP_RECORD))
    }

    /// Take in a change the governor made to `(app, record)` itself, when it
    /// is the layout the room's writes are decided against.
    #[cfg(feature = "agents")]
    pub(super) fn absorb_layout(
        &mut self,
        (app, record): (&str, &str),
        doc: &mut automerge::AutoCommit,
    ) {
        if is_layout(app, record)
            && let Some(guard) = self.layout.as_mut()
        {
            guard.absorb(doc);
        }
    }

    /// What `envelope` carries, opened with the channel's keys when it is
    /// sealed and put back together when it is one segment of a larger
    /// write.
    #[cfg_attr(
        not(any(feature = "agents", feature = "web")),
        expect(
            clippy::unused_async,
            reason = "one signature with the build that opens sealed records"
        )
    )]
    async fn readable_mutation(
        &mut self,
        envelope: &collab_types::SignedOp,
    ) -> Result<Readable, String> {
        let payload = if envelope.encryption.is_some() {
            // A sealed write of a channel this relay does not steward (it
            // holds no keys for it) is the one write it cannot read: the
            // room's devices, which open it, decide it on receipt.
            #[cfg(any(feature = "agents", feature = "web"))]
            match self
                .open_write(DESKTOP_APP, DESKTOP_RECORD, envelope)
                .await?
            {
                Some(plaintext) => plaintext,
                None => return Ok(Readable::Sealed),
            }
            #[cfg(not(any(feature = "agents", feature = "web")))]
            return Ok(Readable::Sealed);
        } else {
            envelope.payload.clone()
        };
        match collab_types::DocumentFrame::decode(&payload).map_err(|e| e.to_string())? {
            collab_types::DocumentFrame::Whole(mutation) => Ok(Readable::Whole(mutation)),
            // The segments of a larger write go on as they come; the last
            // one completes it, and is refused, so none of it ever applies
            // anywhere, when the whole write is.
            collab_types::DocumentFrame::Segment(segment) => match self
                .layout_segments
                .accept(envelope, segment, std::time::Instant::now())
                .map_err(|error| error.to_string())?
            {
                None => Ok(Readable::Partial),
                Some(whole) => DocumentMutation::decode(&whole.payload)
                    .map(Readable::Whole)
                    .map_err(|error| error.to_string()),
            },
        }
    }
}

/// What a desktop write's envelope gives the relay to decide.
enum Readable {
    Whole(DocumentMutation),
    /// One segment of a write still incomplete.
    Partial,
    /// Sealed under a channel this relay holds no keys for.
    Sealed,
}

impl Rooms {
    /// Decide `author`'s write `body` (a retained document message) to
    /// `room`'s layout. Anything else, and the relay's own writes,
    /// pass untouched.
    ///
    /// # Errors
    /// Why the room refuses it: it closes, moves or renames something its
    /// author did not make and may not moderate, or it is not a desktop.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the room stays locked while its desktop decides the write and takes it in"
    )]
    pub(crate) async fn judge_layout_write(
        &self,
        room: &str,
        author: [u8; 32],
        body: &[u8],
    ) -> Result<(), String> {
        if author == self.inner.host.cert.device_public_key {
            return Ok(());
        }
        let Ok(neighbour_wire::NeighbourMsg::AppDocument {
            app_id,
            record_id,
            envelope,
            ..
        }) = postcard::from_bytes::<neighbour_wire::NeighbourMsg>(body)
        else {
            return Ok(());
        };
        if !is_layout(&app_id, &record_id) || envelope.kind != collab_types::OpKind::Change {
            return Ok(());
        }
        let state = self.room(room).await.map_err(|error| error.to_string())?;
        let mut governed = state.lock().await;
        let now = SystemTime::now();
        let member = governed.peer().permissions(author, now)?;
        let mutation = match governed.readable_mutation(&envelope).await? {
            Readable::Whole(mutation) => mutation,
            Readable::Partial => return Ok(()),
            Readable::Sealed => {
                tracing::debug!(%room, "a sealed layout write this relay holds no keys for is left to the room's devices");
                return Ok(());
            }
        };
        let room_id = governed.room().to_owned();
        let allows = |act: &tilo_desktop_model::ownership::Touched| {
            member.allows(&act_node(&room_id, act), ActionKind::Undoable, now)
        };
        let writer = hex::encode(author);
        let guard = governed.layout_guard().await?;
        guard
            .decide(&mutation, &room_id, &writer, &member.person, &allows)
            .map_err(|refusal| refusal.to_string())
    }
}
