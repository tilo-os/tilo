//! Viewer-local workspace requests. These never carry session mutation authority.
use super::ActivityView;
use tilo_desktop_model::{DesktopTree, WorkspaceId};

pub const RESOURCE_ID: &str = "desktop:view";
pub const OPERATION_TYPE: &str = "tilo.desktop.view";

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "request", rename_all = "camelCase")]
pub enum WorkspaceViewRequest {
    Select {
        #[serde(with = "workspace_string")]
        workspace: WorkspaceId,
    },
    OpenActivity {
        #[serde(with = "workspace_string")]
        workspace: WorkspaceId,
        view: ActivityView,
    },
    CloseActivity {
        #[serde(with = "workspace_string")]
        workspace: WorkspaceId,
    },
}
impl WorkspaceViewRequest {
    pub fn workspace(&self) -> WorkspaceId {
        match *self {
            Self::Select { workspace }
            | Self::OpenActivity { workspace, .. }
            | Self::CloseActivity { workspace } => workspace,
        }
    }
    /// Check that the requested workspace is present in `tree`.
    pub fn validate(&self, tree: &DesktopTree) -> Result<(), String> {
        tree.workspaces
            .iter()
            .any(|w| w.id == self.workspace())
            .then_some(())
            .ok_or_else(|| "Workspace unavailable".into())
    }
}
#[derive(Clone, Debug)]
pub struct WorkspaceViewDescriptor {
    pub id: String,
    pub label: String,
    pub request: WorkspaceViewRequest,
}
impl WorkspaceViewDescriptor {
    /// The view operation this descriptor publishes.
    ///
    /// # Panics
    ///
    /// Never in practice: the request is plain ids and enums, which always encode.
    #[expect(
        clippy::expect_used,
        reason = "the request is plain ids and enums, which always encode; hosts take the spec directly"
    )]
    pub fn action(&self) -> tilo_actions::ActionSpec {
        tilo_actions::ActionSpec::operation(
            &self.id,
            &self.label,
            tilo_actions::OperationKind::View,
            OPERATION_TYPE,
            &self.request,
        )
        .expect("workspace view request encodes")
    }
}
pub fn workspace_view_descriptors(
    tree: &DesktopTree,
    activity_available: bool,
) -> Vec<WorkspaceViewDescriptor> {
    let mut result = tree
        .workspaces
        .iter()
        .map(|workspace| WorkspaceViewDescriptor {
            // Named by the room the workspace is, the same on every device:
            // the local number is only a handle.
            id: format!("workspace.select.{}", workspace.room()),
            label: crate::i18n::t_args(
                "desktop-view-switch-workspace",
                &[("workspace", workspace.name.as_str().into())],
            ),
            request: WorkspaceViewRequest::Select {
                workspace: workspace.id,
            },
        })
        .collect::<Vec<_>>();
    if activity_available && let Some(workspace) = tree.active_workspace() {
        for (id, label, view) in [
            (
                "room.activity.open",
                crate::i18n::t("desktop-view-review-activity"),
                ActivityView::CatchUp,
            ),
            (
                "room.activity.explore",
                crate::i18n::t("desktop-view-explore-activity"),
                ActivityView::Explore,
            ),
            (
                "room.activity.replay",
                crate::i18n::t("desktop-view-replay-activity"),
                ActivityView::Replay,
            ),
        ] {
            result.push(WorkspaceViewDescriptor {
                id: id.into(),
                label,
                request: WorkspaceViewRequest::OpenActivity {
                    workspace: workspace.id,
                    view,
                },
            });
        }
    }
    result
}
mod workspace_string {
    use serde::{Deserialize, Deserializer, Serializer};
    use tilo_desktop_model::WorkspaceId;
    #[allow(
        clippy::trivially_copy_pass_by_ref,
        reason = "serde `with` modules receive the field by reference; the lint's size limit, and so whether it fires, depends on the target"
    )]
    pub fn serialize<S: Serializer>(id: &WorkspaceId, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&id.0.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<WorkspaceId, D::Error> {
        let value = String::deserialize(deserializer)?;
        let id = value.parse::<u64>().map_err(serde::de::Error::custom)?;
        if id.to_string() != value {
            return Err(serde::de::Error::custom("invalid opaque workspace id"));
        }
        Ok(WorkspaceId(id))
    }
}
