//! Held surfaces and the one way their state reaches the desktop.
//!
//! [`State::absorb`] takes what a client committed into the surface's [`Kept`] record.
//! [`State::push`] sends a surface's held state to the desktop, compared with what was pushed to
//! that same desktop before: on a client's commit only what changed goes, and for a desktop that
//! was just attached nothing was pushed yet, so everything goes. There is no other path, and no
//! journal to replay.

use std::collections::{HashMap, VecDeque};
use std::os::fd::{AsFd as _, OwnedFd};

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::{Buffer as _, dmabuf::DmabufFlags};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::Resource as _;
use smithay::reexports::wayland_server::backend::ObjectId;
use smithay::reexports::wayland_server::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_surface::WlSurface,
};
use smithay::wayland::compositor::{
    BufferAssignment, Damage, RectangleKind, RegionAttributes, SubsurfaceCachedState,
    SurfaceAttributes, get_parent, is_sync_subsurface, with_states,
};
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, SurfaceCachedState, ToplevelSurface, XdgToplevelSurfaceData,
};
use wayland_client::Proxy as _;
use wayland_client::protocol as c;
use wayland_protocols::xdg::shell::client as cx;

use crate::state::State;
use crate::upstream::{FrameRef, SurfaceRef, Upstream};

/// A rectangle `(x, y, width, height)`.
pub(crate) type Rect = (i32, i32, i32, i32);
/// A region as the rectangles added (`true`) and subtracted, in order.
type Region = Vec<(bool, Rect)>;

/// One surface of the app and everything the keeper holds of it.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is a separate thing the app did or asked for on this surface"
)]
pub(crate) struct Kept {
    pub surface: WlSurface,
    /// Creation order, so a desktop recreates the oldest first.
    pub seq: u64,
    /// The buffer it shows: kept un-released while it is current.
    pub buffer: Option<u64>,
    /// The client attached (a buffer or none) since the last push.
    pub attached: bool,
    pub damage: Vec<Damage>,
    pub scale: i32,
    pub transform: u32,
    pub delta: Option<(i32, i32)>,
    pub opaque: Option<Region>,
    pub input: Option<Region>,
    /// Frame callbacks not yet done, each with the desktop generation it was sent to (0: none).
    pub frames: Vec<(WlCallback, u64)>,
    /// The app made a fractional-scale object for this surface.
    pub wants_fractional_scale: bool,
    /// The scale the desktop last asked for, kept across a restart.
    pub preferred_scale: Option<f64>,
    /// The app's xdg decoration object and what it last asked for.
    pub xdg_decoration: Option<crate::decorations::Xdg>,
    /// The app's KDE decoration object and what it last asked for.
    pub kde_decoration: Option<crate::decorations::Kde>,
    /// The app asked the desktop not to idle while this surface shows.
    pub idle_inhibitor: bool,
    /// The app asked the desktop to stop acting on its own shortcuts here.
    pub shortcuts_inhibitor: Option<crate::keyboard_shortcuts_inhibit::Held>,
    /// The app marked this window a modal dialog.
    pub modal: bool,
    /// The icon the app set on this toplevel, as its last commit left it.
    pub icon: Option<std::sync::Arc<smithay::wayland::xdg_toplevel_icon::ToplevelIconData>>,
    /// The icon of the X11 window this toplevel of the X11 server shows.
    pub x11_icon: Option<std::sync::Arc<Vec<tilo_x11_transfer::windows::IconImage>>>,
    pub role: Role,
    pub up: Option<UpSurface>,
}

impl Kept {
    pub(crate) fn new(surface: WlSurface, seq: u64) -> Self {
        Self {
            surface,
            seq,
            buffer: None,
            attached: false,
            damage: Vec::new(),
            scale: 1,
            transform: 0,
            delta: None,
            opaque: None,
            input: None,
            frames: Vec::new(),
            wants_fractional_scale: false,
            preferred_scale: None,
            xdg_decoration: None,
            kde_decoration: None,
            idle_inhibitor: false,
            shortcuts_inhibitor: None,
            modal: false,
            icon: None,
            x11_icon: None,
            role: Role::None,
            up: None,
        }
    }
}

/// What a surface is.
pub(crate) enum Role {
    /// Nothing shown yet.
    None,
    /// A pointer's image.
    Cursor,
    /// A drag's image, following the pointer while the drag runs.
    DragIcon,
    Sub(Sub),
    Toplevel(Box<Top>),
    Popup(Box<Pop>),
}

pub(crate) struct Sub {
    pub location: (i32, i32),
    pub sync: bool,
    /// The surface it is a subsurface of: its desktop twin is stacked against it.
    pub parent: ObjectId,
    /// Its `wl_subsurface` went while the surface stays: the twin goes at the next commit of
    /// the parent.
    pub ended: bool,
}

impl Sub {
    pub(crate) fn new(parent: ObjectId) -> Self {
        Self {
            location: (0, 0),
            sync: true,
            parent,
            ended: false,
        }
    }
}

/// A window.
pub(crate) struct Top {
    pub toplevel: ToplevelSurface,
    pub number: u64,
    pub tag: String,
    pub geometry: Option<Rect>,
    pub min: (i32, i32),
    pub max: (i32, i32),
    pub maximized: bool,
    /// Asked to be fullscreen, on the output named here when it named one.
    pub fullscreen: (bool, Option<String>),
    /// The configures sent to the client, oldest first: each its serial and, when it came from
    /// a desktop, that desktop's generation and serial.
    pub acks: VecDeque<(u32, Option<(u64, u32)>)>,
}

impl Top {
    pub(crate) fn new(toplevel: ToplevelSurface, number: u64) -> Self {
        Self {
            toplevel,
            number,
            tag: format!("w{number}"),
            geometry: None,
            min: (0, 0),
            max: (0, 0),
            maximized: false,
            fullscreen: (false, None),
            acks: VecDeque::new(),
        }
    }

    pub(crate) fn title_and_app_id(&self) -> (Option<String>, Option<String>) {
        with_states(self.toplevel.wl_surface(), |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|data| data.lock().ok())
                .map(|data| (data.title.clone(), data.app_id.clone()))
                .unwrap_or_default()
        })
    }
}

/// A popup menu.
pub(crate) struct Pop {
    pub popup: PopupSurface,
    pub positioner: PositionerState,
    pub geometry: Option<Rect>,
    /// Told it is done: it never shows on a desktop again.
    pub dismissed: bool,
    pub acks: VecDeque<(u32, Option<(u64, u32)>)>,
}

impl Pop {
    pub(crate) fn new(popup: PopupSurface, positioner: PositionerState) -> Self {
        Self {
            popup,
            positioner,
            geometry: None,
            dismissed: false,
            acks: VecDeque::new(),
        }
    }
}

