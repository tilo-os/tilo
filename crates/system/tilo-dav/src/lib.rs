//! `WebDAV` for Tilo's apps: one bounded multistatus parser, one set of
//! request bodies, a blocking client for desktop-only code (`client`) and an
//! async client on `tilo_net` for every target (`net`).
//!
//! Calendar sync, remote files and account probes all speak the same
//! protocol; keeping its hostile-input handling (size caps, DTD refusal,
//! nesting limits) in one place is what makes it auditable.

pub mod body;
#[cfg(feature = "client")]
pub mod client;
pub mod multistatus;
#[cfg(feature = "net")]
pub mod net;
#[cfg(any(feature = "client", feature = "net"))]
mod request;

pub use body::{PropName, props};
#[cfg(feature = "client")]
pub use client::{
    Body, Credentials, DavClient, DavResponse, Depth, Error, Options, Precondition, Redirects,
    StatusCode, Url,
};
pub use multistatus::{Limits, Multistatus, Property, Propstat, Response, XmlError};
