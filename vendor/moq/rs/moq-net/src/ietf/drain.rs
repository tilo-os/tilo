//! Local patch: a drain gate for a graceful, client-first session close.
//!
//! Chromium can crash its renderer when the server closes a WebTransport session
//! while streams are in flight. A draining session instead stops all MoQ traffic at
//! the transport boundary at once: datagrams are dropped both ways, every gated
//! stream is reset or stopped on its next operation, and no stream is opened or
//! accepted. The session then sends GOAWAY on its SETUP stream (which is not gated)
//! so the peer closes first. The caller still owns the hard close after a grace.

use std::{
	sync::{
		Arc,
		atomic::{AtomicBool, Ordering},
	},
	task::Poll,
};

use bytes::{Buf, BufMut, Bytes};
use web_transport_trait::{MaybeSend, RecvStream as _, SendStream as _};

use crate::Error;

/// Stream code for a stream cut by a drain.
fn drain_code() -> u32 {
	Error::Cancel.to_code()
}

/// Set-once drain request shared by a [`crate::Session`] and its gated transport.
#[derive(Clone, Default)]
pub(crate) struct Drain {
	// Fast path for every datagram and stream operation.
	draining: Arc<AtomicBool>,
	// The GOAWAY timeout in milliseconds, once draining. Wakes the SETUP writer.
	timeout: kio::Shared<Option<u64>>,
}

impl Drain {
	/// Start draining. First call wins; returns whether this call started it.
	pub fn start(&self, timeout_ms: u64) -> bool {
		let mut slot = self.timeout.lock();
		if slot.is_some() {
			return false;
		}
		// Set before the guard drops so a woken waiter already sees the gate closed.
		self.draining.store(true, Ordering::SeqCst);
		*slot = Some(timeout_ms);
		true
	}

	pub fn is_draining(&self) -> bool {
		self.draining.load(Ordering::SeqCst)
	}

	/// Resolve with the GOAWAY timeout once draining.
	pub async fn wait(&self) -> u64 {
		let slot = self
			.timeout
			.wait(|timeout| match timeout.is_some() {
				true => Poll::Ready(()),
				false => Poll::Pending,
			})
			.await;
		(*slot).expect("waited for Some")
	}
}

/// Race `op` against the drain. `None` when the drain started first, or when `op`
/// finished after it had: a result observed after the drain is never surfaced.
async fn gated<T, F: Future<Output = T>>(drain: &Drain, op: F) -> Option<T> {
	if drain.is_draining() {
		return None;
	}
	let op = std::pin::pin!(op);
	let wait = std::pin::pin!(drain.wait());
	match futures::future::select(op, wait).await {
		futures::future::Either::Left((result, _)) if !drain.is_draining() => Some(result),
		_ => None,
	}
}

/// A transport whose MoQ traffic stops when its [`Drain`] starts.
#[derive(Clone)]
pub(crate) struct Gated<S> {
	inner: S,
	drain: Drain,
}

impl<S> Gated<S> {
	pub fn new(inner: S, drain: Drain) -> Self {
		Self { inner, drain }
	}
}

pub(crate) struct GatedSend<S> {
	inner: S,
	drain: Drain,
}

pub(crate) struct GatedRecv<S> {
	inner: S,
	drain: Drain,
}

impl<S: web_transport_trait::Session> Gated<S> {
	fn send(&self, inner: S::SendStream) -> GatedSend<S::SendStream> {
		GatedSend {
			inner,
			drain: self.drain.clone(),
		}
	}

	fn recv(&self, inner: S::RecvStream) -> GatedRecv<S::RecvStream> {
		GatedRecv {
			inner,
			drain: self.drain.clone(),
		}
	}
}

impl<S: web_transport_trait::Session> web_transport_trait::Session for Gated<S> {
	type SendStream = GatedSend<S::SendStream>;
	type RecvStream = GatedRecv<S::RecvStream>;
	type Error = S::Error;

	async fn accept_uni(&self) -> Result<Self::RecvStream, Self::Error> {
		loop {
			let mut recv = self.inner.accept_uni().await?;
			if self.drain.is_draining() {
				// Refuse whatever the peer still opens; never surface it.
				recv.stop(drain_code());
				continue;
			}
			return Ok(self.recv(recv));
		}
	}

	async fn accept_bi(&self) -> Result<(Self::SendStream, Self::RecvStream), Self::Error> {
		loop {
			let (mut send, mut recv) = self.inner.accept_bi().await?;
			if self.drain.is_draining() {
				send.reset(drain_code());
				recv.stop(drain_code());
				continue;
			}
			return Ok((self.send(send), self.recv(recv)));
		}
	}

