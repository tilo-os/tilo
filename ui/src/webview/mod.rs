//! Engine-agnostic embedded web view plumbing.
//!
//! The [`crate::widget::WebView`] widget displays frames produced by a
//! [`WebViewEngine`] backend and forwards input back into it. The engine
//! runs on its own thread (or entirely synchronously, for tests): commands
//! flow in through [`WebViewEngine::send`], rendered pixels come out
//! through a shared [`FrameSlot`], and navigation/UI state changes come
//! out as [`WebViewEvent`]s drained by the host app during `sync`.
//!
//! Backends:
//!  * [`test_engine::TestEngine`] — deterministic, dependency-free engine
//!    used by unit tests and `--shots` headless renders.
//!  * WPE `WebKit` — the qOS system engine, rendering real pages offscreen.
//!
//! Coordinates in commands are **physical pixels relative to the web
//! view's top-left corner** — the widget does the logical→physical
//! translation so engines never see toolkit layout space.

pub mod profile;
pub mod test_engine;

#[cfg(feature = "webview-adblock")]
pub mod adblock;

#[cfg(feature = "webview-wpe")]
pub mod wpe;

use std::sync::{Arc, Mutex};

/// One rendered frame from the engine: tightly-packed RGBA8 at physical
/// pixel resolution. The buffer is shared, never copied per draw — the
/// widget detects staleness via the allocation pointer.
#[derive(Clone)]
pub struct Frame {
    pub rgba: Arc<Vec<u8>>,
    pub width: u32,
    pub height: u32,
}

/// Latest-frame mailbox between an engine and the widget. Engines
/// overwrite the slot on every paint; the widget reads it on every draw.
pub type FrameSlot = Arc<Mutex<Option<Frame>>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

/// Winit-independent so engines don't take a winit dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyInput {
    /// Usually one grapheme.
    Character(String),
    Named(NamedKey),
}

/// Forwarded into the page so editing shortcuts (Ctrl+Backspace,
/// Shift+Arrow, …) reach the web content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one flag per physical modifier key, mirroring winit's own \
              modifiers state; `apps/browser` constructs this by field name \
              across the crate boundary, so reshaping it into flags is out \
              of scope for a lint fix"
)]
pub struct KeyModifiers {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub meta: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedKey {
    Enter,
    Backspace,
    Delete,
    Tab,
    Escape,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Home,
    End,
    PageUp,
    PageDown,
}

#[derive(Debug, Clone, PartialEq)]
pub enum WebViewCommand {
    /// Load the given (already normalised) URL.
    Navigate(String),
    GoBack,
    GoForward,
    Reload,
    StopLoading,
    /// Viewport size in physical pixels plus the display scale factor.
    Resize {
        width: u32,
        height: u32,
        scale_factor: f32,
    },
    MouseMove {
        x: f32,
        y: f32,
    },
    MouseButton {
        button: MouseButton,
        down: bool,
        x: f32,
        y: f32,
    },
    /// Scroll by (dx, dy) physical pixels at pointer position (x, y).
    Scroll {
        dx: f32,
        dy: f32,
        x: f32,
        y: f32,
    },
    Key {
        key: KeyInput,
        down: bool,
        modifiers: KeyModifiers,
    },
    /// Keyboard focus entered / left the page surface.
    SetFocused(bool),
    /// Compiled `WebKit` content-blocker JSON (`None` clears the filter).
    /// Backends without native content filtering ignore this.
    SetContentFilter(Option<Arc<String>>),
    /// Several compiled content-blocker JSON blobs, split because the full
    /// filter-list rule set exceeds `WebKit`'s per-filter cap. An empty
    /// `chunks` clears all filters.
    ///
    /// `version` hashes the *source* filter lists (not the compiled JSON,
    /// since compiler rule ordering isn't stable across runs); the engine
    /// caches each compiled filter under `version`+index so an unchanged
    /// rule set loads from disk instead of recompiling (~280k rules costs
    /// several seconds).
    SetContentFilters {
        version: u64,
        chunks: Arc<Vec<Arc<String>>>,
    },
    /// Scriptlet-style blocking the declarative content filter can't
    /// express (e.g. skipping `YouTube` video ads). Run at document-start
    /// in all frames.
    SetUserScripts(Arc<Vec<String>>),
    /// Find the most useful visible HTML video in the current page. The
    /// result is delivered asynchronously as [`WebViewEvent::MediaElement`]
    /// or [`WebViewEvent::MediaElementUnavailable`]. Browser chrome uses the
    /// returned viewport geometry to create a live, element-sized PiP rather
    /// than mirroring the whole page.
    RequestMediaElement {
        request_id: u64,
    },
    /// Re-resolve a previously selected media element by its stable page-local
    /// identity. Active PiP uses this without silently switching videos.
    RequestMediaElementById {
        request_id: u64,
        element_id: String,
    },
    /// Clear all persistent website data (cookies, caches, storage,
    /// service workers). Backends without a data store ignore this.
    ClearWebsiteData,
}

/// Navigation / UI state changes from the engine to the host app, folded
/// into its own state (address bar text, title, progress) during `sync`.
#[derive(Debug, Clone, PartialEq)]
pub enum WebViewEvent {
    UriChanged(String),
    TitleChanged(String),
    /// Load progress in `0.0..=1.0`.
    LoadProgress(f64),
    LoadFinished,
    LoadFailed {
        uri: String,
        error: String,
    },
    HistoryChanged {
        can_go_back: bool,
        can_go_forward: bool,
    },
    MediaElement {
        request_id: u64,
        element: MediaElementInfo,
    },
    MediaElementUnavailable {
        request_id: u64,
    },
}

/// A visible media element expressed relative to the web viewport. Keeping
/// this normalised makes it independent of physical display scale and lets
/// browser chrome map it onto the current app surface after a resize.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaElementInfo {
    /// Opaque identity stable for the lifetime of the current document.
    pub element_id: String,
    /// `[x, y, width, height]`, each relative to the viewport dimensions.
    pub viewport_rect: [f64; 4],
    pub intrinsic_size: [u32; 2],
    pub label: String,
}

