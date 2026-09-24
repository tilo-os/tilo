use std::{cmp, fmt::Debug, io};

use bytes::{Buf, BufMut, Bytes, BytesMut};

use crate::{Error, coding::*};

/// A reader for decoding messages from a stream.
pub struct Reader<S: web_transport_trait::RecvStream, V> {
	stream: S,
	buffer: BytesMut,
	version: V,
	max_decode_bytes: usize,
}

impl<S: web_transport_trait::RecvStream, V> Reader<S, V> {
	pub fn new(stream: S, version: V) -> Self {
		Self {
			stream,
			buffer: Default::default(),
			version,
			max_decode_bytes: usize::MAX,
		}
	}

	pub(crate) fn with_decode_limit(mut self, maximum: usize) -> Self {
		self.max_decode_bytes = maximum;
		self
	}

	/// Decode the next message from the stream.
	pub async fn decode<T: Decode<V> + Debug>(&mut self) -> Result<T, Error>
	where
		V: Clone,
	{
		loop {
			let mut cursor = io::Cursor::new(&self.buffer);
			match T::decode(&mut cursor, self.version.clone()) {
				Ok(msg) => {
					self.buffer.advance(cursor.position() as usize);
					return Ok(msg);
				}
				Err(DecodeError::Short) => {
					// Try to read more data
					if !self.read_more().await? {
						// Stream closed while we still need more data
						return Err(DecodeError::Short.into());
					}
				}
				Err(e) => return Err(e.into()),
			}
		}
	}

	/// Decode the next message unless the stream is closed.
	///
	/// Cancel-safe with the transports we ship (`web_transport_quinn`, `qmux`).
	/// The only `.await` points are reads from the underlying transport; partial
	/// bytes accumulate in `self.buffer` and a re-entry resumes decoding from
	/// the same position, so dropping the future mid-message never desynchronizes
	/// the stream. This requires the transport's `read_buf` to be cancel-safe
	/// (Quinn's `RecvStream::read` is documented as such, and qmux's
	/// `RecvStream::read_chunk` is a `tokio::sync::mpsc::Receiver::recv`).
	/// New transport impls must preserve this property.
	pub async fn decode_maybe<T: Decode<V> + Debug>(&mut self) -> Result<Option<T>, Error>
	where
		V: Clone,
	{
		if !self.has_more().await? {
			return Ok(None);
		}

		Ok(Some(self.decode().await?))
	}

	/// Decode the next message from the stream without consuming it.
	pub async fn decode_peek<T: Decode<V> + Debug>(&mut self) -> Result<T, Error>
	where
		V: Clone,
	{
		loop {
			let mut cursor = io::Cursor::new(&self.buffer);
			match T::decode(&mut cursor, self.version.clone()) {
				Ok(msg) => return Ok(msg),
				Err(DecodeError::Short) => {
					// Try to read more data
					if !self.read_more().await? {
						// Stream closed while we still need more data
						return Err(DecodeError::Short.into());
					}
				}
				Err(e) => return Err(e.into()),
			}
		}
	}

	/// Peek the next message unless the stream is closed.
	///
	/// The pair of [`Self::decode_maybe`] and [`Self::decode_peek`]: it blocks until the
	/// message arrives or the peer finishes the stream, which is what lets a caller branch
	/// on the first message without consuming it.
	pub async fn decode_peek_maybe<T: Decode<V> + Debug>(&mut self) -> Result<Option<T>, Error>
	where
		V: Clone,
	{
		if !self.has_more().await? {
			return Ok(None);
		}

		Ok(Some(self.decode_peek().await?))
	}

	/// Read the next chunk, draining the reader's internal buffer first.
	pub async fn read_chunk(&mut self, max: usize) -> Result<Option<Bytes>, Error> {
		if !self.buffer.is_empty() {
			let n = cmp::min(self.buffer.len(), max);
			return Ok(Some(self.buffer.split_to(n).freeze()));
		}
		self.stream.read_chunk(max).await.map_err(Error::from_transport)
	}

