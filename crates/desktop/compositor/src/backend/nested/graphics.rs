//! What draws into the session's window: an EGL context and its renderer, and the window's EGL
//! surface, made on the winit window smithay opened and made again on that same window when they
//! are lost.
//!
//! smithay's `WinitGraphicsBackend` holds the window's surface for good. Its own repair after a
//! failed swap makes the new EGL surface before it destroys the old one, and EGL makes one surface
//! per native window at a time (Mesa answers a second with `EGL_BAD_ALLOC`), so a window whose swap
//! failed stays without a surface. And winit opens one event loop per process, and the desktop
//! hands the session one connection, so a second window cannot be opened either. So the window's
//! graphics are the session's own: [`WindowGraphics`] draws as smithay's did, and dropping it
//! destroys the surface first, so the next one can be made on the same window.

use std::cell::RefCell;
use std::ptr::NonNull;
use std::rc::Rc;

use smithay::backend::egl::context::{GlAttributes, PixelFormatRequirements};
use smithay::backend::egl::native::XlibWindow;
use smithay::backend::egl::{EGLContext, EGLDisplay, EGLSurface};
use smithay::backend::renderer::Bind as _;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTarget};
use smithay::backend::winit::{WinitEvent, WinitEventLoop};
use smithay::reexports::calloop::{
    EventIterator, EventSource, Poll, PostAction, Readiness, Token, TokenFactory,
};
use smithay::utils::{Physical, Rectangle, Size};
use winit::raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
use winit::window::Window as WinitWindow;

/// The context smithay's window backend asks for: GLES 3, no vsync (the session paces its frames).
const ATTRIBUTES: GlAttributes = GlAttributes {
    version: (3, 0),
    profile: None,
    debug: cfg!(debug_assertions),
    vsync: false,
};

/// Make OpenGL ES this thread's EGL client API, and say whether it was another.
///
/// Which API a new EGL context speaks is the thread's bound API at `eglCreateContext`, and smithay
/// binds GLES only as it makes a display (`EGLDisplay::new`). Anything else on the session's thread
/// that speaks EGL may bind another API, and a context made on the kept display after that is not
/// a GLES one: it lacks `GL_EXT_texture_format_BGRA8888`, a GLES extension the renderer requires,
/// which is how rebuilt windows failed. So GLES is bound before each context is made and each
/// frame is drawn and presented.
fn bind_gles() -> Result<Option<u32>, String> {
    use smithay::backend::egl::ffi::egl;
    // SAFETY: EGL is loaded (the window's display was made with it); these read and set only this
    // thread's client API.
    let was = unsafe { egl::QueryAPI() };
    if was == egl::OPENGL_ES_API {
        return Ok(None);
    }
    // SAFETY: as above.
    if unsafe { egl::BindAPI(egl::OPENGL_ES_API) } == egl::FALSE {
        // SAFETY: asked right after the refused call, on the same thread.
        let error = unsafe { egl::GetError() };
        return Err(format!("eglBindAPI(EGL_OPENGL_ES_API): {error:#x}"));
    }
    Ok(Some(was))
}

/// The window's size in physical pixels.
pub(super) fn window_size(window: &WinitWindow) -> Size<i32, Physical> {
    let size = window.inner_size();
    Size::from((
        i32::try_from(size.width).unwrap_or(i32::MAX),
        i32::try_from(size.height).unwrap_or(i32::MAX),
    ))
}

/// The window on the desktop, kept open for as long as the session draws into it.
/// The window lives in winit's event loop (smithay keeps it there), which is shared between the
/// compositor's loop that dispatches it and this, which only keeps it alive: graphics made on the
/// window never outlive it.
pub(super) struct HostRef {
    window: NonNull<WinitWindow>,
    _events: Rc<RefCell<WinitEventLoop>>,
}

impl HostRef {
    /// Keep `window`, which `events` holds, alive with `events`.
    ///
    /// # Safety
    /// `window` is the window `events` holds (smithay's `init_from_attributes` hands back both),
    /// so it lives at least as long as `events` does.
    pub(super) unsafe fn new(events: &Rc<RefCell<WinitEventLoop>>, window: &WinitWindow) -> Self {
        Self {
            window: NonNull::from(window),
            _events: Rc::clone(events),
        }
    }

    pub(super) fn window(&self) -> &WinitWindow {
        // SAFETY: the window lives as long as the event loop that holds it, which `self` keeps
        // alive (`HostRef::new`).
        unsafe { self.window.as_ref() }
    }
}

/// winit's event loop as a source of the compositor's loop, shared with the [`HostRef`] that keeps
/// its window alive.
pub(super) struct SharedEvents(pub(super) Rc<RefCell<WinitEventLoop>>);

impl EventSource for SharedEvents {
    type Event = WinitEvent;
    type Metadata = ();
    type Ret = ();
    type Error = std::io::Error;

