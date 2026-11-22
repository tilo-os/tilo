//! A page window: an app's window that the process running the app lays out and writes as a
//! page ([`crate::page`]), for whoever looks at it through the device that started it.
//!
//! The device hands the process one connection per page, and asks on it; the process answers each
//! ask in turn, in the order asked. Every message is one postcard value behind a little-endian
//! `u32` length, at most [`crate::page::MAX_PAGE_FRAME_BYTES`] long.

use std::io::{self, Read, Write};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use tilo_actions::remote::{Caller, Pending};

use crate::WorkspaceId;
use crate::page::{MAX_PAGE_FRAME_BYTES, PageAction, PagePatch, PageRefusal};

/// What a device asks of a page window.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ask {
    /// The page whole, after activating the element whose semantic target is `show`.
    Show { show: Option<String> },
    /// What its person did on the element numbered `id`, at the target it carries.
    Act {
        id: u32,
        target: Option<String>,
        action: PageAction,
    },
    /// What changed on the page since it last answered.
    Changes,
}

/// What a page window answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Answer {
    /// The page whole: the theme's stylesheet and its body.
    Shown { css: String, body: String },
    /// The elements that changed, outermost first.
    Patched(Vec<PagePatch>),
    /// The ask could not be answered, and why.
    Failed(String),
}

/// Write `value` as one message.
///
/// # Errors
/// The connection's error, or a message over the limit.
pub fn write<T: Serialize>(stream: &mut impl Write, value: &T) -> io::Result<()> {
    let bytes = postcard::to_allocvec(value).map_err(io::Error::other)?;
    let len = u32::try_from(bytes.len())
        .ok()
        .filter(|len| *len as usize <= MAX_PAGE_FRAME_BYTES)
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "a page message over its limit")
        })?;
    let mut frame = Vec::with_capacity(bytes.len().saturating_add(4));
    frame.extend_from_slice(&len.to_le_bytes());
    frame.extend_from_slice(&bytes);
    stream.write_all(&frame)?;
    stream.flush()
}

/// Read one message.
///
/// # Errors
/// The connection's error, a message over the limit, or one that does not decode.
pub fn read<T: DeserializeOwned>(stream: &mut impl Read) -> io::Result<T> {
    let mut len = [0; 4];
    stream.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_PAGE_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "a page message over its limit",
        ));
    }
    let mut bytes = vec![0; len];
    stream.read_exact(&mut bytes)?;
    postcard::from_bytes(&bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// The pages of the apps a workspace installed that this device runs itself, each in the process
/// it runs the app in, for the people who look at them.
pub trait HostedPages: Send + Sync {
    /// The apps of `workspace_id` it shows pages of: each one's id and the name people call it.
    fn apps(&self, workspace_id: WorkspaceId) -> Vec<(String, String)>;

    /// Show `caller` the page of `app` (an id [`Self::apps`] named) in `workspace_id`, after
    /// activating the element whose semantic target is `show`.
    fn open(
        &self,
        workspace_id: WorkspaceId,
        app: &str,
        caller: &Caller,
        show: Option<String>,
    ) -> Pending<Result<HostedOpened, PageRefusal>>;
}

/// A page [`HostedPages::open`] opened: the page, and what it shows first. Dropping the last
/// handle to it closes it.
pub struct HostedOpened {
    pub page: std::sync::Arc<dyn HostedPage>,
    pub title: String,
    pub css: String,
    pub body: String,
}

/// One page of an app this device runs itself.
pub trait HostedPage: Send + Sync {
    /// Act on it as its person did, or with no act, answer what changed on it since: the
    /// elements that changed, outermost first.
    fn act(
        &self,
        act: Option<(u32, Option<String>, PageAction)>,
    ) -> Pending<Result<Vec<PagePatch>, PageRefusal>>;
}
