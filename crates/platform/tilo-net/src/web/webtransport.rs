//! WebTransport in a page: the browser's `WebTransport`, reached through
//! `Reflect` because web-sys only binds it behind its unstable-API flag.
//!
//! The types here also implement `web_transport_trait::poll`. A page has no
//! futures executor under a poll loop, so each pending `poll_*` keeps its own
//! browser promise in the handle that started it.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use js_sys::{Array, Object, Reflect, Uint8Array};
use send_wrapper::SendWrapper;
use url::Url;
use wasm_bindgen::JsCast as _;
use wasm_bindgen::JsValue;
use web_sys::{
    ReadableStream, ReadableStreamDefaultReader, WritableStream, WritableStreamDefaultWriter,
};

use super::{call_promise, get, global_get, message, promise, quietly};
use crate::webtransport::Options;
use crate::{Feature, NetError};

type Pending<T> = SendWrapper<Option<Pin<Box<dyn Future<Output = T> + Send>>>>;

fn pending<T>() -> Pending<T> {
    SendWrapper::new(None)
}

fn poll_pending<T, F>(slot: &mut Pending<T>, cx: &mut Context<'_>, start: F) -> Poll<T>
where
    F: FnOnce() -> Pin<Box<dyn Future<Output = T> + Send>>,
{
    let slot = &mut **slot;
    let future = slot.get_or_insert_with(start);
    let ready = std::task::ready!(future.as_mut().poll(cx));
    *slot = None;
    Poll::Ready(ready)
}

pub(crate) struct Session {
    transport: SendWrapper<JsValue>,
    incoming_bi: SendWrapper<ReadableStreamDefaultReader>,
    incoming_uni: SendWrapper<ReadableStreamDefaultReader>,
    datagrams_in: SendWrapper<ReadableStreamDefaultReader>,
    datagrams_out: SendWrapper<WritableStreamDefaultWriter>,
    protocol: Option<String>,
    max_datagram_size: usize,
    accept_bi: Pending<Result<(SendStream, RecvStream), NetError>>,
    accept_uni: Pending<Result<RecvStream, NetError>>,
    open_bi: Pending<Result<(SendStream, RecvStream), NetError>>,
    open_uni: Pending<Result<SendStream, NetError>>,
    recv_datagram: Pending<Result<Bytes, NetError>>,
    closed: Pending<NetError>,
}

// The shared readers and writers are one browser object each, so each clone
// issues its own operations and keeps its own pending state.
/// session issues its own operations against them, so each clone's pending
/// state stays its own.
impl Clone for Session {
    fn clone(&self) -> Self {
        Self {
            transport: self.transport.clone(),
            incoming_bi: self.incoming_bi.clone(),
            incoming_uni: self.incoming_uni.clone(),
            datagrams_in: self.datagrams_in.clone(),
            datagrams_out: self.datagrams_out.clone(),
            protocol: self.protocol.clone(),
            max_datagram_size: self.max_datagram_size,
            accept_bi: pending(),
            accept_uni: pending(),
            open_bi: pending(),
            open_uni: pending(),
            recv_datagram: pending(),
            closed: pending(),
        }
    }
}

pub(crate) struct SendStream {
    writer: SendWrapper<WritableStreamDefaultWriter>,
    write: Pending<Result<(), NetError>>,
    closed: Pending<Result<(), NetError>>,
}

impl Clone for SendStream {
    fn clone(&self) -> Self {
        Self {
            writer: self.writer.clone(),
            write: pending(),
            closed: pending(),
        }
    }
}

pub(crate) struct RecvStream {
    reader: SendWrapper<ReadableStreamDefaultReader>,
    buffered: Bytes,
    read: Pending<Result<Option<Bytes>, NetError>>,
    closed: Pending<Result<(), NetError>>,
}

impl Clone for RecvStream {
    fn clone(&self) -> Self {
        Self {
            reader: self.reader.clone(),
            buffered: self.buffered.clone(),
            read: pending(),
            closed: pending(),
        }
    }
}

fn protocol_error(error: &JsValue) -> NetError {
    NetError::Protocol(message(error))
}

fn reader(stream: &JsValue) -> Result<ReadableStreamDefaultReader, NetError> {
    Ok(stream
        .clone()
        .dyn_into::<ReadableStream>()
        .map_err(|_| NetError::Protocol("not a readable stream".into()))?
        .get_reader()
        .unchecked_into())
}

fn writer(stream: &JsValue) -> Result<WritableStreamDefaultWriter, NetError> {
    stream
        .clone()
        .dyn_into::<WritableStream>()
        .map_err(|_| NetError::Protocol("not a writable stream".into()))?
        .get_writer()
        .map_err(|error| protocol_error(&error))
}

async fn read(reader: ReadableStreamDefaultReader) -> Result<Option<JsValue>, NetError> {
    let result = promise(reader.read()).await.map_err(|_| NetError::Closed)?;
    let done = get(&result, "done")
        .and_then(|done| done.as_bool())
        .unwrap_or(true);
    Ok(if done { None } else { get(&result, "value") })
}

fn settled(object: &JsValue) -> Pin<Box<dyn Future<Output = Result<(), NetError>> + Send>> {
    let Some(closed) = get(object, "closed") else {
        return Box::pin(async { Err(NetError::Closed) });
    };
    let closing = promise(js_sys::Promise::resolve(&closed));
    Box::pin(async move {
        match closing.await {
            Ok(_) => Ok(()),
            Err(_) => Err(NetError::Closed),
        }
    })
}

pub(crate) fn connect(
    url: Url,
    options: Options,
) -> impl Future<Output = Result<Session, NetError>> + Send + 'static {
    SendWrapper::new(open(url, options))
}

