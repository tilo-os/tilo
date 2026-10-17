//! Device-local identity work. Custody persists independently of this controller.
use super::super::prepared_identity::PreparedIdentity;
use std::{
    path::PathBuf,
    sync::{Arc, mpsc},
};
use zeroize::Zeroizing;

enum ResultValue {
    Prepared(Arc<PreparedIdentity>),
    Updated,
    PhotoSelected {
        path: String,
        photo: runtime_identity::ProfilePhoto,
    },
    JourneyPrepared {
        prepared: Arc<PreparedIdentity>,
        path: Option<String>,
        photo: Option<runtime_identity::ProfilePhoto>,
    },
    Exported(runtime_identity::PortableBackupInfo),
    AccountsExported(usize),
    #[cfg(target_os = "linux")]
    SetupExported(super::super::portable_transfer::PackageInfo),
}

struct PhotoEdit {
    path: Option<String>,
    photo: Option<runtime_identity::ProfilePhoto>,
    state: &'static str,
    metadata: Option<runtime_identity::IdentityMetadata>,
}

#[derive(Default)]
pub(super) struct Personal {
    prepared: Option<Arc<PreparedIdentity>>,
    imported: bool,
    worker: Option<mpsc::Receiver<Result<ResultValue, ()>>>,
    state: &'static str,
    photo_edit: Option<PhotoEdit>,
    artifact: Option<serde_json::Value>,
    avatar_path: Option<String>,
    photo: Option<runtime_identity::ProfilePhoto>,
}

