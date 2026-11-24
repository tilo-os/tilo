//! A view's media address, as the medium the window plays.
//!
//! A tilolang view names a song or a video by its content address (`tilo-blob://<hash>`), the same way it
//! names a picture: the bytes live in the workspace's content store, not at a path the view can
//! spell. The window reads them from the device's store and stages a file the toolkit's player
//! opens, so the UI library itself knows nothing of Tilo's addresses. A file or stream address
//! (`file://…`, `http(s)://…`) plays as it stands.
//!
//! A file a package ships beside its sources is named `pkg:<name>` (`pkg:media/first-light.wav`).
//! The host installs those files when it loads the package ([`install_package_assets`]); this
//! module stages the one an address names, natively as a file and on a page as a blob URL of its
//! bytes, so nothing is fetched over the network.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

/// The files the package being shown ships under `media/`, by the name its addresses give them.
static PACKAGE_ASSETS: OnceLock<Mutex<BTreeMap<String, Arc<[u8]>>>> = OnceLock::new();

/// Install the files a package ships beside its sources, as `(name, bytes)` where `name` is the
/// path from the package's directory (`media/first-light.wav`): what a view or a scenario names as
/// `pkg:<name>`. A host calls this when it loads a package, before any of its windows is laid out.
pub fn install_package_assets(assets: impl IntoIterator<Item = (String, Vec<u8>)>) {
    let held = PACKAGE_ASSETS.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut held = match held.lock() {
        Ok(held) => held,
        Err(poisoned) => poisoned.into_inner(),
    };
    for (name, bytes) in assets {
        held.insert(name, Arc::from(bytes));
    }
}

/// The bytes of the package's file `name`, when the host installed it.
fn package_asset(name: &str) -> Option<Arc<[u8]>> {
    let held = PACKAGE_ASSETS.get_or_init(|| Mutex::new(BTreeMap::new()));
    let held = match held.lock() {
        Ok(held) => held,
        Err(poisoned) => poisoned.into_inner(),
    };
    held.get(name).cloned()
}

/// The medium `address` names, staged when it is a package's own file or a content address. `name`
/// is the file's name where the view knows it, so the staged file keeps the extension its container
/// is read by: the bytes alone say an MP4 is a video, never that it is Matroska.
#[must_use]
pub fn playable(address: &str, name: Option<&str>) -> String {
    if let Some(asset) = address.strip_prefix("pkg:") {
        let Some(bytes) = package_asset(asset) else {
            // The package ships no such file, or the host installed none: the player is handed the
            // address it was given, and its own failure says what it could not play.
            return address.to_owned();
        };
        return media::Source::from_contents(asset, &bytes)
            .ok()
            .and_then(|source| source.to_uri().ok())
            .unwrap_or_else(|| address.to_owned());
    }
    #[cfg(target_arch = "wasm32")]
    {
        // A page's media element plays what it names itself, and reads no device content store.
        let _ = name;
        address.to_owned()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let Some(hash) = address.strip_prefix("tilo-blob://") else {
            return address.to_owned();
        };
        let Some(bytes) = tilo_content_device::device().and_then(|store| store.read(hash)) else {
            // The bytes are not here yet, or this process holds no store: the player is handed the
            // address it was given, and its own failure says what it could not play.
            return address.to_owned();
        };
        let file = name.filter(|name| !name.is_empty()).unwrap_or("video");
        media::Source::from_contents(file, &bytes)
            .ok()
            .and_then(|source| source.to_uri().ok())
            .unwrap_or_else(|| address.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file a package ships plays from the file itself, with nothing fetched: a scenario's
    /// sound works with no network at all.
    #[test]
    fn a_packages_own_file_plays_without_the_network() {
        install_package_assets([("media/first-light.wav".to_owned(), b"RIFF....WAVE".to_vec())]);
        let played = playable("pkg:media/first-light.wav", Some("First Light"));
        assert_ne!(played, "pkg:media/first-light.wav");
        assert!(!played.contains("music.example"), "{played}");
        let bytes = std::fs::read(played.strip_prefix("file://").expect("a staged file"))
            .expect("the staged file reads");
        assert_eq!(bytes, b"RIFF....WAVE");
    }

    /// An address naming a file the package did not ship is handed to the player as it is, so its
    /// own failure says what it could not play.
    #[test]
    fn an_address_the_package_does_not_ship_stays_as_it_is() {
        let address = "pkg:media/absent.wav";
        assert_eq!(playable(address, None), address);
    }
}
