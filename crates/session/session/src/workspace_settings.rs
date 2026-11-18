//! Workspace settings: what the sheet shows of the workspace in front, and what the person's
//! choices there change, on a computer and on a page, whose Agents tab is [`PageAgents`].

use tilo_desktop::workspace_settings as domain;

mod agents;
#[cfg(target_os = "linux")]
mod emoji;
mod page;
#[cfg(target_os = "linux")]
mod shell;

pub use agents::Host as AgentsHost;
pub use page::PageAgents;
#[cfg(target_os = "linux")]
pub(crate) use shell::*;

/// This machine's applications, as the workspace settings name them.
#[derive(Default)]
pub(crate) struct Directory {
    /// What the apps publish, where this device opens any: the catalogue that names their actions
    /// as the sheet asking to allow one does.
    actions: Option<std::rc::Rc<dyn crate::search::AppActions>>,
    /// The workspace whose published actions name what its grants are for.
    workspace: String,
}

impl domain::AppDirectory for Directory {
    fn app_display_name(&self, app_id: &str) -> Option<String> {
        crate::app_names::listed_name(app_id)
    }

    fn action_display_name(&self, app_id: &str, action: &str) -> Option<String> {
        self.actions
            .as_ref()?
            .published(&self.workspace)
            .into_iter()
            .filter(|published| published.app == app_id)
            .flat_map(|published| published.actions)
            .find(|descriptor| descriptor.address.action == action)
            .map(|descriptor| descriptor.action_label)
            .filter(|label| !label.trim().is_empty())
    }

    /// The packages installed on this computer; else, and on a page, which keeps none of its own,
    /// the fleet's apps that run here: on a page those its workspace's server serves it.
    fn installed_apps(&self) -> Vec<String> {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(lists) = crate::app_icons::app_lists()
            && !lists.named.is_empty()
        {
            return lists.named.iter().map(|app| app.id.clone()).collect();
        }
        let here: &[&str] = if cfg!(target_arch = "wasm32") {
            &["web"]
        } else {
            &["linux", "macos", "tilo-os"]
        };
        tilo_desktop_search::availability::catalog()
            .iter()
            .filter(|app| app.platforms.iter().any(|platform| here.contains(platform)))
            .map(|app| app.id.to_owned())
            .collect()
    }
}
