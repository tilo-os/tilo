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
