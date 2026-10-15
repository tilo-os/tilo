//! Builds whose images another process signs: a relay member's tools.
//!
//! On a relay, a member's `ush` supervisor runs as the member's uid and must
//! never hold the relay's device key. Its [`ToolBuilder`] is a
//! [`RemoteBuilder`]: it asks the relay's worker over a socket the worker
//! made for that member. The worker runs the build itself — resolve,
//! install, seal — as the member's uid within the member's quota
//! (`tilo-env-tools build-for`, with an [`UnsignedStore`]), so a package
//! scriptlet runs with the member's authority and nothing more. The child
//! leaves a tool image for the relay's public key with a placeholder
//! signature; the worker checks it is a tool for the runtime asked for and
//! signs it ([`sign_delegated`]) in its own process, and only the signed
//! image is handed back where the member's supervisor reads images.
//!
//! The worker never follows a path the member controls beyond opening one
//! regular file without following links, and signs nothing but a `tool`
//! image for the requested runtime and its own architecture.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use package_store::Store;
use serde::{Deserialize, Serialize};
use tilo_app_package::manifest::Arch;
use tilo_app_package::{BlobHash, Kind, PackageId, SigningIdentity};
use tilo_environment::{ToolBuildError, ToolBuilder};

use crate::builder::ToolStore;
use crate::image;
use crate::key::CacheKey;
use tilo_lock::MutexRecover as _;

/// The socket a member's supervisor asks for builds on.
pub const BUILDS_SOCKET_ENV: &str = "TILO_USH_TOOL_BUILDS";

/// Largest delegated tool image the worker reads.
pub const MAX_DELEGATED_TAPP_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// Longest request or reply line.
pub const MAX_LINE_BYTES: usize = 64 * 1024;

/// What a supervisor asks the worker for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildRequest {
    /// The env-runtime image (`tapp:<hex>`) to build on.
    pub runtime: String,
    /// The packages the member asked for.
    pub packages: Vec<String>,
}

/// One line of the worker's answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BuildReply {
    /// The build's output, for the shell that asked.
    Progress(String),
    /// The signed tool image.
    Built(String),
    /// Why there is none.
    Failed(String),
}

/// A [`ToolBuilder`] that asks the worker over `socket`.
#[derive(Clone, Debug)]
pub struct RemoteBuilder {
    socket: PathBuf,
}

impl RemoteBuilder {
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
        }
    }

    /// The builder [`BUILDS_SOCKET_ENV`] names, when it is set.
    pub fn from_env() -> Option<Self> {
        std::env::var_os(BUILDS_SOCKET_ENV)
            .filter(|socket| !socket.is_empty())
            .map(Self::new)
    }
}

impl ToolBuilder for RemoteBuilder {
    fn build(
        &self,
        runtime: &PackageId,
        packages: &[String],
        progress: &mut dyn Write,
    ) -> Result<PackageId, ToolBuildError> {
        let unavailable = |error: std::io::Error| {
            ToolBuildError::Unavailable(format!("the relay's tool builder: {error}"))
        };
        let mut stream =
            std::os::unix::net::UnixStream::connect(&self.socket).map_err(unavailable)?;
        let mut line = serde_json::to_vec(&BuildRequest {
            runtime: runtime.to_string(),
            packages: packages.to_vec(),
        })
        .map_err(|error| ToolBuildError::Failed(error.to_string()))?;
        line.push(b'\n');
        stream.write_all(&line).map_err(unavailable)?;
        let mut replies = BufReader::new(stream);
        loop {
            let mut text = String::new();
            let read = (&mut replies)
                .take(u64::try_from(MAX_LINE_BYTES).unwrap_or(u64::MAX))
                .read_line(&mut text)
                .map_err(unavailable)?;
            if read == 0 {
                return Err(ToolBuildError::Failed(
                    "the relay's tool builder hung up".into(),
                ));
            }
            match serde_json::from_str::<BuildReply>(&text)
                .map_err(|error| ToolBuildError::Failed(format!("tool builder reply: {error}")))?
            {
                BuildReply::Progress(text) => {
                    let _ = progress.write_all(text.as_bytes());
                }
                BuildReply::Built(id) => {
                    return id.parse().map_err(|error| {
                        ToolBuildError::Failed(format!("tool id {id:?}: {error}"))
                    });
                }
                BuildReply::Failed(reason) => return Err(ToolBuildError::Failed(reason)),
            }
        }
    }
}

