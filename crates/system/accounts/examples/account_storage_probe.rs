//! Production account-vault browser probe, driven by ../web/run.mjs.
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod browser {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    use accounts::{AccountId, Saved, Secret, SecretItem, SecretStore, SecretVault};
    use secret_vault::VaultFormat;
    use tilo_files::{Dir, Location, VPath, VirtualRoot};
    // Async wasm-bindgen exports expand to calls into this crate.
    extern crate wasm_bindgen_futures;

    use wasm_bindgen::prelude::*;

    // The pre-existing account vault's format, used only to seed a legacy fixture.
    const LEGACY: VaultFormat = VaultFormat {
        name: "accounts",
        magic: b"QASV",
        secret_service_label: "Tilo account credentials",
        secret_service_attributes: &[("application", "Tilo"), ("purpose", "account-vault")],
        dpapi_entropy: b"Tilo Accounts vault key v1",
    };

    thread_local! {
        static VAULTS: RefCell<BTreeMap<String, SecretVault>> = RefCell::default();
        static STORES: RefCell<BTreeMap<String, SecretStore>> = RefCell::default();
    }

    fn error(error: impl std::fmt::Display) -> JsValue {
        JsValue::from_str(&error.to_string())
    }

    fn path() -> Result<VPath, JsValue> {
        VPath::new("accounts/vault/vault.enc").map_err(error)
    }

    fn dir(profile: &str) -> Result<Dir, JsValue> {
        Dir::open(Location::Virtual(
            VirtualRoot::Opfs {
                profile: profile.to_owned(),
            },
            VPath::new("tilo/config").map_err(error)?,
        ))
        .map_err(error)
    }

    #[wasm_bindgen]
    pub async fn seed(profile: &str, password: &str) -> Result<(), JsValue> {
        tilo_secrets::web::mirror().await.map_err(error)?;
        let key = LEGACY.load_or_create_page_key(false).await.map_err(error)?;
        let entries = BTreeMap::from([("legacy", Secret::password(password))]);
        let plain = serde_json::to_vec(&entries).map_err(error)?;
        let blob = LEGACY
            .seal(&key, &plain)
            .ok_or_else(|| error("seal failed"))?;
        dir(profile)?
            .write_atomic(&path()?, &blob)
            .await
            .map_err(error)
    }

    #[wasm_bindgen]
    pub async fn open(profile: &str, handle: &str) -> Result<(), JsValue> {
        tilo_files::web::mirror(profile).await.map_err(error)?;
        tilo_secrets::web::mirror().await.map_err(error)?;
        let vault = SecretVault::open_page(dir(profile)?);
        if let Some(problem) = vault.recovery_error() {
            return Err(error(problem));
        }
        VAULTS.with_borrow_mut(|vaults| vaults.insert(handle.to_owned(), vault));
        Ok(())
    }

    #[wasm_bindgen]
    pub fn get(handle: &str, id: &str) -> Result<Option<String>, JsValue> {
        VAULTS.with_borrow(|vaults| {
            let vault = vaults.get(handle).ok_or_else(|| error("unknown handle"))?;
            Ok(vault.get(&AccountId::new(id)).map(|secret| secret.password))
        })
    }

    #[wasm_bindgen]
    pub fn edit(handle: &str, id: &str, password: &str) -> Result<(), JsValue> {
        VAULTS.with_borrow_mut(|vaults| {
            let vault = vaults
                .get_mut(handle)
                .ok_or_else(|| error("unknown handle"))?;
            vault.put(&AccountId::new(id), &Secret::password(password));
            Ok(())
        })
    }

    /// Pending and failed saves both retain edits; the driver checks exact errors.
    #[wasm_bindgen]
    pub fn save(handle: &str) -> Result<(), JsValue> {
        VAULTS.with_borrow(|vaults| {
            vaults
                .get(handle)
                .ok_or_else(|| error("unknown handle"))?
                .try_save()
                .map_err(error)
        })
    }

    /// A synchronous service facade may reopen its vault for every call.
    #[wasm_bindgen]
    pub fn save_fresh(profile: &str, id: &str, password: &str) -> Result<(), JsValue> {
        let mut vault = SecretVault::open_page(dir(profile)?);
        vault.put(&AccountId::new(id), &Secret::password(password));
        vault.try_save().map_err(error)
    }

    /// The password manager's store: an item saved while the browser still
    /// writes stays in it, and is kept once the write lands.
    #[wasm_bindgen]
    pub async fn store_open(profile: &str, handle: &str) -> Result<(), JsValue> {
        tilo_files::web::mirror(profile).await.map_err(error)?;
        tilo_secrets::web::mirror().await.map_err(error)?;
        let store = SecretStore::open_page(dir(profile)?);
        STORES.with_borrow_mut(|stores| stores.insert(handle.to_owned(), store));
        Ok(())
    }

    #[wasm_bindgen]
    pub fn store_put(handle: &str, title: &str) -> Result<(), JsValue> {
        STORES.with_borrow_mut(|stores| {
            let store = stores
                .get_mut(handle)
                .ok_or_else(|| error("unknown handle"))?;
            store
                .put(&mut SecretItem::login(title, "me", "hunter2"))
                .map_err(|problem| error(format!("{problem:?}")))
        })
    }

    #[wasm_bindgen]
    pub fn store_titles(handle: &str) -> Result<Vec<String>, JsValue> {
        STORES.with_borrow(|stores| {
            let store = stores.get(handle).ok_or_else(|| error("unknown handle"))?;
            Ok(store.list().into_iter().map(|item| item.title).collect())
        })
    }

    /// `durable` once the browser kept the store's edits, else `pending`.
    #[wasm_bindgen]
    pub fn store_flush(handle: &str) -> Result<String, JsValue> {
        STORES.with_borrow(|stores| {
            let store = stores.get(handle).ok_or_else(|| error("unknown handle"))?;
            match store.flush() {
                Ok(Saved::Durable) => Ok("durable".to_owned()),
                Ok(Saved::Pending) => Ok("pending".to_owned()),
                Err(problem) => Err(error(format!("{problem:?}"))),
            }
        })
    }

    #[wasm_bindgen]
    pub fn pending(profile: &str) -> usize {
        tilo_files::web::pending(profile)
    }

    #[wasm_bindgen]
    pub async fn flush(profile: &str) -> Result<(), JsValue> {
        tilo_files::web::flush(profile).await.map_err(error)
    }

    #[wasm_bindgen]
    pub async fn ambiguous_inventory() -> Result<(), JsValue> {
        tilo_secrets::SecretStore::for_app("tilo.accounts")
            .put(
                "vault.entries",
                br#"{"ambiguous":{"password":"unassigned"}}"#,
                tilo_secrets::PutOptions::default(),
            )
            .await
            .map_err(error)
    }

    #[wasm_bindgen]
    pub async fn ambiguous_retained() -> Result<bool, JsValue> {
        Ok(tilo_secrets::SecretStore::for_app("tilo.accounts")
            .get("vault.entries")
            .await
            .map_err(error)?
            .is_some_and(|secret| secret.expose() == br#"{"ambiguous":{"password":"unassigned"}}"#))
    }

    #[wasm_bindgen]
    pub async fn encrypted(profile: &str) -> Result<bool, JsValue> {
        let bytes = dir(profile)?.read(&path()?).await.map_err(error)?;
        // A production load authenticates it; this also rejects a plaintext JSON inventory.
        Ok(serde_json::from_slice::<serde_json::Value>(&bytes).is_err())
    }
}
