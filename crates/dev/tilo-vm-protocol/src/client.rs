//! The one client implementation, generic over any byte stream so the same
//! code drives `ssh` stdio in production and a subprocess pipe in tests.

use std::io::{BufRead, Read, Write};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use crate::{
    Call, Hello, HelloResult, LayerUploadBegin, LayerUploadChunk, PROTOCOL_VERSION,
    RequestEnvelope, ResponseEnvelope, RpcError, UPLOAD_CHUNK_BYTES,
};

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("connection to tilo-vmd failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("tilo-vmd closed the connection")]
    Closed,
    #[error("malformed response from tilo-vmd: {0}")]
    Protocol(String),
    #[error("tilo-vm speaks protocol {client} but tilo-vmd speaks {daemon}")]
    VersionMismatch { client: u32, daemon: u32 },
    #[error(transparent)]
    Rpc(RpcError),
}

// Retain ordinary upload frames, but do not cache unusually large RPC bodies.
const MAX_RETAINED_REQUEST_BYTES: usize = UPLOAD_CHUNK_BYTES * 4;

pub struct Client<R, W> {
    reader: R,
    writer: W,
    next_id: u64,
    request_buffer: Vec<u8>,
    usable: bool,
    binary_uploads: bool,
}

impl<R: BufRead, W: Write> Client<R, W> {
    pub fn new(reader: R, writer: W) -> Self {
        Self {
            reader,
            writer,
            next_id: 1,
            request_buffer: Vec::new(),
            usable: true,
            binary_uploads: false,
        }
    }

    /// Opt in only on a byte-transparent transport. JSON-line peer bridges
    /// retain their existing framing and session accounting.
    pub fn enable_binary_uploads(&mut self) {
        self.binary_uploads = true;
    }

    /// Connect-time handshake; every other call requires it first.
    ///
    /// # Errors
    /// [`ClientError::VersionMismatch`] when the daemon speaks another version.
    pub fn hello(&mut self, client_version: &str) -> Result<HelloResult, ClientError> {
        let hello = Hello {
            protocol: PROTOCOL_VERSION,
            client_version: client_version.into(),
        };
        // `hello` itself never changes shape, so any daemon answers it with its
        // own version and the mismatch is reported here, precisely.
        let result = self.call(hello)?;
        if result.protocol == PROTOCOL_VERSION {
            Ok(result)
        } else {
            Err(ClientError::VersionMismatch {
                client: PROTOCOL_VERSION,
                daemon: result.protocol,
            })
        }
    }

    /// Send one request and wait for its response.
    ///
    /// # Errors
    /// Transport failures, malformed responses and daemon-reported errors.
    pub fn call<C: Call>(&mut self, params: C) -> Result<C::Output, ClientError> {
        if !self.usable {
            return Err(ClientError::Closed);
        }
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let envelope = RequestEnvelope {
            jsonrpc: "2.0".into(),
            id,
            request: params.into(),
        };
        if matches!(
            &envelope.request,
            crate::Request::DeviceActions(_) | crate::Request::DeviceUnlockEnrol(_)
        ) {
            // Private frames never enter the retained public upload buffer.
            let mut line = zeroize::Zeroizing::new(
                serde_json::to_vec(&envelope).map_err(|e| ClientError::Protocol(e.to_string()))?,
            );
            drop(envelope);
            line.push(b'\n');
            self.writer.write_all(&line)?;
            self.writer.flush()?;
            drop(line);
        } else {
            // Reuse the serialized frame across serial calls instead of allocating
            // and growing a new base64-sized JSON buffer for every upload chunk.
            self.request_buffer.clear();
            serde_json::to_writer(&mut self.request_buffer, &envelope)
                .map_err(|e| ClientError::Protocol(e.to_string()))?;
            self.request_buffer.push(b'\n');
            let sent = self
                .writer
                .write_all(&self.request_buffer)
                .and_then(|()| self.writer.flush());
            if self.request_buffer.capacity() > MAX_RETAINED_REQUEST_BYTES {
                self.request_buffer = Vec::new();
            }
            sent?;
        }

        self.response(id)
    }

