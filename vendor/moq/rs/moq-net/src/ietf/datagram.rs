//! Draft 21 OBJECT_DATAGRAM (section 11.2.1). Local patch.
//!
//! Only the Normal-status payload form with an Object ID of 0 is produced: every datagram is a
//! single-object group whose Group ID is the model datagram sequence (groups and datagrams
//! share the track's sequence namespace), so the END_OF_GROUP, ZERO_OBJECT_ID and
//! DEFAULT_PRIORITY bits are set and no Properties are sent. Decode accepts every valid Type
//! Flags combination. The caller drops (rather than closing the session over) anything
//! malformed: a lossy best-effort datagram never costs the session.

use bytes::{Buf, BufMut, Bytes};

use crate::coding::{Decode, DecodeError, Encode, EncodeError};

use super::Version;

const PROPERTIES: u64 = 0x01;
const END_OF_GROUP: u64 = 0x02;
const ZERO_OBJECT_ID: u64 = 0x04;
const DEFAULT_PRIORITY: u64 = 0x08;
const RESERVED: u64 = 0x10;
const STATUS: u64 = 0x20;
const KNOWN: u64 = PROPERTIES | END_OF_GROUP | ZERO_OBJECT_ID | DEFAULT_PRIORITY | STATUS;

/// The Type Flags sent here: END_OF_GROUP | ZERO_OBJECT_ID | DEFAULT_PRIORITY.
pub const SINGLE_OBJECT_TYPE: u64 = END_OF_GROUP | ZERO_OBJECT_ID | DEFAULT_PRIORITY;

/// A decoded OBJECT_DATAGRAM. `payload` is `None` for an Object Status datagram.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectDatagram {
	pub track_alias: u64,
	pub group_id: u64,
	pub object_id: u64,
	pub end_of_group: bool,
	pub payload: Option<Bytes>,
}

impl ObjectDatagram {
	/// A single-object group carrying `payload`.
	pub fn single(track_alias: u64, group_id: u64, payload: Bytes) -> Self {
		Self {
			track_alias,
			group_id,
			object_id: 0,
			end_of_group: true,
			payload: Some(payload),
		}
	}
}

impl Encode<Version> for ObjectDatagram {
	fn encode<W: BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		let Some(payload) = &self.payload else {
			return Err(EncodeError::InvalidState);
		};
		if self.object_id != 0 || !self.end_of_group {
			return Err(EncodeError::InvalidState);
		}
		SINGLE_OBJECT_TYPE.encode(w, version)?;
		self.track_alias.encode(w, version)?;
		self.group_id.encode(w, version)?;
		if w.remaining_mut() < payload.len() {
			return Err(EncodeError::Short);
		}
		w.put_slice(payload);
		Ok(())
	}
}

impl Decode<Version> for ObjectDatagram {
	fn decode<R: Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let kind = u64::decode(r, version)?;
		if kind > 0x7f || kind & RESERVED != 0 || kind & !KNOWN != 0 || (kind & STATUS != 0 && kind & END_OF_GROUP != 0) {
			return Err(DecodeError::InvalidValue);
		}
		let track_alias = u64::decode(r, version)?;
		let group_id = u64::decode(r, version)?;
		let object_id = match kind & ZERO_OBJECT_ID {
			0 => u64::decode(r, version)?,
			_ => 0,
		};
		if kind & DEFAULT_PRIORITY == 0 {
			if !r.has_remaining() {
				return Err(DecodeError::Short);
			}
			r.advance(1);
		}
		if kind & PROPERTIES != 0 {
			let len = usize::decode(r, version)?;
			if len == 0 || len > r.remaining() {
				return Err(DecodeError::InvalidValue);
			}
			r.advance(len);
		}
		if kind & STATUS != 0 {
			let _status = u64::decode(r, version)?;
			return Ok(Self {
				track_alias,
				group_id,
				object_id,
				end_of_group: false,
				payload: None,
			});
		}
		Ok(Self {
			track_alias,
			group_id,
			object_id,
			end_of_group: kind & END_OF_GROUP != 0,
			payload: Some(r.copy_to_bytes(r.remaining())),
		})
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn single_object_round_trip_draft21() {
		let datagram = ObjectDatagram::single(7, 1234, Bytes::from_static(b"voice"));
		let mut buf = bytes::BytesMut::new();
		datagram.encode(&mut buf, Version::Draft21).unwrap();
		assert_eq!(buf[0], SINGLE_OBJECT_TYPE as u8);
		let decoded = ObjectDatagram::decode(&mut buf.freeze(), Version::Draft21).unwrap();
		assert_eq!(decoded, datagram);
	}

	#[test]
	fn decode_skips_priority_and_properties() {
		// type PROPERTIES only (object id and priority present), alias 1, group 2, object 3,
		// priority 9, properties length 2 [0xaa 0xbb], payload "x".
		let mut raw = bytes::Bytes::from_static(&[0x01, 1, 2, 3, 9, 2, 0xaa, 0xbb, b'x']);
		let decoded = ObjectDatagram::decode(&mut raw, Version::Draft21).unwrap();
		assert_eq!(decoded.object_id, 3);
		assert_eq!(decoded.payload.as_deref(), Some(&b"x"[..]));
		assert!(!decoded.end_of_group);
	}

	#[test]
	fn decode_rejects_reserved_and_status_end_of_group() {
		for kind in [0x10u8, 0x22, 0x40] {
			let mut raw = bytes::Bytes::from(vec![kind, 1, 2, 0]);
			assert!(ObjectDatagram::decode(&mut raw, Version::Draft21).is_err(), "{kind:#x}");
		}
		let mut status = bytes::Bytes::from_static(&[0x2c, 1, 2, 0]);
		assert_eq!(ObjectDatagram::decode(&mut status, Version::Draft21).unwrap().payload, None);
	}
}
