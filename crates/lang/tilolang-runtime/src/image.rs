//! A file's pixels: what its container says about them, and the crop, turn, resize and encode a
//! program applies.
//!
//! All of it runs on the device's own codec ([`tilo_image_codec`]), so no app carries a decoder.
//! The source is read from the content store under the file's address, decoded upright the way
//! the container's orientation says it is shown, and the result is kept back in the same store
//! under the name the operation gives it. What comes back is a `File` like any other: its bytes
//! are addressed once, and replay decodes the same bytes the same way.
//!
//! These are effects (`uses image`): decoding, encoding and the preview cache are the device's,
//! and a view is drawn again from what it is shown rather than working on pixels each frame.

use std::path::Path;

use tilo_image_codec::{AdmissionLimits, DecodedImage, Encoding};

use crate::abi::Imp;
use crate::core::{Args, Core, HResult, trap};
use crate::engine::Wasm;
use crate::value::Value;

/// The bounds one image operation works under: a file a person could choose, decoded to at most
/// 8192 pixels a side and 128 MiB of pixels, which is what an editor holds while it applies an
/// edit.
const LIMITS: AdmissionLimits = AdmissionLimits {
    max_encoded_bytes: crate::blob::MAX_CHOSEN,
    max_rgba_bytes: 128 * 1024 * 1024,
    max_dimension: 8192,
};

impl Core {
    pub(crate) fn image_import(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        match imp {
            Imp::FileImageInfo => self.file_image_info(a),
            Imp::FileCrop | Imp::FileTurn | Imp::FileResize | Imp::FileEncode => {
                self.file_transform(imp, a)
            }
            Imp::FileThumbnail => self.file_thumbnail(a),
            _ => trap("not an image import"),
        }
    }

    /// `File.image_info()`: what the file's container says, as the host's `ImageInfo`, or none
    /// for bytes that are no image.
    fn file_image_info(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let file = self.file_arg(a.h(0)?)?;
        let bytes = self.file_bytes(&file)?;
        let Some(info) = tilo_image_codec::info::read(&bytes) else {
            return self.ret_h(Value::none());
        };
        let optional = |value: Option<Value>| value.map_or_else(Value::none, Value::some);
        let info = Value::Tuple(vec![
            Value::Int(i64::from(info.width)),
            Value::Int(i64::from(info.height)),
            optional(info.captured_at.map(Value::text)),
            optional(
                info.orientation
                    .map(|degrees| Value::Int(i64::from(degrees))),
            ),
            optional(info.latitude.map(Value::Float)),
            optional(info.longitude.map(Value::Float)),
        ]);
        self.ret_h(Value::some(info))
    }

    /// `File.cropped(x, y, w, h)`, `File.turned(quarters)`, `File.resized(width, height)` and
    /// `File.encoded(format:, quality:)`: the file's pixels changed, kept as a new file.
    fn file_transform(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let change = match imp {
            Imp::FileCrop => Change::Crop {
                x: a.float(1)?,
                y: a.float(2)?,
                width: a.float(3)?,
                height: a.float(4)?,
            },
            Imp::FileTurn => Change::Turn(a.long(1)?),
            Imp::FileResize => Change::Resize {
                width: a.long(1)?,
                height: a.long(2)?,
            },
            Imp::FileEncode => Change::Encode {
                format: self.text(a.h(1)?)?,
                quality: a.long(2)?,
            },
            _ => return trap("not an image change"),
        };
        let file = self.file_arg(a.h(0)?)?;
        self.result(self.apply_change(&file, change))
    }