/// A [`ToolStore`] that seals tools for `publisher` without its key,
/// writing the one image it seals to `out`. Runtimes come from `store`.
pub struct UnsignedStore {
    store: Store,
    publisher: [u8; 32],
    arch: Arch,
    out: PathBuf,
    written: Mutex<Option<PackageId>>,
}

impl UnsignedStore {
    /// Seal for `publisher` into `out`, reading runtimes from `store`.
    ///
    /// # Errors
    ///
    /// On an architecture images aren't built for.
    pub fn new(store: Store, publisher: [u8; 32], out: PathBuf) -> anyhow::Result<Self> {
        Ok(Self {
            store,
            publisher,
            arch: image::host_arch()?,
            out,
            written: Mutex::new(None),
        })
    }

    /// The unsigned image written, if one was.
    pub fn written(&self) -> Option<PackageId> {
        *self.written.lock_recover()
    }
}

impl ToolStore for UnsignedStore {
    fn runtime_tree(&self, runtime: &PackageId) -> anyhow::Result<PathBuf> {
        Ok(package_store::materialized_tree(&self.store, runtime)?)
    }

    fn has(&self, _: &PackageId) -> bool {
        // The signed image is not this process's to find.
        false
    }

    fn store_tool(
        &self,
        tree: &Path,
        key: &CacheKey,
        requested: &[String],
    ) -> anyhow::Result<PackageId> {
        let packed = tilo_app_package::seal_image_for(
            tree,
            &image::tool_spec(key, requested, self.arch),
            self.publisher,
        )?;
        image::write_tapp(&self.out, &packed)?;
        *self.written.lock_recover() = Some(packed.id());
        Ok(packed.id())
    }
}