    fn response<T: serde::de::DeserializeOwned>(&mut self, id: u64) -> Result<T, ClientError> {
        let mut response = String::new();
        if self.reader.read_line(&mut response)? == 0 {
            return Err(ClientError::Closed);
        }
        let response: ResponseEnvelope =
            serde_json::from_str(&response).map_err(|e| ClientError::Protocol(e.to_string()))?;
        if response.id != id {
            return Err(ClientError::Protocol(format!(
                "expected response {id}, got {}",
                response.id
            )));
        }
        match (response.result, response.error) {
            (_, Some(error)) => Err(ClientError::Rpc(error)),
            (Some(result), None) => {
                serde_json::from_value(result).map_err(|e| ClientError::Protocol(e.to_string()))
            }
            (None, None) => Err(ClientError::Protocol(
                "response has neither result nor error".into(),
            )),
        }
    }

    /// Prefer the optional bounded binary stream, falling back only when an old
    /// daemon rejects the method before any archive bytes are written.
    /// # Errors
    /// Source, transport, integrity or daemon failures.
    pub fn upload(&mut self, mut bytes: impl Read) -> Result<String, ClientError> {
        if !self.binary_uploads {
            return self.upload_legacy(bytes);
        }
        let id = self.next_id;
        let upload = match self.call(crate::LayerUploadBinary {}) {
            Ok(upload) => upload.upload,
            Err(ClientError::Rpc(error))
                if matches!(error.code, crate::code::INVALID_PARAMS | -32601) =>
            {
                return self.upload_legacy(bytes);
            }
            Err(error) => return Err(error),
        };
        let mut encoder = crate::binary::Encoder::new(&mut self.writer);
        let mut buffer = vec![0; crate::binary::CHUNK_BYTES];
        loop {
            let size = match bytes.read(&mut buffer) {
                Ok(size) => size,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    self.usable = encoder.cancel().is_ok()
                        && matches!(
                            self.response::<crate::LayerUploadChunkResult>(id),
                            Err(ClientError::Rpc(_))
                        );
                    return Err(ClientError::Io(error));
                }
            };
            if size == 0 {
                break;
            }
            if let Err(error) = encoder.write_all(buffer.get(..size).unwrap_or_default()) {
                self.usable = false;
                return Err(ClientError::Io(error));
            }
        }
        let sent = match encoder.finish() {
            Ok(sent) => sent,
            Err(error) => {
                self.usable = false;
                return Err(ClientError::Io(error));
            }
        };
        let result = self.response::<crate::LayerUploadChunkResult>(id);
        match result {
            Ok(reply) if reply.received == sent => Ok(upload),
            Ok(_) => {
                self.usable = false;
                Err(ClientError::Protocol(
                    "binary upload byte acknowledgment mismatch".into(),
                ))
            }
            Err(error) => {
                self.usable = false;
                Err(error)
            }
        }
    }

    /// Stream `bytes` to the daemon in chunks and return the upload id for
    /// [`crate::LayerSource::Upload`].
    ///
    /// # Errors
    /// Read failures on `bytes`, call failures, and incorrect byte acknowledgments.
    pub fn upload_legacy(&mut self, mut bytes: impl Read) -> Result<String, ClientError> {
        let upload = self.call(LayerUploadBegin {})?.upload;
        let mut buffer = vec![0_u8; UPLOAD_CHUNK_BYTES];
        let mut sent = 0_u64;
        loop {
            let mut filled = 0;
            while filled < buffer.len() {
                let Some(rest) = buffer.get_mut(filled..) else {
                    break;
                };
                let n = bytes.read(rest)?;
                if n == 0 {
                    break;
                }
                filled = filled.saturating_add(n);
            }
            if filled == 0 {
                break;
            }
            let data = STANDARD.encode(buffer.get(..filled).unwrap_or_default());
            sent = sent.saturating_add(filled as u64);
            let reply = self.call(LayerUploadChunk {
                upload: upload.clone(),
                data,
            })?;
            if reply.received != sent {
                return Err(ClientError::Protocol(format!(
                    "upload acknowledgment: expected {sent} bytes, got {}",
                    reply.received
                )));
            }
            if filled < buffer.len() {
                break;
            }
        }
        Ok(upload)
    }
}

#[cfg(test)]
mod tests;
