//! `xdg_toplevel_icon_v1`: the icon an app gives its window, a theme name or its own pixels, is
//! held and given to the desktop's toplevel, and the sizes the desktop wants are passed down.
//!
//! The pixels are the ones the app handed over; the desktop gets a shared-memory buffer of its own
//! made from them, because the app may destroy its buffers the moment it set the icon.

use std::os::fd::{AsFd as _, OwnedFd};
use std::sync::Arc;

use smithay::reexports::wayland_server::Resource as _;
use smithay::reexports::wayland_server::backend::ObjectId;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::with_states;
use smithay::wayland::xdg_toplevel_icon::{
    ToplevelIconCachedState, ToplevelIconData, XdgToplevelIconHandler, XdgToplevelIconManager,
};
use wayland_client::protocol as c;
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::xdg::toplevel_icon::v1::client::xdg_toplevel_icon_manager_v1::{
    self as client_manager, XdgToplevelIconManagerV1,
};
use wayland_protocols::xdg::toplevel_icon::v1::client::xdg_toplevel_icon_v1::XdgToplevelIconV1;

use crate::state::State;
use crate::surfaces::{Role, up_toplevel};

/// The desktop's icon object and everything it must keep alive.
pub(crate) struct UpIcon {
    icon: XdgToplevelIconV1,
    /// The pools and buffers the icon shows; destroyed after it.
    held: Vec<(c::wl_shm_pool::WlShmPool, c::wl_buffer::WlBuffer)>,
    pub data: Option<Arc<ToplevelIconData>>,
}

impl UpIcon {
    fn destroy(self) {
        self.icon.destroy();
        for (pool, buffer) in self.held {
            buffer.destroy();
            pool.destroy();
        }
    }
}

/// The user data of a shared-memory buffer the desktop shows an icon from.
pub(crate) struct IconBuffer;

impl State {
    /// The app assigned an icon to a toplevel: hold what its commit made current.
    pub(crate) fn absorb_icon(&mut self, surface: &WlSurface) {
        let id = surface.id();
        let Some(kept) = self.surfaces.get_mut(&id) else {
            return;
        };
        if !matches!(kept.role, Role::Toplevel(_)) {
            return;
        }
        let icon = with_states(surface, |states| {
            states
                .cached_state
                .get::<ToplevelIconCachedState>()
                .current()
                .icon()
                .cloned()
        });
        kept.icon = icon;
    }

    /// Create the app-facing icon manager with the sizes the desktop asked for. A desktop that
    /// asks for other sizes gets a new global, so apps bind again and learn them.
    pub(crate) fn offer_toplevel_icon(&mut self, sizes: &[i32]) {
        // A desktop lists the sizes in no particular order, and the app should not see its global
        // replaced because two desktops listed the same sizes differently.
        let mut sizes = sizes.to_vec();
        sizes.sort_unstable();
        sizes.dedup();
        if self.icons.is_some() && self.icon_sizes == sizes {
            return;
        }
        if let Some(old) = self.icons.take() {
            self.dh.remove_global::<Self>(old.global());
        }
        let mut manager = XdgToplevelIconManager::new::<Self>(&self.dh);
        for size in &sizes {
            manager.add_icon_size(*size);
        }
        self.icon_sizes = sizes;
        self.icons = Some(manager);
    }

