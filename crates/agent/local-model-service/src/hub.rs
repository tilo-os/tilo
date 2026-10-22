//! A person's own models: Hugging Face picks and GGUF files on this device.
//!
//! [`agent_provider_config::local::hub`] decides what can be picked; this
//! module asks the Hub and installs a pick. Every file of a variant is
//! downloaded resumably into the private partial store and verified against
//! the hash the Hub published for the pinned commit — the LFS SHA-256 of a
//! weights file, the git blob id of a small config or tokenizer file —
//! before it enters the content-addressed object store. Once every file is
//! there, the model's directory is assembled from hard links to those
//! objects, named as in the repository, and put in place atomically. What
//! serves it is still a signed catalog runtime (see the module docs of
//! [`agent_provider_config::local::hub`] for why that split is safe).

use std::fs::{self, OpenOptions};
use std::io::{Read as _, Write as _};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use agent_provider_config::local::hub::{
    self as picks, ApiModelInfo, ApiSearchHit, HUB_ORIGIN, HubDigest, HubFile, HubFormat, HubRef,
    HubSearchResult, UserModel, UserModelSource,
};
use agent_provider_config::local::{
    LocalModelRelease, LocalModelStatus, ModelArtifactFormat, PinnedArtifact,
};
use sha1::Sha1;
use sha2::{Digest, Sha256};

use crate::{
    HttpsTransport, LicenseAcceptance, LocalModelService, ManagedModel, ProcessSupervisor,
    Progress, ServiceError, TransferEvent, remove_private_directory, safe_snapshot_path,
    set_private_directory_mode, validate_artifact, validate_snapshot_layout,
};

const SEARCH_ANSWER_BYTES: usize = 4 * 1024 * 1024;
/// The most a model description may be; large repositories list thousands
/// of files.
const INFO_ANSWER_BYTES: usize = 32 * 1024 * 1024;
const COPY_CHUNK: usize = 1024 * 1024;

/// The Hub's JSON API, so tests answer without a network.
pub trait HubApi {
    /// GET `url`, which is always under [`HUB_ORIGIN`], and return its body
    /// of at most `limit` bytes.
    fn get(&mut self, url: &str, limit: usize) -> Result<Vec<u8>, ServiceError>;
}

/// Search the Hub for models with `format`'s files, most downloaded first.
///
/// # Errors
/// An empty or oversized query, or the Hub's refusal or malformed answer.
pub fn search(
    api: &mut dyn HubApi,
    query: &str,
    format: HubFormat,
) -> Result<Vec<HubSearchResult>, ServiceError> {
    let query = query.trim();
    if query.is_empty() || query.len() > 200 || query.chars().any(char::is_control) {
        return Err(ServiceError::Backend(
            "search for a model by a name of at most 200 characters".into(),
        ));
    }
    let body = api.get(&picks::search_url(query, format), SEARCH_ANSWER_BYTES)?;
    let hits: Vec<ApiSearchHit> = serde_json::from_slice(&body)?;
    Ok(hits.into_iter().filter_map(HubSearchResult::of).collect())
}

/// The Hub's description of the model `reference` names.
///
/// # Errors
/// The Hub's refusal or malformed answer, or an answer about another model.
pub fn model_info(api: &mut dyn HubApi, reference: &HubRef) -> Result<ApiModelInfo, ServiceError> {
    let url = picks::model_info_url(&reference.repo, reference.revision.as_deref());
    let info: ApiModelInfo = serde_json::from_slice(&api.get(&url, INFO_ANSWER_BYTES)?)?;
    // The Hub answers with the repository's canonical spelling.
    if !info.id.eq_ignore_ascii_case(&reference.repo) {
        return Err(ServiceError::Backend(
            "Hugging Face answered about a different model".into(),
        ));
    }
    Ok(info)
}

/// The name of a pick's directory in the store: the digest of its record
/// as it was added, so another revision or variant never shares it.
///
/// # Errors
/// Only if the record cannot be encoded.
pub fn identity(model: &UserModel) -> Result<String, ServiceError> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(model)?)))
}