impl Personal {
    pub fn begin_photo_edit(&mut self) -> Result<(), &'static str> {
        let metadata = self
            .prepared
            .as_ref()
            .map(|prepared| prepared.metadata())
            .transpose()
            .map_err(|_| "installer-preview-identity-prepare-failed")?;
        self.photo_edit = Some(PhotoEdit {
            path: self.avatar_path.clone(),
            photo: self.photo.clone(),
            state: self.state,
            metadata,
        });
        Ok(())
    }

    pub fn cancellable_photo_read(&self) -> bool {
        self.photo_edit.is_some() && self.state == "reading-avatar"
    }

    pub fn cancel_photo_edit(&mut self) -> Result<(), &'static str> {
        if self.busy() && !self.cancellable_photo_read() {
            return Err("installer-preview-identity-busy");
        }
        if let Some(edit) = &self.photo_edit
            && let Some(before) = &edit.metadata
        {
            let current = self
                .prepared
                .as_ref()
                .ok_or("installer-preview-identity-prepare-failed")?
                .metadata()
                .map_err(|_| "installer-preview-identity-prepare-failed")?;
            if &current != before {
                // A failed durability acknowledgement may follow publication.
                // Never report cancellation while the canonical identity differs.
                return Err("installer-preview-identity-prepare-failed");
            }
        }
        // Only a source image read can be retired. Durable identity work must
        // finish on its retained receiver and cannot be detached by Cancel.
        if self.cancellable_photo_read() {
            self.worker = None;
        }
        if let Some(edit) = self.photo_edit.take() {
            self.avatar_path = edit.path;
            self.photo = edit.photo;
            self.state = edit.state;
        }
        Ok(())
    }

    pub fn save_profile(
        &mut self,
        name: String,
        avatar: Option<String>,
    ) -> Result<(), &'static str> {
        let prepared = self
            .prepared
            .clone()
            .ok_or("installer-preview-identity-not-prepared")?;
        let photo = if self.avatar_path == avatar {
            self.photo.clone()
        } else {
            return Err("installer-preview-avatar-failed");
        };
        self.start("updating", move || {
            let before = prepared.metadata().map_err(|_| ())?;
            if prepared.update_presentation(&name, photo.clone()).is_err() {
                // Best-effort recovery after an uncertain publication. Cancellation
                // separately verifies exact canonical metadata before restoring UI.
                let _ = prepared.update_presentation(&before.display_name, before.profile_photo);
                return Err(());
            }
            Ok(ResultValue::JourneyPrepared {
                prepared,
                path: avatar,
                photo,
            })
        })
    }

    pub fn commit_photo_edit(&mut self) {
        self.photo_edit = None;
    }

    /// The ordinary Continue action prepares durable custody without activating
    /// a persona in the original OS. A failed attempt keeps its exact path;
    /// retry never silently replaces an identity created before that failure.
    pub fn select_photo(&mut self, path: String) -> Result<(), &'static str> {
        if self.busy() {
            return Err("installer-preview-identity-busy");
        }
        // Every explicit selection invalidates the old pixels, even if the path
        // spelling is identical. No mutable-source preview remains visible.
        self.avatar_path = None;
        self.photo = None;
        self.start("reading-avatar", move || {
            let photo = normalize_avatar(&path).map_err(|_| ())?;
            Ok(ResultValue::PhotoSelected { path, photo })
        })
    }

    pub fn prepare_for_journey(
        &mut self,
        name: String,
        avatar: Option<String>,
        choices: super::draft::PublicChoices,
    ) -> Result<(), &'static str> {
        let cached = (self.avatar_path == avatar).then(|| self.photo.clone());
        let existing = self.prepared.clone();
        self.start(
            if existing.is_some() {
                "updating"
            } else {
                "preparing"
            },
            move || {
                // Decode before touching identity custody; a refused file cannot
                // mint a key or partially change the existing person's name.
                let photo = match cached {
                    Some(photo) => photo,
                    None => return Err(()),
                };
                let store = super::draft::DraftStore::for_session().map_err(|_| ())?;
                let pending = if existing.is_none() {
                    Some(
                        store
                            .reserve(&choices, super::draft::IdentityKind::New)
                            .map_err(|_| ())?,
                    )
                } else {
                    None
                };
                let prepared = if let Some(prepared) = existing {
                    prepared
                } else {
                    let directory = pending.as_ref().ok_or(())?.identity_directory();
                    let pin = ensure_private_tree(directory.parent().ok_or(())?).map_err(|_| ())?;
                    super::super::destination_security::check_private_file(&pin.file)
                        .map_err(|_| ())?;
                    Arc::new(
                        PreparedIdentity::create(
                            &directory,
                            &name,
                            [128, 128, 128],
                            imager_backend::prepared_identity::storage_for_session(),
                        )
                        .map_err(|_| ())?,
                    )
                };
                prepared
                    .update_presentation(&name, photo.clone())
                    .map_err(|_| ())?;
                if let Some(pending) = &pending {
                    store
                        .acknowledge(pending, &prepared, None)
                        .map_err(|_| ())?;
                } else {
                    store
                        .update_choices_for(&prepared, &choices)
                        .map_err(|_| ())?;
                }
                Ok(ResultValue::JourneyPrepared {
                    prepared,
                    path: avatar,
                    photo,
                })
            },
        )
    }

    pub fn prepared_handle(&self) -> Option<Arc<PreparedIdentity>> {
        if self.busy() {
            None
        } else {
            self.prepared.clone()
        }
    }

    /// An explicit first-time choice releases imported custody without altering
    /// its durable files or the original backup.
    pub fn imported(&self) -> bool {
        self.imported
    }

    pub fn reset_preparation(&mut self) -> Result<(), &'static str> {
        if self.busy() {
            return Err("installer-preview-identity-busy");
        }
        self.prepared = None;
        self.imported = false;
        self.state = "";
        self.artifact = None;
        self.avatar_path = None;
        self.photo = None;
        Ok(())
    }

    pub fn adopt(&mut self, prepared: Arc<PreparedIdentity>) -> Result<(), &'static str> {
        if self.busy() {
            return Err("installer-preview-identity-busy");
        }
        let metadata = prepared
            .metadata()
            .map_err(|_| "installer-preview-identity-unavailable")?;
        self.photo = metadata.profile_photo;
        self.avatar_path = None;
        self.prepared = Some(prepared);
        self.imported = true;
        self.state = "prepared";
        self.artifact = None;
        Ok(())
    }

    pub fn resume(
        &mut self,
        prepared: Arc<PreparedIdentity>,
        kind: super::draft::IdentityKind,
        metadata: runtime_identity::IdentityMetadata,
    ) -> Result<(), &'static str> {
        if self.busy() {
            return Err("installer-preview-identity-busy");
        }
        self.photo = metadata.profile_photo;
        self.avatar_path = None;
        self.prepared = Some(prepared);
        self.imported = kind != super::draft::IdentityKind::New;
        self.state = "prepared";
        self.artifact = None;
        Ok(())
    }

    pub fn adopt_for_journey(
        &mut self,
        prepared: Arc<PreparedIdentity>,
        choices: super::draft::PublicChoices,
        kind: super::draft::IdentityKind,
    ) -> Result<(), &'static str> {
        self.adopt(prepared.clone())?;
        let photo = self.photo.clone();
        self.start("saving", move || {
            let store = super::draft::DraftStore::for_session().map_err(|_| ())?;
            store
                .record_prepared(&choices, kind, &prepared)
                .map_err(|_| ())?;
            Ok(ResultValue::JourneyPrepared {
                prepared,
                path: None,
                photo,
            })
        })
    }

    pub fn ready(&self) -> bool {
        self.prepared.is_some() && self.state == "prepared"
    }

    pub fn photo(&self) -> Option<&runtime_identity::ProfilePhoto> {
        self.photo.as_ref()
    }

    fn start(
        &mut self,
        state: &'static str,
        work: impl FnOnce() -> Result<ResultValue, ()> + Send + 'static,
    ) -> Result<(), &'static str> {
        if self.busy() {
            return Err("installer-preview-identity-busy");
        }
        imager_backend::prepared_identity::require_supported()
            .map_err(|_| "installer-preview-identity-unavailable")?;
        let (tx, rx) = mpsc::channel();
        self.worker = Some(rx);
        self.state = state;
        self.artifact = None;
        std::thread::spawn(move || {
            let _ = tx.send(work());
        });
        Ok(())
    }
    pub fn create(
        &mut self,
        directory: PathBuf,
        name: String,
        color: [u8; 3],
    ) -> Result<(), &'static str> {
        if self.prepared.is_some() {
            return Err("installer-preview-identity-already-prepared");
        }
        self.start("preparing", move || {
            PreparedIdentity::create(
                &directory,
                &name,
                color,
                imager_backend::prepared_identity::storage_for_session(),
            )
            .map(|prepared| ResultValue::Prepared(Arc::new(prepared)))
            .map_err(|_| ())
        })
    }
    pub fn update(&mut self, name: String, color: [u8; 3]) -> Result<(), &'static str> {
        let prepared = self
            .prepared
            .clone()
            .ok_or("installer-preview-identity-not-prepared")?;
        self.start("updating", move || {
            prepared
                .update_profile(&name, color)
                .map(|_| ResultValue::Updated)
                .map_err(|_| ())
        })
    }
    pub fn export(
        &mut self,
        destination: PathBuf,
        password: Zeroizing<String>,
    ) -> Result<(), &'static str> {
        let prepared = self
            .prepared
            .clone()
            .ok_or("installer-preview-identity-not-prepared")?;
        self.start("exporting", move || {
            prepared
                .export(&destination, password)
                .map(ResultValue::Exported)
                .map_err(|_| ())
        })
    }
    pub fn busy(&self) -> bool {
        self.worker.is_some()
    }
    #[cfg(target_os = "linux")]
    pub fn export_setup(
        &mut self,
        accounts: Option<super::accounts::PreparedAccounts>,
        destination: PathBuf,
        password: Zeroizing<String>,
        binding: String,
        source_home: Option<PathBuf>,
    ) -> Result<(), &'static str> {
        let prepared = self
            .prepared
            .clone()
            .ok_or("installer-preview-identity-not-prepared")?;
        self.start("exporting-setup", move || {
            use super::super::{portable_transfer, prepared_setup};
            imager_backend::startup_password::validate(&password).map_err(|_| ())?;
            let parent = super::super::migration::PinnedDirectory::open_absolute(
                destination.parent().ok_or(())?,
            )
            .map_err(|_| ())?;
            parent.validate_destination().map_err(|_| ())?;
            super::super::destination_security::check_private_file(&parent.file).map_err(|_| ())?;
            if let Some(source) = source_home {
                let source = super::super::migration::PinnedDirectory::open_absolute(&source)
                    .map_err(|_| ())?;
                if parent.contains_identity(&source.identity) {
                    return Err(());
                }
            }
            if let Some(accounts) = &accounts {
                accounts
                    .validate_artifact_destination(&destination)
                    .map_err(|_| ())?;
            }
            let ids = accounts
                .as_ref()
                .map_or_else(Vec::new, |accounts| accounts.selected_ids());
            let mut manifest =
                prepared_setup::Manifest::new(binding.clone(), prepared.public_key(), ids)
                    .map_err(|_| ())?;
            let staging = tempfile::tempdir().map_err(|_| ())?;
            let root = staging.path().canonicalize().map_err(|_| ())?;
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
                .map_err(|_| ())?;
            prepared
                .export(&root.join(prepared_setup::IDENTITY_FILE), password.clone())
                .map_err(|_| ())?;
            if let Some(accounts) = accounts.filter(|_| !manifest.account_ids.is_empty()) {
                accounts
                    .export_password_sealed(
                        &root.join(prepared_setup::ACCOUNTS_FILE),
                        password.as_bytes(),
                        binding.as_bytes(),
                    )
                    .map_err(|_| ())?;
            }
            if !manifest.account_ids.is_empty() {
                if super::super::account_transfer::PasswordSealedAccounts::inspect_prepared_consumer(
                    &root.join(prepared_setup::ACCOUNTS_FILE),
                    password.as_bytes(),
                    binding.as_bytes(),
                    &manifest.account_ids,
                )
                .map_err(|_| ())?
                {
                    manifest.account_custody_protocol = Some(1);
                }
            }
            manifest.write(&root).map_err(|_| ())?;
            let review = portable_transfer::ExportReview::review(
                &root,
                &destination,
                portable_transfer::Purpose::PreparedSetup,
                prepared_setup::LIMITS,
            )
            .map_err(|_| ())?;
            review
                .export(
                    Zeroizing::new(password.as_bytes().to_vec()),
                    &std::sync::atomic::AtomicBool::new(false),
                )
                .map(ResultValue::SetupExported)
                .map_err(|_| ())
        })
    }
    pub fn export_accounts(
        &mut self,
        prepared: super::accounts::PreparedAccounts,
        destination: PathBuf,
        password: Zeroizing<String>,
        binding: String,
    ) -> Result<(), &'static str> {
        self.start("exporting-accounts", move || {
            imager_backend::startup_password::validate(&password).map_err(|_| ())?;
            prepared
                .export_password_sealed(&destination, password.as_bytes(), binding.as_bytes())
                .map(ResultValue::AccountsExported)
                .map_err(|_| ())
        })
    }
    pub fn poll(&mut self) -> bool {
        let Some(worker) = &self.worker else {
            return false;
        };
        let result = match worker.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(mpsc::TryRecvError::Disconnected) => Err(()),
        };
        self.worker = None;
        match result {
            Ok(ResultValue::PhotoSelected { path, photo }) => {
                self.avatar_path = Some(path);
                self.photo = Some(photo);
                self.state = if self.prepared.is_some() {
                    "prepared"
                } else {
                    "photo-selected"
                };
            }
            Ok(ResultValue::JourneyPrepared {
                prepared,
                path,
                photo,
            }) => {
                self.prepared = Some(prepared);
                self.avatar_path = path;
                self.photo = photo;
                self.state = "prepared";
            }
            Ok(ResultValue::Prepared(prepared)) => {
                self.prepared = Some(prepared);
                self.state = "prepared";
            }
            Ok(ResultValue::Updated) => self.state = "prepared",
            Ok(ResultValue::Exported(info)) => {
                self.artifact = Some(
                    serde_json::json!({"public_key":hex::encode(info.group_public_key),"bytes":info.bytes_written}),
                );
                self.state = "exported";
            }
            Ok(ResultValue::AccountsExported(count)) => {
                self.artifact = Some(serde_json::json!({"accounts":count}));
                self.state = "accounts-exported";
            }
            #[cfg(target_os = "linux")]
            Ok(ResultValue::SetupExported(info)) => {
                self.artifact = Some(serde_json::json!({"purpose":info.purpose,
                    "sealed_bytes":info.sealed_bytes,"entries":info.entries}));
                self.state = "setup-exported";
            }
            Err(()) => self.state = "failed",
        }
        true
    }
    pub fn projection(&self) -> serde_json::Value {
        serde_json::json!({"state":if self.state.is_empty() {"unprepared"} else {self.state},
            "public_key":self.prepared.as_ref().map(|prepared|hex::encode(prepared.public_key())),
            "artifact":self.artifact})
    }
}