    /// Give the desktop the icon the app set on the toplevel `id`, if it differs from what the
    /// desktop has.
    pub(crate) fn push_icon(&mut self, id: &ObjectId) {
        let Some((manager, qh, shm)) = self.up.as_ref().and_then(|up| {
            up.toplevel_icon
                .clone()
                .map(|manager| (manager, up.qh.clone(), up.shm.clone()))
        }) else {
            return;
        };
        let Some(kept) = self.surfaces.get_mut(id) else {
            return;
        };
        if !matches!(kept.role, Role::Toplevel(_)) {
            return;
        }
        let wanted = kept.icon.clone();
        let Some(us) = kept.up.as_mut() else {
            return;
        };
        if us.icon_pushed
            && same(
                us.icon.as_ref().and_then(|icon| icon.data.as_ref()),
                wanted.as_ref(),
            )
        {
            return;
        }
        let Some(toplevel) = up_toplevel(&us.role) else {
            return;
        };
        if let Some(old) = us.icon.take() {
            old.destroy();
        }
        us.icon_pushed = true;
        let Some(wanted) = wanted else {
            manager.set_icon(&toplevel, None);
            return;
        };
        let Some(shm) = shm else {
            return;
        };
        let icon = manager.create_icon(&qh, ());
        if let Some(name) = wanted.icon_name() {
            icon.set_name(name.to_owned());
        }
        let mut held = Vec::new();
        for source in wanted.buffers() {
            let Some((pool, buffer)) = shm_buffer(&shm, source, &qh) else {
                continue;
            };
            icon.add_buffer(&buffer, source.scale());
            held.push((pool, buffer));
        }
        manager.set_icon(&toplevel, Some(&icon));
        us.icon = Some(UpIcon {
            icon,
            held,
            data: Some(wanted),
        });
    }
}

/// Whether two held icons are the same picture, by the identity the surface's cache gives one.
fn same(left: Option<&Arc<ToplevelIconData>>, right: Option<&Arc<ToplevelIconData>>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => Arc::ptr_eq(left, right),
        (None, Some(_)) | (Some(_), None) => false,
    }
}

/// A shared-memory buffer on the desktop holding an icon's pixels.
fn shm_buffer(
    shm: &c::wl_shm::WlShm,
    source: &smithay::wayland::xdg_toplevel_icon::ToplevelIconBuffer,
    qh: &QueueHandle<State>,
) -> Option<(c::wl_shm_pool::WlShmPool, c::wl_buffer::WlBuffer)> {
    let pixels = source.pixels();
    let fd: OwnedFd = rustix::fs::memfd_create("icon", rustix::fs::MemfdFlags::CLOEXEC).ok()?;
    let mut file = std::fs::File::from(fd);
    std::io::Write::write_all(&mut file, pixels).ok()?;
    let size = i32::try_from(pixels.len()).ok()?;
    let pool = shm.create_pool(file.as_fd(), size, qh, ());
    let format = c::wl_shm::Format::try_from(u32::from(source.format())).ok()?;
    let buffer = pool.create_buffer(
        0,
        source.width(),
        source.height(),
        source.stride(),
        format,
        qh,
        IconBuffer,
    );
    Some((pool, buffer))
}

impl XdgToplevelIconHandler for State {
    fn set_icon(
        &mut self,
        _toplevel: smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::XdgToplevel,
        _wl_surface: WlSurface,
    ) {
        // The icon takes effect on the surface's next commit, where `absorb_icon` reads it.
    }
}

impl Dispatch<XdgToplevelIconManagerV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &XdgToplevelIconManagerV1,
        event: client_manager::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            client_manager::Event::IconSize { size } => {
                if let Some(up) = state.up.as_mut()
                    && !up.icon_sizes.contains(&size)
                {
                    up.icon_sizes.push(size);
                }
            }
            client_manager::Event::Done => {
                let sizes = state
                    .up
                    .as_ref()
                    .map(|up| up.icon_sizes.clone())
                    .unwrap_or_default();
                state.offer_toplevel_icon(&sizes);
            }
            _ => {}
        }
    }
}

impl Dispatch<c::wl_buffer::WlBuffer, IconBuffer> for State {
    fn event(
        _: &mut Self,
        _: &c::wl_buffer::WlBuffer,
        _: c::wl_buffer::Event,
        _: &IconBuffer,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

wayland_client::delegate_noop!(State: ignore XdgToplevelIconV1);

smithay::delegate_xdg_toplevel_icon!(State);
