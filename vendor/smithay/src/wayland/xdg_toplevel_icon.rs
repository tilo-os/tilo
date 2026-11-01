//! XDG Toplevel Icon
//!
//! This protocol allows clients to set icons for their toplevel surfaces either via the XDG icon stock (using an icon name), or from pixel data.
//!
//! In order to advertise toplevel icon global call [XdgToplevelIconManager::new] and delegate
//! events to it with [`delegate_xdg_toplevel_icon`][crate::delegate_xdg_toplevel_icon].
//! Currently attached icon is available in double-buffered [ToplevelIconCachedState]

use std::{
    collections::HashSet,
    sync::{Arc, Mutex, OnceLock},
};

use wayland_protocols::xdg::{
    shell::server::xdg_toplevel::XdgToplevel,
    toplevel_icon::v1::server::{
        xdg_toplevel_icon_manager_v1::{self, XdgToplevelIconManagerV1},
        xdg_toplevel_icon_v1::{self, XdgToplevelIconV1},
    },
};

use wayland_server::{
    backend::{ClientId, GlobalId},
    protocol::{wl_buffer::WlBuffer, wl_shm, wl_surface::WlSurface},
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};

use crate::{
    utils::HookId,
    wayland::{
        compositor::{self, Cacheable},
        shell::xdg::XdgShellSurfaceUserData,
        shm::{self, BufferData, ShmBufferUserData},
    },
};

/// Handler trait for xdg toplevel icon events.
pub trait XdgToplevelIconHandler:
    GlobalDispatch<XdgToplevelIconManagerV1, XdgToplevelIconManagerUserData>
    + Dispatch<XdgToplevelIconManagerV1, ()>
    + Dispatch<XdgToplevelIconV1, XdgToplevelIconUserData>
    + 'static
{
    /// Called when icon becomes pending, and awaits surface commit.
    /// Icon is stored in wl_surface [ToplevelIconCachedState]
    fn set_icon(&mut self, toplevel: XdgToplevel, wl_surface: WlSurface) {
        let _ = toplevel;
        let _ = wl_surface;
    }
}

/// Data associated with WlSurface
/// Represents the client pending state
///
/// ```no_run
/// use smithay::wayland::compositor;
/// use smithay::wayland::xdg_toplevel_icon::ToplevelIconCachedState;
///
/// # let wl_surface = todo!();
/// compositor::with_states(&wl_surface, |states| {
///     let mut data = states.cached_state.get::<ToplevelIconCachedState>();
///     dbg!(data.current().icon_name());
/// });
/// ```
#[derive(Debug, Clone, Default)]
pub struct ToplevelIconCachedState {
    icon: Option<Arc<ToplevelIconData>>,
}

impl ToplevelIconCachedState {
    /// The icon the toplevel has, if it has one.
    pub fn icon(&self) -> Option<&Arc<ToplevelIconData>> {
        self.icon.as_ref()
    }

    /// Icon name getter
    pub fn icon_name(&self) -> Option<&str> {
        self.icon.as_ref()?.icon_name()
    }

    /// List of icon buffers and buffer scale
    pub fn buffers(&self) -> &[ToplevelIconBuffer] {
        self.icon.as_ref().map_or(&[], |icon| icon.buffers())
    }
}

/// What an icon object held when it was assigned to a toplevel: its name and
/// the pixels of every size it offers.
///
/// The pixels are a copy, because the protocol lets a client destroy the icon
/// object, and then its buffers, right after assigning it, and the toplevel
/// still shows the icon it was given.
#[derive(Debug, Clone, Default)]
pub struct ToplevelIconData {
    icon_name: Option<String>,
    buffers: Vec<ToplevelIconBuffer>,
}

impl ToplevelIconData {
    /// The icon name the client set, if it set one.
    pub fn icon_name(&self) -> Option<&str> {
        self.icon_name.as_deref()
    }

    /// The pixels the icon offers, one entry per size and scale.
    pub fn buffers(&self) -> &[ToplevelIconBuffer] {
        &self.buffers
    }
}

/// The pixels an icon offers for one size and scale.
#[derive(Debug, Clone)]
pub struct ToplevelIconBuffer {
    buffer: WlBuffer,
    scale: i32,
    format: wl_shm::Format,
    width: i32,
    height: i32,
    stride: i32,
    pixels: Arc<[u8]>,
}

