//! The profile's workspace as this host keeps it: its own `desktop.json`, or
//! the desktop session's `workspace.json` adopted read only; and what the
//! broker records in it (grants, effect and task records).

use crate::apps;
use std::{
    io,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};
use tilo_action_broker::services::{
    AppActionGrants, LegacyWorkspaceProjection, WorkspaceAuthority,
};
use tilo_desktop_model::{
    DesktopTree, IdMinter, RoomId, Strip, StripId, Workspace, WorkspaceEffectReceipt, WorkspaceId,
};
use tilo_identity_host::broker::WorkspaceMembership;

/// The desktop session's workspace file. Read, never written, here: its
/// native authority fields belong to that host.
pub(super) const DESKTOP_WORKSPACE: &str = "workspace.json";
/// This host's own workspace file, for a profile the desktop never served.
pub(super) const STANDALONE_WORKSPACE: &str = "desktop.json";
/// The broker's durable invocation journal: a retried invocation id never
/// starts twice, across restarts too.
pub(super) const INVOCATION_JOURNAL: &str = "invocations";
/// The profile's committed workspace tree.
pub(super) struct ProfileWorkspace {
    root: PathBuf,
    tree: RwLock<DesktopTree>,
    /// Where this host keeps the tree; `None` when the desktop session's
    /// file is authoritative and read only.
    path: Option<PathBuf>,
}

impl ProfileWorkspace {
    /// The desktop session's tree when that host served this profile,
    /// otherwise this host's own, created on first start.
    pub(super) fn open(root: &Path) -> io::Result<Self> {
        let desktop = root.join(DESKTOP_WORKSPACE);
        if desktop.exists() {
            // The desktop session versions this file by its native layout,
            // which only adds fields this host ignores; it is read only here.
            return Ok(Self {
                root: root.to_path_buf(),
                tree: RwLock::new(parse_tree(&std::fs::read(&desktop)?)?),
                path: None,
            });
        }
        let path = root.join(STANDALONE_WORKSPACE);
        let tree = match read_tree(&path) {
            Ok(tree) => tree,
            Err(error) if error.kind() == io::ErrorKind::NotFound => bootstrap(),
            Err(error) => return Err(error),
        };
        if tree.workspaces.is_empty() {
            return Err(io::Error::other("runtime profile has no workspace"));
        }
        Ok(Self {
            root: root.to_path_buf(),
            tree: RwLock::new(tree),
            path: Some(path),
        })
    }