async fn open(url: Url, options: Options) -> Result<Session, NetError> {
    let constructor: js_sys::Function = global_get("WebTransport")
        .and_then(|constructor| constructor.dyn_into().ok())
        .ok_or_else(|| NetError::from(crate::Unsupported::of(Feature::WebTransport)))?;
    let init = Object::new();
    if !options.cert_hashes.is_empty() {
        let hashes: Array = options
            .cert_hashes
            .iter()
            .map(|hash| {
                let entry = Object::new();
                let _ = Reflect::set(&entry, &"algorithm".into(), &"sha-256".into());
                let _ = Reflect::set(&entry, &"value".into(), &Uint8Array::from(&hash[..]));
                JsValue::from(entry)
            })
            .collect();
        let _ = Reflect::set(&init, &"serverCertificateHashes".into(), &hashes);
    }
    if !options.protocols.is_empty() {
        let protocols: Array = options
            .protocols
            .iter()
            .map(|protocol| JsValue::from_str(protocol))
            .collect();
        let _ = Reflect::set(&init, &"protocols".into(), &protocols);
    }
    let args: Array = [JsValue::from_str(url.as_str()), init.into()]
        .into_iter()
        .collect();
    let transport = Reflect::construct(&constructor, &args)
        .map_err(|error| NetError::InvalidUrl(message(&error)))?;
    let ready = get(&transport, "ready")
        .ok_or_else(|| NetError::Protocol("WebTransport has no ready promise".into()))?;
    promise(js_sys::Promise::resolve(&ready))
        .await
        .map_err(|error| NetError::connect(format!("{url}: {}", message(&error))))?;

    let missing = |what: &str| NetError::Protocol(format!("WebTransport has no {what}"));
    let incoming_bi = reader(
        &get(&transport, "incomingBidirectionalStreams")
            .ok_or_else(|| missing("incomingBidirectionalStreams"))?,
    )?;
    let incoming_uni = reader(
        &get(&transport, "incomingUnidirectionalStreams")
            .ok_or_else(|| missing("incomingUnidirectionalStreams"))?,
    )?;
    let datagrams = get(&transport, "datagrams").ok_or_else(|| missing("datagrams"))?;
    let datagrams_in =
        reader(&get(&datagrams, "readable").ok_or_else(|| missing("datagrams.readable"))?)?;
    // Newer browsers make a writable per call; older ones have one.
    let outgoing = match get(&datagrams, "createWritable")
        .and_then(|create| create.dyn_into::<js_sys::Function>().ok())
    {
        Some(create) => create
            .call0(&datagrams)
            .map_err(|error| protocol_error(&error))?,
        None => get(&datagrams, "writable").ok_or_else(|| missing("datagrams.writable"))?,
    };
    let datagrams_out = writer(&outgoing)?;
    let protocol = get(&transport, "protocol")
        .and_then(|protocol| protocol.as_string())
        .filter(|protocol| !protocol.is_empty());
    // A browser that exposes datagrams also reports the path's size; one that
    // does not cannot carry them, and the protocol's minimum is the ceiling.
    let max_datagram_size = get(&transport, "maxDatagramSize")
        .and_then(|size| size.as_f64())
        .map_or(1200, |size| {
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a datagram size is a whole, non-negative count of bytes"
            )]
            let size = size.max(0.0) as usize;
            size
        });
    Ok(Session {
        transport: SendWrapper::new(transport),
        incoming_bi: SendWrapper::new(incoming_bi),
        incoming_uni: SendWrapper::new(incoming_uni),
        datagrams_in: SendWrapper::new(datagrams_in),
        datagrams_out: SendWrapper::new(datagrams_out),
        protocol,
        max_datagram_size,
        accept_bi: pending(),
        accept_uni: pending(),
        open_bi: pending(),
        open_uni: pending(),
        recv_datagram: pending(),
        closed: pending(),
    })
}