	/// Reject an advertised length before allocating or reading its body.
	pub async fn read_exact_bounded(&mut self, size: usize, max: usize) -> Result<Bytes, Error> {
		if size > max {
			return Err(DecodeError::MessageTooLarge { size, max }.into());
		}
		self.read_exact(size).await
	}

	/// Read exactly the given number of bytes from the stream.
	pub async fn read_exact(&mut self, size: usize) -> Result<Bytes, Error> {
		// An optimization to avoid a copy if we have enough data in the buffer
		if self.buffer.len() >= size {
			return Ok(self.buffer.split_to(size).freeze());
		}

		let data = BytesMut::with_capacity(size.min(u16::MAX as usize));
		let mut buf = data.limit(size);

		let size = cmp::min(buf.remaining_mut(), self.buffer.len());
		let data = self.buffer.split_to(size);
		buf.put(data);

		while buf.has_remaining_mut() {
			match self.stream.read_buf(&mut buf).await {
				Ok(Some(_)) => {}
				Ok(None) => return Err(DecodeError::Short.into()),
				Err(e) => return Err(Error::from_transport(e)),
			}
		}

		Ok(buf.into_inner().freeze())
	}

	/// Wait until the stream is closed, erroring if there are any additional bytes.
	pub async fn closed(&mut self) -> Result<(), Error> {
		if self.has_more().await? {
			return Err(DecodeError::Short.into());
		}

		Ok(())
	}

	/// Returns true if there is more data available in the buffer or stream.
	async fn has_more(&mut self) -> Result<bool, Error> {
		if !self.buffer.is_empty() {
			return Ok(true);
		}

		self.read_more().await
	}

	/// Try to read more data from the stream. Returns true if data was read, false if stream closed.
	async fn read_more(&mut self) -> Result<bool, Error> {
		let remaining = self.max_decode_bytes.saturating_sub(self.buffer.len());
		if remaining == 0 {
			return Err(DecodeError::MessageTooLarge {
				size: self.buffer.len().saturating_add(1),
				max: self.max_decode_bytes,
			}
			.into());
		}
		// A transport cannot cause speculative read-ahead allocations beyond the
		// decoder's ceiling. Incremental reads also keep unknown headers cheap.
		let mut buffer = (&mut self.buffer).limit(remaining.min(4096));
		match self.stream.read_buf(&mut buffer).await {
			Ok(Some(_)) => Ok(true),
			Ok(None) => Ok(false),
			Err(e) => Err(Error::from_transport(e)),
		}
	}

	/// Abort the stream with the given error.
	pub fn abort(&mut self, err: &Error) {
		self.stream.stop(err.to_code());
	}

	/// Abort the stream with a raw application code.
	///
	/// [`Self::abort`] encodes the moq-lite error space. A protocol with its own registry
	/// of stream reset codes has to name one directly, since the two spaces do not agree
	/// on what a given number means.
	pub fn stop(&mut self, code: u32) {
		self.stream.stop(code);
	}

	/// Cast the reader to a different version, used during version negotiation.
	pub fn with_version<V2>(self, version: V2) -> Reader<S, V2> {
		Reader {
			stream: self.stream,
			buffer: self.buffer,
			max_decode_bytes: self.max_decode_bytes,
			version,
		}
	}
}

#[cfg(test)]
mod decode_limit_tests {
	use super::*;
	use web_transport_trait::Session;
	#[derive(Debug)]
	struct Incomplete;
	impl Decode<()> for Incomplete {
		fn decode<B: Buf>(_buf: &mut B, _version: ()) -> Result<Self, DecodeError> {
			Err(DecodeError::Short)
		}
	}
	#[tokio::test]
	async fn wire_limits_decode_buffer_stops_before_growing_past_ceiling() {
		let peer = crate::lite::test_transport::ScriptedSession::eof(vec![0; 16384]);
		let (_, recv) = peer.open_bi().await.unwrap();
		let mut reader = Reader::new(recv, ()).with_decode_limit(8192);
		let result = reader.decode::<Incomplete>().await;
		assert!(matches!(
			result,
			Err(Error::Decode(DecodeError::MessageTooLarge { max: 8192, .. }))
		));
		assert!(reader.buffer.len() <= 8192);
	}
}