    pub(super) fn room(&self) -> String {
        self.tree
            .read()
            .map(|tree| {
                tree.workspaces
                    .first()
                    .map(|workspace| workspace.room().to_string())
            })
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    /// The workspace's name, as people see it.
    pub(super) fn name(&self) -> String {
        self.tree
            .read()
            .ok()
            .and_then(|tree| {
                tree.workspaces
                    .first()
                    .map(|workspace| workspace.name.clone())
            })
            .unwrap_or_default()
    }

    /// Give an unowned workspace to `actor` and persist this host's tree.
    pub(super) fn adopt(&self, actor: &str) -> io::Result<()> {
        let mut tree = self
            .tree
            .write()
            .map_err(|_| io::Error::other("runtime workspace lock poisoned"))?;
        let workspace = tree
            .workspaces
            .first_mut()
            .ok_or_else(|| io::Error::other("runtime profile has no workspace"))?;
        if workspace.owner().is_none() {
            if self.path.is_none() {
                return Err(io::Error::other(
                    "the desktop session's runtime workspace has no owner",
                ));
            }
            workspace.set_owner(actor.to_owned());
        }
        let Some(path) = &self.path else {
            return Ok(());
        };
        let bytes = serde_json::to_vec_pretty(&*tree).map_err(io::Error::other)?;
        drop(tree);
        tilo_files::native::atomic::write_with_mode(path, &bytes, 0o600)
    }

    /// Whether this host governs the tree: it keeps its own file, rather
    /// than reading the desktop session's.
    pub(super) fn governs(&self) -> bool {
        self.path.is_some()
    }

    /// Keep the broker's already redacted `receipt` in the workspace of
    /// `room`; a retried invocation keeps its first receipt.
    pub(super) fn keep_effect_receipt(
        &self,
        room: &str,
        receipt: WorkspaceEffectReceipt,
    ) -> Result<(), String> {
        self.record(room, |tree, workspace| {
            let workspace = tree
                .workspaces
                .iter_mut()
                .find(|candidate| candidate.id == workspace)?;
            if !workspace
                .effect_receipts
                .iter()
                .any(|kept| kept.invocation == receipt.invocation)
            {
                workspace.effect_receipts.push(receipt);
            }
            Some(())
        })
    }

    /// Apply `change` to this host's own tree and persist it. `None`, with
    /// the tree unchanged, when this host does not govern it, `change`
    /// declines, or the result cannot be saved.
    fn update<R>(&self, change: impl FnOnce(&mut DesktopTree) -> Option<R>) -> Option<R> {
        let path = self.path.as_ref()?;
        let mut tree = self.tree.write().ok()?;
        let before = tree.clone();
        let result = change(&mut tree)?;
        let saved = serde_json::to_vec_pretty(&*tree)
            .map_err(io::Error::other)
            .and_then(|bytes| tilo_files::native::atomic::write_with_mode(path, &bytes, 0o600));
        if let Err(error) = saved {
            *tree = before;
            drop(tree);
            tracing::warn!(%error, "runtime workspace change not saved; undone");
            return None;
        }
        drop(tree);
        Some(result)
    }

    /// Apply `change` to the workspace of `room` and persist it, refused
    /// when this host does not govern the tree.
    fn record(
        &self,
        room: &str,
        change: impl FnOnce(&mut DesktopTree, WorkspaceId) -> Option<()>,
    ) -> Result<(), String> {
        if !self.governs() {
            return Err("the desktop session's workspace is read only in the runtime".into());
        }
        self.update(|tree| {
            let workspace = tree
                .workspaces
                .iter()
                .find(|workspace| workspace.room().to_string() == room)?
                .id;
            change(tree, workspace)
        })
        .ok_or_else(|| format!("room {room} has no such record target, or it was not saved"))
    }
}

/// Grants people give applications for other apps' actions, recorded in
/// this host's own workspace by the person who gave them and noted in its
/// history. A profile adopted from the desktop session is that host's to
/// govern: nothing is granted or written here.
pub(super) struct ProfileGrants {
    pub(super) workspace: Arc<ProfileWorkspace>,
    pub(super) apps: Arc<apps::Apps>,
}

impl AppActionGrants for ProfileGrants {
    fn may_grant(&self, room: &str, person: &str) -> bool {
        self.workspace.governs()
            && self.workspace.tree.read().is_ok_and(|tree| {
                tree.workspaces
                    .iter()
                    .find(|workspace| workspace.room().to_string() == room)
                    .is_some_and(|workspace| {
                        workspace.may_govern_at(
                            person,
                            tilo_action_policy::roles::governance::GRANT,
                            tilo_clock::unix_now(),
                        )
                    })
            })
    }

    fn record(
        &self,
        request: &tilo_action_broker::app_actions::AppActionConsentRequest,
        grant: tilo_action_policy::Grant,
        answer: tilo_action_broker::app_actions::AppActionConsent,
    ) -> Option<String> {
        if !self.workspace.governs() {
            return None;
        }
        let context = tilo_desktop_model::grants::app_action_grant_context(
            &request.room,
            &self.apps.display_name(&request.app_id),
            &self.apps.display_name(&request.target_app),
            &request.action_label,
            answer.lifetime(),
        );
        let now = tilo_clock::unix_now();
        self.workspace.update(|tree| {
            let workspace = tree
                .workspaces
                .iter()
                .find(|workspace| workspace.room().to_string() == request.room)?
                .id;
            let stored = tree
                .grant_in_workspace_at(workspace, &request.person, grant, now)
                .ok()?;
            tree.append_workspace_effect_receipt(
                workspace,
                tilo_desktop_model::WorkspaceEffectReceipt::recorded(
                    tilo_actions::InvocationId::new(),
                    &request.person,
                    context,
                    tilo_actions::EffectReceiptStatus::Succeeded,
                    i64::try_from(now).unwrap_or(i64::MAX),
                ),
            );
            Some(stored.id.as_str().to_owned())
        })
    }

