//! `tilo-sdk-native`: Tilo's API in process for the TypeScript SDK, as a key
//! the program holds ([`tilo_api::direct`]), so `Tilo.host(...)` reaches a
//! Tilo host directly as a device of the person, with no API server between.
//!
//! Every request runs on napi's tokio runtime and settles a promise; a
//! streamed answer (server-sent events) is handed over as [`Chunks`], which
//! the package's `index.js` turns into an async iterable. The Automerge bytes
//! the host sends are decoded in sandboxed workers, started as the
//! `tilo-automerge-worker` program beside the addon.

use napi::bindgen_prelude::{Either, Uint8Array};
use napi_derive::napi;
use std::collections::HashMap;
use std::sync::Arc;
use tilo_api::direct::{AnswerBody, Direct, KeyFile};

/// Start the client's Automerge workers as `program`, the
/// `tilo-automerge-worker` program the platform package carries beside the
/// addon: Node cannot be started again as one. `index.js` names it before the
/// first [`Host`].
#[napi]
pub fn serve_workers_from(program: String) {
    tilo_automerge_isolate::serve_workers_from(program.into());
}

/// A Tilo host reached directly with the key a key file holds.
#[napi]
pub struct Host {
    direct: Arc<Direct>,
}

#[napi]
impl Host {
    /// `key_file`: the path of a key file, or its contents
    /// (`{host, name, key, cert}`).
    #[napi(constructor)]
    pub fn new(key_file: serde_json::Value) -> napi::Result<Self> {
        let file = match key_file {
            serde_json::Value::String(path) => KeyFile::read(std::path::Path::new(&path)),
            contents => {
                serde_json::from_value(contents).map_err(|error| format!("not a key file: {error}"))
            }
        }
        .map_err(napi::Error::from_reason)?;
        let direct = Direct::new(&file).map_err(napi::Error::from_reason)?;
        Ok(Self {
            direct: Arc::new(direct),
        })
    }

    /// Answer `method` on `path` (with its query) as the API does.
    #[napi]
    pub async fn request(
        &self,
        method: String,
        path: String,
        headers: HashMap<String, String>,
        body: Uint8Array,
    ) -> Reply {
        let headers: Vec<(String, String)> = headers.into_iter().collect();
        let answer = self.direct.answer(&method, &path, &headers, &body).await;
        Reply {
            status: u32::from(answer.status),
            headers: answer.headers.into_iter().collect(),
            body: match answer.body {
                AnswerBody::Whole(bytes) => Either::A(Uint8Array::from(bytes.to_vec())),
                AnswerBody::Stream(chunks) => Either::B(Chunks {
                    chunks: Arc::new(chunks),
                }),
            },
        }
    }
}

/// An answer: its status, its headers (a name's values joined with `, `) and
/// its body, whole or streamed.
#[napi(object, object_from_js = false)]
pub struct Reply {
    pub status: u32,
    pub headers: HashMap<String, String>,
    pub body: Either<Uint8Array, Chunks>,
}

/// A streamed answer's chunks, in order.
#[napi]
pub struct Chunks {
    chunks: Arc<tilo_api::direct::Chunks>,
}

#[napi]
impl Chunks {
    /// The next chunk, or `null` once the stream has ended.
    #[napi]
    pub async fn next(&self) -> Option<Uint8Array> {
        self.chunks
            .next()
            .await
            .map(|chunk| Uint8Array::from(chunk.to_vec()))
    }
}