fn bidirectional(pair: &JsValue) -> Result<(SendStream, RecvStream), NetError> {
    let readable = get(pair, "readable").ok_or(NetError::Closed)?;
    let writable = get(pair, "writable").ok_or(NetError::Closed)?;
    Ok((
        SendStream {
            writer: SendWrapper::new(writer(&writable)?),
            write: pending(),
            closed: pending(),
        },
        RecvStream {
            reader: SendWrapper::new(reader(&readable)?),
            buffered: Bytes::new(),
            read: pending(),
            closed: pending(),
        },
    ))
}

impl Session {
    pub(crate) fn open_bi(
        &self,
    ) -> impl Future<Output = Result<(SendStream, RecvStream), NetError>> + Send + '_ {
        let created = call_promise(&self.transport, "createBidirectionalStream", &[]);
        SendWrapper::new(async move {
            let pair = created.await.map_err(|_| NetError::Closed)?;
            bidirectional(&pair)
        })
    }

    pub(crate) fn accept_bi(
        &self,
    ) -> impl Future<Output = Result<(SendStream, RecvStream), NetError>> + Send + '_ {
        let incoming = self.incoming_bi.clone();
        SendWrapper::new(async move {
            let pair = read((*incoming).clone()).await?.ok_or(NetError::Closed)?;
            bidirectional(&pair)
        })
    }

    pub(crate) fn open_uni(
        &self,
    ) -> impl Future<Output = Result<SendStream, NetError>> + Send + '_ {
        let created = call_promise(&self.transport, "createUnidirectionalStream", &[]);
        SendWrapper::new(async move {
            let stream = created.await.map_err(|_| NetError::Closed)?;
            Ok(SendStream {
                writer: SendWrapper::new(writer(&stream)?),
                write: pending(),
                closed: pending(),
            })
        })
    }

    pub(crate) fn accept_uni(
        &self,
    ) -> impl Future<Output = Result<RecvStream, NetError>> + Send + '_ {
        let incoming = self.incoming_uni.clone();
        SendWrapper::new(async move {
            let stream = read((*incoming).clone()).await?.ok_or(NetError::Closed)?;
            Ok(RecvStream {
                reader: SendWrapper::new(reader(&stream)?),
                buffered: Bytes::new(),
                read: pending(),
                closed: pending(),
            })
        })
    }

    pub(crate) fn send_datagram(
        &self,
        data: Bytes,
    ) -> impl Future<Output = Result<(), NetError>> + Send + '_ {
        let out = self.datagrams_out.clone();
        SendWrapper::new(async move { write(&out, &data).await })
    }

    pub(crate) fn recv_datagram(
        &self,
    ) -> impl Future<Output = Result<Bytes, NetError>> + Send + '_ {
        let incoming = self.datagrams_in.clone();
        SendWrapper::new(async move {
            let value = read((*incoming).clone()).await?.ok_or(NetError::Closed)?;
            let bytes: Uint8Array = value
                .dyn_into()
                .map_err(|_| NetError::Protocol("a datagram was not bytes".into()))?;
            Ok(Bytes::from(bytes.to_vec()))
        })
    }

    pub(crate) fn close(&self, code: u32, reason: &str) {
        let info = Object::new();
        let _ = Reflect::set(&info, &"closeCode".into(), &JsValue::from(code));
        let _ = Reflect::set(&info, &"reason".into(), &JsValue::from_str(reason));
        if let Some(close) = get(&self.transport, "close")
            .and_then(|close| close.dyn_into::<js_sys::Function>().ok())
        {
            let _ = close.call1(&self.transport, &info);
        }
    }

    pub(crate) fn protocol(&self) -> Option<&str> {
        self.protocol.as_deref()
    }

    pub(crate) fn max_datagram_size(&self) -> usize {
        self.max_datagram_size
    }
}

