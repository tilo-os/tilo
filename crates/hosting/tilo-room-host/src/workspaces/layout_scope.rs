//! Whether a workspace shares its members' layout is its governance's word
//! ([`WorkspaceLayout`]), and the governor holds the workspace to it.
//!
//! In a workspace whose layout is personal each member's strips and windows stay
//! on their device: which apps someone has open, and their titles, are the
//! person's own. Only the workspace's records (Chat, Notes, Canvas documents)
//! and its people are shared. A workspace admitted through a public door is
//! personal (the persistent workspace a community site runs, settled at every
//! start), and so is one with open admission (listed in the public
//! directory) unless its governor chose otherwise; an invite-only workspace
//! shares its layout.
//!
//! The governor writes the scope into the workspace's governance
//! ([`tilo_desktop_model::projection::WORKSPACE_LAYOUT_KEY`]), where every
//! member's device reads it, and in a personal workspace:
//! - refuses a member's change to the workspace's topology (`SyncResp`) or to its
//!   shared desktop record at every door it enters by
//!   ([`Workspaces::decide_layout_write`]): neither kept, nor forwarded, nor
//!   taken in;
//! - replays to nobody what the relay retained of such changes before
//!   ([`crate::store::WorkspaceStore::replay_member_layout`]);
//! - removes what it committed of members' windows while the workspace shared
//!   its layout, from the workspace document and the shared desktop record, when
//!   the workspace becomes personal and whenever the relay opens it
//!   ([`Workspaces::keep_personal`]). Each device keeps its own layout: that
//!   lives on the device, not in the workspace.

use std::collections::HashMap;

#[cfg(feature = "agents")]
use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_RECORD};
use tilo_desktop_model::projection::WorkspaceLayout;
use tilo_lock::MutexRecover as _;

use super::desktop_writes::is_desktop;
use super::{CollaborationError, GovernedWorkspace, SystemTime, Workspaces};

/// Why a workspace whose layout is personal refuses a member's change to it. A
/// device from before personal workspaces sends its windows again every few
/// seconds, so the refusal is not worth a warning each time.
pub(crate) const LAYOUT_REFUSAL: &str = tilo_relay_protocol::refusal::PERSONAL_LAYOUT;

/// Each governed workspace's layout scope, by workspace UUID: what its governance
/// says, read when the relay opens it and written with it
/// ([`Workspaces::set_layout`], the only writer of a workspace's scope here). Kept
/// apart from the workspace's lock because every door decides a member's write
/// by it.
#[derive(Default)]
pub(super) struct Layouts(std::sync::Mutex<HashMap<String, WorkspaceLayout>>);

impl Layouts {
    fn get(&self, workspace: &str) -> WorkspaceLayout {
        self.0
            .lock_recover()
            .get(workspace)
            .copied()
            .unwrap_or_default()
    }

    fn set(&self, workspace: &str, layout: WorkspaceLayout) {
        self.0.lock_recover().insert(workspace.to_owned(), layout);
    }

    fn personal(&self) -> Vec<String> {
        self.0
            .lock_recover()
            .iter()
            .filter(|(_, layout)| layout.is_personal())
            .map(|(workspace, _)| workspace.clone())
            .collect()
    }
}

/// What a member's retained write `body` does to a workspace's shared layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LayoutWrite {
    /// Changes the workspace document of `workspace`: its topology, windows included.
    Topology,
    /// Writes the workspace's shared desktop record.
    Desktop,
}

/// Whether `body`, a member's retained write to `workspace`, changes the workspace's
/// shared layout, and how: what a door asks before deciding it.
pub(super) fn layout_write(workspace: &str, body: &[u8]) -> Option<LayoutWrite> {
    // optional-decode: a classifier; a write that does not decode is reported where the governor takes it in (Workspaces::absorb)
    match postcard::from_bytes::<neighbour_wire::NeighbourMsg>(body).ok()? {
        neighbour_wire::NeighbourMsg::SyncResp { room: named, .. } if named == workspace => {
            Some(LayoutWrite::Topology)
        }
        neighbour_wire::NeighbourMsg::AppDocument {
            app_id, record_id, ..
        } if is_desktop(&app_id, &record_id) => Some(LayoutWrite::Desktop),
        _ => None,
    }
}