/// A held surface's objects on the desktop.
pub(crate) struct UpSurface {
    pub surface: c::wl_surface::WlSurface,
    pub role: UpRole,
    pub pushed: Pushed,
    pub viewport: Option<wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport>,
    pub content_type: Option<wayland_protocols::wp::content_type::v1::client::wp_content_type_v1::WpContentTypeV1>,
    pub fractional_scale: Option<wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1::WpFractionalScaleV1>,
    /// The desktop's window decorations for this surface.
    pub xdg_decoration: Option<
        wayland_protocols::xdg::decoration::zv1::client::zxdg_toplevel_decoration_v1::ZxdgToplevelDecorationV1,
    >,
    pub kde_decoration: Option<
        wayland_protocols_misc::server_decoration::client::org_kde_kwin_server_decoration::OrgKdeKwinServerDecoration,
    >,
    /// The desktop's inhibitors while it is attached.
    pub idle_inhibitor: Option<
        wayland_protocols::wp::idle_inhibit::zv1::client::zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1,
    >,
    pub shortcuts_inhibitor: Option<
        wayland_protocols::wp::keyboard_shortcuts_inhibit::zv1::client::zwp_keyboard_shortcuts_inhibitor_v1::ZwpKeyboardShortcutsInhibitorV1,
    >,
    /// The desktop's dialog object while it is attached, and whether the hint was set.
    pub dialog: Option<wayland_protocols::xdg::dialog::v1::client::xdg_dialog_v1::XdgDialogV1>,
    pub modal_sent: Option<bool>,
    /// The desktop's icon object and whether one was sent.
    pub icon: Option<crate::toplevel_icon::UpIcon>,
    pub icon_pushed: bool,
    /// The desktop's colour objects for this surface.
    pub color: crate::color::UpSurfaceColor,
}

pub(crate) enum UpRole {
    Plain,
    Sub(c::wl_subsurface::WlSubsurface),
    Xdg(Box<UpXdg>),
}

pub(crate) struct UpXdg {
    pub xdg: cx::xdg_surface::XdgSurface,
    pub kind: UpXdgKind,
    /// The desktop's configures not yet acknowledged, oldest first.
    pub pending: VecDeque<u32>,
    /// A configure was acknowledged: the surface may show a buffer.
    pub configured: bool,
    /// What the toplevel's next `xdg_surface.configure` applies.
    pub next_top: Option<(i32, i32, Vec<u32>)>,
    pub next_bounds: Option<(i32, i32)>,
    pub next_popup: Option<Rect>,
    pub repositioned: Option<u32>,
}

pub(crate) enum UpXdgKind {
    Toplevel(cx::xdg_toplevel::XdgToplevel),
    Popup(cx::xdg_popup::XdgPopup),
}

/// The desktop's toplevel object of a surface, if its role is a toplevel.
pub(crate) fn up_toplevel(role: &UpRole) -> Option<cx::xdg_toplevel::XdgToplevel> {
    match role {
        UpRole::Xdg(xdg) => match &xdg.kind {
            UpXdgKind::Toplevel(toplevel) => Some(toplevel.clone()),
            UpXdgKind::Popup(_) => None,
        },
        UpRole::Plain | UpRole::Sub(_) => None,
    }
}

/// What was pushed to the current desktop; `None` is "nothing yet".
#[derive(Default)]
#[allow(
    clippy::option_option,
    reason = "the outer `None` is nothing pushed yet, the inner one a value of none pushed"
)]
pub(crate) struct Pushed {
    buffer: Option<Option<u64>>,
    scale: Option<i32>,
    transform: Option<u32>,
    opaque: Option<Option<Region>>,
    input: Option<Option<Region>>,
    location: Option<(i32, i32)>,
    sync: Option<bool>,
    geometry: Option<Option<Rect>>,
    title: Option<Option<String>>,
    app_id: Option<Option<String>>,
    parent: Option<Option<ObjectId>>,
    min: Option<(i32, i32)>,
    max: Option<(i32, i32)>,
    maximized: Option<bool>,
    fullscreen: Option<(bool, Option<String>)>,
    viewport: Option<crate::viewporter::Viewport>,
    content_type: Option<u32>,
    /// The image description (by identity) and rendering intent the desktop's surface was given.
    pub color: Option<Option<(u32, u32)>>,
}

/// Whether `value` differs from what was pushed, recording it as pushed.
fn differs<T: PartialEq + Clone>(slot: &mut Option<T>, value: &T) -> bool {
    if slot.as_ref() == Some(value) {
        return false;
    }
    *slot = Some(value.clone());
    true
}

fn region(attributes: Option<&RegionAttributes>) -> Option<Region> {
    attributes.map(|region| {
        region
            .rects
            .iter()
            .map(|(kind, rect)| {
                (
                    matches!(kind, RectangleKind::Add),
                    (rect.loc.x, rect.loc.y, rect.size.w, rect.size.h),
                )
            })
            .collect()
    })
}

/// Every buffer the app attached that the keeper still holds or still owes a release.
#[derive(Default)]
pub(crate) struct Buffers {
    next_key: u64,
    by_client: HashMap<ObjectId, u64>,
    kept: HashMap<u64, KeptBuffer>,
    pools: HashMap<(u64, u64), KeptPool>,
}

impl Buffers {
    /// The size in pixels of the held buffer `key`.
    pub(crate) fn size_of(&self, key: u64) -> Option<(i32, i32)> {
        match &self.kept.get(&key)?.source {
            Source::Shm { width, height, .. } => Some((*width, *height)),
            Source::Dmabuf(dmabuf) => {
                let size = dmabuf.size();
                Some((size.w, size.h))
            }
        }
    }
}

struct KeptBuffer {
    /// The client's buffer, until the client destroys it.
    client: Option<WlBuffer>,
    source: Source,
    /// Surfaces showing it.
    current: usize,
    /// The client attached it and has not been told it is released since.
    owed: bool,
    up: Option<(u64, c::wl_buffer::WlBuffer)>,
    /// The desktop holds it (attached, not released).
    up_busy: bool,
}

enum Source {
    Shm {
        pool: (u64, u64),
        offset: i32,
        width: i32,
        height: i32,
        stride: i32,
        format: u32,
    },
    Dmabuf(Dmabuf),
}

/// One shared-memory pool of the app: a descriptor of its memory, never a copy.
struct KeptPool {
    fd: OwnedFd,
    size: usize,
    users: usize,
    up: Option<(u64, c::wl_shm_pool::WlShmPool, usize)>,
}

/// The upstream object's user data: which held buffer it is.
pub(crate) struct BufferRef(pub u64);

impl Buffers {
    /// The held buffer for a client's, recorded on first sight.
    fn key_for(&mut self, buffer: &WlBuffer) -> Option<u64> {
        let known = self.by_client.get(&buffer.id()).copied();
        if let Some(key) = known {
            if let Some(KeptBuffer {
                source: Source::Shm { pool, .. },
                ..
            }) = self.kept.get(&key)
            {
                let pool = *pool;
                self.refresh_pool(buffer, pool);
            }
            return Some(key);
        }
        let source = if let Ok(dmabuf) = smithay::wayland::dmabuf::get_dmabuf(buffer) {
            Source::Dmabuf(dmabuf.clone())
        } else {
            self.shm_source(buffer)?
        };
        self.next_key = self.next_key.wrapping_add(1);
        let key = self.next_key;
        self.by_client.insert(buffer.id(), key);
        self.kept.insert(
            key,
            KeptBuffer {
                client: Some(buffer.clone()),
                source,
                current: 0,
                owed: false,
                up: None,
                up_busy: false,
            },
        );
        Some(key)
    }

