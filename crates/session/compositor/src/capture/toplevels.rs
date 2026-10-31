//! `ext_foreign_toplevel_list_v1`: the windows, by title and app id, for
//! tools that pick a window to capture (the desktop portal's window
//! sharing). Window titles are private, so only clients the session
//! trusts see the list (as with capture itself).

use smithay::desktop::Window;
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::wayland::foreign_toplevel_list::{
    ForeignToplevelHandle, ForeignToplevelListHandler, ForeignToplevelListState,
};

use crate::policy::WindowInfo;
use crate::state::{ClientState, State, describe};

/// A listed window and what its handle last said.
struct Listed {
    window: Window,
    handle: ForeignToplevelHandle,
    info: WindowInfo,
}

/// The list's global and the windows on it.
pub(crate) struct Toplevels {
    state: ForeignToplevelListState,
    listed: Vec<Listed>,
}

impl Toplevels {
    pub(crate) fn new(display: &DisplayHandle) -> Self {
        Self {
            state: ForeignToplevelListState::new_with_filter::<State>(display, |client| {
                client
                    .get_data::<ClientState>()
                    .is_some_and(ClientState::trusted)
            }),
            listed: Vec::new(),
        }
    }

    /// The listed windows: their identifiers and what they say.
    pub(crate) fn windows(&self) -> Vec<(String, &WindowInfo)> {
        self.listed
            .iter()
            .map(|listed| (listed.handle.identifier(), &listed.info))
            .collect()
    }

    /// The window a handle's identifier names, while it is listed.
    pub(crate) fn window(&self, identifier: &str) -> Option<&Window> {
        self.listed
            .iter()
            .find(|listed| listed.handle.identifier() == identifier)
            .map(|listed| &listed.window)
    }
}

impl State {
    /// List the mapped windows: announce new ones, update changed titles
    /// and app ids, close those that went away.
    pub(crate) fn sync_toplevels(&mut self) {
        let windows: Vec<(Window, WindowInfo)> = self
            .space
            .elements()
            .filter_map(|window| {
                Some((
                    window.clone(),
                    describe(&self.display, &self.icons, window)?,
                ))
            })
            .collect();
        let Toplevels { state, listed } = &mut self.capture.toplevels;
        listed.retain(|listed| {
            let mapped = windows.iter().any(|(window, _)| window == &listed.window);
            if !mapped {
                state.remove_toplevel(&listed.handle);
            }
            mapped
        });
        for (window, info) in windows {
            match listed.iter_mut().find(|listed| listed.window == window) {
                Some(listed) if listed.info != info => {
                    if listed.info.title != info.title {
                        listed
                            .handle
                            .send_title(info.title.as_deref().unwrap_or(""));
                    }
                    if listed.info.app_id != info.app_id {
                        listed
                            .handle
                            .send_app_id(info.app_id.as_deref().unwrap_or(""));
                    }
                    listed.handle.send_done();
                    listed.info = info;
                }
                Some(_) => {}
                None => {
                    let handle = state.new_toplevel::<Self>(
                        info.title.clone().unwrap_or_default(),
                        info.app_id.clone().unwrap_or_default(),
                    );
                    listed.push(Listed {
                        window,
                        handle,
                        info,
                    });
                }
            }
        }
    }
}

impl ForeignToplevelListHandler for State {
    fn foreign_toplevel_list_state(&mut self) -> &mut ForeignToplevelListState {
        &mut self.capture.toplevels.state
    }
}

smithay::delegate_foreign_toplevel_list!(State);
