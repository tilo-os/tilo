//! Members' writes to a governed workspace's shared desktop, decided before the
//! relay keeps them.
//!
//! Whoever may edit the workspace may write its desktop. What a write does to a
//! strip or window someone else made (closing, moving, renaming it) is
//! moderation, decided on that item's `moderation` node with the writer's
//! own grants ([`tilo_desktop_model::shared_record::guard`]). Every door a member's write enters
//! by asks [`Workspaces::decide_desktop_write`] first; a refused write is neither
//! kept nor forwarded, like any change the workspace does not grant.

use collab_types::DocumentMutation;
use tilo_action_policy::ActionKind;
use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_RECORD, act_node};

use super::{GovernedWorkspace, Workspaces};
use tilo_desktop_model::shared_record::guard::{self, DesktopGuard};

pub(crate) use tilo_desktop_model::shared_record::guard::is_desktop;

/// Whether `body`, a member's retained write, is one of the workspace's records of the desktop:
/// its layout, or a person's view of it. What a door asks before taking the workspace's lock for
/// it.
pub(crate) fn writes_desktop(body: &[u8]) -> bool {
    matches!(
        postcard::from_bytes::<neighbour_wire::NeighbourMsg>(body),
        Ok(neighbour_wire::NeighbourMsg::AppDocument { app_id, record_id, .. })
            if is_desktop(&app_id, &record_id)
    )
}

impl GovernedWorkspace {
    /// The shared desktop as the workspace's writes are decided against, started
    /// from the governor's copy the first time a write needs it.
    async fn desktop_guard(&mut self) -> Result<&mut DesktopGuard, String> {
        if self.desktop.is_none() {
            let held = self.held_desktop().await?;
            let doc = match held {
                Some(bytes) => automerge::AutoCommit::load(&bytes).map_err(|e| e.to_string())?,
                None => automerge::AutoCommit::new(),
            };
            self.desktop = Some(DesktopGuard::new(doc));
        }
        self.desktop
            .as_mut()
            .ok_or_else(|| "the shared desktop is not followed".into())
    }

    /// The governor's copy of the shared desktop, sealed or not.
    #[cfg(any(feature = "agents", feature = "web"))]
    async fn held_desktop(&self) -> Result<Option<Vec<u8>>, String> {
        self.readable_document(DESKTOP_APP, DESKTOP_RECORD).await
    }

    /// The governor's copy of the shared desktop, written in the clear.
    #[cfg(not(any(feature = "agents", feature = "web")))]
    #[expect(
        clippy::unused_async,
        reason = "one signature with the build that opens sealed records"
    )]
    async fn held_desktop(&self) -> Result<Option<Vec<u8>>, String> {
        Ok(self.peer().document(DESKTOP_APP, DESKTOP_RECORD))
    }

    /// Take in a change the governor made to `(app, record)` itself, when it
    /// is the shared desktop the workspace's writes are decided against.
    #[cfg(feature = "agents")]
    pub(super) fn absorb_desktop(
        &mut self,
        (app, record): (&str, &str),
        doc: &mut automerge::AutoCommit,
    ) {
        if is_desktop(app, record)
            && let Some(guard) = self.desktop.as_mut()
        {
            guard.absorb(doc);
        }
    }

    /// Follow `envelope`, the host's own write to the shared desktop, in the desktop the
    /// members' writes are decided against, once that is followed at all: before, it starts
    /// from the governor's copy, which holds it.
    async fn absorb_host_desktop_write(
        &mut self,
        envelope: &collab_types::SignedOp,
    ) -> Result<(), String> {
        if self.desktop.is_none() {
            return Ok(());
        }
        let Readable::Whole(mutation) = self.readable_mutation(envelope).await? else {
            return Ok(());
        };
        self.desktop_guard()
            .await?
            .absorb_write(&mutation)
            .map_err(|error| error.to_string())
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
            // A sealed write of a channel this relay does not take part in (it
            // holds no keys for it) is the one write it cannot read: the
            // workspace's devices, which open it, decide it on receipt.
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
                .desktop_segments
                .accept(envelope, segment, tilo_clock::now())
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

impl Workspaces {
    /// Decide `author`'s write `body` (a retained document message) to
    /// `workspace`'s shared desktop, with `author`'s own standing in the workspace.
    /// Anything else, and what the workspace's governor writes, pass untouched.
    ///
    /// # Errors
    /// Why the workspace refuses it: it closes, moves or renames something its
    /// author did not make and may not moderate, or it is not a desktop.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the workspace stays locked while its desktop decides the write and takes it in"
    )]
    pub(crate) async fn decide_desktop_write(
        &self,
        workspace: &str,
        author: [u8; 32],
        body: &[u8],
    ) -> Result<(), String> {
        let host = author == self.inner.host.cert.device_public_key;
        let Ok(neighbour_wire::NeighbourMsg::AppDocument {
            app_id,
            record_id,
            envelope,
            ..
        }) = tilo_failure::ignored!(
            "a classifier: a write that does not decode is reported where the governor takes it in",
            postcard::from_bytes::<neighbour_wire::NeighbourMsg>(body)
        )
        else {
            return Ok(());
        };
        if !is_desktop(&app_id, &record_id) || envelope.kind != collab_types::OpKind::Change {
            return Ok(());
        }
        let state = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let mut governed = state.lock().await;
        if host {
            // What the host itself writes (a window an agent's action opened through the host's
            // own session) is the workspace's: not decided, but followed, so the members' writes
            // that build on it are decided against the desktop they saw.
            if guard::is_shared_desktop(&app_id, &record_id) {
                return governed.absorb_host_desktop_write(&envelope).await;
            }
            return Ok(());
        }
        let now = tilo_clock::wall();
        let member = governed
            .peer()
            .permissions(author, now)
            .map_err(|error| error.to_string())?;
        // A person's view of the layout is theirs to write: every member reads it.
        guard::may_write(&app_id, &record_id, &member.person)
            .map_err(|refusal| refusal.to_string())?;
        if !guard::is_shared_desktop(&app_id, &record_id) {
            // Nothing of the workspace's own layout is judged where this record holds none: what a
            // view names is the person's, and it is theirs alone.
            return Ok(());
        }
        let mutation = match governed.readable_mutation(&envelope).await? {
            Readable::Whole(mutation) => mutation,
            Readable::Partial => return Ok(()),
            Readable::Sealed => {
                tracing::debug!(%workspace, "a sealed shared desktop write this relay holds no keys for is left to the room's devices");
                return Ok(());
            }
        };
        let identity = governed.workspace().to_owned();
        let members = governed
            .peer()
            .admitted_workspace()
            .and_then(tilo_peer_state::members::people);
        let allows = |act: &tilo_desktop_model::ownership::Touched| {
            member.allows(&act_node(&identity, act), ActionKind::Undoable, now)
        };
        let guard = governed.desktop_guard().await?;
        guard
            .decide(
                &mutation,
                &guard::Writer {
                    person: &member.person,
                    members: members.as_ref(),
                    allows: &allows,
                },
            )
            .map_err(|refusal| refusal.to_string())
    }
}
