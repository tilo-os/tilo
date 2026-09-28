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
    ) -> Result<
        (
            tilo_desktop_model::WorkspaceId,
            tilo_desktop_model::StripId,
            u64,
            u64,
        ),
        String,
    > {
        fn id(value: &str) -> Result<u64, String> {
            value
                .parse::<u64>()
                .ok()
                .filter(|id| id.to_string() == value)
                .ok_or_else(|| "Invalid navigation identity".into())
        }
        Ok((
            tilo_desktop_model::WorkspaceId(id(&self.workspace)?),
            tilo_desktop_model::StripId(id(&self.strip)?),
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
        view: tilo_desktop::activity::ActivityView,
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
    /// Open an installed app in a new window of the active workspace, as
    /// the launcher's `desktop:requests#open-<app>` does; on a local desktop,
    /// with `record`, on that existing record of the app.
    Open {
        generation: String,
        app: String,
        #[serde(default)]
        record: Option<String>,
    },
    /// Show the active workspace's settings on `tab`, as the top bar does.
    WorkspaceSettings {
        generation: String,
        workspace: String,
        tab: SettingsTab,
    },
    CloseWorkspaceSettings {
        generation: String,
        workspace: String,
    },
}
/// A tab of the workspace settings.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum SettingsTab {
    General,
    Members,
    Agents,
    Permissions,
    Apps,
}
impl From<SettingsTab> for tilo_desktop::workspace_settings::WsTab {
    fn from(tab: SettingsTab) -> Self {
        match tab {
            SettingsTab::General => Self::General,
            SettingsTab::Members => Self::Members,
            SettingsTab::Agents => Self::Agents,
            SettingsTab::Permissions => Self::Permissions,
            SettingsTab::Apps => Self::Apps,
        }
    }
}
impl From<tilo_desktop::workspace_settings::WsTab> for SettingsTab {
    fn from(tab: tilo_desktop::workspace_settings::WsTab) -> Self {
        use tilo_desktop::workspace_settings::WsTab;
        match tab {
            WsTab::General => Self::General,
            WsTab::Members => Self::Members,
            WsTab::Agents => Self::Agents,
            WsTab::Permissions => Self::Permissions,
            WsTab::Apps => Self::Apps,
        }
    }
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
            | Self::Scroll { generation, .. }
            | Self::Open { generation, .. }
            | Self::WorkspaceSettings { generation, .. }
            | Self::CloseWorkspaceSettings { generation, .. } => generation,
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
    fn an_open_may_name_an_existing_record() {
        let open = |wire: &str| {
            serde_json::from_str::<Command>(wire).map(|command| match command {
                Command::Open { app, record, .. } => (app, record),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            open(r#"{"kind":"open","generation":"g","app":"so.tilo.ebook"}"#).unwrap(),
            ("so.tilo.ebook".into(), None)
        );
        assert_eq!(
            open(r#"{"kind":"open","generation":"g","app":"so.tilo.ebook","record":"uuid:1"}"#)
                .unwrap()
                .1
                .as_deref(),
            Some("uuid:1")
        );
        assert!(
            open(r#"{"kind":"open","generation":"g","app":"so.tilo.ebook","record":7}"#).is_err()
        );
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
