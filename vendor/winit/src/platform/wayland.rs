//! # Wayland
//!
//! **Note:** Windows don't appear on Wayland until you draw/present to them.
//!
//! By default, Winit loads system libraries using `dlopen`. This can be
//! disabled by disabling the `"wayland-dlopen"` cargo feature.
//!
//! ## Client-side decorations
//!
//! Winit provides client-side decorations by default, but the behaviour can
//! be controlled with the following feature flags:
//!
//! * `wayland-csd-adwaita` (default).
//! * `wayland-csd-adwaita-crossfont`.
//! * `wayland-csd-adwaita-notitle`.

use std::ffi::c_void;
use std::ptr::NonNull;

use crate::event_loop::{ActiveEventLoop, EventLoop, EventLoopBuilder};
use crate::monitor::MonitorHandle;
use crate::window::{Window, WindowAttributes};

pub use crate::window::Theme;

/// Additional methods on [`ActiveEventLoop`] that are specific to Wayland.
pub trait ActiveEventLoopExtWayland {
    /// True if the [`ActiveEventLoop`] uses Wayland.
    fn is_wayland(&self) -> bool;
}

impl ActiveEventLoopExtWayland for ActiveEventLoop {
    #[inline]
    fn is_wayland(&self) -> bool {
        self.p.is_wayland()
    }
}

/// Additional methods on [`EventLoop`] that are specific to Wayland.
pub trait EventLoopExtWayland {
    /// True if the [`EventLoop`] uses Wayland.
    fn is_wayland(&self) -> bool;
}

impl<T: 'static> EventLoopExtWayland for EventLoop<T> {
    #[inline]
    fn is_wayland(&self) -> bool {
        self.event_loop.is_wayland()
    }
}

/// Additional methods on [`EventLoopBuilder`] that are specific to Wayland.
pub trait EventLoopBuilderExtWayland {
    /// Force using Wayland.
    fn with_wayland(&mut self) -> &mut Self;

    /// Whether to allow the event loop to be created off of the main thread.
    ///
    /// By default, the window is only allowed to be created on the main
    /// thread, to make platform compatibility easier.
    fn with_any_thread(&mut self, any_thread: bool) -> &mut Self;
}

impl<T> EventLoopBuilderExtWayland for EventLoopBuilder<T> {
    #[inline]
    fn with_wayland(&mut self) -> &mut Self {
        self.platform_specific.forced_backend = Some(crate::platform_impl::Backend::Wayland);
        self
    }

    #[inline]
    fn with_any_thread(&mut self, any_thread: bool) -> &mut Self {
        self.platform_specific.any_thread = any_thread;
        self
    }
}

/// Outcome of the exact surface commit observed through `wp_presentation`.
/// Times use the clock advertised by the compositor, never callback receipt time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaylandPresentation {
    Presented {
        clock_id: u32,
        timestamp: std::time::Duration,
        refresh: std::time::Duration,
        sequence: u64,
        flags: u32,
    },
    Discarded,
}

impl WaylandPresentation {
    #[doc(hidden)]
    pub fn from_protocol(
        clock_id: Option<u32>,
        time: (u32, u32, u32),
        refresh: u32,
        sequence: (u32, u32),
        flags: Option<u32>,
    ) -> Self {
        match (clock_id, flags) {
            (Some(clock_id), Some(flags)) if time.2 < 1_000_000_000 => Self::Presented {
                clock_id,
                timestamp: std::time::Duration::new(
                    (u64::from(time.0) << 32) | u64::from(time.1),
                    time.2,
                ),
                refresh: std::time::Duration::from_nanos(u64::from(refresh)),
                sequence: (u64::from(sequence.0) << 32) | u64::from(sequence.1),
                flags,
            },
            _ => Self::Discarded,
        }
    }
}

/// One-shot observation of the next surface commit. No pixels or input text.
pub type WaylandPresentationObserver = Box<dyn FnOnce(WaylandPresentation) + Send + 'static>;

