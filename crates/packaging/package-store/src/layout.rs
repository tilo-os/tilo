//! Where a package store keeps things, and where the stores are.

use std::path::{Path, PathBuf};

use tilo_app_package::PackageId;

use crate::dev_env::dev_override;

pub const REGISTRY_FILE: &str = "registry.toml";
pub const PACKAGES_DIR: &str = "pkgs";
pub const CONTENT_DIR: &str = "content";
pub const BIN_DIR: &str = "bin";
pub const TMP_DIR: &str = "tmp";
pub const LOCK_FILE: &str = ".lock";
pub const LEASES_DIR: &str = "leases";

pub const SIGNED_ROOT_FILE: &str = "signed-root";
pub const MANIFEST_FILE: &str = "manifest.toml";
pub const COLLECTION_FILE: &str = "collection.qcl";

const META_SUFFIX: &str = ".pkg";

/// The image-wide store. Read-only at runtime: populated during the image
/// build so a first boot already has applications.
pub const SYSTEM_DATA_DIR: &str = "/usr/share/tilo-apps";
pub const SYSTEM_PAYLOAD_DIR: &str = "/usr/lib/tilo-apps";

pub const SYSTEM_DIR_ENV: &str = "TILO_APPS_SYSTEM_DIR";
/// Names the system payload root instead of [`SYSTEM_PAYLOAD_DIR`].
pub const SYSTEM_PAYLOAD_DIR_ENV: &str = "TILO_APPS_SYSTEM_PAYLOAD_DIR";

/// Paths inside one store root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    root: PathBuf,
}

impl Layout {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn registry(&self) -> PathBuf {
        self.root.join(REGISTRY_FILE)
    }

    pub fn packages(&self) -> PathBuf {
        self.root.join(PACKAGES_DIR)
    }

    pub fn payload(&self, id: PackageId) -> PathBuf {
        self.packages().join(id.to_hex())
    }

    pub fn meta_dir(&self, id: PackageId) -> PathBuf {
        self.packages()
            .join(format!("{}{META_SUFFIX}", id.to_hex()))
    }

    pub fn manifest(&self, id: PackageId) -> PathBuf {
        self.meta_dir(id).join(MANIFEST_FILE)
    }

    pub fn content(&self) -> PathBuf {
        self.root.join(CONTENT_DIR)
    }

    pub fn bin(&self) -> PathBuf {
        self.root.join(BIN_DIR)
    }

    pub fn tmp(&self) -> PathBuf {
        self.root.join(TMP_DIR)
    }

    pub fn lock(&self) -> PathBuf {
        self.root.join(LOCK_FILE)
    }

    pub fn lease(&self, id: PackageId) -> PathBuf {
        self.root.join(LEASES_DIR).join(id.to_hex())
    }

    /// The package id whose materialised tree `path` is, if it is one of
    /// this store's.
    pub fn materialised_id(&self, path: &Path) -> Option<PackageId> {
        if path.parent()? != self.packages() {
            return None;
        }
        let name = path.file_name()?.to_str()?;
        parse_id_hex(name)
    }
}

/// Parse a bare 64-character hex package id.
pub fn parse_id_hex(text: &str) -> Option<PackageId> {
    let mut bytes = [0u8; 32];
    hex::decode_to_slice(text, &mut bytes).ok()?;
    Some(PackageId::from_bytes(bytes))
}

/// The per-user store root: `TILO_APPS_DATA_DIR` when set (tests and dev
/// installs), else `$XDG_DATA_HOME/tilo-apps`, else
/// `$HOME/.local/share/tilo-apps`. `None` when none of these is set.
pub fn default_user_root() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("TILO_APPS_DATA_DIR") {
        return Some(PathBuf::from(dir));
    }
    Some(tilo_paths::xdg::data_home()?.join("tilo-apps"))
}

