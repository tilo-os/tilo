//! WebTransport in a page: the browser's `WebTransport`, reached through
//! `Reflect` because web-sys only binds it behind its unstable-API flag.

use bytes::Bytes;
use js_sys::{Array, Object, Reflect, Uint8Array};
use send_wrapper::SendWrapper;
use url::Url;
use wasm_bindgen::JsCast as _;
use wasm_bindgen::JsValue;
use web_sys::{
    ReadableStream, ReadableStreamDefaultReader, WritableStream, WritableStreamDefaultWriter,
};

use super::{call_promise, get, global_get, message, promise};
use crate::webtransport::Options;
use crate::{Feature, NetError};

pub(crate) struct Session(SendWrapper<Inner>);
pub(crate) struct SendStream(SendWrapper<WritableStreamDefaultWriter>);
pub(crate) struct RecvStream(SendWrapper<ReadableStreamDefaultReader>);

struct Inner {
    transport: JsValue,
    incoming_bi: ReadableStreamDefaultReader,
    incoming_uni: ReadableStreamDefaultReader,
    datagrams_in: ReadableStreamDefaultReader,
    datagrams_out: WritableStreamDefaultWriter,
}

fn protocol(error: &JsValue) -> NetError {
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
        .map_err(|error| protocol(&error))
}

/// `{done, value}` from a reader: `None` when done.
async fn read(reader: ReadableStreamDefaultReader) -> Result<Option<JsValue>, NetError> {
    let result = promise(reader.read()).await.map_err(|_| NetError::Closed)?;
    let done = get(&result, "done")
        .and_then(|done| done.as_bool())
        .unwrap_or(true);
    Ok(if done { None } else { get(&result, "value") })
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
        Some(create) => create.call0(&datagrams).map_err(|error| protocol(&error))?,
        None => get(&datagrams, "writable").ok_or_else(|| missing("datagrams.writable"))?,
    };
    let datagrams_out = writer(&outgoing)?;
    Ok(Session(SendWrapper::new(Inner {
        transport,
        incoming_bi,
        incoming_uni,
        datagrams_in,
        datagrams_out,
    })))
}

fn bidirectional(pair: &JsValue) -> Result<(SendStream, RecvStream), NetError> {
    let readable = get(pair, "readable").ok_or(NetError::Closed)?;
    let writable = get(pair, "writable").ok_or(NetError::Closed)?;
    Ok((
        SendStream(SendWrapper::new(writer(&writable)?)),
        RecvStream(SendWrapper::new(reader(&readable)?)),
    ))
}

impl Session {
    pub(crate) fn open_bi(
        &self,
    ) -> impl Future<Output = Result<(SendStream, RecvStream), NetError>> + Send + '_ {
        let created = call_promise(&self.0.transport, "createBidirectionalStream", &[]);
        SendWrapper::new(async move {
            let pair = created.await.map_err(|_| NetError::Closed)?;
            bidirectional(&pair)
        })
    }

    pub(crate) fn accept_bi(
        &self,
    ) -> impl Future<Output = Result<(SendStream, RecvStream), NetError>> + Send + '_ {
        let incoming = self.0.incoming_bi.clone();
        SendWrapper::new(async move {
            let pair = read(incoming).await?.ok_or(NetError::Closed)?;
            bidirectional(&pair)
        })
    }

    pub(crate) fn open_uni(
        &self,
    ) -> impl Future<Output = Result<SendStream, NetError>> + Send + '_ {
        let created = call_promise(&self.0.transport, "createUnidirectionalStream", &[]);
        SendWrapper::new(async move {
            let stream = created.await.map_err(|_| NetError::Closed)?;
            Ok(SendStream(SendWrapper::new(writer(&stream)?)))
        })
    }

    pub(crate) fn accept_uni(
        &self,
    ) -> impl Future<Output = Result<RecvStream, NetError>> + Send + '_ {
        let incoming = self.0.incoming_uni.clone();
        SendWrapper::new(async move {
            let stream = read(incoming).await?.ok_or(NetError::Closed)?;
            Ok(RecvStream(SendWrapper::new(reader(&stream)?)))
        })
    }

    pub(crate) fn send_datagram(
        &self,
        data: Bytes,
    ) -> impl Future<Output = Result<(), NetError>> + Send + '_ {
        let out = self.0.datagrams_out.clone();
        SendWrapper::new(async move { write(&out, &data).await })
    }

    pub(crate) fn recv_datagram(
        &self,
    ) -> impl Future<Output = Result<Bytes, NetError>> + Send + '_ {
        let incoming = self.0.datagrams_in.clone();
        SendWrapper::new(async move {
            let value = read(incoming).await?.ok_or(NetError::Closed)?;
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
        if let Some(close) = get(&self.0.transport, "close")
            .and_then(|close| close.dyn_into::<js_sys::Function>().ok())
        {
            let _ = close.call1(&self.0.transport, &info);
        }
    }
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
        let out = (*self.0).clone();
        SendWrapper::new(async move { write(&out, data).await })
    }

    pub(crate) async fn finish(&mut self) -> Result<(), NetError> {
        let closing = SendWrapper::new(promise(self.0.close()));
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
        let incoming = (*self.0).clone();
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
