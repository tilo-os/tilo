//! A page window: an app's window that the process running the app lays out and writes as a
//! page ([`crate::page`]), for whoever looks at it through the device that started it.
//!
//! The device hands the process one connection per page, and asks on it; the process answers each
//! ask in turn, in the order asked. Every message is one postcard value behind a little-endian
//! `u32` length, at most [`crate::page::MAX_PAGE_FRAME_BYTES`] long.

use std::io::{self, Read, Write};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::page::{MAX_PAGE_FRAME_BYTES, PageAction, PagePatch};

/// What a device asks of a page window.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ask {
    /// The page whole, after activating the element whose semantic target is `show`.
    Show {
        show: Option<String>,
        /// The call its person made, as they signed it, when they signed one: what the changes
        /// the page's own show makes are made for.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signed: Option<Box<tilo_actions::remote::SignedCall>>,
        /// Who that call is made for, as the device computed it from the signer's certificate:
        /// the actor text each change names.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        invoker: Option<String>,
    },
    /// What its person did on the element numbered `id`, at the target it carries.
    Act {
        id: u32,
        target: Option<String>,
        action: PageAction,
        /// The call its person made, as they signed it: what the change the act makes is made
        /// for, and checked against at every holder.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signed: Option<Box<tilo_actions::remote::SignedCall>>,
        /// Who that call is made for, as the device computed it from the signer's certificate.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        invoker: Option<String>,
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
