//! The registries the workspace this device keeps fetches Tilo apps from:
//! `workspace/registries`, read through the desktop's action API.
//!
//! The Store keeps no list of its own: a copy in the sandbox would be a second
//! list the workspace's action never changes. **Owned by [`app_cli::config`]**,
//! which reads the reading, so the app, the palette and the installer agree on
//! which registries exist.

pub use app_cli::config::{
    DEFAULT_REGISTRY_BASE_URL, RegistriesFile, RegistryConfig, cache_dir, config_dir,
    load_registries, placeholder_owner_pubkey,
};

/// The workspace's registries, as its `registry.list` reading answers them.
///
/// # Errors
/// Never: a reading that cannot be made is no registries.
pub fn repositories() -> Result<RegistriesFile, String> {
    Ok(load_registries())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use tilo_test_env::ScopedEnv;

    fn scoped_dir(td: &TempDir) -> ScopedEnv {
        let mut env = ScopedEnv::new();
        env.set("TILO_STORE_CONFIG_DIR", td.path());
        env
    }

    #[test]
    fn the_store_reads_the_workspace_list_and_never_writes_one() {
        // One list, one reader: what the Store shows is what `app repo list`
        // shows, because both ask the workspace's reading.
        {
            let td = TempDir::new().unwrap();
            let _g = scoped_dir(&td);
            assert_eq!(
                repositories().unwrap(),
                load_registries(),
                "one registry list, two readers"
            );
        }

        // Nothing configured is no registries, and nothing is written into the
        // Store's sandbox: a copy there would be a list no action changes.
        {
            let td = TempDir::new().unwrap();
            let _g = scoped_dir(&td);
            assert!(repositories().unwrap().registries.is_empty());
            assert!(!td.path().join("registries.toml").exists());
        }

        // A test or development build names a file to read instead of a
        // workspace: the row round-trips as the list the Store uses.
        {
            let td = TempDir::new().unwrap();
            let _g = scoped_dir(&td);
            let text = r#"
[[registry]]
name = "my mirror"
base_url = "https://mirror.example.com"
owner_pubkey_hex = "ab"
enabled = false
"#;
            std::fs::write(td.path().join("registries.toml"), text).unwrap();
            let regs = load_registries();
            assert_eq!(regs.registries.len(), 1);
            assert_eq!(regs.registries[0].name, "my mirror");
            assert!(!regs.registries[0].enabled);
        }
    }
}