    const NEEDS_EXTRA_LIFECYCLE_EVENTS: bool = WinitEventLoop::NEEDS_EXTRA_LIFECYCLE_EVENTS;

    fn process_events<F>(
        &mut self,
        readiness: Readiness,
        token: Token,
        callback: F,
    ) -> Result<PostAction, Self::Error>
    where
        F: FnMut(Self::Event, &mut Self::Metadata) -> Self::Ret,
    {
        self.0
            .borrow_mut()
            .process_events(readiness, token, callback)
    }

    fn register(
        &mut self,
        poll: &mut Poll,
        token_factory: &mut TokenFactory,
    ) -> smithay::reexports::calloop::Result<()> {
        self.0.borrow_mut().register(poll, token_factory)
    }

    fn reregister(
        &mut self,
        poll: &mut Poll,
        token_factory: &mut TokenFactory,
    ) -> smithay::reexports::calloop::Result<()> {
        self.0.borrow_mut().reregister(poll, token_factory)
    }

    fn unregister(&mut self, poll: &mut Poll) -> smithay::reexports::calloop::Result<()> {
        self.0.borrow_mut().unregister(poll)
    }

    fn before_sleep(&mut self) -> smithay::reexports::calloop::Result<Option<(Readiness, Token)>> {
        self.0.borrow_mut().before_sleep()
    }

    fn before_handle_events(&mut self, events: EventIterator<'_>) {
        self.0.borrow_mut().before_handle_events(events);
    }
}

/// The renderer drawing into the window, and the window's EGL surface.
pub(super) struct WindowGraphics {
    // Dropped in this order: the renderer releases its context, then the surface is destroyed, which
    // frees the window for the next one.
    renderer: GlesRenderer,
    surface: EGLSurface,
    /// Whether the display takes a frame's damage (`EGL_EXT_buffer_age` and swap with damage).
    damage_tracking: bool,
    /// The window size the surface was last drawn at.
    bind_size: Option<Size<i32, Physical>>,
    /// Whether these graphics already told that another EGL user on the thread changed its state
    /// under them (bound another API, or made its own context current): once is enough to know.
    told_foreign: bool,
}

impl WindowGraphics {
    /// Make a context and a renderer on `display`, and the window's surface, as smithay's window
    /// backend does. The window must have no other EGL surface.
    pub(super) fn new(host: &HostRef, display: &EGLDisplay) -> Result<Self, String> {
        let rebound = bind_gles()?;
        if let Some(was) = rebound {
            tracing::warn!(
                api = format_args!("{was:#x}"),
                "the thread's EGL API was not OpenGL ES as the window's context was made; bound it"
            );
        }
        let context =
            EGLContext::new_with_config(display, ATTRIBUTES, PixelFormatRequirements::_10_bit())
                .or_else(|_| {
                    EGLContext::new_with_config(
                        display,
                        ATTRIBUTES,
                        PixelFormatRequirements::_8_bit(),
                    )
                })
                .map_err(|error| format!("context: {error}"))?;
        let pixel_format = context
            .pixel_format()
            .ok_or_else(|| "context: it has no pixel format".to_owned())?;
        let window = host.window();
        let handle = window
            .window_handle()
            .map_err(|error| format!("window handle: {error}"))?;
        let surface = match handle.as_raw() {
            RawWindowHandle::Wayland(handle) => {
                let size = window_size(window);
                // SAFETY: the window's own `wl_surface`, alive as long as the window, which
                // outlives these graphics (`HostRef`).
                let native = unsafe {
                    wayland_egl::WlEglSurface::new_from_raw(
                        handle.surface.as_ptr().cast(),
                        size.w,
                        size.h,
                    )
                }
                .map_err(|error| format!("window surface: {error}"))?;
                // SAFETY: the config is the context's own, on the display it was made on.
                unsafe { EGLSurface::new(display, pixel_format, context.config_id(), native) }
            }
            RawWindowHandle::Xlib(handle) => {
                // SAFETY: as above, for the window's X11 window.
                unsafe {
                    EGLSurface::new(
                        display,
                        pixel_format,
                        context.config_id(),
                        XlibWindow(handle.window),
                    )
                }
            }
            other => return Err(format!("no EGL surface for a {other:?} window")),
        }
        .map_err(|error| format!("EGL surface: {error}"))?;
        // A renderer takes a context that is current nowhere.
        let _ = context.unbind();
        // SAFETY: the context was made above and is not current on any thread.
        let renderer =
            unsafe { GlesRenderer::new(context) }.map_err(|error| format!("renderer: {error}"))?;
        let mut graphics = Self {
            renderer,
            surface,
            damage_tracking: display.supports_damage(),
            bind_size: None,
            told_foreign: false,
        };
        graphics.present_without_waiting()?;
        Ok(graphics)
    }

    pub(super) fn renderer(&mut self) -> &mut GlesRenderer {
        &mut self.renderer
    }