    fn shm_source(&mut self, buffer: &WlBuffer) -> Option<Source> {
        let found = smithay::wayland::shm::with_buffer_pool(buffer, |fd, size, data| {
            let stat = rustix::fs::fstat(fd).ok()?;
            let id = (stat.st_dev, stat.st_ino);
            if let std::collections::hash_map::Entry::Vacant(vacant) = self.pools.entry(id) {
                vacant.insert(KeptPool {
                    fd: fd.try_clone_to_owned().ok()?,
                    size,
                    users: 0,
                    up: None,
                });
            }
            let pool = self.pools.get_mut(&id)?;
            pool.size = pool.size.max(size);
            pool.users = pool.users.saturating_add(1);
            Some(Source::Shm {
                pool: id,
                offset: data.offset,
                width: data.width,
                height: data.height,
                stride: data.stride,
                format: data.format.into(),
            })
        });
        match found {
            Ok(Some(source)) => Some(source),
            Ok(None) => {
                tracing::warn!("an app buffer's pool could not be read; it is not shown");
                None
            }
            Err(error) => {
                tracing::warn!(
                    ?error,
                    "an app attached a buffer of no kind the keeper knows"
                );
                None
            }
        }
    }

    /// A pool grows while the app uses it; the desktop's copy grows with it on the next push.
    fn refresh_pool(&mut self, buffer: &WlBuffer, pool: (u64, u64)) {
        let size = smithay::wayland::shm::with_buffer_pool(buffer, |_, size, _| size).ok();
        if let (Some(size), Some(kept)) = (size, self.pools.get_mut(&pool)) {
            kept.size = kept.size.max(size);
        }
    }

    /// `new` replaces `old` on a surface.
    fn replace_current(&mut self, old: Option<u64>, new: Option<u64>) {
        if let Some(buffer) = new.and_then(|key| self.kept.get_mut(&key)) {
            buffer.current = buffer.current.saturating_add(1);
            buffer.owed = true;
        }
        if let Some(key) = old {
            if let Some(buffer) = self.kept.get_mut(&key) {
                buffer.current = buffer.current.saturating_sub(1);
            }
            self.settle(key);
        }
    }

    /// Release a buffer to its client once neither a surface nor the desktop holds it, and
    /// forget it once its client destroyed it too.
    fn settle(&mut self, key: u64) {
        let Some(buffer) = self.kept.get_mut(&key) else {
            return;
        };
        if buffer.current > 0 || buffer.up_busy {
            return;
        }
        if buffer.owed {
            buffer.owed = false;
            if let Some(client) = buffer.client.as_ref().filter(|client| client.is_alive()) {
                client.release();
            }
        }
        if buffer.client.is_some() {
            return;
        }
        if let Some(buffer) = self.kept.remove(&key) {
            if let Some((_, up)) = buffer.up {
                up.destroy();
            }
            if let Source::Shm { pool, .. } = buffer.source {
                let unused = self.pools.get_mut(&pool).is_some_and(|kept| {
                    kept.users = kept.users.saturating_sub(1);
                    kept.users == 0
                });
                if unused
                    && let Some(KeptPool {
                        up: Some((_, up, _)),
                        ..
                    }) = self.pools.remove(&pool)
                {
                    up.destroy();
                }
            }
        }
    }

    pub(crate) fn client_destroyed(&mut self, id: &ObjectId) {
        let Some(key) = self.by_client.remove(id) else {
            return;
        };
        if let Some(buffer) = self.kept.get_mut(&key) {
            buffer.client = None;
        }
        self.settle(key);
    }

    pub(crate) fn up_released(&mut self, key: u64) {
        if let Some(buffer) = self.kept.get_mut(&key) {
            buffer.up_busy = false;
        }
        self.settle(key);
    }

    /// The desktop went away with every buffer it held.
    pub(crate) fn detached(&mut self) {
        for buffer in self.kept.values_mut() {
            buffer.up = None;
            buffer.up_busy = false;
        }
        for pool in self.pools.values_mut() {
            pool.up = None;
        }
        let keys: Vec<u64> = self.kept.keys().copied().collect();
        for key in keys {
            self.settle(key);
        }
    }

    /// The buffer on the current desktop, made there from the same memory when it is not yet.
    fn upstream(&mut self, key: u64, up: &Upstream) -> Option<c::wl_buffer::WlBuffer> {
        let generation = up.generation;
        let buffer = self.kept.get_mut(&key)?;
        if let Some((made, existing)) = &buffer.up
            && *made == generation
        {
            return Some(existing.clone());
        }
        let made = match &buffer.source {
            Source::Shm {
                pool,
                offset,
                width,
                height,
                stride,
                format,
            } => {
                let kept_pool = self.pools.get_mut(pool)?;
                let shm = up.shm.as_ref()?;
                let size = i32::try_from(kept_pool.size).ok()?;
                let up_pool = match &mut kept_pool.up {
                    Some((made, pool, pushed)) if *made == generation => {
                        if *pushed < kept_pool.size {
                            pool.resize(size);
                            *pushed = kept_pool.size;
                        }
                        pool.clone()
                    }
                    slot => {
                        let pool = shm.create_pool(kept_pool.fd.as_fd(), size, &up.qh, ());
                        *slot = Some((generation, pool.clone(), kept_pool.size));
                        pool
                    }
                };
                let format = c::wl_shm::Format::try_from(*format).ok()?;
                up_pool.create_buffer(
                    *offset,
                    *width,
                    *height,
                    *stride,
                    format,
                    &up.qh,
                    BufferRef(key),
                )
            }
            Source::Dmabuf(dmabuf) => {
                let factory = up.dmabuf.as_ref()?;
                let format = dmabuf.format();
                if !up
                    .dmabuf_formats
                    .as_ref()
                    .is_some_and(|formats| formats.contains(&format))
                {
                    tracing::warn!(
                        ?format,
                        "this desktop takes no dma-buf of the app's format; it shows on the next \
                         commit in one it does"
                    );
                    return None;
                }
                let params = factory.create_params(&up.qh, ());
                let modifier = u64::from(format.modifier);
                for (plane, ((fd, offset), stride)) in dmabuf
                    .handles()
                    .zip(dmabuf.offsets())
                    .zip(dmabuf.strides())
                    .enumerate()
                {
                    params.add(
                        fd,
                        u32::try_from(plane).ok()?,
                        offset,
                        stride,
                        (modifier >> 32) as u32,
                        (modifier & 0xffff_ffff) as u32,
                    );
                }
                let size = dmabuf.size();
                let flags = if dmabuf.y_inverted() {
                    DmabufFlags::Y_INVERT.bits()
                } else {
                    0
                };
                let made = params.create_immed(
                    size.w,
                    size.h,
                    format.code as u32,
                    wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_buffer_params_v1::Flags::from_bits_truncate(flags),
                    &up.qh,
                    BufferRef(key),
                );
                params.destroy();
                made
            }
        };
        buffer.up = Some((generation, made.clone()));
        Some(made)
    }

    fn mark_up_busy(&mut self, key: u64) {
        if let Some(buffer) = self.kept.get_mut(&key) {
            buffer.up_busy = true;
        }
    }
}

/// The edge of a resize as the desktop's protocol names it.
pub(crate) fn client_resize_edge(
    edges: xdg_toplevel::ResizeEdge,
) -> Option<cx::xdg_toplevel::ResizeEdge> {
    cx::xdg_toplevel::ResizeEdge::try_from(u32::from(edges)).ok()
}

