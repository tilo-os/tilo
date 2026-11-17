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
struct Directory;

impl domain::AppDirectory for Directory {
    fn app_display_name(&self, app_id: &str) -> Option<String> {
        crate::app_names::listed_name(app_id)
    }
}
