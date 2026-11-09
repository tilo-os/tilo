//! A host the crate's tests can drive: every change allowed, and every window a notice, so a test
//! looks at the desk's layers rather than at an application.

use tilo_desktop_model::commands::DesktopCommand;
use tilo_desktop_model::{DesktopTree, Tab, Workspace};
use tilo_desktop_surface::{DesktopHandle, TileSource};

use crate::host_contract::DesktopRules;

/// The rules of a host whose person may change anything on the desk it shows, for the crate's
/// tests: the desk is `handle`'s tree and every change is allowed.
#[doc(hidden)]
pub struct PermissiveRules {
    pub handle: DesktopHandle,
}

impl PermissiveRules {
    #[must_use]
    pub fn new(handle: DesktopHandle) -> Self {
        Self { handle }
    }
}

impl DesktopRules for PermissiveRules {
    fn tree(&self) -> DesktopTree {
        self.handle.read(Clone::clone)
    }

    fn author(&self) -> String {
        "Ada".into()
    }

    fn may(&self, _: &DesktopCommand) -> Result<(), String> {
        Ok(())
    }

    fn may_create_workspace(&self) -> Result<(), String> {
        Ok(())
    }
}

/// A host that draws every window as a notice, for the crate's tests: a window's title over no
/// content, so the desk's layers are what a test looks at.
#[doc(hidden)]
pub struct NoticeTiles;

impl TileSource for NoticeTiles {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> ui::widget::TileHost {
        ui::widget::TileHost::new(ui::widget::TileContent::Notice {
            title: tab.title.clone(),
            detail: String::new(),
        })
    }
}