impl web_transport_trait::poll::Session for Session {
    type SendStream = SendStream;
    type RecvStream = RecvStream;
    type Error = NetError;

    fn poll_accept_uni(&mut self, cx: &mut Context<'_>) -> Poll<Result<RecvStream, NetError>> {
        let incoming = self.incoming_uni.clone();
        poll_pending(&mut self.accept_uni, cx, || {
            Box::pin(SendWrapper::new(async move {
                let stream = read((*incoming).clone()).await?.ok_or(NetError::Closed)?;
                Ok(RecvStream {
                    reader: SendWrapper::new(reader(&stream)?),
                    buffered: Bytes::new(),
                    read: pending(),
                    closed: pending(),
                })
            }))
        })
    }

    fn poll_accept_bi(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(SendStream, RecvStream), NetError>> {
        let incoming = self.incoming_bi.clone();
        poll_pending(&mut self.accept_bi, cx, || {
            Box::pin(SendWrapper::new(async move {
                let pair = read((*incoming).clone()).await?.ok_or(NetError::Closed)?;
                bidirectional(&pair)
            }))
        })
    }

    fn poll_open_uni(&mut self, cx: &mut Context<'_>) -> Poll<Result<SendStream, NetError>> {
        let transport = (*self.transport).clone();
        poll_pending(&mut self.open_uni, cx, || {
            Box::pin(SendWrapper::new(async move {
                let created = call_promise(&transport, "createUnidirectionalStream", &[]);
                let stream = created.await.map_err(|_| NetError::Closed)?;
                Ok(SendStream {
                    writer: SendWrapper::new(writer(&stream)?),
                    write: pending(),
                    closed: pending(),
                })
            }))
        })
    }

    fn poll_open_bi(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(SendStream, RecvStream), NetError>> {
        let transport = (*self.transport).clone();
        poll_pending(&mut self.open_bi, cx, || {
            Box::pin(SendWrapper::new(async move {
                let created = call_promise(&transport, "createBidirectionalStream", &[]);
                let pair = created.await.map_err(|_| NetError::Closed)?;
                bidirectional(&pair)
            }))
        })
    }

    fn poll_send_datagram(
        &mut self,
        _cx: &mut Context<'_>,
        payload: &[u8],
    ) -> Poll<Result<(), NetError>> {
        // The browser's datagram writer queues or refuses the payload right away;
        // there is no capacity to wait for. A refusal is a dropped datagram, which
        // the protocol allows.
        write_now(&self.datagrams_out, payload);
        Poll::Ready(Ok(()))
    }

    fn poll_recv_datagram(&mut self, cx: &mut Context<'_>) -> Poll<Result<Bytes, NetError>> {
        let incoming = self.datagrams_in.clone();
        poll_pending(&mut self.recv_datagram, cx, || {
            Box::pin(SendWrapper::new(async move {
                let value = read((*incoming).clone()).await?.ok_or(NetError::Closed)?;
                let bytes: Uint8Array = value
                    .dyn_into()
                    .map_err(|_| NetError::Protocol("a datagram was not bytes".into()))?;
                Ok(Bytes::from(bytes.to_vec()))
            }))
        })
    }

    fn max_datagram_size(&self) -> usize {
        self.max_datagram_size
    }

    fn protocol(&self) -> Option<&str> {
        self.protocol.as_deref()
    }

    fn close(&mut self, code: u32, reason: &str) {
        Session::close(self, code, reason);
    }

    fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<NetError> {
        let transport = (*self.transport).clone();
        poll_pending(&mut self.closed, cx, || {
            Box::pin(SendWrapper::new(async move {
                // The browser exposes no application close code here.
                let _ = settled(&transport).await;
                NetError::Closed
            }))
        })
    }

    fn stats(&self) -> impl web_transport_trait::Stats {
        web_transport_trait::StatsUnavailable
    }
}

impl web_transport_trait::poll::SendStream for SendStream {
    type Error = NetError;

    fn poll_write(&mut self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize, NetError>> {
        let writer = self.writer.clone();
        let payload = Bytes::copy_from_slice(buf);
        let len = payload.len();
        let written = poll_pending(&mut self.write, cx, || {
            Box::pin(SendWrapper::new(
                async move { write(&writer, &payload).await },
            ))
        });
        match written {
            Poll::Ready(Ok(())) if len > buf.len() => Poll::Ready(Err(NetError::Protocol(
                "a pending write was retried with a shorter buffer".into(),
            ))),
            Poll::Ready(Ok(())) => Poll::Ready(Ok(buf.len())),
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Pending => Poll::Pending,
        }
    }

    fn set_priority(&mut self, _order: u8) {
        // The browser keeps stream priority on the writable, which the
        // `WritableStreamDefaultWriter` does not expose.
    }

    fn finish(&mut self) -> Result<(), NetError> {
        // A stream the peer reset is already errored: closing it throws, and the
        // close rejects when the peer resets it as the close lands. Both mean the
        // stream is gone, which the caller reads elsewhere.
        quietly((*self.writer).as_ref(), "close", &JsValue::UNDEFINED);
        Ok(())
    }

    fn reset(&mut self, code: u32) {
        quietly((*self.writer).as_ref(), "abort", &JsValue::from(code));
    }

    fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), NetError>> {
        let writer = self.writer.clone();
        poll_pending(&mut self.closed, cx, || {
            Box::pin(SendWrapper::new(async move {
                let closed = writer.closed();
                match promise(closed).await {
                    Ok(_) => Ok(()),
                    Err(_) => Err(NetError::Closed),
                }
            }))
        })
    }
}

impl web_transport_trait::poll::RecvStream for RecvStream {
    type Error = NetError;

    fn poll_read(
        &mut self,
        cx: &mut Context<'_>,
        dst: &mut [u8],
    ) -> Poll<Result<Option<usize>, NetError>> {
        if dst.is_empty() {
            return Poll::Ready(Ok(Some(0)));
        }
        if !self.buffered.is_empty() {
            let copied = self.buffered.len().min(dst.len());
            let Some(into) = dst.get_mut(..copied) else {
                return Poll::Ready(Err(NetError::Protocol("a read did not fit".into())));
            };
            into.copy_from_slice(&self.buffered.slice(..copied));
            self.buffered = self.buffered.slice(copied..);
            return Poll::Ready(Ok(Some(copied)));
        }
        let reader = self.reader.clone();
        let chunk = poll_pending(&mut self.read, cx, || {
            Box::pin(SendWrapper::new(async move {
                let Some(value) = read((*reader).clone()).await? else {
                    return Ok(None);
                };
                let bytes: Uint8Array = value
                    .dyn_into()
                    .map_err(|_| NetError::Protocol("a stream chunk was not bytes".into()))?;
                Ok(Some(Bytes::from(bytes.to_vec())))
            }))
        });
        match chunk {
            Poll::Ready(Ok(Some(chunk))) => {
                let copied = chunk.len().min(dst.len());
                let Some(into) = dst.get_mut(..copied) else {
                    return Poll::Ready(Err(NetError::Protocol("a read did not fit".into())));
                };
                into.copy_from_slice(&chunk.slice(..copied));
                self.buffered = chunk.slice(copied..);
                Poll::Ready(Ok(Some(copied)))
            }
            Poll::Ready(Ok(None)) => Poll::Ready(Ok(None)),
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Pending => Poll::Pending,
        }
    }

    fn stop(&mut self, _code: u32) {
        quietly((*self.reader).as_ref(), "cancel", &JsValue::UNDEFINED);
    }

    fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), NetError>> {
        let reader = self.reader.clone();
        poll_pending(&mut self.closed, cx, || {
            Box::pin(SendWrapper::new(async move {
                match promise(reader.closed()).await {
                    Ok(_) => Ok(()),
                    Err(_) => Err(NetError::Closed),
                }
            }))
        })
    }
}

fn write_now(writer: &WritableStreamDefaultWriter, data: &[u8]) {
    // A datagram the transport can no longer carry is dropped, as the protocol
    // allows; its refusal is not a page error.
    quietly(writer.as_ref(), "write", &Uint8Array::from(data));
}

async fn write(writer: &WritableStreamDefaultWriter, data: &[u8]) -> Result<(), NetError> {
    promise(writer.ready())
        .await
        .map_err(|_| NetError::Closed)?;
    promise(writer.write_with_chunk(&Uint8Array::from(data)))
        .await
        .map_err(|_| NetError::Closed)?;
    Ok(())
}

#[allow(
    clippy::needless_pass_by_ref_mut,
    reason = "the same signatures as the desktop backend, whose streams are mutable"
)]
impl SendStream {
    pub(crate) fn write_all<'a>(
        &'a mut self,
        data: &'a [u8],
    ) -> impl Future<Output = Result<(), NetError>> + Send + 'a {
        let out = (*self.writer).clone();
        SendWrapper::new(async move { write(&out, data).await })
    }

    pub(crate) async fn finish_stream(&mut self) -> Result<(), NetError> {
        let closing = SendWrapper::new(promise(self.writer.close()));
        closing.await.map(drop).map_err(|_| NetError::Closed)
    }
}

#[allow(
    clippy::needless_pass_by_ref_mut,
    reason = "the same signatures as the desktop backend, whose streams are mutable"
)]
impl RecvStream {
    pub(crate) fn read(
        &mut self,
    ) -> impl Future<Output = Result<Option<Bytes>, NetError>> + Send + '_ {
        let incoming = (*self.reader).clone();
        SendWrapper::new(async move {
            let Some(value) = read(incoming).await? else {
                return Ok(None);
            };
            let bytes: Uint8Array = value
                .dyn_into()
                .map_err(|_| NetError::Protocol("a stream chunk was not bytes".into()))?;
            Ok(Some(Bytes::from(bytes.to_vec())))
        })
    }
}