/// Root of the system store, or `None` when there is no system store to
/// read.
///
/// `TILO_APPS_SYSTEM_DIR` names it explicitly (developer builds only — see
/// [`crate::dev_env::DEV_ENV_OVERRIDES`]); otherwise it's
/// [`SYSTEM_DATA_DIR`] — *unless* `TILO_APPS_DATA_DIR` is set, which
/// suppresses it entirely. That's what makes the override usable for
/// tests: `TILO_APPS_DATA_DIR` means "the store is here", and a test
/// asserting on package count needs that to be the whole answer, or the
/// same test would pass on a dev box and fail inside a Tilo image with real
/// preinstalled packages counted alongside fixtures.
pub fn system_data_dir() -> Option<PathBuf> {
    system_data_dir_from(
        dev_override(SYSTEM_DIR_ENV),
        std::env::var_os("TILO_APPS_DATA_DIR").is_some(),
    )
}

fn system_data_dir_from(
    allowed_system_override: Option<std::ffi::OsString>,
    user_store_overridden: bool,
) -> Option<PathBuf> {
    if let Some(dir) = allowed_system_override {
        return Some(PathBuf::from(dir));
    }
    if user_store_overridden {
        return None;
    }
    Some(PathBuf::from(SYSTEM_DATA_DIR))
}

/// Where system payload trees may live besides the system store itself:
/// `TILO_APPS_SYSTEM_PAYLOAD_DIR` (developer builds only), else
/// [`SYSTEM_PAYLOAD_DIR`].
pub fn system_payload_dir() -> PathBuf {
    dev_override(SYSTEM_PAYLOAD_DIR_ENV)
        .map_or_else(|| PathBuf::from(SYSTEM_PAYLOAD_DIR), PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dev_env::dev_override_from;

    #[test]
    fn system_store_overrides_are_gated() {
        {
            for name in [SYSTEM_DIR_ENV, SYSTEM_PAYLOAD_DIR_ENV] {
                assert_eq!(
                    dev_override_from(false, name, Some("/tmp/attacker-store".into())),
                    None,
                    "{name}"
                );
                assert_eq!(
                    dev_override_from(true, name, Some("/dev/store".into())),
                    Some("/dev/store".into()),
                    "{name}"
                );
            }
            let gated = dev_override_from(false, SYSTEM_DIR_ENV, Some("/tmp/attacker".into()));
            assert_eq!(
                system_data_dir_from(gated, false),
                Some(PathBuf::from(SYSTEM_DATA_DIR)),
                "a release build reads the image's store whatever the environment says",
            );
            assert_eq!(system_data_dir_from(None, true), None);
            assert_eq!(
                system_data_dir_from(Some("/dev/store".into()), true),
                Some(PathBuf::from("/dev/store"))
            );
        }
        {
            // The isolation rule the shell's fixture tests depend on: setting only
            // TILO_APPS_DATA_DIR means "the store is exactly here" — otherwise a
            // test asserting a package count would pass on a dev box and fail
            // inside a Tilo image where /usr/share/tilo-apps is populated.
            let mut env = tilo_test_env::ScopedEnv::new();
            env.remove("TILO_APPS_SYSTEM_DIR")
                .set("TILO_APPS_DATA_DIR", "/tmp/tilo-apps-fixture");
            assert_eq!(system_data_dir(), None);
            assert_eq!(
                default_user_root(),
                Some(PathBuf::from("/tmp/tilo-apps-fixture"))
            );

            env.set("TILO_APPS_SYSTEM_DIR", "/tmp/tilo-apps-system");
            assert_eq!(
                system_data_dir(),
                Some(PathBuf::from("/tmp/tilo-apps-system"))
            );

            env.remove("TILO_APPS_DATA_DIR")
                .remove("TILO_APPS_SYSTEM_DIR");
            assert_eq!(
                system_data_dir(),
                Some(PathBuf::from(SYSTEM_DATA_DIR)),
                "production reads the image's store",
            );
        }
    }

    #[test]
    fn a_payload_tree_is_recognised_by_its_package_id() {
        let layout = Layout::new("/store");
        let id = PackageId::from_bytes([7; 32]);
        assert_eq!(layout.materialised_id(&layout.payload(id)), Some(id));
        assert_eq!(layout.materialised_id(&layout.meta_dir(id)), None);
        assert_eq!(
            layout.materialised_id(Path::new("/store/payloads/so.tilo.app")),
            None
        );
        assert_eq!(
            layout.manifest(id),
            layout.meta_dir(id).join("manifest.toml")
        );
    }
}
