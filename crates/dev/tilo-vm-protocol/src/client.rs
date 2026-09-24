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

pub struct Client<R, W> {
    reader: R,
    writer: W,
    next_id: u64,
}

impl<R: BufRead, W: Write> Client<R, W> {
    pub fn new(reader: R, writer: W) -> Self {
        Self {
            reader,
            writer,
            next_id: 1,
        }
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
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let envelope = RequestEnvelope {
            jsonrpc: "2.0".into(),
            id,
            request: params.into(),
        };
        let mut line =
            serde_json::to_vec(&envelope).map_err(|e| ClientError::Protocol(e.to_string()))?;
        line.push(b'\n');
        self.writer.write_all(&line)?;
        self.writer.flush()?;

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

    /// Stream `bytes` to the daemon in chunks and return the upload id for
    /// [`crate::LayerSource::Upload`].
    ///
    /// # Errors
    /// Read failures on `bytes` and any call failure.
    pub fn upload(&mut self, mut bytes: impl Read) -> Result<String, ClientError> {
        let upload = self.call(LayerUploadBegin {})?.upload;
        let mut buffer = vec![0_u8; UPLOAD_CHUNK_BYTES];
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
            self.call(LayerUploadChunk {
                upload: upload.clone(),
                data,
            })?;
            if filled < buffer.len() {
                break;
            }
        }
        Ok(upload)
    }
}
