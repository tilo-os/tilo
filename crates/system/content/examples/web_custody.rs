//! Production ContentStore/FilesBackend custody exercised by web/run.mjs.
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod probe {
    use tilo_content::{ContentLimits, ContentStore, FilesBackend};
    use tilo_files::{Location, VPath, VirtualRoot};
    use wasm_bindgen::prelude::*;

    fn store(profile: &str) -> Result<ContentStore, JsValue> {
        let location = Location::Virtual(
            VirtualRoot::Opfs {
                profile: profile.to_owned(),
            },
            VPath::new("probe/data").map_err(error)?,
        );
        Ok(ContentStore::new(FilesBackend::new(
            location,
            ContentLimits {
                max_item_bytes: 128,
                ..ContentLimits::default()
            },
        )))
    }
    fn error(error: impl std::fmt::Display) -> JsValue {
        JsValue::from_str(&error.to_string())
    }

    #[wasm_bindgen]
    pub fn prepare(profile: &str) -> js_sys::Promise {
        let profile = profile.to_owned();
        wasm_bindgen_futures::future_to_promise(async move {
            tilo_files::web::mirror(&profile).await.map_err(error)?;
            Ok(JsValue::UNDEFINED)
        })
    }
    #[wasm_bindgen]
    pub fn put(profile: &str, bytes: &[u8]) -> Result<String, JsValue> {
        store(profile)?.put(bytes).map_err(error)
    }
    #[wasm_bindgen]
    pub fn put_committed(profile: &str, bytes: &[u8]) -> js_sys::Promise {
        let profile = profile.to_owned();
        let bytes = bytes.to_vec();
        wasm_bindgen_futures::future_to_promise(async move {
            let store = store(&profile)?;
            let stored = store.put_async(&bytes).await.map_err(error)?;
            Ok(JsValue::from_str(&stored.hash))
        })
    }
    #[wasm_bindgen]
    pub fn mirrored(profile: &str, hash: &str) -> Result<Vec<u8>, JsValue> {
        store(profile)?
            .read(hash)
            .ok_or_else(|| error("missing mirrored content"))
    }
    #[wasm_bindgen]
    pub fn custody(profile: &str, hash: &str) -> js_sys::Promise {
        let profile = profile.to_owned();
        let hash = hash.to_owned();
        wasm_bindgen_futures::future_to_promise(async move {
            let store = store(&profile)?;
            let status = store.durability_async(&hash).await.map_err(error)?;
            Ok(JsValue::from_str(&format!("{status:?}")))
        })
    }
    #[wasm_bindgen]
    pub fn flush(profile: &str) -> js_sys::Promise {
        let profile = profile.to_owned();
        wasm_bindgen_futures::future_to_promise(async move {
            tilo_files::web::flush(&profile).await.map_err(error)?;
            Ok(JsValue::UNDEFINED)
        })
    }
}

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub use probe::*;