impl State {
    /// Take what the client committed on `surface` into its held record.
    pub(crate) fn absorb(&mut self, surface: &WlSurface) {
        let id = surface.id();
        let (assignment, damage, frames, scale, transform, delta, opaque, input) =
            with_states(surface, |states| {
                let mut guard = states.cached_state.get::<SurfaceAttributes>();
                let attributes = guard.current();
                let taken = (
                    attributes.buffer.take(),
                    std::mem::take(&mut attributes.damage),
                    std::mem::take(&mut attributes.frame_callbacks),
                    attributes.buffer_scale,
                    u32::from(attributes.buffer_transform),
                    attributes.buffer_delta.take(),
                    region(attributes.opaque_region.as_ref()),
                    region(attributes.input_region.as_ref()),
                );
                drop(guard);
                taken
            });
        let (location, xdg) = with_states(surface, |states| {
            let location = states
                .cached_state
                .get::<SubsurfaceCachedState>()
                .current()
                .location;
            let xdg = *states.cached_state.get::<SurfaceCachedState>().current();
            ((location.x, location.y), xdg)
        });
        let sync = is_sync_subsurface(surface);
        let parent = get_parent(surface).map(|parent| parent.id());
        let new = match assignment {
            Some(BufferAssignment::NewBuffer(buffer)) => Some(self.buffers.key_for(&buffer)),
            Some(BufferAssignment::Removed) => Some(None),
            None => None,
        };
        let Some(kept) = self.surfaces.get_mut(&id) else {
            return;
        };
        let replaced = new.map(|new| {
            kept.attached = true;
            (std::mem::replace(&mut kept.buffer, new), new)
        });
        kept.damage.extend(damage);
        kept.frames
            .extend(frames.into_iter().map(|frame| (frame, 0)));
        if self.up.is_none() {
            // No desktop is showing these commits, and the next one gets the whole picture
            // (a fresh desktop is pushed every buffer and full damage). Damage from the
            // commits in between could never be used, and an app that commits without waiting
            // would otherwise collect a callback per commit until a desktop returns: keep the
            // newest callback, which wakes it once on re-attach, and complete the rest now,
            // which the protocol allows for a frame nobody will show.
            kept.damage.clear();
            while kept.frames.len() > 1 {
                let (frame, _) = kept.frames.remove(0);
                if frame.is_alive() {
                    frame.done(0);
                }
            }
        }
        kept.scale = scale;
        kept.transform = transform;
        if let Some(delta) = delta {
            let (x, y) = kept.delta.unwrap_or_default();
            kept.delta = Some((x.saturating_add(delta.x), y.saturating_add(delta.y)));
        }
        kept.opaque = opaque;
        kept.input = input;
        match &mut kept.role {
            Role::Sub(sub) => {
                match parent {
                    Some(parent) => sub.parent = parent,
                    // The `wl_subsurface` is gone; its twin goes on the parent's commit.
                    None => sub.ended = true,
                }
                sub.location = location;
                sub.sync = sync;
            }
            Role::Toplevel(top) => {
                top.geometry = xdg
                    .geometry
                    .map(|rect| (rect.loc.x, rect.loc.y, rect.size.w, rect.size.h));
                top.min = (xdg.min_size.w, xdg.min_size.h);
                top.max = (xdg.max_size.w, xdg.max_size.h);
            }
            Role::Popup(pop) => {
                pop.geometry = xdg
                    .geometry
                    .map(|rect| (rect.loc.x, rect.loc.y, rect.size.w, rect.size.h));
            }
            Role::None | Role::Cursor | Role::DragIcon => {}
        }
        if let Some((old, new)) = replaced {
            self.buffers.replace_current(old, new);
        }
    }

    /// A window's first commit while no desktop is attached is answered here, so the app maps
    /// it: the desktop shows it once one is attached. A popup cannot be placed without one and
    /// is done at once.
    pub(crate) fn first_configure_while_away(&mut self, surface: &WlSurface) {
        if self.up.is_some() {
            return;
        }
        let Some(kept) = self.surfaces.get_mut(&surface.id()) else {
            return;
        };
        match &mut kept.role {
            Role::Toplevel(top) if !top.toplevel.is_initial_configure_sent() => {
                top.toplevel.with_pending_state(|state| {
                    state.states.set(xdg_toplevel::State::Suspended);
                });
                let serial = top.toplevel.send_configure();
                top.acks.push_back((serial.into(), None));
            }
            Role::Popup(pop) if !pop.dismissed => {
                pop.dismissed = true;
                pop.popup.send_popup_done();
            }
            _ => {}
        }
    }

    /// Make sure the surface has its objects on the current desktop; false when it cannot (no
    /// desktop, no role yet, a dismissed popup).
    pub(crate) fn ensure_up(&mut self, id: &ObjectId) -> bool {
        let Some(kept) = self.surfaces.get(id) else {
            return false;
        };
        if kept.up.is_some() {
            return true;
        }
        if !self.up.as_ref().is_some_and(|up| up.globals_bound) {
            return false;
        }
        let surface = kept.surface.clone();
        // The parent first: a subsurface and a popup are made against it.
        let parent = match &kept.role {
            Role::None => return false,
            Role::Cursor | Role::DragIcon | Role::Toplevel(_) => None,
            Role::Sub(_) => get_parent(&surface),
            Role::Popup(pop) => {
                if pop.dismissed {
                    return false;
                }
                pop.popup.get_parent_surface()
            }
        };
        let parent_up = match &parent {
            Some(parent) => {
                if !self.ensure_up(&parent.id()) {
                    self.dismiss_popup(id);
                    return false;
                }
                self.surfaces
                    .get(&parent.id())
                    .and_then(|kept| kept.up.as_ref())
            }
            None => None,
        };
        let parent_surface = parent_up.map(|up| up.surface.clone());
        let parent_xdg = parent_up.and_then(|up| match &up.role {
            UpRole::Xdg(xdg) => Some(xdg.xdg.clone()),
            _ => None,
        });
        let owner = surface.client().map(|client| client.id());
        let Some(up) = self.up.as_ref() else {
            return false;
        };
        let (Some(compositor), Some(kept)) = (up.compositor.clone(), self.surfaces.get_mut(id))
        else {
            return false;
        };
        let up_surface = compositor.create_surface(&up.qh, SurfaceRef(id.clone()));
        up.own(&up_surface, owner.as_ref());
        let mut pushed = Pushed::default();
        let made = make_role(
            up,
            &kept.role,
            &up_surface,
            (parent_surface.as_ref(), parent_xdg.as_ref()),
            id,
            &mut pushed,
        );
        let Some(role) = made else {
            up_surface.destroy();
            self.dismiss_popup(id);
            return false;
        };
        if let Some(owner) = &owner {
            role.own(up, owner);
        }
        kept.up = Some(UpSurface {
            surface: up_surface,
            role,
            pushed,
            viewport: None,
            content_type: None,
            fractional_scale: None,
            xdg_decoration: None,
            kde_decoration: None,
            idle_inhibitor: None,
            shortcuts_inhibitor: None,
            dialog: None,
            modal_sent: None,
            icon: None,
            icon_pushed: false,
            color: crate::color::UpSurfaceColor::default(),
        });
        self.request_surface_feedback(id);
        self.request_fractional_scale(id);
        self.ensure_decorations(id);
        self.ensure_idle_inhibit(id);
        self.ensure_shortcuts_inhibitor(id);
        self.ensure_dialog(id);
        self.push_icon(id);
        self.ensure_up_feedback(id);
        true
    }

