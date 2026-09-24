//! An implementation of the IETF MoQ specification.
//!
//! Not all features are supported; just to provide compatibility with the crate API.
//!
//! You should not use this module directly; see [crate] for the high-level API.

#[macro_use]
mod parameters;
mod adapter;
mod budget;
pub mod cluster;
mod control;
mod datagram;
mod error;
mod fetch;
mod filter;
mod goaway;
// Local patch: server drain gate (GOAWAY, client-first close).
mod drain;
pub(crate) use drain::Drain;
mod group;
mod location;
pub mod message;
mod namespace;
pub mod peer;
mod priority;
mod properties;
mod publish;
mod publish_namespace;
mod publisher;
mod request;
mod session;
pub mod solicit;
mod subscribe;
mod subscribe_namespace;
mod subscriber;
mod track;
mod version;

use control::Control;
pub use datagram::ObjectDatagram;
pub use fetch::*;
pub use filter::*;
pub use goaway::*;
pub use group::*;
pub use location::*;
pub use message::Message;
pub use parameters::*;
pub use properties::Properties;
pub use publish::*;
pub use publish_namespace::*;
use publisher::*;
pub use request::*;
pub use session::*;
pub use subscribe::*;
pub use subscribe_namespace::*; // includes PublishBlocked
use subscriber::*;
pub use track::*;
pub use version::Version;

// Cloud voice profile resource ceilings. SETUP has its own u16 wire bound and
// separate 8 KiB opaque authorization payload bound.
pub(crate) const MAX_CONTROL_BYTES: usize = 8192;
pub(crate) const MAX_OBJECT_EXTENSION_BYTES: usize = 4096;

// SETUP's u16 body plus its maximum type/length prefix. Kept separate from
// post-admission control bodies, so a valid 8192-byte auth payload still fits.
pub(crate) const MAX_SETUP_DECODE_BYTES: usize = u16::MAX as usize + 11;
pub(crate) const MAX_CONTROL_DECODE_BYTES: usize = MAX_GOAWAY_BYTES + 11;

async fn open_control<S: web_transport_trait::Session>(session: &S, version: Version) -> Result<crate::coding::Stream<S, Version>, crate::Error> {
    let mut stream = crate::coding::Stream::open(session, version).await?;
    stream.reader = stream.reader.with_decode_limit(MAX_CONTROL_DECODE_BYTES);
    Ok(stream)
}

// GOAWAY allows 8192 URI bytes plus two maximum-width vi64 fields.
pub(crate) const MAX_GOAWAY_BYTES: usize = 8192 + 18;
pub(crate) fn control_body_limit(id: u64) -> usize {
    if id == GoAway::ID { MAX_GOAWAY_BYTES } else { MAX_CONTROL_BYTES }
}