/// Whether `changes`, a member's batch to the workspace document `before`,
/// leave its shared layout as it stood at the batch's own dependencies.
pub(super) fn leaves_layout(before: &automerge::AutoCommit, changes: &[Vec<u8>]) -> bool {
    let Ok((at, after)) = tilo_peer_state::workspace::membership::position(before, changes) else {
        return false;
    };
    let layout = |doc: &automerge::AutoCommit| {
        serde_json::to_value(tilo_desktop_model::shared_record::read(doc, None)).ok()
    };
    layout(&at).is_some_and(|held| Some(held) == layout(&after))
}

/// Remove every window and window group from `doc`, a shared desktop
/// record, and every strip a person made; the strips the workspace itself has
/// (its operator's, which name no author) stay. Returns whether it changed
/// the record.
#[cfg(any(feature = "agents", test))]
pub(super) fn drop_shared_windows(doc: &mut automerge::AutoCommit) -> Result<bool, String> {
    use automerge::transaction::Transactable as _;
    use automerge::{ObjType, ROOT, ReadDoc as _, Value};
    let mut changed = false;
    for level in ["windows", "groups"] {
        if let Some((Value::Object(ObjType::Map), map)) =
            doc.get(ROOT, level).map_err(|error| error.to_string())?
        {
            let keys: Vec<String> = doc.keys(&map).collect();
            for key in keys {
                doc.delete(&map, key.as_str())
                    .map_err(|error| error.to_string())?;
                changed = true;
            }
        }
    }
    if let Some((Value::Object(ObjType::Map), strips)) =
        doc.get(ROOT, "strips").map_err(|error| error.to_string())?
    {
        let made: Vec<String> = doc
            .keys(&strips)
            .filter(|key| {
                doc.get(&strips, key.as_str())
                    .ok()
                    .flatten()
                    .and_then(|(_, strip)| doc.get(&strip, "author").ok().flatten())
                    .is_some()
            })
            .collect();
        for key in made {
            doc.delete(&strips, key.as_str())
                .map_err(|error| error.to_string())?;
            changed = true;
        }
    }
    Ok(changed)
}

impl Workspaces {
    /// `workspace`'s layout scope, as its governance says.
    #[must_use]
    pub fn layout(&self, workspace: &str) -> WorkspaceLayout {
        self.inner.layouts.get(workspace)
    }

    /// Whether `workspace` takes in none of its members' layout, as a workspace
    /// behind a public door does: its layout is personal.
    #[must_use]
    pub fn is_public(&self, workspace: &str) -> bool {
        self.layout(workspace).is_personal()
    }

    /// Read each workspace's layout scope from its governance as the relay
    /// opens, before any door decides a write by it.
    pub(super) async fn learn_layouts(&self) {
        let workspaces = self.inner.workspaces.lock().await.clone();
        for (workspace, state) in workspaces {
            let layout = state.lock().await.peer().governance().workspace_layout();
            self.inner.layouts.set(&workspace, layout);
        }
    }

    /// Settle every workspace's layout as the relay opens: a workspace listed in the
    /// directory whose governor chose no scope becomes personal (open
    /// admission), and each personal workspace loses what was committed of its
    /// members' windows before it was ([`Self::keep_personal`]). A workspace
    /// that cannot be settled is reported and left to the next start; it
    /// does not keep the others, or the relay, from opening.
    pub(super) async fn settle_layouts(&self) {
        let workspaces: Vec<String> = self.inner.workspaces.lock().await.keys().cloned().collect();
        for workspace in workspaces {
            if self.listing(&workspace).is_some()
                && let Err(error) = self.default_to_personal(&workspace).await
            {
                tracing::warn!(%workspace, %error, "a listed room still shares its layout");
            }
        }
        for workspace in self.inner.layouts.personal() {
            if let Err(error) = self.keep_personal(&workspace).await {
                tracing::warn!(%workspace, %error, "a personal room still holds its members' windows");
            }
        }
    }

    /// Make `workspace`, which just opened a door (a public listing), personal
    /// unless its governor already chose its scope.
    pub(super) async fn default_to_personal(
        &self,
        workspace: &str,
    ) -> Result<(), CollaborationError> {
        let chosen = {
            let state = self.workspace(workspace).await?;
            let governed = state.lock().await;
            governed.peer().governance().workspace_layout_choice()
        };
        if chosen.is_some() {
            return Ok(());
        }
        self.set_layout(workspace, WorkspaceLayout::Personal).await
    }

