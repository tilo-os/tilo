//! The X11 server's windows behind its toplevels: the icon each X11 window names
//! (`_NET_WM_ICON`) goes on the toplevel that shows it.
//!
//! The X11 server tells its toplevels nothing of the X11 window behind them; it names one by the
//! window's class (`WM_CLASS`, the app id) and title. The keeper's own connection to the server
//! hears windows map in the order the server maps them, so a new toplevel goes with the
//! earliest mapped window not yet matched that has its app id as class, one with its title
//! first. Two windows of one program mapped at once with the same title may be matched the other
//! way round; they are one program's, so each still shows an icon that program gave.
//!
//! The icon is the keeper's to hold, as an app's own is, so it is given to every desktop the
//! keeper attaches to.

use std::sync::Arc;

use smithay::reexports::wayland_server::Resource as _;
use smithay::reexports::wayland_server::backend::ObjectId;
use tilo_x11_transfer::windows::{IconImage, WindowEvent};

use crate::state::State;
use crate::surfaces::Role;

/// A mapped window of the X11 server.
pub(crate) struct X11Window {
    /// The X11 window's id.
    window: u32,
    class: String,
    title: String,
    icon: Option<Arc<Vec<IconImage>>>,
    /// The toplevel showing it, once matched.
    toplevel: Option<ObjectId>,
}

fn held(icon: Vec<IconImage>) -> Option<Arc<Vec<IconImage>>> {
    (!icon.is_empty()).then(|| Arc::new(icon))
}

impl State {
    /// What the keeper's connection heard of the X11 server's windows.
    pub(crate) fn x11_window_event(&mut self, event: WindowEvent) {
        match event {
            WindowEvent::Mapped {
                window,
                class,
                title,
                icon,
            } => {
                self.x11_windows.push(X11Window {
                    window,
                    class,
                    title,
                    icon: held(icon),
                    toplevel: None,
                });
                self.match_x11_windows(true);
            }
            WindowEvent::Icon { window, icon } => {
                let Some(known) = self
                    .x11_windows
                    .iter_mut()
                    .find(|known| known.window == window)
                else {
                    return;
                };
                known.icon = held(icon);
                if let Some(toplevel) = known.toplevel.clone() {
                    let icon = known.icon.clone();
                    self.show_x11_icon(&toplevel, icon, true);
                }
            }
            WindowEvent::Unmapped { window } => {
                self.x11_windows.retain(|known| known.window != window);
            }
            WindowEvent::Answered { .. } => self.x11_ponged(),
        }
    }

    /// The keeper's connections to the X11 server ended: its windows are gone with it.
    pub(crate) fn x11_windows_gone(&mut self) {
        self.x11_windows.clear();
        self.x11_ponged();
    }

    /// Match the X11 server's toplevels not matched yet with the windows not matched yet; outside
    /// a push of the toplevel, `commit` its desktop surface so the icon takes effect.
    pub(crate) fn match_x11_windows(&mut self, commit: bool) {
        if self
            .x11_windows
            .iter()
            .all(|known| known.toplevel.is_some())
        {
            return;
        }
        let Some(server) = self.xdnd.server().cloned() else {
            return;
        };
        let mut toplevels: Vec<(u64, ObjectId, String, String)> = self
            .surfaces
            .iter()
            .filter_map(|(id, kept)| {
                let Role::Toplevel(top) = &kept.role else {
                    return None;
                };
                if kept.surface.client().map(|client| client.id()) != Some(server.clone())
                    || self
                        .x11_windows
                        .iter()
                        .any(|known| known.toplevel.as_ref() == Some(id))
                {
                    return None;
                }
                let (title, app_id) = top.title_and_app_id();
                Some((
                    kept.seq,
                    id.clone(),
                    app_id.unwrap_or_default(),
                    title.unwrap_or_default(),
                ))
            })
            .collect();
        toplevels.sort_by_key(|(seq, ..)| *seq);
        for (_, id, app_id, title) in toplevels {
            let unmatched =
                |known: &&mut X11Window| known.toplevel.is_none() && known.class == app_id;
            let index = self
                .x11_windows
                .iter_mut()
                .position(|known| unmatched(&known) && known.title == title)
                .or_else(|| {
                    self.x11_windows
                        .iter_mut()
                        .position(|known| unmatched(&known))
                });
            let Some(known) = index.and_then(|index| self.x11_windows.get_mut(index)) else {
                continue;
            };
            known.toplevel = Some(id.clone());
            let icon = known.icon.clone();
            self.show_x11_icon(&id, icon, commit);
        }
    }

    /// Hold `icon` for `toplevel` and give it to the desktop, which takes it with the surface's
    /// next commit.
    fn show_x11_icon(
        &mut self,
        toplevel: &ObjectId,
        icon: Option<Arc<Vec<IconImage>>>,
        commit: bool,
    ) {
        let Some(kept) = self.surfaces.get_mut(toplevel) else {
            return;
        };
        kept.x11_icon = icon;
        self.push_icon(toplevel);
        if commit
            && let Some(up) = self
                .surfaces
                .get(toplevel)
                .and_then(|kept| kept.up.as_ref())
        {
            up.surface.commit();
        }
    }
}