pub(in crate::setup) use imager_backend::session::preparation_namespace;

pub(super) fn preparation_directory() -> Option<PathBuf> {
    Some(
        preparation_namespace()?
            .join(uuid::Uuid::new_v4().to_string())
            .join("identity"),
    )
}

#[cfg(test)]
use imager_backend::session::resolve_state_root;

#[cfg(all(test, unix))]
mod state_root_tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    #[test]
    fn home_alias_with_missing_state_root_opens_a_fresh_draft() {
        let mut env = tilo_test_env::ScopedEnv::new();
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let actual = root.join("actual-home");
        std::fs::create_dir(&actual).unwrap();
        std::fs::set_permissions(&actual, std::fs::Permissions::from_mode(0o700)).unwrap();
        let alias = root.join("home-alias");
        symlink(&actual, &alias).unwrap();
        env.set("HOME", &alias);
        env.remove("XDG_STATE_HOME");
        let custody = preparation_directory().unwrap();
        assert!(custody.starts_with(actual.join(".local/state/tilo/installer/preparations")));
        let store = super::super::draft::DraftStore::for_session().unwrap();
        assert!(matches!(
            store.review().unwrap(),
            super::super::draft::DraftStatus::Missing
        ));
        assert!(!custody.exists());
    }

    #[test]
    fn explicit_state_alias_does_not_relax_custody_symlinks_or_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let state = root.join("state");
        std::fs::create_dir(&state).unwrap();
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
        let alias = root.join("state-alias");
        symlink(&state, &alias).unwrap();
        let resolved = resolve_state_root(&alias).unwrap();
        assert_eq!(resolved, state);
        let custody = resolved.join("tilo/installer/preparations");
        let pin = ensure_private_tree(&custody).unwrap();
        std::fs::rename(&custody, custody.with_file_name("retained")).unwrap();
        std::fs::create_dir(&custody).unwrap();
        assert!(pin.validate_destination().is_err());
        std::fs::remove_dir(&custody).unwrap();
        symlink(custody.with_file_name("retained"), &custody).unwrap();
        assert!(ensure_private_tree(&custody).is_err());
        assert!(resolve_state_root(std::path::Path::new("relative/state")).is_err());
    }
}