impl<T: HttpsTransport, P: ProcessSupervisor> LocalModelService<T, P> {
    /// Record a person's pick, and their acceptance of its licence, for a
    /// background worker to install. Adding the same pick again (a retry)
    /// keeps its place; another pick under an added model's id must wait
    /// until that model is removed.
    ///
    /// # Errors
    /// An invalid record, a model that is running or another pick with the
    /// same id, or a persistence error.
    pub fn queue_user_model(
        &mut self,
        model: UserModel,
    ) -> Result<LocalModelRelease, ServiceError> {
        model
            .validate()
            .map_err(|error| ServiceError::Backend(error.to_string()))?;
        self.require_process_absent(&model.id)?;
        let identity = match (
            self.state.user_models.get(&model.id),
            self.state.models.get(&model.id),
        ) {
            (Some(added), Some(managed)) if *added == model => managed.model_sha256.clone(),
            (None, None) => identity(&model)?,
            _ => {
                return Err(ServiceError::Backend(format!(
                    "{} is already added: remove it before adding another revision",
                    model.id
                )));
            }
        };
        let release = model.release(&identity);
        if release.requires_acceptance {
            self.state.accepted_licenses.insert(
                model.id.clone(),
                LicenseAcceptance {
                    model_version: release.version.clone(),
                    model_sha256: identity.clone(),
                    license_id: release.license_id.clone(),
                },
            );
        }
        self.state.models.insert(
            model.id.clone(),
            ManagedModel {
                model_sha256: identity,
                status: LocalModelStatus::Downloading {
                    received_bytes: 0,
                    total_bytes: model.download_bytes(),
                },
                process: None,
                in_process: None,
                objects: Vec::new(),
            },
        );
        self.state.user_models.insert(model.id.clone(), model);
        self.persist()?;
        Ok(release)
    }

    /// The release the runtime path launches for the person's model `id`.
    #[must_use]
    pub fn user_release(&self, id: &str) -> Option<LocalModelRelease> {
        let model = self.state.user_models.get(id)?;
        let managed = self.state.models.get(id)?;
        Some(model.release(&managed.model_sha256))
    }

    /// Background install of the person's model `id`: every file is fetched
    /// (or, for a file the person chose, copied from `chosen`, the handle
    /// the file chooser gave) and verified, progress is published as for a
    /// catalog release, then the directory the runtime mounts is assembled.
    /// It ends `Installed`.
    ///
    /// # Errors
    /// An unknown model, a missing licence acceptance, a chosen file without
    /// its handle, or any transfer, verification, filesystem or persistence
    /// error.
    pub fn install_user_model(
        &mut self,
        id: &str,
        chosen: Option<fs::File>,
    ) -> Result<LocalModelStatus, ServiceError> {
        self.require_process_absent(id)?;
        let release = self
            .user_release(id)
            .ok_or_else(|| ServiceError::ModelNotInstalled(id.into()))?;
        self.require_license_acceptance(&release)?;
        let mut model = self
            .state
            .user_models
            .get(id)
            .cloned()
            .ok_or_else(|| ServiceError::ModelNotInstalled(id.into()))?;
        let identity = release.artifact.sha256;
        let mut progress = Progress::new(self, id, model.download_bytes(), |_| {});
        let fetched = self.fetch_user_files(&mut model, (&identity, chosen), &mut |event| {
            progress.event(event);
        });
        let (objects, outcome) = match fetched {
            Ok(objects) => (objects, Ok(())),
            Err(error) => (Vec::new(), Err(error)),
        };
        progress.settle(self, outcome)?;
        self.assemble(&identity, &model, &objects)?;
        self.state.models.insert(
            id.to_owned(),
            ManagedModel {
                model_sha256: identity,
                status: LocalModelStatus::Installed,
                process: None,
                in_process: None,
                objects,
            },
        );
        self.state.user_models.insert(id.to_owned(), model);
        self.persist()?;
        Ok(LocalModelStatus::Installed)
    }

