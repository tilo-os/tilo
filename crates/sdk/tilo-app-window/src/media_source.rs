//! A view's media address, as the medium the window plays.
//!
//! A tilolang view names a song or a video by its content address (`tilo-blob://<hash>`), the same way it
//! names a picture: the bytes live in the workspace's content store, not at a path the view can
//! spell. The window reads them from the device's store and stages a file the toolkit's player
//! opens, so the UI library itself knows nothing of Tilo's addresses. A file or stream address
//! (`file://…`, `http(s)://…`) plays as it stands.

/// The medium `address` names, staged when it is a content address. `name` is the file's name
/// where the view knows it, so the staged file keeps the extension its container is read by: the
/// bytes alone say an MP4 is a video, never that it is Matroska.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub(crate) fn playable(address: &str, name: Option<&str>) -> String {
    media::playable(address, name, |hash| {
        tilo_content_device::device().and_then(|store| store.read(hash))
    })
}

/// The address as it stands: a page's media element plays what it names, and reads no device
/// content store.
#[cfg(target_arch = "wasm32")]
#[must_use]
pub(crate) fn playable(address: &str, _name: Option<&str>) -> String {
    address.to_owned()
}
