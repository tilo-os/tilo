//! What a device asks of a window it shows on a web page, and what the window answers: the link
//! between the device that serves a hosted page and the window process that lays the view out.
//!
//! A hosted page is a view the device opens on the app's own process for whoever opened the
//! page, as any window is shown elsewhere. Its window runs in a sandboxed window process of its
//! own that nobody draws: it lays the view out, answers the page's markup, and takes what the
//! person did on the page as the view's semantic actions. The device holds the other end of this
//! link and asks one thing at a time, so every answer follows the ask it answers.
//!
//! Each message is one frame of JSON on a stream socket, framed as a page's stream frames its
//! messages: a page whole is larger than a packet carries.

use std::io::{Read, Write};

use serde::{Deserialize, Serialize};

use super::{PageAction, PagePatch};

fn framing() -> wire_frame::Framing {
    wire_frame::Framing::little_endian(super::MAX_PAGE_FRAME_BYTES)
}

/// What the device asks the window.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WindowAsk {
    /// The page whole, after the view opens what `show` names, when the page's address names
    /// something it shows.
    Open { show: Option<String> },
    /// What the person did on the element numbered `id`, at the semantic `target` it carries:
    /// under `signed`, the call they signed for it, as the member `invoker` names, when they
    /// signed one. Each change the act makes keeps it.
    Act {
        id: u32,
        target: Option<String>,
        action: PageAction,
        signed: Option<Box<tilo_actions::remote::SignedCall>>,
        invoker: Option<String>,
    },
    /// What changed on the page since the window last answered.
    Changed,
}

/// What the window answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WindowAnswer {
    /// The page whole: the window's name, the theme's stylesheet and the body.
    Shown {
        title: String,
        css: String,
        body: String,
    },
    /// The elements that changed, each by its number with its markup, outermost first.
    Changed(Vec<PagePatch>),
}

/// Write `message` as one frame on `stream`.
///
/// # Errors
/// The message does not encode, is over [`super::MAX_PAGE_FRAME_BYTES`], or the stream refuses it.
pub fn send<T: Serialize>(stream: &mut impl Write, message: &T) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(message).map_err(std::io::Error::other)?;
    framing()
        .write(stream, &bytes)
        .map_err(std::io::Error::other)?;
    stream.flush()
}

/// The next message on `stream`; `None` once the other side is gone.
///
/// # Errors
/// The stream's error, or a frame that is not one message.
pub fn receive<T: serde::de::DeserializeOwned>(
    stream: &mut impl Read,
) -> std::io::Result<Option<T>> {
    let Some(bytes) = framing().read(stream).map_err(std::io::Error::other)? else {
        return Ok(None);
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(std::io::Error::other)
}

/// The device's end of a page's window: one ask at a time, each answered before the next.
#[cfg(unix)]
pub struct WindowLink {
    stream: std::os::unix::net::UnixStream,
}

#[cfg(unix)]
impl WindowLink {
    /// The window at the other end of `stream`.
    #[must_use]
    pub const fn over(stream: std::os::unix::net::UnixStream) -> Self {
        Self { stream }
    }

    fn ask(&mut self, ask: &WindowAsk) -> std::io::Result<WindowAnswer> {
        send(&mut self.stream, ask)?;
        receive(&mut self.stream)?.ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the page's window closed",
            )
        })
    }

    /// The page whole, as [`WindowAsk::Open`] asks it: `(title, css, body)`.
    ///
    /// # Errors
    /// The window is gone, or answered something else.
    pub fn open(&mut self, show: Option<String>) -> std::io::Result<(String, String, String)> {
        match self.ask(&WindowAsk::Open { show })? {
            WindowAnswer::Shown { title, css, body } => Ok((title, css, body)),
            WindowAnswer::Changed(_) => Err(std::io::Error::other(
                "the page's window answered an open with changes",
            )),
        }
    }

    /// What `act` changed, as [`WindowAsk::Act`] asks it.
    ///
    /// # Errors
    /// As [`Self::open`].
    pub fn act(&mut self, act: &WindowAsk) -> std::io::Result<Vec<PagePatch>> {
        self.changes(act)
    }

    /// What changed since the window last answered.
    ///
    /// # Errors
    /// As [`Self::open`].
    pub fn changed(&mut self) -> std::io::Result<Vec<PagePatch>> {
        self.changes(&WindowAsk::Changed)
    }

    fn changes(&mut self, ask: &WindowAsk) -> std::io::Result<Vec<PagePatch>> {
        match self.ask(ask)? {
            WindowAnswer::Changed(patches) => Ok(patches),
            WindowAnswer::Shown { .. } => Err(std::io::Error::other(
                "the page's window answered changes with a page whole",
            )),
        }
    }
}