    /// Fetch every file of `model`, reporting bytes across all of them, and
    /// return the SHA-256 objects they became, in file order. A copied local
    /// file's record learns its SHA-256 here.
    fn fetch_user_files(
        &mut self,
        model: &mut UserModel,
        (identity, mut chosen): (&str, Option<fs::File>),
        progress: &mut dyn FnMut(TransferEvent),
    ) -> Result<Vec<String>, ServiceError> {
        let mut done = 0_u64;
        let mut objects = Vec::with_capacity(model.files.len());
        let source = model.source.clone();
        let urls: Vec<Option<String>> = model
            .files
            .iter()
            .map(|file| model.file_url(file))
            .collect();
        for (file, url) in model.files.iter_mut().zip(urls) {
            let size = file.size_bytes;
            let mut relay = |event| {
                progress(match event {
                    TransferEvent::Received(bytes) => {
                        TransferEvent::Received(done.saturating_add(bytes))
                    }
                    TransferEvent::Verifying => TransferEvent::Verifying,
                });
            };
            let sha256 = match &source {
                UserModelSource::Hub { .. } => {
                    let url = url.ok_or(ServiceError::InvalidArtifact)?;
                    self.fetch_hub_file(&url, file, &mut relay)?
                }
                UserModelSource::File { .. } => {
                    let input = chosen.take().ok_or_else(|| {
                        ServiceError::Backend("choose the model file again to add it".into())
                    })?;
                    let sha256 = self.import_file(input, file, identity, &mut relay)?;
                    file.digest = HubDigest::Sha256 {
                        sha256: sha256.clone(),
                    };
                    sha256
                }
            };
            done = done.saturating_add(size);
            objects.push(sha256);
        }
        Ok(objects)
    }