/// Why a delegated image was not signed.
#[derive(Debug, thiserror::Error)]
pub enum DelegatedError {
    #[error("the built image is unreadable: {0}")]
    Io(#[from] std::io::Error),
    #[error("the built image is not a tool image: {0}")]
    Malformed(String),
    #[error("the built image is not what was asked for: {0}")]
    Refused(String),
}

/// Check the image a member's build left at `tapp` and sign it with
/// `identity`: a complete `tool` image for `identity`, built for `runtime`
/// on `arch`. The signed image is written to `<dest>/<hex id>.tapp`;
/// returns its id.
///
/// # Errors
///
/// [`DelegatedError`] for a file that is not a regular file, too large,
/// malformed, incomplete, or anything but the tool asked for.
pub fn sign_delegated(
    tapp: &Path,
    runtime: &PackageId,
    arch: Arch,
    identity: &SigningIdentity,
    dest: &Path,
) -> Result<PackageId, DelegatedError> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(tapp)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(DelegatedError::Malformed("not a regular file".into()));
    }
    if metadata.len() > MAX_DELEGATED_TAPP_BYTES {
        return Err(DelegatedError::Malformed(
            "larger than an image may be".into(),
        ));
    }
    let mut blobs = std::collections::BTreeMap::<BlobHash, Vec<u8>>::new();
    let read = tilo_app_package::tapp::read(
        BufReader::new(file.take(MAX_DELEGATED_TAPP_BYTES)),
        &mut blobs,
    )
    .map_err(|error| DelegatedError::Malformed(error.to_string()))?;
    if !read.is_complete() {
        return Err(DelegatedError::Malformed(
            "payload blobs are missing".into(),
        ));
    }
    let meta = read.meta;
    let package = &meta.manifest.package;
    if package.kind != Kind::Tool {
        return Err(DelegatedError::Refused(format!(
            "a {:?} image, not a tool",
            package.kind
        )));
    }
    let built_for = meta.manifest.image.as_ref().and_then(|image| image.runtime);
    if built_for != Some(*runtime) {
        return Err(DelegatedError::Refused(format!(
            "built for {built_for:?}, not the runtime {runtime}"
        )));
    }
    let for_arch = match arch {
        Arch::X86_64 => package.isa.x86_64.is_some() && package.isa.aarch64.is_none(),
        Arch::AArch64 => package.isa.aarch64.is_some() && package.isa.x86_64.is_none(),
    };
    if !for_arch {
        return Err(DelegatedError::Refused(
            "not built for this architecture".into(),
        ));
    }
    let signed = tilo_app_package::sign_placeholder_root(&meta, identity)
        .map_err(|error| DelegatedError::Refused(error.to_string()))?;
    std::fs::create_dir_all(dest)?;
    let path = dest.join(format!("{}.tapp", signed.id.to_hex()));
    let staged = dest.join(format!(
        ".{}.{}.partial",
        signed.id.to_hex(),
        std::process::id()
    ));
    let result = (|| -> Result<(), DelegatedError> {
        let out = std::fs::File::create(&staged)?;
        let payload: &dyn tilo_app_package::BlobSource = &blobs;
        let mut out =
            tilo_app_package::tapp::write(std::io::BufWriter::new(out), &signed, Some(payload))
                .map_err(|error| DelegatedError::Malformed(error.to_string()))?;
        out.flush()?;
        out.into_inner()
            .map_err(std::io::IntoInnerError::into_error)?
            .sync_all()?;
        std::fs::rename(&staged, &path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    result.map(|()| signed.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("usr/bin")).unwrap();
        std::fs::write(dir.path().join("usr/bin/hello"), "hi").unwrap();
        dir
    }

    fn unsigned(tree: &Path, runtime: PackageId, publisher: [u8; 32], out: &Path) -> PackageId {
        let key = CacheKey::new(runtime, ["hello-1-1.x86_64"]);
        let packed = tilo_app_package::seal_image_for(
            tree,
            &image::tool_spec(&key, &["hello".to_owned()], image::host_arch().unwrap()),
            publisher,
        )
        .unwrap();
        image::write_tapp(out, &packed).unwrap();
        packed.id()
    }

    #[test]
    fn the_worker_signs_only_the_tool_it_was_asked_for() {
        let tree = tree();
        let work = tempfile::tempdir().unwrap();
        let relay = SigningIdentity::from_seed(&[7; 32]);
        let runtime = PackageId::from_bytes([1; 32]);
        let arch = image::host_arch().unwrap();
        let built = work.path().join("built.tapp");
        let unsigned_id = unsigned(tree.path(), runtime, relay.public_key_bytes(), &built);

        let shelf = work.path().join("shelf");
        let id = sign_delegated(&built, &runtime, arch, &relay, &shelf).unwrap();
        assert_ne!(id, unsigned_id, "signing makes a new package");
        let bytes = std::fs::read(shelf.join(format!("{}.tapp", id.to_hex()))).unwrap();
        let mut blobs = std::collections::BTreeMap::<BlobHash, Vec<u8>>::new();
        let read = tilo_app_package::tapp::read(bytes.as_slice(), &mut blobs).unwrap();
        assert_eq!(read.meta.id, id);
        assert!(
            tilo_app_package::verify(&read.meta, &[relay.public_key_bytes()])
                .manifest()
                .is_some(),
            "the image verifies against the relay's key"
        );

        // Another runtime, another publisher, a link: refused.
        assert!(matches!(
            sign_delegated(
                &built,
                &PackageId::from_bytes([2; 32]),
                arch,
                &relay,
                &shelf
            ),
            Err(DelegatedError::Refused(_))
        ));
        let other = SigningIdentity::from_seed(&[8; 32]);
        assert!(sign_delegated(&built, &runtime, arch, &other, &shelf).is_err());
        let link = work.path().join("link.tapp");
        std::os::unix::fs::symlink(&built, &link).unwrap();
        assert!(matches!(
            sign_delegated(&link, &runtime, arch, &relay, &shelf),
            Err(DelegatedError::Io(_))
        ));
    }
}
