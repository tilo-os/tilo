//! The codec's browser backend: the browser's own decoders and encoders
//! (`createImageBitmap`, `OffscreenCanvas`), so a page carries none of its
//! own for them.

use wasm_bindgen::{Clamped, JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

use crate::codec::{EncodeError, Encoding};
use crate::{DecodeError, DecodedImage};

/// `createImageBitmap` on the page or in a worker, whichever this is.
fn create_image_bitmap(
    blob: &web_sys::Blob,
    options: &web_sys::ImageBitmapOptions,
) -> Result<js_sys::Promise, JsValue> {
    let global = js_sys::global();
    if let Some(window) = global.dyn_ref::<web_sys::Window>() {
        window.create_image_bitmap_with_blob_and_image_bitmap_options(blob, options)
    } else {
        global
            .unchecked_into::<web_sys::WorkerGlobalScope>()
            .create_image_bitmap_with_blob_and_image_bitmap_options(blob, options)
    }
}

fn context(
    canvas: &web_sys::OffscreenCanvas,
) -> Result<web_sys::OffscreenCanvasRenderingContext2d, JsValue> {
    canvas
        .get_context("2d")?
        .ok_or_else(|| JsValue::from_str("no 2D context"))?
        .dyn_into()
        .map_err(Into::into)
}

pub(crate) async fn decode(
    bytes: &[u8],
    max_dimension: Option<u32>,
) -> Result<DecodedImage, DecodeError> {
    send_wrapper::SendWrapper::new(decode_here(bytes, max_dimension)).await
}

pub(crate) async fn encode(
    image: &DecodedImage,
    encoding: Encoding,
) -> Result<Vec<u8>, EncodeError> {
    send_wrapper::SendWrapper::new(encode_here(image, encoding)).await
}

#[expect(
    clippy::future_not_send,
    reason = "a browser future runs where it was made; `decode` and `encode` wrap it"
)]
async fn decode_here(
    bytes: &[u8],
    max_dimension: Option<u32>,
) -> Result<DecodedImage, DecodeError> {
    let invalid = |_| DecodeError::InvalidImage;
    // Where the header names a size, the browser scales while it decodes.
    let target = max_dimension.and_then(|max| {
        let (width, height) = crate::format::dimensions(bytes)?;
        crate::pixels::fitted_size(width, height, max)
    });
    let parts = js_sys::Array::of1(&js_sys::Uint8Array::from(bytes));
    let blob = web_sys::Blob::new_with_u8_array_sequence(&parts).map_err(invalid)?;
    let options = web_sys::ImageBitmapOptions::new();
    options.set_premultiply_alpha(web_sys::PremultiplyAlpha::None);
    if let Some((width, height)) = target {
        options.set_resize_width(width);
        options.set_resize_height(height);
        options.set_resize_quality(web_sys::ResizeQuality::High);
    }
    let bitmap: web_sys::ImageBitmap =
        JsFuture::from(create_image_bitmap(&blob, &options).map_err(invalid)?)
            .await
            .map_err(invalid)?
            .unchecked_into();
    let (width, height) = (bitmap.width(), bitmap.height());
    let pixels = (|| {
        let canvas = web_sys::OffscreenCanvas::new(width, height)?;
        let context = context(&canvas)?;
        context.draw_image_with_image_bitmap(&bitmap, 0.0, 0.0)?;
        context.get_image_data(0.0, 0.0, f64::from(width), f64::from(height))
    })();
    bitmap.close();
    let rgba = pixels.map_err(invalid)?.data().0;
    let decoded = DecodedImage {
        rgba,
        width,
        height,
    };
    // A format whose header the codec cannot size is decoded whole first.
    Ok(match (max_dimension, target) {
        (Some(max), None) => decoded.fitted(max),
        _ => decoded,
    })
}

#[expect(
    clippy::future_not_send,
    reason = "a browser future runs where it was made; `decode` and `encode` wrap it"
)]
async fn encode_here(image: &DecodedImage, encoding: Encoding) -> Result<Vec<u8>, EncodeError> {
    let failed = |error: JsValue| EncodeError::Failed(format!("{error:?}"));
    let canvas = web_sys::OffscreenCanvas::new(image.width, image.height).map_err(failed)?;
    let data = web_sys::ImageData::new_with_u8_clamped_array_and_sh(
        Clamped(&image.rgba),
        image.width,
        image.height,
    )
    .map_err(failed)?;
    context(&canvas)
        .map_err(failed)?
        .put_image_data(&data, 0.0, 0.0)
        .map_err(failed)?;
    let options = web_sys::ImageEncodeOptions::new();
    options.set_type(encoding.mime());
    if let Encoding::Jpeg { quality } = encoding {
        options.set_quality(f64::from(quality.clamp(1, 100)) / 100.0);
    }
    let blob: web_sys::Blob = JsFuture::from(
        canvas
            .convert_to_blob_with_options(&options)
            .map_err(failed)?,
    )
    .await
    .map_err(failed)?
    .unchecked_into();
    // A browser that cannot write the format (Safari has no WebP encoder)
    // writes PNG instead; the codec's own encoder writes what was asked.
    if blob.type_() != encoding.mime() {
        return crate::codec::encode_now(image, encoding);
    }
    let buffer = JsFuture::from(blob.array_buffer()).await.map_err(failed)?;
    Ok(js_sys::Uint8Array::new(&buffer).to_vec())
}
