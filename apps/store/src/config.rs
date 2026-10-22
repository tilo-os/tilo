//! The registries the user pulls Tilo apps from: `registries.toml` under
//! `$XDG_CONFIG_HOME/tilo-store/`, the list `app repo` edits.
//!
//! The sandboxed Store has a home of its own, so it never reads that file
//! itself: it asks the app host, which runs `app repo list --json` as the
//! person ([`repositories`]). A copy in the sandbox would be a second list
//! `app repo` never edits.
//!
//! **Owned by [`app_cli::config`]**, not by this app. The desktop's search
//! box reads the same file to decide what software the machine could open,
//! and the privileged app host verifies installs against it, so a second
//! definition of its shape would eventually mean the store, the palette and
//! the installer disagreed about which registries exist. The shared crate is
//! the owner because the app, the compositor and the host all depend on it.
//!
//! Reading it is best-effort: a missing or malformed file is never fatal —
//! the store falls back to the default registry and keeps rendering.

// The registry list, its file format and the directories it lives under,
// re-exported so there is still exactly one definition of each.
pub use app_cli::config::{
    DEFAULT_REGISTRY_BASE_URL, RegistriesFile, RegistryConfig, cache_dir, config_dir,
    load_registries, placeholder_owner_pubkey, registries_path,
};

/// The person's app repositories, through the app host. Without one (a
/// test, a development run outside a session) the configuration directory
/// the environment names is read, and nothing is ever written there.
pub fn repositories(
    host: Option<&app_host::client::BlockingClient>,
) -> Result<RegistriesFile, String> {
    let Some(host) = host else {
        return Ok(app_cli::config::load_registries_no_seed());
    };
    let json = host.repositories()?;
    serde_json::from_str(&json).map_err(|error| format!("unreadable repository list: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use tilo_test_env::ScopedEnv;

    /// Point the store's config directory and official registry trust key
    /// at `td` for as long as the returned scope lives.
    fn scoped_dir(td: &TempDir) -> ScopedEnv {
        let trust = td.path().join("registry-owner.pub");
        std::fs::write(
            &trust,
            "d5dd5851bebac876b19986485184213ead36b67595b7efedf1144c120033b0bb\n",
        )
        .unwrap();
        let mut env = ScopedEnv::new();
        env.set("TILO_STORE_CONFIG_DIR", td.path())
            .set("TILO_OFFICIAL_REGISTRY_OWNER_KEY_PATH", trust);
        env
    }

    #[test]
    fn registry_config_shares_the_desktop_path_seeds_and_round_trips() {
        // Case `the_store_reads_the_same_registry_file_the_desktop_does`.
        // The store and the desktop have to resolve `registries.toml` to
        // the same path, or "what this machine can install" and "what this
        // machine can open" become two different answers. Asserted here
        // rather than left to the re-export being obviously correct: the
        // re-export is what someone would undo first when adding a
        // store-only setting.
        {
            let td = TempDir::new().unwrap();
            let _g = scoped_dir(&td);
            assert_eq!(
                registries_path(),
                app_cli::config::registries_path(),
                "one registry list, two readers"
            );
            assert_eq!(config_dir().as_deref(), Some(td.path()));
        }

        // Case `without_an_app_host_nothing_is_written`.
        // The Store's own reading never seeds a list in its sandbox.
        {
            let td = TempDir::new().unwrap();
            let _g = scoped_dir(&td);
            assert!(repositories(None).unwrap().registries.is_empty());
            assert!(!td.path().join("registries.toml").exists());
        }

        {
            let td = TempDir::new().unwrap();
            let _g = scoped_dir(&td);
            let regs = load_registries();
            assert_eq!(regs.registries.len(), 1);
            assert_eq!(regs.registries[0].name, "Tilo main");
            assert!(td.path().join("registries.toml").exists());
        }

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
