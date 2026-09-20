//! Viewer-local workspace requests. These never carry session mutation authority.
use super::ActivityView;
use crate::{WorkspaceId, WorkspaceTree};

pub const RESOURCE_ID: &str = "desktop:view";
pub const OPERATION_TYPE: &str = "qos.desktop.view";

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
    pub fn validate(&self, tree: &WorkspaceTree) -> Result<(), String> {
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
    tree: &WorkspaceTree,
    activity_available: bool,
) -> Vec<WorkspaceViewDescriptor> {
    let mut result = tree
        .workspaces
        .iter()
        .map(|workspace| WorkspaceViewDescriptor {
            id: format!("workspace.select.{}", workspace.id.0),
            label: format!("Switch to {}", workspace.name),
            request: WorkspaceViewRequest::Select {
                workspace: workspace.id,
            },
        })
        .collect::<Vec<_>>();
    if activity_available && let Some(workspace) = tree.active_workspace() {
        for (id, label, view) in [
            (
                "room.activity.open",
                "Review workspace activity",
                ActivityView::CatchUp,
            ),
            (
                "room.activity.explore",
                "Explore workspace activity",
                ActivityView::Explore,
            ),
            (
                "room.activity.replay",
                "Replay workspace activity",
                ActivityView::Replay,
            ),
        ] {
            result.push(WorkspaceViewDescriptor {
                id: id.into(),
                label: label.into(),
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
    use crate::WorkspaceId;
    use serde::{Deserialize, Deserializer, Serializer};
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