	async fn open_bi(&self) -> Result<(Self::SendStream, Self::RecvStream), Self::Error> {
		match gated(&self.drain, self.inner.open_bi()).await {
			Some(result) => {
				let (send, recv) = result?;
				Ok((self.send(send), self.recv(recv)))
			}
			// A draining session opens nothing; the task parks until the close.
			None => std::future::pending().await,
		}
	}

	async fn open_uni(&self) -> Result<Self::SendStream, Self::Error> {
		match gated(&self.drain, self.inner.open_uni()).await {
			Some(result) => Ok(self.send(result?)),
			None => std::future::pending().await,
		}
	}

	fn send_datagram(&self, payload: Bytes) -> Result<(), Self::Error> {
		if self.drain.is_draining() {
			return Ok(());
		}
		self.inner.send_datagram(payload)
	}

	async fn recv_datagram(&self) -> Result<Bytes, Self::Error> {
		loop {
			let payload = self.inner.recv_datagram().await?;
			if !self.drain.is_draining() {
				return Ok(payload);
			}
		}
	}

	fn max_datagram_size(&self) -> usize {
		self.inner.max_datagram_size()
	}

	fn protocol(&self) -> Option<&str> {
		self.inner.protocol()
	}

	fn close(&self, code: u32, reason: &str) {
		self.inner.close(code, reason)
	}

	async fn closed(&self) -> Self::Error {
		self.inner.closed().await
	}

	fn stats(&self) -> impl web_transport_trait::Stats {
		self.inner.stats()
	}
}

impl<S: web_transport_trait::SendStream> GatedSend<S> {
	/// Reset on drain, then surface only the transport's own error for the reset
	/// stream. A backend that would still accept writes parks instead.
	async fn cut<T>(&mut self) -> Result<T, S::Error> {
		self.inner.reset(drain_code());
		match self.inner.write(&[0]).await {
			Err(err) => Err(err),
			Ok(_) => std::future::pending().await,
		}
	}
}

impl<S: web_transport_trait::SendStream> web_transport_trait::SendStream for GatedSend<S> {
	type Error = S::Error;

	async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
		match gated(&self.drain, self.inner.write(buf)).await {
			Some(result) => result,
			None => self.cut().await,
		}
	}

	// Forwarded rather than the trait defaults, so the backend's own cancel-safe
	// capacity wait and zero-copy chunk path are kept.
	async fn write_buf<B: Buf + MaybeSend>(&mut self, buf: &mut B) -> Result<usize, Self::Error> {
		match gated(&self.drain, self.inner.write_buf(buf)).await {
			Some(result) => result,
			None => self.cut().await,
		}
	}

	async fn write_chunk(&mut self, chunk: Bytes) -> Result<(), Self::Error> {
		match gated(&self.drain, self.inner.write_chunk(chunk)).await {
			Some(result) => result,
			None => self.cut().await,
		}
	}

	fn set_priority(&mut self, order: u8) {
		self.inner.set_priority(order)
	}

	fn finish(&mut self) -> Result<(), Self::Error> {
		if self.drain.is_draining() {
			// Nothing is delivered after the drain, not even a buffered tail.
			self.inner.reset(drain_code());
			return Ok(());
		}
		self.inner.finish()
	}

	fn reset(&mut self, code: u32) {
		self.inner.reset(code)
	}

	async fn closed(&mut self) -> Result<(), Self::Error> {
		self.inner.closed().await
	}
}

impl<S: web_transport_trait::RecvStream> GatedRecv<S> {
	/// Stop on drain, then surface only the transport's own error for the stopped
	/// stream. A backend that would still hand out data parks instead.
	async fn cut<T>(&mut self) -> Result<T, S::Error> {
		self.inner.stop(drain_code());
		let mut scratch = [0u8; 1];
		match self.inner.read(&mut scratch).await {
			Err(err) => Err(err),
			Ok(_) => std::future::pending().await,
		}
	}
}

impl<S: web_transport_trait::RecvStream> web_transport_trait::RecvStream for GatedRecv<S> {
	type Error = S::Error;

	async fn read(&mut self, dst: &mut [u8]) -> Result<Option<usize>, Self::Error> {
		match gated(&self.drain, self.inner.read(dst)).await {
			Some(result) => result,
			None => self.cut().await,
		}
	}

	async fn read_buf<B: BufMut + MaybeSend>(&mut self, buf: &mut B) -> Result<Option<usize>, Self::Error> {
		match gated(&self.drain, self.inner.read_buf(buf)).await {
			Some(result) => result,
			None => self.cut().await,
		}
	}

	async fn read_chunk(&mut self, max: usize) -> Result<Option<Bytes>, Self::Error> {
		match gated(&self.drain, self.inner.read_chunk(max)).await {
			Some(result) => result,
			None => self.cut().await,
		}
	}

	fn stop(&mut self, code: u32) {
		self.inner.stop(code)
	}

	async fn closed(&mut self) -> Result<(), Self::Error> {
		self.inner.closed().await
	}
}