    /// Set `workspace`'s layout scope to `layout`, as its governor: written into
    /// its governance for every member's device to read, and held to here.
    /// Making it personal removes what the workspace holds of its members'
    /// windows (`Self::keep_personal`); what could not be removed now is
    /// reported, and removed when the relay next opens the workspace.
    ///
    /// A workspace made personal is held to it here at once, even when its
    /// governance cannot be written: the relay never shares more than the
    /// workspace was asked to.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the room stays locked until its decision is committed and settled; whether the lint sees it depends on the features built"
    )]
    pub async fn set_layout(
        &self,
        workspace: &str,
        layout: WorkspaceLayout,
    ) -> Result<(), CollaborationError> {
        let host = &self.inner.host;
        if layout.is_personal() {
            self.inner.layouts.set(workspace, layout);
        }
        {
            let state = self.workspace(workspace).await?;
            let mut governed = state.lock().await;
            let now = SystemTime::now();
            let mut candidate = governed.peer().clone();
            let changed = candidate
                .governance_mut()
                .author_workspace_layout(layout, &host.key, now)
                .map_err(CollaborationError::new)?;
            if changed {
                governed
                    .commit(candidate, &host.key, now, |record, _| {
                        Ok(record.collaboration.clone())
                    })
                    .map_err(CollaborationError::new)?;
                tracing::info!(%workspace, layout = layout.id(), "room layout scope set");
            }
            self.inner.layouts.set(workspace, layout);
            self.bind(&governed)
                .await
                .map_err(CollaborationError::new)?;
            if changed {
                self.settle(&mut governed)
                    .await
                    .map_err(CollaborationError::new)?;
            }
        }
        if layout.is_personal()
            && let Err(error) = self.keep_personal(workspace).await
        {
            tracing::warn!(%workspace, %error, "a personal room still holds its members' windows");
        }
        Ok(())
    }

    /// Remove from `workspace`, whose layout is personal, what its governor
    /// committed of its members' windows while it shared them: from the
    /// workspace document (each entry in a member's topology namespace) and from
    /// the shared desktop record (every window, and the strips people made).
    /// What the workspace itself holds stays. Changes nothing a second time.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the room stays locked until its decision is committed and settled; whether the lint sees it depends on the features built"
    )]
    pub(super) async fn keep_personal(&self, workspace: &str) -> Result<(), CollaborationError> {
        let host = &self.inner.host;
        {
            let state = self.workspace(workspace).await?;
            let mut governed = state.lock().await;
            let now = SystemTime::now();
            let mut candidate = governed.peer().clone();
            let dropped = candidate
                .governance_mut()
                .drop_member_windows(&host.key, now)
                .map_err(CollaborationError::new)?;
            if dropped {
                governed
                    .commit(candidate, &host.key, now, |record, _| {
                        Ok(record.collaboration.clone())
                    })
                    .map_err(CollaborationError::new)?;
                self.settle(&mut governed)
                    .await
                    .map_err(CollaborationError::new)?;
                tracing::info!(%workspace, "removed members' windows from a personal room's document");
            }
        }
        self.clear_shared_desktop(workspace).await
    }

    /// Remove what members put on `workspace`'s shared desktop record
    /// ([`drop_shared_windows`]), as its governor.
    ///
    /// # Errors
    ///
    /// The record could not be read or the change committed.
    #[cfg(feature = "agents")]
    async fn clear_shared_desktop(&self, workspace: &str) -> Result<(), CollaborationError> {
        let cleared = self
            .author_room_record(
                workspace,
                &self.inner.host.cert,
                (DESKTOP_APP, DESKTOP_RECORD),
                drop_shared_windows,
            )
            .await?;
        if cleared {
            tracing::info!(%workspace, "removed members' windows from a personal room's shared desktop");
        }
        Ok(())
    }

    /// Without the browser plane's record authoring this relay keeps no
    /// shared desktop record a member's page wrote: nothing to remove.
    #[cfg(not(feature = "agents"))]
    #[expect(
        clippy::unused_async,
        reason = "one signature with the build that authors room records"
    )]
    async fn clear_shared_desktop(&self, workspace: &str) -> Result<(), CollaborationError> {
        let _ = workspace;
        Ok(())
    }

    /// Decide `author`'s write `body` (a retained message) to `workspace`'s
    /// shared layout: in a workspace whose layout is personal a member's change
    /// to the workspace's topology or shared desktop is refused, and a desktop
    /// write anywhere is decided by what it does to what others made
    /// ([`Self::decide_desktop_write`]). Anything else passes untouched,
    /// and so does what the workspace's governor, this host's own device, writes:
    /// it writes as the workspace's governance writer, whose batches the workspace's
    /// admission takes whatever the layout (`personal_layout_kept`). A
    /// device hosting its person's workspace writes its own windows so.
    ///
    /// # Errors
    /// Why the workspace refuses it.
    pub(crate) async fn decide_layout_write(
        &self,
        workspace: &str,
        author: [u8; 32],
        body: &[u8],
    ) -> Result<(), String> {
        if author == self.inner.host.cert.device_public_key {
            return Ok(());
        }
        let Some(write) = layout_write(workspace, body) else {
            return Ok(());
        };
        if self.is_public(workspace) {
            // A member's change to the workspace's people, their roles and the
            // workspace's grants is no layout: it passes, and the workspace's
            // admission decides it as it decides it everywhere.
            if write == LayoutWrite::Topology && self.only_membership(workspace, body).await {
                return Ok(());
            }
            return Err(LAYOUT_REFUSAL.into());
        }
        match write {
            LayoutWrite::Topology => Ok(()),
            LayoutWrite::Desktop => self.decide_desktop_write(workspace, author, body).await,
        }
    }

    /// Whether `body`, a member's change to `workspace`'s document, leaves the
    /// workspace's shared layout as it was where its writer made it: a change to
    /// its membership alone.
    async fn only_membership(&self, workspace: &str, body: &[u8]) -> bool {
        // optional-decode: only called once layout_write decoded this body
        let Ok(neighbour_wire::NeighbourMsg::SyncResp { changes, .. }) =
            postcard::from_bytes::<neighbour_wire::NeighbourMsg>(body)
        else {
            return false;
        };
        let Ok(state) = self.workspace(workspace).await else {
            return false;
        };
        let governed = state.lock().await;
        governed
            .peer()
            .admitted_workspace()
            .is_some_and(|before| leaves_layout(before, &changes))
    }

    /// Whether the governor of `workspace` takes in the retained write `message` when it replays what the relay retains: never a shared
    /// desktop write in a workspace whose layout is personal, whichever device
    /// wrote it.
    pub(super) fn recalls(
        &self,
        workspace: &GovernedWorkspace,
        message: &neighbour_wire::NeighbourMsg,
    ) -> bool {
        let neighbour_wire::NeighbourMsg::AppDocument {
            app_id, record_id, ..
        } = message
        else {
            return true;
        };
        !is_desktop(app_id, record_id) || !self.is_public(workspace.workspace())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_personal_rooms_shared_desktop_keeps_only_the_rooms_own_strips() {
        use automerge::transaction::Transactable as _;
        use automerge::{ObjType, ROOT, ReadDoc as _};
        let mut doc = automerge::AutoCommit::new();
        let strips = doc.put_object(ROOT, "strips", ObjType::Map).unwrap();
        let operator = doc.put_object(&strips, "1", ObjType::Map).unwrap();
        doc.put(&operator, "name", "Lobby").unwrap();
        let made = doc.put_object(&strips, "2", ObjType::Map).unwrap();
        doc.put(&made, "name", "Mine").unwrap();
        doc.put(&made, "author", "ab".repeat(32)).unwrap();
        let groups = doc.put_object(ROOT, "groups", ObjType::Map).unwrap();
        doc.put_object(&groups, "3", ObjType::Map).unwrap();
        let windows = doc.put_object(ROOT, "windows", ObjType::Map).unwrap();
        let window = doc.put_object(&windows, "4", ObjType::Map).unwrap();
        doc.put(&window, "tab", "{\"title\":\"Alacritty\"}")
            .unwrap();
        doc.commit();

        assert!(drop_shared_windows(&mut doc).unwrap());
        assert_eq!(doc.keys(&strips).collect::<Vec<_>>(), ["1"]);
        assert_eq!(doc.keys(&groups).count(), 0);
        assert_eq!(doc.keys(&windows).count(), 0);
        assert!(!drop_shared_windows(&mut doc).unwrap(), "nothing left");
    }

    #[test]
    fn a_door_knows_a_layout_write_from_any_other() {
        let workspace = "07848ffe-1898-4f72-8419-0b54c93e6dad";
        let topology = postcard::to_stdvec(&neighbour_wire::NeighbourMsg::SyncResp {
            changes: vec![vec![1]],
            room: workspace.into(),
        })
        .unwrap();
        assert_eq!(
            layout_write(workspace, &topology),
            Some(LayoutWrite::Topology)
        );
        assert_eq!(layout_write("another-room", &topology), None);
        assert_eq!(layout_write(workspace, b"not a message"), None);
    }
}