pub(super) use imager_backend::prepared_identity::ensure_private_tree;

/// The explicit selected-file grant is consumed only on the local worker.
/// Hold a nofollow descriptor throughout admission/read, and refuse in-flight
/// changes; canonical pixels are then retained so exports never reread it.
fn normalize_avatar(path: &str) -> std::io::Result<runtime_identity::ProfilePhoto> {
    use std::io;
    let bytes = imager_backend::prepared_identity::read_avatar(std::path::Path::new(path))?;
    let input = tilo_image_codec::AdmissionLimits::default();
    input
        .admit(&bytes)
        .map_err(|_| io::Error::other("avatar source refused"))?;
    let decoded = tilo_image_codec::DecodeImage {
        encoded: &bytes,
        max_dimension: 128,
    }
    .decode_native()
    .map_err(|_| io::Error::other("avatar decode failed"))?;
    let output = tilo_image_codec::AdmissionLimits {
        max_encoded_bytes: 16 * 1024 * 1024,
        max_rgba_bytes: runtime_identity::MAX_PHOTO_BYTES as u64,
        max_dimension: 128,
    };
    output
        .validate(&decoded)
        .map_err(|_| io::Error::other("avatar output refused"))?;
    runtime_identity::ProfilePhoto::new(
        u16::try_from(decoded.width).map_err(io::Error::other)?,
        u16::try_from(decoded.height).map_err(io::Error::other)?,
        decoded.rgba,
    )
    .map_err(io::Error::other)
}
