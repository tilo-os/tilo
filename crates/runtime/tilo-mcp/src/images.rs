//! The pictures an answer shows: what an app stored, read back through its
//! own scope, and carried to the client as MCP image blocks.

use serde_json::Value;

/// Reads content an app of this profile stored, through that app's own
/// scope, so an answer can carry what an action produced (the picture a
/// `look` rendered) instead of only naming it.
pub trait MediaReader: Send + Sync {
    /// The bytes of `hash` as `app_id` stored them; `Ok(None)` when that app
    /// may not read them or they are not here.
    fn read(&self, app_id: &str, hash: &str) -> std::io::Result<Option<Vec<u8>>>;
}

/// Where an answer carries the images it shows until `tool_result` moves
/// them out of the JSON and into MCP image blocks. Not a valid JSON key an
/// app could produce in an output.
pub(crate) const IMAGES: &str = "\u{1}images";
/// At most this many images answer one action.
pub(crate) const MAX_IMAGES: usize = 4;
/// Larger content is named, not shown.
pub(crate) const MAX_IMAGE_BYTES: usize = 8 << 20;
/// The MIME type of image bytes an MCP client can show.
pub(crate) fn image_mime(bytes: &[u8]) -> Option<&'static str> {
    tilo_content::image_format(bytes)
        .map(|format| format.mime)
        .filter(|mime| matches!(*mime, "image/png" | "image/jpeg" | "image/webp"))
}

/// The images an answer carries, taken out of it: its own, and those of
/// each result of a batch.
pub(crate) fn take_images(value: &mut Value) -> Vec<Value> {
    let mut images = Vec::new();
    if let Some(fields) = value.as_object_mut() {
        if let Some(Value::Array(own)) = fields.remove(IMAGES) {
            images.extend(own);
        }
        if let Some(Value::Array(results)) = fields.get_mut("results") {
            for result in results {
                images.extend(take_images(result));
            }
        }
    }
    images
}

/// Where the pictures an answer names are read from, to show them as images.
pub(crate) enum PictureSource {
    /// The store an app of this desktop wrote them to, read in that app's own scope.
    Media(std::sync::Arc<dyn crate::MediaReader>, String),
    /// The workspace, through this device's session with it, as the caller reads it.
    Workspace(
        std::sync::Arc<dyn tilo_actions::remote::ContentGets>,
        Box<tilo_actions::remote::Caller>,
    ),
}

impl PictureSource {
    /// The pictures `fresh` names (each its hash and the reference it is read by), as MCP image
    /// blocks: those that can be read, are no larger than an image this server shows, and are
    /// a kind of image a client draws.
    pub(crate) async fn images(self, fresh: Vec<(String, String)>) -> Vec<Value> {
        tokio::task::spawn_blocking(move || {
            fresh
                .iter()
                .filter_map(|(hash, reference)| self.read(hash, reference))
                .filter(|bytes| bytes.len() <= MAX_IMAGE_BYTES)
                .filter_map(|bytes| {
                    use base64::Engine as _;
                    let mime = image_mime(&bytes)?;
                    Some(serde_json::json!({
                        "type": "image",
                        "mimeType": mime,
                        "data": base64::engine::general_purpose::STANDARD.encode(&bytes),
                    }))
                })
                .collect()
        })
        .await
        .unwrap_or_default()
    }

    /// The bytes of the picture `hash` (`reference` as the answer named it), when they can be
    /// read and are no larger than an image this server shows.
    pub(crate) fn read(&self, hash: &str, reference: &str) -> Option<Vec<u8>> {
        match self {
            Self::Media(media, app_id) => media.read(app_id, hash).ok().flatten(),
            Self::Workspace(gets, caller) => {
                let mut bytes = Vec::new();
                loop {
                    let offset = u64::try_from(bytes.len()).ok()?;
                    let piece = gets.get(caller, reference, offset, 0).ok()?;
                    if usize::try_from(piece.size).ok()? > MAX_IMAGE_BYTES || piece.offset != offset
                    {
                        return None;
                    }
                    if piece.bytes.is_empty() {
                        break;
                    }
                    bytes.extend_from_slice(&piece.bytes);
                    if u64::try_from(bytes.len()).ok()? >= piece.size {
                        break;
                    }
                }
                Some(bytes)
            }
        }
    }
}

/// The pictures a succeeded answer names, at most [`MAX_IMAGES`]: each by its hash (what tells
/// one already shown) and the reference the workspace reads it by, which a sealed piece of
/// content needs whole.
pub(crate) fn named_pictures(value: &Value) -> Vec<(String, String)> {
    value
        .pointer("/update/output/resources")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(|reference| {
            tilo_actions::content_hash(reference)
                .map(|hash| (hash.to_owned(), reference.to_owned()))
        })
        .take(MAX_IMAGES)
        .collect()
}