    /// A surface that asked for its own dma-buf feedback is answered by the desktop for the
    /// desktop's surface that mirrors it. The answer is kept while no desktop is attached and
    /// asked again on the next one.
    pub(crate) fn request_surface_feedback(&mut self, id: &ObjectId) {
        let wanted = self.surfaces.get(id).is_some_and(|kept| {
            with_states(&kept.surface, |states| {
                smithay::wayland::dmabuf::SurfaceDmabufFeedbackState::from_states(states).is_some()
            })
        });
        if wanted {
            self.ask_surface_feedback(id);
        }
    }

    /// Ask the desktop for the feedback of the surface mirroring `id`. Also called while
    /// smithay holds the surface's state for the app's own request, so it reads none of it.
    pub(crate) fn ask_surface_feedback(&mut self, id: &ObjectId) {
        let Some(up) = self.up.as_ref() else {
            return;
        };
        // A version-3 desktop has no feedback to ask for; the surface keeps what it was told.
        let Some(dmabuf) = up.dmabuf.clone().filter(|dmabuf| dmabuf.version() >= 4) else {
            return;
        };
        let Some(surface) = self
            .surfaces
            .get(id)
            .and_then(|kept| kept.up.as_ref())
            .map(|up| up.surface.clone())
        else {
            return;
        };
        self.surface_feedback.entry(id.clone()).or_default();
        dmabuf.get_surface_feedback(&surface, &up.qh, SurfaceRef(id.clone()));
    }

    pub(crate) fn dismiss_popup(&mut self, id: &ObjectId) {
        if let Some(Role::Popup(pop)) = self.surfaces.get_mut(id).map(|kept| &mut kept.role)
            && !pop.dismissed
        {
            pop.dismissed = true;
            pop.popup.send_popup_done();
        }
    }

    /// Push the held state of a surface to the desktop: only what changed since the last push
    /// to this desktop, which for a fresh desktop is everything.
    pub(crate) fn push(&mut self, id: &ObjectId) {
        if let (Some(up), Some(kept)) = (&self.up, self.surfaces.get(id)) {
            up.sending_for(&kept.surface);
        }
        // A `wl_subsurface` destroyed on its own leaves nothing to show on the desktop.
        self.forget_ended_children(id);
        if !self.ensure_up(id) {
            return;
        }
        // A window's own requests (title, parent, size limits) go before its commit.
        self.push_toplevel_meta(id);
        if self.surfaces.get(id).is_some_and(|kept| {
            matches!(kept.role, Role::Toplevel(_)) && self.xdnd.is_server(&kept.surface)
        }) {
            self.match_x11_windows(false);
        }
        self.push_icon(id);
        self.push_attributes(id);
        self.push_content(id);
        self.stack_children(id);
    }

    /// A child's `wl_subsurface` was destroyed while its surface stays: the desktop's twin
    /// goes, and the surface stops showing anything.
    fn forget_ended_children(&mut self, parent: &ObjectId) {
        let ended: Vec<ObjectId> = self
            .surfaces
            .iter()
            .filter_map(|(id, kept)| {
                let Role::Sub(sub) = &kept.role else {
                    return None;
                };
                (sub.parent == *parent && (sub.ended || get_parent(&kept.surface).is_none()))
                    .then(|| id.clone())
            })
            .collect();
        for id in ended {
            let Some(kept) = self.surfaces.get_mut(&id) else {
                continue;
            };
            kept.role = Role::None;
            kept.attached = false;
            kept.damage.clear();
            let buffer = kept.buffer.take();
            if let Some(us) = kept.up.as_mut() {
                destroy_role(std::mem::replace(&mut us.role, UpRole::Plain));
                us.pushed = Pushed::default();
            }
            self.buffers.replace_current(buffer, None);
        }
    }

    /// The parent's children, stacked on the desktop exactly as the app has them: below the
    /// parent, above it, and in the order between them.
    fn stack_children(&mut self, parent_id: &ObjectId) {
        let Some(parent) = self
            .surfaces
            .get(parent_id)
            .map(|kept| kept.surface.clone())
        else {
            return;
        };
        let order = smithay::wayland::compositor::children_in_stack_order(&parent);
        if order.len() <= 1 {
            return;
        }
        for child in &order {
            if child.id() != *parent_id {
                self.ensure_up(&child.id());
            }
        }
        let Some(parent_up) = self.up_surface(parent_id) else {
            return;
        };
        let mut previous: Option<c::wl_surface::WlSurface> = None;
        for child in &order {
            let id = child.id();
            if id == *parent_id {
                previous = Some(parent_up.clone());
                continue;
            }
            let Some(up) = self.surfaces.get(&id).and_then(|kept| kept.up.as_ref()) else {
                continue;
            };
            match (&up.role, previous.as_ref()) {
                (UpRole::Sub(sub), Some(above_this)) => sub.place_above(above_this),
                (UpRole::Sub(sub), None) => sub.place_below(&parent_up),
                _ => {}
            }
            previous = Some(up.surface.clone());
        }
    }

    fn up_surface(&self, id: &ObjectId) -> Option<c::wl_surface::WlSurface> {
        Some(self.surfaces.get(id)?.up.as_ref()?.surface.clone())
    }

    /// The surface's role state and attributes that changed: position and sync of a
    /// subsurface, window geometry, scale, transform and regions.
    fn push_attributes(&mut self, id: &ObjectId) {
        let Some(up) = self.up.as_ref() else {
            return;
        };
        let viewport = self
            .surfaces
            .get(id)
            .map(|kept| Self::viewport_of(&kept.surface));
        let content_type = self.surfaces.get(id).map(|kept| {
            with_states(&kept.surface, |states| {
                u32::from(
                    *states
                        .cached_state
                        .get::<smithay::wayland::content_type::ContentTypeSurfaceCachedState>()
                        .current()
                        .content_type(),
                )
            })
        });
        let Some(kept) = self.surfaces.get_mut(id) else {
            return;
        };
        let Some(us) = kept.up.as_mut() else {
            return;
        };
        let surface = &us.surface;
        let version = surface.version();
        match (&kept.role, &us.role) {
            (Role::Sub(sub), UpRole::Sub(up_sub)) => {
                if differs(&mut us.pushed.location, &sub.location) {
                    up_sub.set_position(sub.location.0, sub.location.1);
                }
                if differs(&mut us.pushed.sync, &sub.sync) {
                    if sub.sync {
                        up_sub.set_sync();
                    } else {
                        up_sub.set_desync();
                    }
                }
            }
            (Role::Toplevel(top), UpRole::Xdg(xdg)) => {
                if differs(&mut us.pushed.geometry, &top.geometry)
                    && let Some((x, y, w, h)) = top.geometry
                {
                    xdg.xdg.set_window_geometry(x, y, w, h);
                }
            }
            (Role::Popup(pop), UpRole::Xdg(xdg)) => {
                if differs(&mut us.pushed.geometry, &pop.geometry)
                    && let Some((x, y, w, h)) = pop.geometry
                {
                    xdg.xdg.set_window_geometry(x, y, w, h);
                }
            }
            _ => {}
        }
        if differs(&mut us.pushed.scale, &kept.scale) && version >= 3 {
            surface.set_buffer_scale(kept.scale);
        }
        if differs(&mut us.pushed.transform, &kept.transform)
            && version >= 2
            && let Ok(transform) = c::wl_output::Transform::try_from(kept.transform)
        {
            surface.set_buffer_transform(transform);
        }
        if differs(&mut us.pushed.opaque, &kept.opaque) {
            let region = up.region(kept.opaque.as_deref());
            surface.set_opaque_region(region.as_ref());
            if let Some(region) = region {
                region.destroy();
            }
        }
        if differs(&mut us.pushed.input, &kept.input) {
            let region = up.region(kept.input.as_deref());
            surface.set_input_region(region.as_ref());
            if let Some(region) = region {
                region.destroy();
            }
        }
        self.push_protocol_state(id, viewport, content_type);
        self.push_color(id);
    }

