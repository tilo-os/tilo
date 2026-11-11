//! The codec's native backend: the `image` crate.

use crate::codec::{EncodeError, Encoding};
use crate::{DecodeError, DecodedImage};

#[allow(
    clippy::unused_async,
    reason = "the browser's backend awaits; callers see one signature"
)]
pub(crate) async fn decode(
    bytes: &[u8],
    max_dimension: Option<u32>,
) -> Result<DecodedImage, DecodeError> {
    crate::codec::decode_now(bytes, max_dimension)
}

#[allow(
    clippy::unused_async,
    reason = "the browser's backend awaits; callers see one signature"
)]
pub(crate) async fn encode(
    image: &DecodedImage,
    encoding: Encoding,
) -> Result<Vec<u8>, EncodeError> {
    crate::codec::encode_now(image, encoding)
}
