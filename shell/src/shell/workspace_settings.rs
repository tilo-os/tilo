//! `WorkspaceSettings` — the three-tabbed modal opened from the TopBar's
//! workspace-name button. This module covers the **General** tab: rename the
//! workspace, generate a `quickdesktop://invite/{id}` link, and remove
//! collaborators. Roles/Files tabs land in follow-up tasks (T16/T17).
//!
//! The modal is a retained-mode shell widget: it owns its `ws_id` + active tab
//! + (eventually) per-tab sub-state, and mutates a borrowed `&mut Workspace`
//! when the caller invokes one of its `*_mut`-flavoured methods. Rendering
//! sits on Plan 1's `overlay::Modal` + `FocusTrap` scaffolding so tabbing
//! within the dialog is trapped and the scrim dismisses it.

use quick_ui::overlay::{FocusTrap, Modal};

use crate::shell::state::{Workspace, WorkspaceId};

/// Tab selector for the WorkspaceSettings modal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WsTab {
    General,
    Roles,
    Files,
}

impl WsTab {
    pub fn label(&self) -> &'static str {
        match self {
            WsTab::General => "General",
            WsTab::Roles => "Roles",
            WsTab::Files => "Files",
        }
    }
}

/// The WorkspaceSettings modal state. Retains the target workspace id and
/// currently-active tab; Plan 2 adds `selected_role` + `DataTable` caches for
/// the Roles and Files tabs in T16/T17.
#[derive(Debug)]
pub struct WorkspaceSettings {
    pub ws_id: WorkspaceId,
    pub active_tab: WsTab,
    modal: Modal,
    focus_trap: FocusTrap,
}

impl WorkspaceSettings {
    /// Construct a fresh modal targeting `ws_id` with the General tab
    /// selected and a default focus trap spanning the three tab buttons.
    pub fn new(ws_id: WorkspaceId) -> Self {
        // Tab button ids are stable constants so the caller (shell root) can
        // render them without maintaining a separate id registry.
        let members = vec![
            tab_focus_id(WsTab::General),
            tab_focus_id(WsTab::Roles),
            tab_focus_id(WsTab::Files),
        ];
        Self {
            ws_id,
            active_tab: WsTab::General,
            modal: Modal::new("workspace-settings"),
            focus_trap: FocusTrap::new(members),
        }
    }

    /// The Plan-1 modal scaffold (scrim + id tag) that owns this dialog.
    pub fn modal(&self) -> &Modal {
        &self.modal
    }

    /// The focus trap bounding tabbing to the three header buttons.
    pub fn focus_trap(&self) -> &FocusTrap {
        &self.focus_trap
    }

    /// Switch the active tab.
    pub fn set_tab(&mut self, tab: WsTab) {
        self.active_tab = tab;
    }

    /// Currently-active tab.
    pub fn active_tab(&self) -> WsTab {
        self.active_tab
    }

    /// Rename the given workspace. Caller is expected to pass the same
    /// `Workspace` the modal was opened against (`ws.id == self.ws_id`);
    /// we do not re-check here so the caller can rename any workspace they
    /// still hold a reference to (simplifies the test harness).
    pub fn rename(&mut self, ws: &mut Workspace, new_name: &str) {
        ws.name = new_name.to_string();
    }

    /// Remove the collaborator with the given id from this workspace. No-op
    /// when the id isn't present.
    pub fn remove_collaborator(&mut self, ws: &mut Workspace, collab_id: &str) {
        ws.collaborators.retain(|c| c.id != collab_id);
    }

    /// Return a `quickdesktop://invite/{id}` link for this workspace. The
    /// TopBar's "Invite" button copies this to the clipboard; full P2P
    /// invite handshake lives in Plan 4.
    pub fn invite_link(&self, ws: &Workspace) -> String {
        format!("quickdesktop://invite/{}", u64::from(ws.id))
    }
}

/// Stable focus id for the tab button representing `tab`, used by the
/// dialog's `FocusTrap`. Assigning them via a deterministic function
/// lets tests and the shell agree on the ids without a shared registry.
fn tab_focus_id(tab: WsTab) -> u64 {
    // Reserve a small id range so the modal's trap members can't collide
    // with shell-level focusables.
    const BASE: u64 = 0xFFFF_0000;
    match tab {
        WsTab::General => BASE,
        WsTab::Roles => BASE + 1,
        WsTab::Files => BASE + 2,
    }
}