/// A field's `zwp_text_input_v3` content type: the `content_hint` bits and
/// the `content_purpose` value, as the protocol defines them. Unknown hint
/// bits are dropped and an unknown purpose is sent as `normal`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ImeContentType {
    pub hint: u32,
    pub purpose: u32,
}

/// Additional methods on [`Window`] that are specific to Wayland.
///
/// [`Window`]: crate::window::Window
pub trait WindowExtWayland {
    /// Send this content type to the input method instead of the one
    /// [`Window::set_ime_purpose`](crate::window::Window::set_ime_purpose)
    /// stands for; `None` goes back to the purpose's.
    fn set_ime_content_type(&self, content: Option<ImeContentType>);
    /// Publish a private, ephemeral equality stamp for the original editable field.
    /// An empty stamp disables admission. Does not grant microphone or IME ownership.
    fn private_text_input(&self, snapshot: Option<[u8; 32]>) -> bool;
    /// Resolve the exact original request after its actual renderer result.
    /// Outcome is accepted (0), known rejection (1), or unknown delivery (2).
    fn acknowledge_private_text_input(&self, serial: u32, outcome: u32);

    /// Observe the next committed buffer with the standard presentation protocol.
    /// Holds the native window state lock across the request and `present` so an
    /// IME/configure commit cannot take the feedback for an unrelated buffer.
    /// Always calls `present`; returns false when the protocol is unavailable.
    /// A frame callback or client submission never completes this observer.
    fn present_with_feedback(
        &self,
        observer: WaylandPresentationObserver,
        present: impl FnOnce(),
    ) -> bool;

    /// Returns `xdg_toplevel` of the window or [`None`] if the window is X11 window.
    fn xdg_toplevel(&self) -> Option<NonNull<c_void>>;

    /// Start a drag-and-drop from this window, offering `offers` as
    /// mime type → bytes.
    ///
    /// Call it while a pointer button is held — the compositor validates
    /// the drag against that implicit grab, so a drag started without one
    /// is ignored. The receiving application asks for whichever offered
    /// mime type it understands and is handed those bytes.
    ///
    /// Bytes rather than a file path, because the receiver may be a
    /// sandboxed app in its own mount namespace where a path from this
    /// process does not resolve. Offer a `text/uri-list` too if the
    /// receiver may want a name, but never offer only that.
    ///
    /// Returns whether the drag was started: `false` on X11, when no
    /// pointer is on the seat, when the compositor has no
    /// `wl_data_device_manager`, or when `offers` is empty.
    fn start_drag(&self, offers: Vec<(String, Vec<u8>)>) -> bool;

    /// Whether the compositor draws this window's decorations, as settled by
    /// `xdg-decoration` in the last configure. `None` before the first
    /// configure. An X11 window manager always decorates.
    fn server_side_decorations(&self) -> Option<bool>;
}

impl WindowExtWayland for Window {
    fn set_ime_content_type(&self, content: Option<ImeContentType>) {
        match &self.window {
            #[cfg(x11_platform)]
            crate::platform_impl::Window::X(_) => (),
            #[cfg(wayland_platform)]
            crate::platform_impl::Window::Wayland(window) => window.set_ime_content_type(content),
        }
    }
    fn private_text_input(&self, snapshot: Option<[u8; 32]>) -> bool {
        match &self.window {
            #[cfg(x11_platform)]
            crate::platform_impl::Window::X(_) => false,
            #[cfg(wayland_platform)]
            crate::platform_impl::Window::Wayland(window) => window.private_text_input(snapshot),
        }
    }
    fn acknowledge_private_text_input(&self, serial: u32, outcome: u32) {
        match &self.window {
            #[cfg(x11_platform)]
            crate::platform_impl::Window::X(_) => (),
            #[cfg(wayland_platform)]
            crate::platform_impl::Window::Wayland(window) => {
                window.acknowledge_private_text_input(serial, outcome)
            }
        }
    }

