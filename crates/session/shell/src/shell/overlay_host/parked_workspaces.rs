//! The content of workspaces shown recently, kept while another one is on
//! screen so that switching back puts it back instead of building it again.
//!
//! Building a workspace's content from nothing mounts every app on it afresh
//! (each opens its record and runs its first sync and update) and draws every
//! widget for the first time, with no text laid out and no scene cached. On
//! the VM a switch back to a workspace holding Notes and a Terminal spent 84 ms
//! of the frame loop's CPU in one tick doing that. A kept workspace comes back
//! as it was left: its apps still mounted, its widgets' caches warm, its
//! strips' scroll where the person left them; only what changed in the tree
//! while it was away is reconciled.
//!
//! Kept content is not laid out or updated, and nothing routes input to it.
//! It is drawn only as it was laid out: leaving the screen in a workspace
//! slide (`workspace_slide`) and into a window's picture in picture. Its apps stay mounted, as they would on screen, until the content is
//! dropped: when more than [`KEPT_WORKSPACES`] are kept, when its workspace is
//! removed or becomes another room, or when one of its tabs moves to another
//! workspace (which then mounts the tab itself).

use std::collections::HashMap;

use tilo_desktop::composition::DesktopSurface;
use ui::application::ComponentCloseReport;
use ui::widget::Widget;

use crate::shell::state::DesktopTree;

/// How many workspaces' content is kept besides the one on screen. Switching
/// between a few workspaces is the gesture this serves; each kept workspace
/// keeps its apps mounted, so the bound is what a person switching through
/// many workspaces pays in memory.
pub(super) const KEPT_WORKSPACES: usize = 4;

/// Content kept for workspaces that are not on screen.
#[derive(Default)]
pub(super) struct ParkedWorkspaces {
    /// Least recently shown first.
    entries: Vec<Parked>,
}

struct Parked {
    workspace: u64,
    content: Box<dyn Widget>,
}

fn surface(content: &dyn Widget) -> Option<&DesktopSurface> {
    content.as_any()?.downcast_ref()
}

/// Close the components of `contents` and hand back their recovery reports,
/// as dropping a workspace's content on screen does.
pub(super) fn close(contents: Vec<Box<dyn Widget>>) -> Vec<ComponentCloseReport> {
    let mut reports = Vec::new();
    for mut content in contents {
        reports.extend(ui::component_lifecycle::close_components(content.as_mut()));
        reports.extend(ui::component_lifecycle::take_component_close_reports(
            content.as_mut(),
        ));
    }
    reports
}

impl ParkedWorkspaces {
    /// Keep `content`, which was on screen for `workspace` until now.
    /// Returns what it no longer keeps to stay within [`KEPT_WORKSPACES`],
    /// for the caller to [`close`].
    pub(super) fn park(
        &mut self,
        workspace: u64,
        content: Box<dyn Widget>,
    ) -> Vec<Box<dyn Widget>> {
        let mut dropped: Vec<Box<dyn Widget>> = Vec::new();
        if let Some(index) = self.entries.iter().position(|e| e.workspace == workspace) {
            dropped.push(self.entries.remove(index).content);
        }
        self.entries.push(Parked { workspace, content });
        while self.entries.len() > KEPT_WORKSPACES {
            dropped.push(self.entries.remove(0).content);
        }
        dropped
    }

    /// The content kept for `workspace`, no longer kept. Call
    /// [`Self::stale`] first: it drops content `desktop` has moved on from.
    pub(super) fn take(&mut self, workspace: u64) -> Option<Box<dyn Widget>> {
        let index = self.entries.iter().position(|e| e.workspace == workspace)?;
        Some(self.entries.remove(index).content)
    }

    /// The content kept for `workspace`, still kept.
    pub(super) fn content_mut(&mut self, workspace: u64) -> Option<&mut (dyn Widget + 'static)> {
        self.entries
            .iter_mut()
            .find(|e| e.workspace == workspace)
            .map(|entry| entry.content.as_mut())
    }

    /// Remove and return the content `desktop` has moved on from: its
    /// workspace is gone or is another room now, or (with `moved_tabs`, which
    /// walks every tab) one of its tabs lives in another workspace. Without
    /// `moved_tabs` it is cheap enough for every frame.
    pub(super) fn stale(
        &mut self,
        desktop: &DesktopTree,
        moved_tabs: bool,
    ) -> Vec<Box<dyn Widget>> {
        if self.entries.is_empty() {
            return Vec::new();
        }
        let homes: HashMap<u64, tilo_desktop_model::WorkspaceId> = if moved_tabs {
            desktop
                .workspaces
                .iter()
                .flat_map(|workspace| {
                    workspace
                        .strips
                        .iter()
                        .flat_map(|strip| &strip.window_groups)
                        .flat_map(|group| &group.tabs)
                        .map(move |tab| (tab.id, workspace.id))
                })
                .collect()
        } else {
            HashMap::new()
        };
        let keeps = |entry: &Parked| {
            let Some(surface) = surface(entry.content.as_ref()) else {
                return false;
            };
            let Some((id, room)) = surface.workspace_context() else {
                return false;
            };
            id.raw() == entry.workspace
                && desktop
                    .workspaces
                    .iter()
                    .any(|workspace| workspace.id == id && workspace.room() == room)
                && surface
                    .tab_ids()
                    .all(|tab| homes.get(&tab).is_none_or(|home| *home == id))
        };
        let (kept, stale): (Vec<Parked>, Vec<Parked>) = std::mem::take(&mut self.entries)
            .into_iter()
            .partition(keeps);
        self.entries = kept;
        stale.into_iter().map(|entry| entry.content).collect()
    }

    /// All the kept content, no longer kept.
    pub(super) fn drain(&mut self) -> Vec<Box<dyn Widget>> {
        std::mem::take(&mut self.entries)
            .into_iter()
            .map(|entry| entry.content)
            .collect()
    }

    /// The kept content, for the session's close lifecycle.
    pub(super) fn contents(&self) -> impl Iterator<Item = &dyn Widget> {
        self.entries.iter().map(|entry| entry.content.as_ref())
    }

    /// The kept content, for the session's close lifecycle and for painting
    /// a window kept off screen into its picture in picture.
    pub(super) fn contents_mut(&mut self) -> impl Iterator<Item = &mut (dyn Widget + 'static)> {
        self.entries.iter_mut().map(|entry| entry.content.as_mut())
    }

    /// The workspaces whose content is kept, least recently shown first.
    #[cfg(test)]
    pub(super) fn workspaces(&self) -> Vec<u64> {
        self.entries.iter().map(|entry| entry.workspace).collect()
    }
}