/// One plane of a [`DmabufFrame`]: its own fd + layout. A GPU-tiled RGBA
/// buffer has 2 planes (pixels + compression metadata); a linear one, 1.
#[derive(Debug)]
pub struct DmabufPlane {
    pub fd: std::os::fd::OwnedFd,
    pub stride: u32,
    pub offset: u32,
}

/// A GPU frame exported as a DMA-BUF handle (the `webview-wpe` EGL export
/// path). Feature-neutral so it can cross the engine trait — only the WPE
/// engine produces one and only the wgpu import consumes it. `fourcc` +
/// `modifier` describe the pixel format and tiling.
#[derive(Debug)]
pub struct DmabufFrame {
    pub planes: Vec<DmabufPlane>,
    pub fourcc: u32,
    pub modifier: u64,
    pub width: u32,
    pub height: u32,
}

/// A running web engine instance. Implementations are `Send + Sync` so
/// the widget, the host app, and the engine's own thread can share one
/// handle.
pub trait WebViewEngine: Send + Sync {
    /// Enqueue a command. Must never block on engine work.
    fn send(&self, cmd: WebViewCommand);

    fn frames(&self) -> FrameSlot;

    /// Take all pending events, oldest first.
    fn drain_events(&self) -> Vec<WebViewEvent>;

    /// Invoked (from any thread) whenever a new frame or event is ready —
    /// typically wired to the winit event loop proxy so idle apps repaint
    /// on engine activity. Synchronous engines don't need one.
    fn set_waker(&self, _waker: Box<dyn Fn() + Send + Sync>) {}

    /// The zero-copy path, if a GPU frame is pending. Default: none — SHM
    /// and test engines deliver pixels through [`frames`](Self::frames).
    fn take_dmabuf(&self) -> Option<DmabufFrame> {
        None
    }
}

/// Shared engine handle, as stored by the widget and the host app.
pub type SharedEngine = Arc<dyn WebViewEngine>;

/// Normalise user-typed address-bar input into a loadable URL: pass
/// through explicit schemes, otherwise treat input with spaces or
/// without dots as a search query and everything else as an `https://`
/// host.
pub fn normalise_url(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
        || trimmed.starts_with("file://")
        || trimmed.starts_with("about:")
    {
        return trimmed.to_string();
    }
    let looks_like_host =
        !trimmed.contains(' ') && (trimmed.contains('.') || trimmed.starts_with("localhost"));
    if looks_like_host {
        format!("https://{trimmed}")
    } else {
        format!("https://duckduckgo.com/?q={}", urlencode(trimmed))
    }
}

/// Minimal percent-encoding for search queries (RFC 3986 unreserved set
/// stays literal, space becomes `+`).
fn urlencode(s: &str) -> String {
    use std::fmt::Write as _;

    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            // `write!` to a `String` is infallible; there is nothing
            // useful to do with the `Result`.
            _ => {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalise_passes_through_schemes() {
        assert_eq!(
            normalise_url("https://example.org/a"),
            "https://example.org/a"
        );
        assert_eq!(normalise_url("http://example.org"), "http://example.org");
        assert_eq!(normalise_url("about:blank"), "about:blank");
    }

    #[test]
    fn normalise_prepends_https_for_hosts() {
        assert_eq!(normalise_url("example.org"), "https://example.org");
        assert_eq!(normalise_url("localhost:8080"), "https://localhost:8080");
    }

    #[test]
    fn normalise_searches_for_queries() {
        assert_eq!(
            normalise_url("rust webview"),
            "https://duckduckgo.com/?q=rust+webview"
        );
        assert_eq!(
            normalise_url("what is qOS?"),
            "https://duckduckgo.com/?q=what+is+qOS%3F"
        );
    }
}