    /// A surface's viewport and content type, which the desktop has objects of its own for.
    fn push_protocol_state(
        &mut self,
        id: &ObjectId,
        viewport: Option<crate::viewporter::Viewport>,
        content_type: Option<u32>,
    ) {
        let Some(up) = self.up.as_ref() else {
            return;
        };
        let Some(kept) = self.surfaces.get_mut(id) else {
            return;
        };
        let Some(us) = kept.up.as_mut() else {
            return;
        };
        if let Some(viewport) = viewport
            && (viewport != (None, None) || us.viewport.is_some())
            && differs(&mut us.pushed.viewport, &viewport)
        {
            if let Some(known) = us.viewport.as_ref().filter(|known| known.is_alive()) {
                crate::viewporter::set_viewport(known, viewport);
            } else if let Some(manager) = up.viewporter.clone() {
                let made = crate::viewporter::make_viewport(&manager, &us.surface, &up.qh);
                crate::viewporter::set_viewport(&made, viewport);
                us.viewport = Some(made);
            }
        }
        if let Some(raw) = content_type
            && differs(&mut us.pushed.content_type, &raw)
            && let Ok(kind) =
                wayland_protocols::wp::content_type::v1::client::wp_content_type_v1::Type::try_from(
                    raw,
                )
            && let Some(manager) = up.content_type.clone()
        {
            if let Some(known) = us.content_type.as_ref().filter(|known| known.is_alive()) {
                known.set_content_type(kind);
            } else {
                let made = manager.get_surface_content_type(&us.surface, &up.qh, ());
                made.set_content_type(kind);
                us.content_type = Some(made);
            }
        }
    }

    /// The surface's content: its buffer when it changed (or the desktop never had it), the
    /// damage, the frame callbacks this desktop was not asked for yet, and the commit.
    fn push_content(&mut self, id: &ObjectId) {
        // A surface whose image description is not ready on the desktop yet shows nothing new
        // until it is, so no frame is ever shown in the wrong colour.
        if self.color_waits(id) {
            return;
        }
        let Some(up) = self.up.as_ref() else {
            return;
        };
        let generation = up.generation;
        let Some(kept) = self.surfaces.get_mut(id) else {
            return;
        };
        let Some(us) = kept.up.as_mut() else {
            return;
        };
        let version = us.surface.version();
        let may_show = match &us.role {
            UpRole::Xdg(xdg) => xdg.configured,
            UpRole::Plain | UpRole::Sub(_) => true,
        };
        let mut attach = None;
        if may_show && (kept.attached || us.pushed.buffer != Some(kept.buffer)) {
            attach = Some(kept.buffer);
        }
        let damage = std::mem::take(&mut kept.damage);
        let delta = if attach.is_some() {
            kept.delta.take()
        } else {
            None
        };
        let fresh = us.pushed.buffer != Some(kept.buffer);
        let frames: Vec<WlCallback> = kept
            .frames
            .iter_mut()
            .filter(|(_, sent)| *sent != generation)
            .map(|(frame, sent)| {
                *sent = generation;
                frame.clone()
            })
            .collect();
        let up_surface = us.surface.clone();
        if let Some(buffer) = attach {
            kept.attached = false;
            us.pushed.buffer = Some(buffer);
            if buffer.is_none()
                && let UpRole::Xdg(xdg) = &mut us.role
            {
                // A window that shows no buffer is unmapped; it maps again after a configure.
                xdg.configured = false;
            }
        }
        let owner = kept.surface.client().map(|client| client.id());
        let Some(up) = self.up.as_ref() else {
            return;
        };
        if let Some(buffer) = attach {
            let up_buffer = buffer.and_then(|key| self.buffers.upstream(key, up));
            if let (Some(made), Some(owner)) = (&up_buffer, &owner) {
                up.resource_owner(made, owner);
            }
            let (dx, dy) = delta.unwrap_or_default();
            if version >= 5 {
                if (dx, dy) != (0, 0) {
                    up_surface.offset(dx, dy);
                }
                up_surface.attach(up_buffer.as_ref(), 0, 0);
            } else {
                up_surface.attach(up_buffer.as_ref(), dx, dy);
            }
            if let Some(key) = buffer {
                self.buffers.mark_up_busy(key);
            }
        }
        if attach.is_some() && fresh {
            up_surface.damage_buffer(0, 0, i32::MAX, i32::MAX);
        } else {
            for damage in damage {
                match damage {
                    Damage::Surface(rect) => {
                        up_surface.damage(rect.loc.x, rect.loc.y, rect.size.w, rect.size.h);
                    }
                    Damage::Buffer(rect) => {
                        up_surface.damage_buffer(rect.loc.x, rect.loc.y, rect.size.w, rect.size.h);
                    }
                }
            }
        }
        for callback in frames {
            up_surface.frame(
                &up.qh,
                FrameRef {
                    surface: id.clone(),
                    callback,
                },
            );
        }
        up_surface.commit();
    }

    /// Send a window's own requests that changed: title, app id, parent, size limits,
    /// maximized and fullscreen.
    pub(crate) fn push_toplevel_meta(&mut self, id: &ObjectId) {
        let Some(kept) = self.surfaces.get(id) else {
            return;
        };
        let Role::Toplevel(top) = &kept.role else {
            return;
        };
        let parent = top.toplevel.parent();
        if let Some(parent) = &parent {
            self.ensure_up(&parent.id());
        }
        let parent_up = parent.as_ref().and_then(|parent| {
            match &self.surfaces.get(&parent.id())?.up.as_ref()?.role {
                UpRole::Xdg(xdg) => match &xdg.kind {
                    UpXdgKind::Toplevel(toplevel) => Some((parent.id(), toplevel.clone())),
                    UpXdgKind::Popup(_) => None,
                },
                _ => None,
            }
        });
        let Some(kept) = self.surfaces.get(id) else {
            return;
        };
        let Role::Toplevel(top) = &kept.role else {
            return;
        };
        let (title, app_id) = top.title_and_app_id();
        let app_id = self.upstream_app_id(app_id.as_deref());
        let Some(up) = self.up.as_ref() else {
            return;
        };
        let outputs: Vec<(String, c::wl_output::WlOutput)> = up
            .outputs
            .iter()
            .filter_map(|output| Some((output.name.clone()?, output.wl.clone())))
            .collect();
        let Some(kept) = self.surfaces.get_mut(id) else {
            return;
        };
        let (Role::Toplevel(top), Some(us)) = (&kept.role, kept.up.as_mut()) else {
            return;
        };
        let UpRole::Xdg(xdg) = &us.role else {
            return;
        };
        let UpXdgKind::Toplevel(toplevel) = &xdg.kind else {
            return;
        };
        let pushed = &mut us.pushed;
        if differs(&mut pushed.title, &title)
            && let Some(title) = &title
        {
            toplevel.set_title(title.clone());
        }
        if differs(&mut pushed.app_id, &app_id)
            && let Some(app_id) = &app_id
        {
            toplevel.set_app_id(app_id.clone());
        }
        let parent_id = parent_up.as_ref().map(|(id, _)| id.clone());
        if differs(&mut pushed.parent, &parent_id) {
            toplevel.set_parent(parent_up.as_ref().map(|(_, toplevel)| toplevel));
        }
        if differs(&mut pushed.min, &top.min) {
            toplevel.set_min_size(top.min.0, top.min.1);
        }
        if differs(&mut pushed.max, &top.max) {
            toplevel.set_max_size(top.max.0, top.max.1);
        }
        if differs(&mut pushed.maximized, &top.maximized) {
            if top.maximized {
                toplevel.set_maximized();
            } else {
                toplevel.unset_maximized();
            }
        }
        if differs(&mut pushed.fullscreen, &top.fullscreen) {
            match &top.fullscreen {
                (true, output) => {
                    let output = output.as_ref().and_then(|name| {
                        outputs
                            .iter()
                            .find(|(candidate, _)| candidate == name)
                            .map(|(_, output)| output)
                    });
                    toplevel.set_fullscreen(output);
                }
                (false, _) => toplevel.unset_fullscreen(),
            }
        }
    }