    fn revoke(&self, room: &str, grant: &str) {
        self.workspace.update(|tree| {
            tree.workspaces
                .iter_mut()
                .find(|workspace| workspace.room().to_string() == room)?
                .revoke_grant(&tilo_action_policy::GrantId::new(grant))
                .then_some(())
        });
    }
}

/// Effect and task records the broker keeps in this host's own workspace.
/// A profile adopted from the desktop session refuses them: its file is
/// that host's to write.
impl LegacyWorkspaceProjection for ProfileWorkspace {
    fn record_effect(
        &self,
        room: &str,
        invocation: tilo_actions::InvocationId,
        actor: &str,
        context: tilo_actions::ActionContext,
        status: tilo_actions::EffectReceiptStatus,
    ) -> Result<(), String> {
        let completed_at = tilo_clock::unix_now_i64();
        self.record(room, |tree, workspace| {
            // A retried invocation keeps its first receipt.
            tree.append_workspace_effect_receipt(
                workspace,
                tilo_desktop_model::WorkspaceEffectReceipt::redacted(
                    invocation,
                    actor,
                    context,
                    status,
                    completed_at,
                ),
            );
            Some(())
        })
    }
}

impl WorkspaceAuthority for ProfileWorkspace {
    fn with_tree(&self, read: &mut dyn FnMut(&DesktopTree)) -> Result<(), String> {
        let tree = self
            .tree
            .read()
            .map_err(|_| "runtime workspace lock poisoned".to_owned())?;
        read(&tree);
        Ok(())
    }

    /// This host's own journal, also for an adopted profile: the desktop
    /// session's files stay untouched.
    fn legacy_journal_root(&self) -> Option<PathBuf> {
        Some(self.root.join(INVOCATION_JOURNAL))
    }
}

impl WorkspaceMembership for ProfileWorkspace {
    fn with_workspace(&self, room: &RoomId, read: &mut dyn FnMut(&Workspace)) {
        if let Ok(tree) = self.tree.read()
            && let Some(workspace) = tree
                .workspaces
                .iter()
                .find(|workspace| &workspace.room() == room)
        {
            read(workspace);
        }
    }
}

fn parse_tree(bytes: &[u8]) -> io::Result<DesktopTree> {
    serde_json::from_slice(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

pub(super) fn read_tree(path: &Path) -> io::Result<DesktopTree> {
    let tree = parse_tree(&std::fs::read(path)?)?;
    if tree.schema_version > tilo_desktop_model::STATE_SCHEMA_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "workspace layout {} is newer than the {} this build understands",
                tree.schema_version,
                tilo_desktop_model::STATE_SCHEMA_VERSION
            ),
        ));
    }
    Ok(tree)
}

/// One empty home workspace with one strip, as a new desktop starts.
pub(super) fn bootstrap() -> DesktopTree {
    let mut ids = IdMinter::default();
    let workspace = WorkspaceId(ids.mint());
    let strip = StripId(ids.mint());
    let home = {
        let mut workspace = Workspace::new(workspace, "Home");
        workspace.strips = vec![Strip {
            id: strip,
            name: "Main".into(),
            window_groups: Vec::new(),
        }];
        workspace
    };
    DesktopTree {
        workspaces: vec![home],
        active: workspace,
        active_strip: std::iter::once((workspace, strip)).collect(),
        ..DesktopTree::default()
    }
}
