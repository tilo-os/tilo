//! Frames whose payload is one postcard-encoded value.

use std::io::{Read, Write};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{FrameError, Framing, PREFIX_BYTES};

pub fn decode_value<T: DeserializeOwned>(payload: &[u8]) -> Result<T, FrameError> {
    postcard::from_bytes(payload).map_err(FrameError::Decode)
}

impl Framing {
    pub fn encode_value<T: Serialize + ?Sized>(self, value: &T) -> Result<Vec<u8>, FrameError> {
        let mut out =
            postcard::to_extend(value, vec![0_u8; PREFIX_BYTES]).map_err(FrameError::Encode)?;
        let prefix = self.encode_prefix(out.len().saturating_sub(PREFIX_BYTES))?;
        if let Some(head) = out.get_mut(..PREFIX_BYTES) {
            head.copy_from_slice(&prefix);
        }
        Ok(out)
    }

    pub fn write_value<W: Write + ?Sized, T: Serialize + ?Sized>(
        self,
        writer: &mut W,
        value: &T,
    ) -> Result<(), FrameError> {
        writer.write_all(&self.encode_value(value)?)?;
        Ok(())
    }

    pub fn read_value<R: Read + ?Sized, T: DeserializeOwned>(
        self,
        reader: &mut R,
    ) -> Result<Option<T>, FrameError> {
        self.read(reader)?
            .map(|payload| decode_value(&payload))
            .transpose()
    }
}

#[cfg(feature = "tokio")]
impl Framing {
    pub async fn write_value_async<W, T>(self, writer: &mut W, value: &T) -> Result<(), FrameError>
    where
        W: tokio::io::AsyncWrite + Unpin + ?Sized,
        T: Serialize + Sync + ?Sized,
    {
        use tokio::io::AsyncWriteExt as _;
        writer.write_all(&self.encode_value(value)?).await?;
        Ok(())
    }

    pub async fn read_value_async<R, T>(self, reader: &mut R) -> Result<Option<T>, FrameError>
    where
        R: tokio::io::AsyncRead + Unpin + ?Sized,
        T: DeserializeOwned,
    {
        self.read_async(reader)
            .await?
            .map(|payload| decode_value(&payload))
            .transpose()
    }
}