    /// The client acknowledged the configure `serial`: acknowledge the desktop's configure it
    /// carried, unless the keeper already did (a serial is acknowledged once).
    pub(crate) fn client_acked(&mut self, id: &ObjectId, serial: u32) {
        let generation = self.generation;
        let Some(kept) = self.surfaces.get_mut(id) else {
            return;
        };
        let acks = match &mut kept.role {
            Role::Toplevel(top) => &mut top.acks,
            Role::Popup(pop) => &mut pop.acks,
            _ => return,
        };
        let mut upstream = None;
        while let Some((local, carried)) = acks.pop_front() {
            if local == serial {
                upstream = carried;
                break;
            }
        }
        let Some((made, up_serial)) = upstream else {
            return;
        };
        if made != generation {
            return;
        }
        if let Some(UpSurface {
            role: UpRole::Xdg(xdg),
            ..
        }) = kept.up.as_mut()
        {
            xdg.ack(up_serial);
        }
    }

    /// The desktop configured the window or popup `id`: pass it on to the client. A window the
    /// keeper still shows (a desktop attached after it mapped) is acknowledged here at once and
    /// shown with its held buffer, without waiting for the app to draw.
    pub(crate) fn desktop_configured(&mut self, id: &ObjectId, up_serial: u32) {
        let generation = self.generation;
        let Some(kept) = self.surfaces.get_mut(id) else {
            return;
        };
        let mapped = kept.buffer.is_some();
        let Some(UpSurface {
            role: UpRole::Xdg(xdg),
            ..
        }) = kept.up.as_mut()
        else {
            return;
        };
        xdg.pending.push_back(up_serial);
        let mut show_now = false;
        match &mut kept.role {
            Role::Toplevel(top) => {
                let (width, height, states) = xdg.next_top.take().unwrap_or_default();
                let bounds = xdg.next_bounds.take();
                top.toplevel.with_pending_state(|state| {
                    state.size = (width > 0 && height > 0).then(|| (width, height).into());
                    state.states = smithay::wayland::shell::xdg::ToplevelStateSet::default();
                    for raw in states {
                        if let Ok(value) = xdg_toplevel::State::try_from(raw) {
                            state.states.set(value);
                        }
                    }
                    if let Some((w, h)) = bounds {
                        state.bounds = (w > 0 && h > 0).then(|| (w, h).into());
                    }
                });
                if mapped && !xdg.configured {
                    xdg.ack(up_serial);
                    show_now = true;
                }
                let local = top.toplevel.send_configure();
                top.acks
                    .push_back((local.into(), Some((generation, up_serial))));
            }
            Role::Popup(pop) => {
                if let Some((x, y, w, h)) = xdg.next_popup.take() {
                    pop.popup.with_pending_state(|state| {
                        state.geometry =
                            smithay::utils::Rectangle::new((x, y).into(), (w, h).into());
                    });
                }
                let local = match xdg.repositioned.take() {
                    Some(token) => Ok(pop.popup.send_repositioned(token)),
                    None => pop.popup.send_configure(),
                };
                match local {
                    Ok(local) => pop
                        .acks
                        .push_back((local.into(), Some((generation, up_serial)))),
                    Err(error) => tracing::warn!(?error, "a popup's configure could not be sent"),
                }
            }
            _ => {}
        }
        if show_now {
            self.push(id);
        }
    }

    pub(crate) fn popup_grab(&self, surface: &WlSurface, serial: u32) {
        let Some(up) = self.up.as_ref() else {
            return;
        };
        let Some(seat) = &up.seat else {
            return;
        };
        if let Some(UpSurface {
            role: UpRole::Xdg(xdg),
            ..
        }) = self
            .surfaces
            .get(&surface.id())
            .and_then(|kept| kept.up.as_ref())
            && let UpXdgKind::Popup(popup) = &xdg.kind
        {
            up.sending_for(surface);
            popup.grab(seat, serial);
        }
    }

    pub(crate) fn popup_reposition(
        &mut self,
        surface: &WlSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        let Some(kept) = self.surfaces.get_mut(&surface.id()) else {
            return;
        };
        if let Role::Popup(pop) = &mut kept.role {
            pop.positioner = positioner;
        }
        let Some(up) = self.up.as_ref() else {
            return;
        };
        if let (
            Role::Popup(pop),
            Some(UpSurface {
                role: UpRole::Xdg(xdg),
                ..
            }),
        ) = (&kept.role, kept.up.as_ref())
            && let UpXdgKind::Popup(popup) = &xdg.kind
            && popup.version() >= 3
            && let Some(positioner) = up.positioner(&pop.positioner)
        {
            popup.reposition(&positioner, token);
            positioner.destroy();
        }
    }

    pub(crate) fn toplevel_request(
        &self,
        surface: &WlSurface,
        request: impl FnOnce(&Upstream, &cx::xdg_toplevel::XdgToplevel),
    ) {
        let Some(up) = self.up.as_ref() else {
            return;
        };
        if let Some(UpSurface {
            role: UpRole::Xdg(xdg),
            ..
        }) = self
            .surfaces
            .get(&surface.id())
            .and_then(|kept| kept.up.as_ref())
            && let UpXdgKind::Toplevel(toplevel) = &xdg.kind
        {
            up.sending_for(surface);
            request(up, toplevel);
        }
    }

    /// A window or popup role object was destroyed: its desktop objects go, the surface stays.
    pub(crate) fn role_destroyed(&mut self, id: &ObjectId) {
        let Some(kept) = self.surfaces.get_mut(id) else {
            return;
        };
        if let Role::Toplevel(top) = &kept.role {
            self.placements.remove(&top.tag);
        }
        kept.role = Role::None;
        if let Some(us) = kept.up.as_mut() {
            destroy_role(std::mem::replace(&mut us.role, UpRole::Plain));
            us.pushed = Pushed::default();
        }
    }

    /// The client destroyed a surface.
    pub(crate) fn forget_surface(&mut self, id: &ObjectId) {
        let Some(kept) = self.surfaces.remove(id) else {
            return;
        };
        if let Role::Toplevel(top) = &kept.role {
            self.placements.remove(&top.tag);
            self.windows_changed = true;
        }
        self.buffers.replace_current(kept.buffer, None);
        if let Some(us) = kept.up {
            destroy_role(us.role);
            us.surface.destroy();
        }
        self.surface_feedback.remove(id);
        self.color_surface_gone(id);
        self.presentation_surface_gone(id);
        self.seat.surface_gone(id);
        self.xdnd_surface_gone(id);
    }