impl ToplevelIconBuffer {
    /// The buffer these pixels were read from; the client may have destroyed
    /// it since, and the pixels stay readable all the same.
    pub fn buffer(&self) -> &WlBuffer {
        &self.buffer
    }

    /// The scale the client added these pixels for.
    pub fn scale(&self) -> i32 {
        self.scale
    }

    /// The shm format the pixels are in.
    pub fn format(&self) -> wl_shm::Format {
        self.format
    }

    /// Width of the icon in pixels; an icon is square, so this is also its
    /// height.
    pub fn width(&self) -> i32 {
        self.width
    }

    /// Height of the icon in pixels.
    pub fn height(&self) -> i32 {
        self.height
    }

    /// Bytes per row of [`pixels`][Self::pixels].
    pub fn stride(&self) -> i32 {
        self.stride
    }

    /// The pixels as the client wrote them: `stride * height` bytes.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }
}

impl Cacheable for ToplevelIconCachedState {
    fn commit(&mut self, _dh: &DisplayHandle) -> Self {
        self.clone()
    }

    fn merge_into(self, into: &mut Self, _dh: &DisplayHandle) {
        *into = self;
    }
}

#[derive(Debug, Default)]
struct IconData {
    icon_name: Option<String>,
    buffers: Vec<(WlBuffer, i32)>,
}

impl IconData {
    /// What the icon holds, with the pixel data copied out of the client's
    /// pool. `None` for an icon with neither a name nor pixels, which the
    /// protocol says resets the toplevel's icon.
    fn snapshot(&self) -> Option<ToplevelIconData> {
        let buffers: Vec<ToplevelIconBuffer> = self
            .buffers
            .iter()
            .filter_map(|(buffer, scale)| {
                let shm = buffer.data::<ShmBufferUserData>()?;
                let data = shm.data;
                let pixels = copy_pixels(buffer, data)?;
                Some(ToplevelIconBuffer {
                    buffer: buffer.clone(),
                    scale: *scale,
                    format: data.format,
                    width: data.width,
                    height: data.height,
                    stride: data.stride,
                    pixels,
                })
            })
            .collect();
        if self.icon_name.is_none() && buffers.is_empty() {
            return None;
        }
        Some(ToplevelIconData {
            icon_name: self.icon_name.clone(),
            buffers,
        })
    }
}

/// The pixels `buffer` holds, copied so that they outlive the client's pool.
fn copy_pixels(buffer: &WlBuffer, data: BufferData) -> Option<Arc<[u8]>> {
    let offset = usize::try_from(data.offset).ok()?;
    let stride = usize::try_from(data.stride).ok()?;
    let height = usize::try_from(data.height).ok()?;
    let length = stride.checked_mul(height)?;
    let end = offset.checked_add(length)?;
    shm::with_buffer_contents(buffer, |contents, mapped, _| {
        if end > mapped {
            return None;
        }
        // SAFETY: just checked that the range lies inside the mapping.
        let bytes = unsafe { std::slice::from_raw_parts(contents.add(offset), length) };
        Some(Arc::from(bytes))
    })
    .ok()
    .flatten()
}

/// User data of [XdgToplevelIconV1]
#[derive(Debug, Default)]
pub struct XdgToplevelIconUserData {
    builder: Mutex<IconData>,
    constructed: OnceLock<Option<Arc<ToplevelIconData>>>,
    buffer_destruction_hooks: Mutex<Vec<(WlBuffer, HookId)>>,
}

impl XdgToplevelIconUserData {
    fn freeze(&self) -> Option<Arc<ToplevelIconData>> {
        self.constructed
            .get_or_init(|| {
                let data = std::mem::take(&mut *self.builder.lock().unwrap());
                data.snapshot().map(Arc::new)
            })
            .clone()
    }

    fn is_immutable(&self) -> bool {
        self.constructed.get().is_some()
    }

    fn set_icon_name(&self, icon_name: String) {
        debug_assert!(!self.is_immutable());
        self.builder.lock().unwrap().icon_name = Some(icon_name)
    }

    fn register_buffer_destruction_hook(
        &self,
        buffer: WlBuffer,
        shm: &ShmBufferUserData,
        hook: impl Fn() + Send + Sync + 'static,
    ) {
        let buffer_destruction_hook = shm.add_destruction_hook(move |_, _| hook());
        self.buffer_destruction_hooks
            .lock()
            .unwrap()
            .push((buffer, buffer_destruction_hook));
    }

