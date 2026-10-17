//! Root-attested firstboot seed authority. Importers cannot supply a verifier.
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io,
    path::{Component, Path},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stamp {
    pub inode: u64,
    pub uid: u32,
    pub mode: u32,
    pub bytes: u64,
    pub mtime_ns: i64,
    pub ctime_ns: i64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Generated {
    pub path: String,
    pub stamp: Stamp,
    pub sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Proof {
    schema: u32,
    home_inode: u64,
    filesystem: u64,
    uid: u32,
    files: Vec<Generated>,
}

/// An authenticated root-controlled proof for one exact Home directory.
/// This type cannot be constructed from deserialized importer records.
pub struct Authority {
    proof: Proof,
    digest: String,
}
impl Authority {
    /// Read the installed firstboot proof, bound to the supplied Home handle.
    /// A missing proof or a proof for a different Home is not seed authority.
    /// # Errors
    /// Refuses unsafe namespaces, changed files and invalid proof records.
    pub fn read(home: &File) -> io::Result<Option<Self>> {
        #[cfg(target_os = "linux")]
        {
            native::read(home)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = home;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Firstboot seed authority requires Linux",
            ))
        }
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn generated(&self, path: &Path) -> Option<&Generated> {
        self.proof
            .files
            .iter()
            .find(|item| Path::new(&item.path) == path)
    }
    /// Rebind a retained witness to the freshly read installed authority.
    /// # Errors
    /// Refuses a digest or record not present in the authenticated proof.
    pub fn verify(&self, digest: &str, generated: &Generated) -> io::Result<()> {
        if self.digest != digest || !self.proof.files.contains(generated) {
            return Err(changed(
                "Firstboot seed authority differs from reviewed operation",
            ));
        }
        Ok(())
    }
}
fn changed(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
/// Validate a generated default's relative path without granting target access.
/// # Errors
/// Refuses traversal, oversized paths and active Tilo custody namespaces.
pub fn relative(value: &str) -> io::Result<&Path> {
    let path = Path::new(value);
    if value.is_empty()
        || value.len() > 4096
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        || path.starts_with(".config/tilo")
        || path.starts_with(".local/state/tilo")
    {
        return Err(changed("Invalid generated home seed path"));
    }
    Ok(path)
}

/// Durable recovery binding for one displaced firstboot default.
/// The serializable witness must be reauthenticated before any target write.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Witness {
    pub proof_sha256: String,
    pub generated: Generated,
    pub target: super::protocol::Fact,
}

impl Witness {
    /// # Errors
    /// Refuses records not bound to the requested relative adoption path.
    pub fn validate(&self, path: &Path) -> io::Result<()> {
        if relative(&self.generated.path)? != path
            || self.target.directory
            || self.target.mode > 0o777
        {
            return Err(changed(
                "Firstboot seed witness does not bind the adoption path",
            ));
        }
        Ok(())
    }
    /// Recheck the real installed authority on every recovery/preservation.
    /// # Errors
    /// Refuses missing authority or a changed proof digest or generated record.
    pub fn verify_authority(&self, home: &File) -> io::Result<()> {
        let proof = Authority::read(home)?
            .ok_or_else(|| changed("Firstboot seed authority disappeared"))?;
        proof.verify(&self.proof_sha256, &self.generated)
    }
    pub fn backup_name(&self) -> std::ffi::OsString {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(self.generated.path.as_bytes())).into()
    }
}

#[cfg(target_os = "linux")]
mod native;
#[cfg(test)]
mod tests;
#[cfg(target_os = "linux")]
pub use native::stamp;