    /// The pixels `change` names, encoded and kept under the file's name.
    fn apply_change(&self, file: &crate::blob::FileValue, change: Change) -> Result<Value, String> {
        let original = self.image_pixels(file)?;
        let (image, encoding, operation) = match change {
            Change::Crop {
                x,
                y,
                width,
                height,
            } => {
                let cropped = original
                    .cropped_fraction(x, y, width, height)
                    .ok_or_else(|| format!("the crop of `{}` names no area of it", file.name))?;
                (cropped, Encoding::Png, "cropped")
            }
            Change::Turn(quarters) => {
                let quarters = quarters.rem_euclid(4);
                let turned = original.turned(u32::try_from(quarters).unwrap_or(0));
                (turned, Encoding::Png, "turned")
            }
            Change::Resize { width, height } => {
                let width = u32::try_from(width).unwrap_or(0);
                let height = u32::try_from(height).unwrap_or(0);
                LIMITS
                    .rgba_len(width, height)
                    .map_err(|why| format!("a resized `{}` is refused: {why}", file.name))?;
                (original.resized(width, height), Encoding::Png, "resized")
            }
            Change::Encode { format, quality } => {
                let quality = u8::try_from(quality.clamp(1, 100)).unwrap_or(85);
                let encoding = match format.to_ascii_lowercase().as_str() {
                    "png" => Encoding::Png,
                    "jpeg" | "jpg" => Encoding::Jpeg { quality },
                    "webp" => Encoding::WebP,
                    other => {
                        return Err(format!(
                            "`encoded` writes `png`, `jpeg` or `webp`, not `{other}`"
                        ));
                    }
                };
                (original, encoding, "encoded")
            }
        };
        let bytes = tilo_image_codec::encode_now(&image, encoding)
            .map_err(|why| format!("`{}` did not encode: {why}", file.name))?;
        let name = format!(
            "{}-{operation}.{}",
            stem_of(&file.name),
            extension_of(encoding)
        );
        let kept = self.keep_bytes(&name, encoding.mime(), &bytes)?;
        Ok(Value::file(kept))
    }

    /// `File.thumbnail(size)`: a preview no larger than `size` pixels a side, from the device's
    /// own preview cache, or none when the file has no preview.
    fn file_thumbnail(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let file = self.file_arg(a.h(0)?)?;
        let size = a.long(1)?;
        if !(1..=4096).contains(&size) {
            return trap(format!(
                "a thumbnail is 1 to 4096 pixels a side, and this asks for {size}"
            ));
        }
        let preview = {
            let blobs = self.blobs.borrow();
            let Some(blobs) = blobs.as_ref() else {
                return trap("this host keeps no previews");
            };
            blobs
                .thumbnail(&file, u32::try_from(size).unwrap_or(1))
                .map_err(crate::engine::HostFail::Trap)?
        };
        self.ret_h(match preview {
            Some(preview) => Value::some(Value::file(preview)),
            None => Value::none(),
        })
    }

    /// The file's content, decoded upright from its container's orientation.
    fn image_pixels(&self, file: &crate::blob::FileValue) -> Result<DecodedImage, String> {
        let bytes = self.file_bytes(file)?;
        LIMITS
            .admit(&bytes)
            .map_err(|why| format!("`{}` is no image this device opens: {why}", file.name))?;
        tilo_image_codec::decode_upright_now(&bytes, None)
            .map_err(|why| format!("`{}` did not decode: {why}", file.name))
    }

    /// The content `file` names, when this device holds it.
    fn file_bytes(&self, file: &crate::blob::FileValue) -> Result<Vec<u8>, String> {
        let blobs = self.blobs.borrow();
        let Some(blobs) = blobs.as_ref() else {
            return Err("this host holds no content store".to_owned());
        };
        blobs
            .read(&file.hash)
            .ok_or_else(|| format!("the content of `{}` is not on this device", file.name))
    }

    /// `bytes` of `media` kept in the content store as a file called `name`.
    fn keep_bytes(
        &self,
        name: &str,
        media: &str,
        bytes: &[u8],
    ) -> Result<crate::blob::FileValue, String> {
        let blobs = self.blobs.borrow();
        let Some(blobs) = blobs.as_ref() else {
            return Err("this host keeps no files".to_owned());
        };
        blobs.keep(name, media, bytes)
    }
}

/// What a program asks a file's pixels: the arguments read where a trap is still the answer,
/// and the pixels changed where the failure is the action's to read.
enum Change {
    Crop {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    },
    Turn(i64),
    Resize {
        width: i64,
        height: i64,
    },
    Encode {
        format: String,
        quality: i64,
    },
}

/// The name a transformed file is kept under: the source's stem, so a person can tell what it
/// came from.
fn stem_of(name: &str) -> &str {
    Path::new(name)
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .filter(|stem| !stem.is_empty())
        .unwrap_or("image")
}

fn extension_of(encoding: Encoding) -> &'static str {
    match encoding {
        Encoding::Png => "png",
        Encoding::Jpeg { .. } => "jpg",
        Encoding::WebP => "webp",
    }
}