    fn present_with_feedback(
        &self,
        observer: WaylandPresentationObserver,
        present: impl FnOnce(),
    ) -> bool {
        match &self.window {
            #[cfg(x11_platform)]
            crate::platform_impl::Window::X(_) => {
                present();
                false
            }
            #[cfg(wayland_platform)]
            crate::platform_impl::Window::Wayland(window) => {
                window.present_with_feedback(observer, present)
            }
        }
    }

    #[inline]
    fn xdg_toplevel(&self) -> Option<NonNull<c_void>> {
        #[allow(clippy::single_match)]
        match &self.window {
            #[cfg(x11_platform)]
            crate::platform_impl::Window::X(_) => None,
            #[cfg(wayland_platform)]
            crate::platform_impl::Window::Wayland(window) => window.xdg_toplevel(),
        }
    }

    #[inline]
    fn start_drag(&self, offers: Vec<(String, Vec<u8>)>) -> bool {
        #[allow(clippy::single_match)]
        match &self.window {
            #[cfg(x11_platform)]
            crate::platform_impl::Window::X(_) => false,
            #[cfg(wayland_platform)]
            crate::platform_impl::Window::Wayland(window) => window.start_drag(offers),
        }
    }

    #[inline]
    fn server_side_decorations(&self) -> Option<bool> {
        #[allow(clippy::single_match)]
        match &self.window {
            #[cfg(x11_platform)]
            crate::platform_impl::Window::X(_) => Some(true),
            #[cfg(wayland_platform)]
            crate::platform_impl::Window::Wayland(window) => window.server_side_decorations(),
        }
    }
}

/// Content-free identity of a key delivered by the Wayland compositor.
/// The timestamp is the protocol's wrapping millisecond clock, not an
/// assumed Unix/monotonic epoch. Repeats and synthesized keys have no stamp.
pub trait KeyEventExtWayland {
    fn wayland_serial_time(&self) -> Option<(u32, u32)>;
}

impl KeyEventExtWayland for crate::event::KeyEvent {
    fn wayland_serial_time(&self) -> Option<(u32, u32)> {
        self.platform_specific.wayland_stamp
    }
}

/// Additional methods on [`WindowAttributes`] that are specific to Wayland.
pub trait WindowAttributesExtWayland {
    /// Build window with the given name.
    ///
    /// The `general` name sets an application ID, which should match the `.desktop`
    /// file distributed with your program. The `instance` is a `no-op`.
    ///
    /// For details about application ID conventions, see the
    /// [Desktop Entry Spec](https://specifications.freedesktop.org/desktop-entry-spec/desktop-entry-spec-latest.html#desktop-file-id)
    fn with_name(self, general: impl Into<String>, instance: impl Into<String>) -> Self;
}

impl WindowAttributesExtWayland for WindowAttributes {
    #[inline]
    fn with_name(mut self, general: impl Into<String>, instance: impl Into<String>) -> Self {
        self.platform_specific.name = Some(crate::platform_impl::ApplicationName::new(
            general.into(),
            instance.into(),
        ));
        self
    }
}

/// Additional methods on `MonitorHandle` that are specific to Wayland.
pub trait MonitorHandleExtWayland {
    /// Returns the inner identifier of the monitor.
    fn native_id(&self) -> u32;

    /// The display's physical size in millimetres, when the compositor
    /// reports one. Lets a client tell a 14" panel from a 65" television
    /// that happen to share a resolution.
    fn physical_size_mm(&self) -> Option<(u32, u32)>;
}

impl MonitorHandleExtWayland for MonitorHandle {
    #[inline]
    fn native_id(&self) -> u32 {
        self.inner.native_identifier()
    }

    #[inline]
    fn physical_size_mm(&self) -> Option<(u32, u32)> {
        self.inner.physical_size_mm()
    }
}

#[doc(hidden)]
#[cfg(wayland_platform)]
pub use crate::platform_impl::wayland::private_text_input_protocol;