    /// Present the window's frames without waiting for the desktop to ask for the next one.
    ///
    /// EGL's default swap interval is 1, and on Wayland that makes a swap wait for the desktop's
    /// frame callback for the frame before. A desktop sends none to a window it does not show
    /// (hidden, another workspace, not mapped yet), so the swap blocks the session's whole loop:
    /// its clients, its input, its MCP server and its control socket all stop after the first
    /// frame. The session paces its own frames (`next_frame`, one refresh apart, and none while
    /// idle), so the swap only hands the frame over. The interval belongs to the surface, so each
    /// new one is set.
    fn present_without_waiting(&mut self) -> Result<(), String> {
        let context = self.renderer.egl_context();
        let display = context.display().get_display_handle();
        // The interval belongs to the surface current on the context: make the window's current
        // (the renderer's own bind only makes it current as a frame is drawn).
        // SAFETY: the context and surface are the window's own, on the thread that draws it.
        unsafe { context.make_current_with_surface(&self.surface) }
            .map_err(|error| format!("make the window current: {error}"))?;
        // SAFETY: the display is the window's own, alive for this call, with its context and
        // surface current just above.
        let set = unsafe { smithay::backend::egl::ffi::egl::SwapInterval(**display, 0) };
        if set == smithay::backend::egl::ffi::egl::FALSE {
            // SAFETY: asked right after the refused call, on the same thread.
            let error = unsafe { smithay::backend::egl::ffi::egl::GetError() };
            return Err(format!(
                "present without waiting: eglSwapInterval(0): {error:#x}"
            ));
        }
        Ok(())
    }

    /// The age of the back buffer about to be drawn, 0 when unknown.
    pub(super) fn buffer_age(&self) -> usize {
        if !self.damage_tracking {
            return 0;
        }
        self.surface
            .buffer_age()
            .and_then(|age| usize::try_from(age).ok())
            .unwrap_or(0)
    }

    /// Bind the window's surface to draw a frame, at the window's current size.
    pub(super) fn bind(
        &mut self,
        window: &WinitWindow,
    ) -> Result<(&mut GlesRenderer, GlesTarget<'_>), String> {
        let rebound = bind_gles()?;
        if let Some(was) = rebound {
            self.tell_foreign(format_args!(
                "the thread's EGL API was {was:#x}, not OpenGL ES"
            ));
        }
        // The surface is resized before its context is made current: Mesa latches the back buffer
        // as it is made current.
        let size = window_size(window);
        if Some(size) != self.bind_size {
            self.surface.resize(size.w, size.h, 0, 0);
        }
        self.bind_size = Some(size);
        let target = self
            .renderer
            .bind(&mut self.surface)
            .map_err(|error| format!("bind window: {error}"))?;
        Ok((&mut self.renderer, target))
    }

    /// Log, once for these graphics, that another EGL user on the thread changed its state under
    /// the window.
    fn tell_foreign(&mut self, what: std::fmt::Arguments<'_>) {
        if !self.told_foreign {
            self.told_foreign = true;
            tracing::warn!("{what}: another EGL user on the session's thread changed it; restored");
        }
    }

    /// Hand the frame drawn since [`Self::bind`] to the host, with its damage in the output's
    /// physical space.
    pub(super) fn submit(
        &mut self,
        window: &WinitWindow,
        damage: Option<&[Rectangle<i32, Physical>]>,
    ) -> Result<(), String> {
        // A swap is of the surface current on the thread's current context (Mesa answers any other
        // with `EGL_BAD_SURFACE`). The renderer leaves the window's current after a frame, but
        // what ran since may have made another context current — a client buffer imported
        // surfaceless, another EGL user on the thread — so the window's is made current again.
        let rebound = bind_gles()?;
        if let Some(was) = rebound {
            self.tell_foreign(format_args!(
                "the thread's EGL API was {was:#x}, not OpenGL ES"
            ));
        }
        let context = self.renderer.egl_context();
        if !(context.is_current() && self.surface.is_current()) {
            // SAFETY: the window's own context and surface, on the thread that draws them.
            unsafe { context.make_current_with_surface(&self.surface) }
                .map_err(|error| format!("make the window current to present: {error}"))?;
            self.tell_foreign(format_args!(
                "the window's context and surface were not current at present"
            ));
        }
        // The window's buffer is counted from its bottom edge.
        let mut damage = match (damage, self.bind_size) {
            (Some(damage), Some(size)) if self.damage_tracking && !damage.is_empty() => Some(
                damage
                    .iter()
                    .map(|rect| {
                        Rectangle::new(
                            (
                                rect.loc.x,
                                size.h
                                    .saturating_sub(rect.loc.y)
                                    .saturating_sub(rect.size.h),
                            )
                                .into(),
                            rect.size,
                        )
                    })
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        };
        // The desktop is asked for a frame callback.
        window.pre_present_notify();
        self.surface
            .swap_buffers(damage.as_deref_mut())
            .map_err(|error| format!("present window: {error}"))
    }
}
