//! `WebDAV` for Tilo's apps: one bounded multistatus parser, one set of
//! request bodies and one blocking client.
//!
//! Calendar sync, remote files and account probes all speak the same
//! protocol; keeping its hostile-input handling (size caps, DTD refusal,
//! nesting limits) in one place is what makes it auditable.

pub mod body;
#[cfg(feature = "client")]
pub mod client;
pub mod multistatus;

pub use body::{PropName, props};
#[cfg(feature = "client")]
pub use client::{
    Credentials, DavClient, DavResponse, Depth, Error, Options, Precondition, Redirects,
    StatusCode, Url,
};
pub use multistatus::{Limits, Multistatus, Property, Propstat, Response, XmlError};
