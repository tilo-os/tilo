//! WebSocket in a page: the browser's `WebSocket`, its events queued to the
//! task that reads them.

use bytes::Bytes;
use js_sys::{ArrayBuffer, Uint8Array};
use send_wrapper::SendWrapper;
use tokio::sync::mpsc;
use url::Url;
use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::*;
use web_sys::{BinaryType, CloseEvent, MessageEvent};

use super::message;
use crate::NetError;
use crate::ws::{Message, Options};

enum Event {
    Open,
    Message(Message),
    Error,
    Closed {
        clean: bool,
        code: u16,
        reason: String,
    },
}

pub(crate) struct Socket(SendWrapper<Inner>);

struct Inner {
    socket: web_sys::WebSocket,
    events: mpsc::UnboundedReceiver<Event>,
    closed: bool,
    _handlers: Vec<Closure<dyn FnMut(JsValue)>>,
}

pub(crate) fn connect(
    url: Url,
    options: Options,
) -> impl Future<Output = Result<(Socket, Option<String>), NetError>> + Send + 'static {
    SendWrapper::new(open(url, options))
}

async fn open(url: Url, options: Options) -> Result<(Socket, Option<String>), NetError> {
    let socket = if options.protocols.is_empty() {
        web_sys::WebSocket::new(url.as_str())
    } else {
        let protocols: js_sys::Array = options
            .protocols
            .iter()
            .map(|protocol| JsValue::from_str(protocol))
            .collect();
        web_sys::WebSocket::new_with_str_sequence(url.as_str(), &protocols)
    }
    .map_err(|error| NetError::InvalidUrl(message(&error)))?;
    socket.set_binary_type(BinaryType::Arraybuffer);

    let (sender, events) = mpsc::unbounded_channel();
    let handler = |sender: &mpsc::UnboundedSender<Event>, make: fn(JsValue) -> Option<Event>| {
        let sender = sender.clone();
        Closure::<dyn FnMut(JsValue)>::new(move |event: JsValue| {
            if let Some(event) = make(event) {
                let _ = sender.send(event);
            }
        })
    };
    let on_open = handler(&sender, |_| Some(Event::Open));
    let on_error = handler(&sender, |_| Some(Event::Error));
    let on_message = handler(&sender, |event| {
        let data = event.dyn_into::<MessageEvent>().ok()?.data();
        if let Some(text) = data.as_string() {
            return Some(Event::Message(Message::Text(text)));
        }
        let buffer = data.dyn_into::<ArrayBuffer>().ok()?;
        Some(Event::Message(Message::Binary(Bytes::from(
            Uint8Array::new(&buffer).to_vec(),
        ))))
    });
    let on_close = handler(&sender, |event| {
        let event = event.dyn_into::<CloseEvent>().ok()?;
        Some(Event::Closed {
            clean: event.was_clean(),
            code: event.code(),
            reason: event.reason(),
        })
    });
    socket.set_onopen(Some(on_open.as_ref().unchecked_ref()));
    socket.set_onerror(Some(on_error.as_ref().unchecked_ref()));
    socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));
    let mut inner = Inner {
        socket,
        events,
        closed: false,
        _handlers: vec![on_open, on_error, on_message, on_close],
    };

    loop {
        match inner.events.recv().await {
            Some(Event::Open) => break,
            Some(Event::Message(_) | Event::Error) => {}
            Some(Event::Closed { code, reason, .. }) => {
                inner.closed = true;
                return Err(NetError::connect(if reason.is_empty() {
                    format!("{url}: the WebSocket closed before opening (code {code})")
                } else {
                    format!("{url}: {reason} (code {code})")
                }));
            }
            None => return Err(NetError::Closed),
        }
    }
    let protocol = Some(inner.socket.protocol()).filter(|protocol| !protocol.is_empty());
    Ok((Socket(SendWrapper::new(inner)), protocol))
}

impl Socket {
    pub(crate) fn send(
        &mut self,
        message: Message,
    ) -> impl Future<Output = Result<(), NetError>> + Send + '_ {
        let inner = &mut *self.0;
        let sent = if inner.closed || inner.socket.ready_state() != web_sys::WebSocket::OPEN {
            Err(NetError::Closed)
        } else {
            match message {
                Message::Text(text) => inner.socket.send_with_str(&text),
                Message::Binary(bytes) => inner.socket.send_with_u8_array(&bytes),
            }
            .map_err(|error| NetError::Protocol(super::message(&error)))
        };
        std::future::ready(sent)
    }

    pub(crate) fn recv(
        &mut self,
    ) -> impl Future<Output = Option<Result<Message, NetError>>> + Send + '_ {
        SendWrapper::new(self.0.recv())
    }

    #[allow(
        clippy::unused_async,
        reason = "the same signature as the desktop backend"
    )]
    pub(crate) async fn close(&mut self) -> Result<(), NetError> {
        let inner = &mut *self.0;
        if !inner.closed {
            inner.closed = true;
            inner
                .socket
                .close_with_code(1000)
                .map_err(|error| NetError::Protocol(message(&error)))?;
        }
        Ok(())
    }
}

impl Inner {
    async fn recv(&mut self) -> Option<Result<Message, NetError>> {
        if self.closed {
            return None;
        }
        loop {
            match self.events.recv().await? {
                Event::Message(message) => return Some(Ok(message)),
                Event::Open | Event::Error => {}
                Event::Closed {
                    clean,
                    code,
                    reason,
                } => {
                    self.closed = true;
                    return if clean {
                        None
                    } else {
                        Some(Err(NetError::Protocol(format!(
                            "the WebSocket closed abnormally (code {code}) {reason}"
                        ))))
                    };
                }
            }
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.socket.set_onopen(None);
        self.socket.set_onerror(None);
        self.socket.set_onmessage(None);
        self.socket.set_onclose(None);
        if !self.closed {
            let _ = self.socket.close();
        }
    }
}