    fn unregister_buffer_destruction_hook(&self, buffer: &WlBuffer) {
        let mut guard = self.buffer_destruction_hooks.lock().unwrap();

        if let Some((buffer, hook)) = guard
            .iter()
            .position(|(b, _)| b == buffer)
            .map(|id| guard.remove(id))
        {
            if let Some(shm) = buffer.data::<ShmBufferUserData>() {
                shm.remove_destruction_hook(hook);
            }
        }
    }

    fn unregister_all_hooks(&self) {
        let mut guard = self.buffer_destruction_hooks.lock().unwrap();

        for (buffer, hook) in guard.drain(..) {
            if let Some(shm) = buffer.data::<ShmBufferUserData>() {
                shm.remove_destruction_hook(hook);
            }
        }
    }

    fn add_buffer(&self, buffer: WlBuffer, scale: i32, shm: &ShmBufferUserData) {
        debug_assert!(!self.is_immutable());

        let mut guard = self.builder.lock().unwrap();
        let list = &mut guard.buffers;

        for (existing_buffer, existing_scale) in list.iter_mut() {
            let Some(existing_data) = existing_buffer.data::<ShmBufferUserData>().map(|shm| shm.data) else {
                continue;
            };

            let same_width = existing_data.width == shm.data.width;
            let same_height = existing_data.height == shm.data.height;
            let same_scale = *existing_scale == scale;

            // If this icon instance already has a buffer of the same size and scale from a previous 'add_buffer' request,
            // data from the last request overrides the preexisting pixel data.
            if same_width && same_height && same_scale {
                // The existing buffer will not be used, so let's assume we don't care about it's
                // destruction
                self.unregister_buffer_destruction_hook(existing_buffer);
                *existing_buffer = buffer;
                return;
            }
        }

        list.push((buffer, scale));
    }
}

#[derive(Debug, Default)]
struct ManagerGlobalData {
    sizes: HashSet<i32>,
}

/// User data of [XdgToplevelIconManagerV1] global
#[derive(Debug)]
pub struct XdgToplevelIconManagerUserData(Arc<Mutex<ManagerGlobalData>>);

/// Delegate type for handling xdg toplevel icon events.
#[derive(Debug)]
pub struct XdgToplevelIconManager {
    global: GlobalId,
    data: Arc<Mutex<ManagerGlobalData>>,
}

impl XdgToplevelIconManager {
    /// Creates a new delegate type for handling xdg toplevel icon events.
    pub fn new<D: XdgToplevelIconHandler>(display: &DisplayHandle) -> Self {
        let data = Arc::new(Mutex::new(ManagerGlobalData::default()));
        let global = display
            .create_global::<D, XdgToplevelIconManagerV1, _>(1, XdgToplevelIconManagerUserData(data.clone()));
        XdgToplevelIconManager { global, data }
    }

    /// Request given icon size
    ///
    /// - `size` the edge size of the square icon in surface-local coordinates
    pub fn add_icon_size(&mut self, size: i32) {
        self.data.lock().unwrap().sizes.insert(size);
    }

    /// Returns the [XdgToplevelIconManagerV1] global id.
    pub fn global(&self) -> GlobalId {
        self.global.clone()
    }
}

impl<D: XdgToplevelIconHandler> GlobalDispatch<XdgToplevelIconManagerV1, XdgToplevelIconManagerUserData, D>
    for XdgToplevelIconManager
{
    fn bind(
        _: &mut D,
        _: &DisplayHandle,
        _: &Client,
        resource: New<XdgToplevelIconManagerV1>,
        data: &XdgToplevelIconManagerUserData,
        data_init: &mut DataInit<'_, D>,
    ) {
        let manager = data_init.init(resource, ());

        for size in data.0.lock().unwrap().sizes.iter() {
            manager.icon_size(*size);
        }
        manager.done();
    }
}

