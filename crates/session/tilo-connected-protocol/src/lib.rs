//! Bounded, transport-independent candidate framing for Connected folders.
//!
//! Parsing an [`Envelope`] or [`NativeString`] establishes only structural
//! validity. It grants no path access, identity, permission, consent, durable
//! operation result, or replay authority.

mod codec;
mod envelope;
mod native_string;

pub use codec::{DecodeStatus, FrameDecoder, encode_frame};
pub use envelope::{CodecError, Envelope, EnvelopeKind};
pub use native_string::NativeString;

/// Maximum JSON envelope body size, excluding the four-byte frame prefix.
pub const MAX_FRAME_BODY_BYTES: usize = 262_144;

/// Maximum JSON object/array nesting accepted across a complete envelope.
pub const MAX_JSON_DEPTH: usize = 64;
