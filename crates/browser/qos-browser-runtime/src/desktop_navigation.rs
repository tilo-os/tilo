//! Versioned per-surface local presentation commands. IDs never cross JS numbers.
use serde::{Deserialize, Serialize};
use ui::widget::PrimaryScrollAction;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Route {
    pub workspace: String,
    pub strip: String,
    pub group: String,
    pub tab: String,
}
impl Route {
    pub fn ids(
        &self,
    ) -> Result<(qos_desktop::WorkspaceId, qos_desktop::StripId, u64, u64), String> {
        fn id(value: &str) -> Result<u64, String> {
            value
                .parse::<u64>()
                .ok()
                .filter(|id| id.to_string() == value)
                .ok_or_else(|| "Invalid navigation identity".into())
        }
        Ok((
            qos_desktop::WorkspaceId(id(&self.workspace)?),
            qos_desktop::StripId(id(&self.strip)?),
            id(&self.group)?,
            id(&self.tab)?,
        ))
    }
}
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) enum Command {
    Close {
        generation: String,
        route: Route,
    },
    Workspace {
        generation: String,
        workspace: String,
    },
    WorkspaceActivity {
        generation: String,
        workspace: String,
        view: qos_desktop::activity::ActivityView,
    },
    CloseWorkspaceActivity {
        generation: String,
        workspace: String,
    },
    History {
        generation: String,
        route: Route,
        target: String,
    },
    Select {
        generation: String,
        route: Route,
    },
    Strip {
        generation: String,
        workspace: String,
        strip: String,
    },
    Pan {
        generation: String,
        workspace: String,
        strip: String,
        offset: f64,
    },
    Width {
        generation: String,
        route: Route,
        permille: u16,
    },
    Maximize {
        generation: String,
        route: Route,
        maximized: bool,
    },
    Scroll {
        generation: String,
        region: String,
        action: ScrollAction,
    },
}
impl Command {
    pub fn generation(&self) -> &str {
        match self {
            Self::Close { generation, .. }
            | Self::Workspace { generation, .. }
            | Self::WorkspaceActivity { generation, .. }
            | Self::CloseWorkspaceActivity { generation, .. }
            | Self::Pan { generation, .. }
            | Self::History { generation, .. }
            | Self::Select { generation, .. }
            | Self::Strip { generation, .. }
            | Self::Width { generation, .. }
            | Self::Maximize { generation, .. }
            | Self::Scroll { generation, .. } => generation,
        }
    }
}
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub(crate) enum ScrollAction {
    Line(i8),
    Page(i8),
    Absolute(f64),
    Start,
    End,
}
impl From<ScrollAction> for PrimaryScrollAction {
    fn from(value: ScrollAction) -> Self {
        match value {
            ScrollAction::Line(v) => Self::Line(v),
            ScrollAction::Page(v) => Self::Page(v),
            ScrollAction::Absolute(v) => Self::Absolute(v),
            ScrollAction::Start => Self::Start,
            ScrollAction::End => Self::End,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn workspace_wire_commands_are_additive_and_generation_bound() {
        for kind in [
            "workspace",
            "workspace-activity",
            "close-workspace-activity",
        ] {
            let mut wire = serde_json::json!({"kind":kind,"generation":"mount-1","workspace":"9007199254740993"});
            if kind == "workspace-activity" {
                wire["view"] = "catch-up".into();
            }
            let command: Command =
                serde_json::from_value(wire.clone()).expect("new workspace command");
            assert_eq!(command.generation(), "mount-1");
            wire["workspace"] = 1.into();
            assert!(serde_json::from_value::<Command>(wire).is_err());
        }
        assert!(serde_json::from_str::<Command>(r#"{"kind":"workspace-activity","generation":"mount-1","workspace":"1","view":"catchUp"}"#).is_err());
    }
    #[test]
    fn route_round_trips_full_u64_and_rejects_numeric_ids() {
        let route = Route {
            workspace: u64::MAX.to_string(),
            strip: "9007199254740993".into(),
            group: "3".into(),
            tab: "4".into(),
        };
        assert_eq!(route.ids().unwrap().0.0, u64::MAX);
        assert_eq!(route.ids().unwrap().1.0, 9_007_199_254_740_993);
        assert_eq!(
            serde_json::from_str::<Route>(&serde_json::to_string(&route).unwrap()).unwrap(),
            route
        );
        assert!(
            serde_json::from_str::<Route>(r#"{"workspace":1,"strip":"2","group":"3","tab":"4"}"#)
                .is_err()
        );
    }
}