    /// Recreate every held surface on a desktop that was just attached, oldest first.
    pub(crate) fn push_all(&mut self) {
        let mut ids: Vec<(u64, ObjectId)> = self
            .surfaces
            .iter()
            .map(|(id, kept)| (kept.seq, id.clone()))
            .collect();
        ids.sort_by_key(|(seq, _)| *seq);
        for (_, id) in ids {
            self.push(&id);
        }
    }

    /// The desktop went away: hold everything, and tell the app its windows are suspended and
    /// its popups done.
    pub(crate) fn hold_all(&mut self) {
        self.buffers.detached();
        self.hold_activation();
        self.hold_presentation();
        self.hold_cursor_shape();
        self.hold_shortcuts_inhibit();
        for kept in self.surfaces.values_mut() {
            kept.up = None;
            match &mut kept.role {
                Role::Toplevel(top) if top.toplevel.is_initial_configure_sent() => {
                    top.toplevel.with_pending_state(|state| {
                        state.states.unset(xdg_toplevel::State::Activated);
                        state.states.set(xdg_toplevel::State::Suspended);
                    });
                    let serial = top.toplevel.send_configure();
                    top.acks.push_back((serial.into(), None));
                }
                Role::Popup(pop) if !pop.dismissed => {
                    pop.dismissed = true;
                    pop.popup.send_popup_done();
                }
                _ => {}
            }
        }
    }

    /// A test makes the keeper send the desktop a request the protocol forbids.
    #[cfg(feature = "test-support")]
    pub(crate) fn break_protocol(&self, tag: &str) -> bool {
        self.surfaces
            .values()
            .any(|kept| match (&kept.role, &kept.up) {
                (
                    Role::Toplevel(top),
                    Some(UpSurface {
                        role: UpRole::Xdg(xdg),
                        ..
                    }),
                ) if top.tag == tag => {
                    // A surface of this app breaks the protocol, as a bad request of its would:
                    // the desktop's error names this surface, which is the app's.
                    if let Some(us) = &kept.up {
                        us.surface.attach(None, 1, 0);
                        us.surface.commit();
                    }
                    true
                }
                _ => false,
            })
    }
}

impl UpXdg {
    fn new(xdg: cx::xdg_surface::XdgSurface, kind: UpXdgKind) -> Self {
        Self {
            xdg,
            kind,
            pending: VecDeque::new(),
            configured: false,
            next_top: None,
            next_bounds: None,
            next_popup: None,
            repositioned: None,
        }
    }

    /// Acknowledge `serial` (and every older one) on the desktop, once.
    fn ack(&mut self, serial: u32) {
        let Some(at) = self.pending.iter().position(|pending| *pending == serial) else {
            return;
        };
        self.pending.drain(..=at);
        self.xdg.ack_configure(serial);
        self.configured = true;
    }
}

/// The desktop's role objects for a surface of `role`, made against its parent's.
fn make_role(
    up: &Upstream,
    role: &Role,
    surface: &c::wl_surface::WlSurface,
    (parent, parent_xdg): (
        Option<&c::wl_surface::WlSurface>,
        Option<&cx::xdg_surface::XdgSurface>,
    ),
    id: &ObjectId,
    pushed: &mut Pushed,
) -> Option<UpRole> {
    let data = || SurfaceRef(id.clone());
    Some(match role {
        Role::None | Role::Cursor | Role::DragIcon => UpRole::Plain,
        Role::Sub(_) => {
            let sub = up
                .subcompositor
                .as_ref()?
                .get_subsurface(surface, parent?, &up.qh, data());
            // A new subsurface is synchronized, as the protocol starts every one.
            pushed.sync = Some(true);
            UpRole::Sub(sub)
        }
        Role::Toplevel(top) => {
            let xdg = up
                .wm_base
                .as_ref()?
                .get_xdg_surface(surface, &up.qh, data());
            let toplevel = xdg.get_toplevel(&up.qh, data());
            // Before its first commit, so the desktop knows the window by its tag when it maps.
            if let Some(tags) = &up.toplevel_tag {
                tags.set_toplevel_tag(&toplevel, top.tag.clone());
            }
            pushed.maximized = Some(false);
            pushed.fullscreen = Some((false, None));
            pushed.min = Some((0, 0));
            pushed.max = Some((0, 0));
            UpRole::Xdg(Box::new(UpXdg::new(xdg, UpXdgKind::Toplevel(toplevel))))
        }
        Role::Popup(pop) => {
            let parent_xdg = parent_xdg?;
            let positioner = up.positioner(&pop.positioner)?;
            let xdg = up
                .wm_base
                .as_ref()?
                .get_xdg_surface(surface, &up.qh, data());
            let popup = xdg.get_popup(Some(parent_xdg), &positioner, &up.qh, data());
            positioner.destroy();
            UpRole::Xdg(Box::new(UpXdg::new(xdg, UpXdgKind::Popup(popup))))
        }
    })
}

impl UpRole {
    /// Record the app connection these objects were made for.
    fn own(&self, up: &Upstream, owner: &smithay::reexports::wayland_server::backend::ClientId) {
        match self {
            Self::Plain => {}
            Self::Sub(sub) => up.resource_owner(sub, owner),
            Self::Xdg(xdg) => {
                up.resource_owner(&xdg.xdg, owner);
                match &xdg.kind {
                    UpXdgKind::Toplevel(toplevel) => up.resource_owner(toplevel, owner),
                    UpXdgKind::Popup(popup) => up.resource_owner(popup, owner),
                }
            }
        }
    }
}

fn destroy_role(role: UpRole) {
    match role {
        UpRole::Plain => {}
        UpRole::Sub(sub) => sub.destroy(),
        UpRole::Xdg(xdg) => {
            match xdg.kind {
                UpXdgKind::Toplevel(toplevel) => toplevel.destroy(),
                UpXdgKind::Popup(popup) => popup.destroy(),
            }
            xdg.xdg.destroy();
        }
    }
}

/// The positioner's rules in the desktop's protocol.
pub(crate) fn positioner_rules(
    positioner: &PositionerState,
    up: &cx::xdg_positioner::XdgPositioner,
) {
    let size = positioner.rect_size;
    up.set_size(size.w.max(1), size.h.max(1));
    let anchor = positioner.anchor_rect;
    up.set_anchor_rect(
        anchor.loc.x,
        anchor.loc.y,
        anchor.size.w.max(1),
        anchor.size.h.max(1),
    );
    if let Ok(edges) = cx::xdg_positioner::Anchor::try_from(u32::from(positioner.anchor_edges)) {
        up.set_anchor(edges);
    }
    if let Ok(gravity) = cx::xdg_positioner::Gravity::try_from(u32::from(positioner.gravity)) {
        up.set_gravity(gravity);
    }
    up.set_constraint_adjustment(
        cx::xdg_positioner::ConstraintAdjustment::from_bits_truncate(
            positioner.constraint_adjustment.bits(),
        ),
    );
    up.set_offset(positioner.offset.x, positioner.offset.y);
    if up.version() >= 3 {
        if positioner.reactive {
            up.set_reactive();
        }
        if let Some(parent) = positioner.parent_size {
            up.set_parent_size(parent.w, parent.h);
        }
    }
}
