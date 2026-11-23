//! What the page waits on, and what it last reported of itself.
//!
//! A page asks its person questions (a dialog, a password, a permission, a held request) and
//! reports its own state (its title, how far it has loaded, its zoom, a context menu). The
//! engine's events carry all of it; this keeps the latest of each, so the app's actions read one
//! answer instead of every frame's events. What a question's answer names (an id) is kept with
//! it, so the answer goes to the question the person saw and no other.

use tilo_webview::{
    ContextMenuItem, HeldRequest, HttpAuthChallenge, PageDialog, WebViewEvent,
};

/// One download the page started, as its last update said.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Download {
    pub id: u32,
    /// `awaiting`, `downloading`, `completed`, `failed` or `cancelled`.
    pub state: String,
    pub name: String,
    pub url: String,
    pub path: String,
    pub retryable: bool,
    pub received: u64,
    pub total: u64,
    pub error: String,
}

/// A context menu the page showed, where it was shown, and what it offers.
#[derive(Debug, Clone, PartialEq)]
pub struct Menu {
    pub id: u32,
    pub x: f64,
    pub y: f64,
    pub link: String,
    pub items: Vec<ContextMenuItem>,
}

/// An ask a person answers: the id that names the question, and what it asks.
pub type Permission = (u32, String, String);

/// What the page waits on and last reported, as the app reads it.
#[derive(Debug, Clone, Default)]
pub struct Asks {
    pub dialog: Option<PageDialog>,
    pub sign_in: Option<HttpAuthChallenge>,
    pub permission: Option<Permission>,
    pub held: Vec<HeldRequest>,
    pub menu: Option<Menu>,
    /// The find bar's count and the place it is at, from its last answer.
    pub find: Option<(i32, i32)>,
    pub zoom: Option<f64>,
    pub fullscreen: bool,
    pub progress: f64,
    pub title: String,
    pub loading: bool,
    pub downloads: Vec<Download>,
}

impl Asks {
    /// Take what `event` says of the page. An event that was answered or closed clears what it
    /// asked.
    pub fn apply(&mut self, event: &WebViewEvent) {
        match event {
            WebViewEvent::PageDialog(dialog) => self.dialog = Some(dialog.clone()),
            WebViewEvent::PageDialogClosed { id } => {
                if self.dialog.as_ref().is_some_and(|dialog| dialog.id == *id) {
                    self.dialog = None;
                }
            }
            WebViewEvent::HttpAuth(challenge) => self.sign_in = Some(challenge.clone()),
            WebViewEvent::HttpAuthClosed { id } => {
                if self.sign_in.as_ref().is_some_and(|ask| ask.id == *id) {
                    self.sign_in = None;
                }
            }
            WebViewEvent::PermissionRequested {
                id,
                origin,
                permissions,
            } => self.permission = Some((*id, origin.clone(), permissions.clone())),
            WebViewEvent::PermissionClosed { id } => {
                if self.permission.as_ref().is_some_and(|(held, ..)| held == id) {
                    self.permission = None;
                }
            }
            WebViewEvent::HeldRequest(held) => self.held.push(held.clone()),
            WebViewEvent::HeldRequestClosed { id } => self.held.retain(|held| held.id != *id),
            WebViewEvent::ContextMenu {
                id,
                x,
                y,
                link,
                items,
            } => {
                self.menu = Some(Menu {
                    id: *id,
                    x: *x,
                    y: *y,
                    link: link.clone(),
                    items: items.clone(),
                });
            }
            WebViewEvent::ContextMenuClosed { id } => {
                if self.menu.as_ref().is_some_and(|menu| menu.id == *id) {
                    self.menu = None;
                }
            }
            WebViewEvent::FindResult { count, active, .. } => self.find = Some((*active, *count)),
            WebViewEvent::ZoomChanged(level) => self.zoom = Some(*level),
            WebViewEvent::FullscreenChanged(on) => self.fullscreen = *on,
            WebViewEvent::LoadProgress(share) => self.progress = *share,
            WebViewEvent::TitleChanged(title) => self.title.clone_from(title),
            WebViewEvent::LoadFinished => self.loading = false,
            WebViewEvent::UriChanged(_) => self.loading = true,
            WebViewEvent::DownloadUpdated {
                id,
                state,
                path,
                name,
                url,
                retryable,
                received,
                total,
                error,
            } => self.downloaded(Download {
                id: *id,
                state: state.clone(),
                name: name.clone(),
                url: url.clone(),
                path: path.clone(),
                retryable: *retryable,
                received: *received,
                total: *total,
                error: error.clone(),
            }),
            _ => {}
        }
    }

    fn downloaded(&mut self, held: Download) {
        match self.downloads.iter_mut().find(|known| known.id == held.id) {
            Some(known) => *known = held,
            None => self.downloads.push(held),
        }
    }
}