    /// Fetch one Hub file into the object store, verified against the hash
    /// the Hub published, and return its SHA-256.
    fn fetch_hub_file(
        &mut self,
        url: &str,
        file: &HubFile,
        progress: &mut dyn FnMut(TransferEvent),
    ) -> Result<String, ServiceError> {
        if !url.starts_with(&format!("{HUB_ORIGIN}/")) {
            return Err(ServiceError::NonHttpsArtifact);
        }
        match &file.digest {
            HubDigest::Sha256 { sha256 } => {
                let artifact = PinnedArtifact {
                    url: url.to_owned(),
                    sha256: sha256.clone(),
                    size_bytes: file.size_bytes,
                };
                validate_artifact(&artifact)?;
                self.fetch_verified_with_progress(&artifact, progress)?;
                Ok(sha256.clone())
            }
            HubDigest::GitBlob { oid } => {
                if oid.len() != 40 || !oid.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    return Err(ServiceError::InvalidArtifact);
                }
                let partial = self.partial_path(&format!("git-{oid}"));
                self.resume_into(url, &partial, file.size_bytes, &mut |bytes| {
                    progress(TransferEvent::Received(bytes));
                })?;
                progress(TransferEvent::Verifying);
                let sha256 = match verify_git_blob(&partial, oid, file.size_bytes) {
                    Ok(sha256) => sha256,
                    Err(error) => {
                        // Bad bytes can never complete: the next try starts over.
                        let _ = fs::remove_file(&partial);
                        return Err(error);
                    }
                };
                self.promote(&partial, &sha256)?;
                Ok(sha256)
            }
        }
    }

    /// Copy the GGUF file the person chose, read from its open handle
    /// `input`, into the object store, hashing it on the way, and return its
    /// SHA-256. The copy is the model from then on: changing or deleting the
    /// original does not change what runs.
    fn import_file(
        &self,
        mut input: fs::File,
        file: &HubFile,
        identity: &str,
        progress: &mut dyn FnMut(TransferEvent),
    ) -> Result<String, ServiceError> {
        let metadata = input.metadata()?;
        if !metadata.is_file() || metadata.len() != file.size_bytes {
            return Err(ServiceError::Backend(
                "the model file changed after it was added; add it again".into(),
            ));
        }
        let mut magic = [0_u8; 4];
        input.read_exact(&mut magic)?;
        if &magic != b"GGUF" {
            return Err(ServiceError::Backend("the file is not a GGUF model".into()));
        }
        let partial = self.partial_path(&format!("import-{identity}"));
        // A local copy is fast and cannot be resumed against a published
        // hash: start it over.
        let _ = fs::remove_file(&partial);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut output = options.open(&partial)?;
        output.write_all(&magic)?;
        let mut hasher = Sha256::new();
        hasher.update(magic);
        let mut copied = 4_u64;
        let mut buffer = vec![0_u8; COPY_CHUNK];
        let mut reader = input.take(file.size_bytes.saturating_sub(3));
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            let chunk = buffer.get(..read).ok_or(ServiceError::PartialTooLarge)?;
            hasher.update(chunk);
            output.write_all(chunk)?;
            copied = copied
                .saturating_add(u64::try_from(read).map_err(|_| ServiceError::PartialTooLarge)?);
            progress(TransferEvent::Received(copied));
        }
        output.sync_all()?;
        if copied != file.size_bytes {
            let _ = fs::remove_file(&partial);
            return Err(ServiceError::Backend(
                "the model file changed while it was copied; add it again".into(),
            ));
        }
        progress(TransferEvent::Verifying);
        let sha256 = hex::encode(hasher.finalize());
        self.promote(&partial, &sha256)?;
        Ok(sha256)
    }

    /// Move verified `partial` to the object `sha256`, or drop it when that
    /// object is already there.
    fn promote(&self, partial: &Path, sha256: &str) -> Result<(), ServiceError> {
        let object = self.object_path(sha256)?;
        if object.exists() {
            fs::remove_file(partial)?;
        } else {
            fs::rename(partial, object)?;
        }
        Ok(())
    }

    /// Assemble `model`'s directory from hard links to its `objects`, check
    /// the layout its runtime needs, and put it in place atomically.
    fn assemble(
        &self,
        identity: &str,
        model: &UserModel,
        objects: &[String],
    ) -> Result<PathBuf, ServiceError> {
        if objects.len() != model.files.len() {
            return Err(ServiceError::InvalidArtifact);
        }
        let final_path = self.snapshot_path(identity)?;
        let staging = self
            .root
            .join("snapshot-staging")
            .join(format!("{identity}.assemble"));
        remove_private_directory(&staging)?;
        fs::create_dir(&staging)?;
        set_private_directory_mode(&staging)?;
        let built = (|| -> Result<(), ServiceError> {
            for (file, object) in model.files.iter().zip(objects) {
                let relative = Path::new(&file.path);
                safe_snapshot_path(relative)?;
                let target = staging.join(relative);
                let parent = target.parent().ok_or(ServiceError::UnsafeSnapshot)?;
                fs::create_dir_all(parent)?;
                set_private_directory_mode(parent)?;
                fs::hard_link(self.object_path(object)?, &target)?;
            }
            if model.format == ModelArtifactFormat::HfSnapshotFiles {
                validate_snapshot_layout(&staging)?;
            }
            Ok(())
        })();
        if let Err(error) = built {
            let _ = remove_private_directory(&staging);
            return Err(error);
        }
        // An earlier assembly of the same pick holds the same links.
        remove_private_directory(&final_path)?;
        fs::rename(&staging, &final_path)?;
        Ok(final_path)
    }

    /// Delete what an unfinished install of `model` left in the partial store.
    pub(crate) fn remove_partials(&self, model: &UserModel, identity: &str) {
        let mut names: Vec<String> = model
            .files
            .iter()
            .map(|file| match &file.digest {
                HubDigest::Sha256 { sha256 } => sha256.clone(),
                HubDigest::GitBlob { oid } => format!("git-{oid}"),
            })
            .collect();
        names.push(format!("import-{identity}"));
        for name in names {
            let path = self.partial_path(&name);
            if path.is_file() {
                let _ = fs::remove_file(path);
            }
        }
    }
}

/// Check that `path` holds the git blob `oid` of `size` bytes, and return
/// the SHA-256 of its bytes.
fn verify_git_blob(path: &Path, oid: &str, size: u64) -> Result<String, ServiceError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() != size {
        return Err(ServiceError::DigestMismatch);
    }
    let mut blob = Sha1::new();
    blob.update(format!("blob {size}\0").as_bytes());
    let mut content = Sha256::new();
    let mut reader = file.take(size.saturating_add(1));
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut total = 0_u64;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let chunk = buffer.get(..read).ok_or(ServiceError::DigestMismatch)?;
        blob.update(chunk);
        content.update(chunk);
        total =
            total.saturating_add(u64::try_from(read).map_err(|_| ServiceError::DigestMismatch)?);
    }
    if total == size && hex::encode(blob.finalize()) == oid.to_ascii_lowercase() {
        Ok(hex::encode(content.finalize()))
    } else {
        Err(ServiceError::DigestMismatch)
    }
}

#[cfg(test)]
mod tests;