impl<D: XdgToplevelIconHandler> Dispatch<XdgToplevelIconManagerV1, (), D> for XdgToplevelIconManager {
    fn request(
        state: &mut D,
        _: &Client,
        _resource: &XdgToplevelIconManagerV1,
        request: xdg_toplevel_icon_manager_v1::Request,
        _: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        use xdg_toplevel_icon_manager_v1::Request;

        match request {
            Request::CreateIcon { id } => {
                data_init.init(id, XdgToplevelIconUserData::default());
            }
            Request::SetIcon { toplevel, icon } => {
                let data = toplevel.data::<XdgShellSurfaceUserData>().unwrap();
                let wl_surface = data.wl_surface.clone();

                // An icon that carries neither a name nor pixels resets the
                // toplevel's icon, just like a null icon does.
                let icon = icon.and_then(|icon| {
                    let icon_data = icon.data::<XdgToplevelIconUserData>()?;
                    icon_data.freeze()
                });

                compositor::with_states(&data.wl_surface, |states| {
                    let mut state = states.cached_state.get::<ToplevelIconCachedState>();
                    state.pending().icon = icon;
                });

                state.set_icon(toplevel, wl_surface);
            }
            Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

impl<D: XdgToplevelIconHandler> Dispatch<XdgToplevelIconV1, XdgToplevelIconUserData, D>
    for XdgToplevelIconManager
{
    fn request(
        _state: &mut D,
        _: &Client,
        icon: &XdgToplevelIconV1,
        request: xdg_toplevel_icon_v1::Request,
        data: &XdgToplevelIconUserData,
        dh: &DisplayHandle,
        _: &mut DataInit<'_, D>,
    ) {
        use xdg_toplevel_icon_v1::Request;

        match request {
            Request::SetName { icon_name } => {
                if data.is_immutable() {
                    dh.post_error(
                        icon,
                        xdg_toplevel_icon_v1::Error::Immutable as u32,
                        "Request made after the icon has been assigned to a toplevel via 'set_icon'"
                            .to_string(),
                    );
                    return;
                }

                data.set_icon_name(icon_name);
            }
            Request::AddBuffer { buffer, scale } => {
                if data.is_immutable() {
                    dh.post_error(
                        icon,
                        xdg_toplevel_icon_v1::Error::Immutable as u32,
                        "Request made after the icon has been assigned to a toplevel via 'set_icon'"
                            .to_string(),
                    );
                    return;
                }

                let Some(shm) = buffer.data::<ShmBufferUserData>() else {
                    dh.post_error(
                        icon,
                        xdg_toplevel_icon_v1::Error::InvalidBuffer as u32,
                        "The wl_buffer must be backed by wl_shm".to_string(),
                    );
                    return;
                };

                if shm.data.width != shm.data.height {
                    dh.post_error(
                        icon,
                        xdg_toplevel_icon_v1::Error::InvalidBuffer as u32,
                        "The wl_buffer must be a square".to_string(),
                    );
                    return;
                };

                // Let's listen for buffer destruction event to catch no_buffer protocol error
                // This hook has to be unregistered once the icon is destroyed
                data.register_buffer_destruction_hook(buffer.clone(), shm, {
                    let icon = icon.clone();
                    move || {
                        icon.post_error(
                            xdg_toplevel_icon_v1::Error::NoBuffer,
                            "The provided buffer has been destroyed before the toplevel icon",
                        )
                    }
                });
                data.add_buffer(buffer.clone(), scale, shm);
            }
            Request::Destroy => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(
        _state: &mut D,
        _client: ClientId,
        _resource: &XdgToplevelIconV1,
        data: &XdgToplevelIconUserData,
    ) {
        data.unregister_all_hooks();
    }
}

/// Macro to delegate implementation of the xdg toplevel icon to [`XdgToplevelIconManager`].
///
/// You must also implement [`XdgToplevelIconHandler`] to use this.
#[macro_export]
macro_rules! delegate_xdg_toplevel_icon {
    ($(@<$( $lt:tt $( : $clt:tt $(+ $dlt:tt )* )? ),+>)? $ty: ty) => {
        $crate::reexports::wayland_server::delegate_global_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            $crate::reexports::wayland_protocols::xdg::toplevel_icon::v1::server::xdg_toplevel_icon_manager_v1::XdgToplevelIconManagerV1: $crate::wayland::xdg_toplevel_icon::XdgToplevelIconManagerUserData
        ] => $crate::wayland::xdg_toplevel_icon::XdgToplevelIconManager);

        $crate::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            $crate::reexports::wayland_protocols::xdg::toplevel_icon::v1::server::xdg_toplevel_icon_manager_v1::XdgToplevelIconManagerV1: ()
        ] => $crate::wayland::xdg_toplevel_icon::XdgToplevelIconManager);
        $crate::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            $crate::reexports::wayland_protocols::xdg::toplevel_icon::v1::server::xdg_toplevel_icon_v1::XdgToplevelIconV1: $crate::wayland::xdg_toplevel_icon::XdgToplevelIconUserData
        ] => $crate::wayland::xdg_toplevel_icon::XdgToplevelIconManager);
    };
}
