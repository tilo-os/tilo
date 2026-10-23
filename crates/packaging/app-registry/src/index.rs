//! The signed registry index — the wire format a Tilo registry serves.
//!
//! Maps a reverse-DNS **app-id** to published versions, each naming its
//! tapp by content hash (blake3, same as iroh-blobs) plus the metadata
//! a client needs to pick a build before downloading: target arch, ISA
//! level, and runtime ABI.
//!
//! Two independent signatures protect the JSON document served over
//! HTTPS: the **index signature** (registry operator's Ed25519 key
//! over the whole document — pinned in `registries.toml`, so a MITM or
//! compromised host can't add/drop/rewrite entries) and the **per-entry
//! publisher signature** (binds `(app-id, version, tapp-hash,
//! package-id, arch, isa, runtime, size)` so the advertised metadata
//! can't be relabelled by the registry).

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use tilo_clock::unix_now;

use tilo_app_package::PackageId;
use tilo_app_package::manifest::{AArch64Level, Arch, IsaLevel, X86_64Level};
use tilo_app_package::pack::SigningIdentity;

/// Version of the index wire format; bumped when the canonical signing bytes change shape.
pub const INDEX_FORMAT: u32 = 1;

const INDEX_MAGIC: &[u8; 12] = b"TLOIDX-DOC\0\0";
const ENTRY_MAGIC: &[u8; 12] = b"TLOIDX-ENT\0\0";

/// Errors surfaced by index construction / verification.
#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("hex decode: {0}")]
    Hex(String),
    #[error("bad ed25519 identity {0:?}: expected `ed25519:<hex32>`")]
    BadIdentity(String),
    #[error("signature is {got} bytes, expected 64")]
    BadSignatureLen { got: usize },
    #[error("public key is not a valid ed25519 point")]
    InvalidKey,
    #[error("the index carries no signature — sign it before serving")]
    Unsigned,
    #[error("index signature does not verify against {0}")]
    IndexSignatureMismatch(String),
    #[error("entry {app_id}@{version}: publisher signature does not verify")]
    EntrySignatureMismatch { app_id: String, version: String },
    #[error("invalid semver {0:?}: {1}")]
    BadSemver(String, semver::Error),
    #[error("bad runtime ref {0:?}: expected `<name>/<semver>`")]
    BadRuntimeRef(String),
    #[error("unknown isa label {label:?} for arch {arch:?}")]
    UnknownIsa { arch: Arch, label: String },
    #[error("sign: {0}")]
    Sign(String),
    #[error("value of length {0} does not fit the wire format's u32 length field")]
    TooLong(usize),
}

fn parse_ed25519(s: &str) -> Result<[u8; 32], IndexError> {
    let rest = s
        .strip_prefix("ed25519:")
        .ok_or_else(|| IndexError::BadIdentity(s.to_string()))?;
    let bytes = hex::decode(rest).map_err(|e| IndexError::Hex(e.to_string()))?;
    let arr: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| IndexError::BadIdentity(s.to_string()))?;
    Ok(arr)
}

fn parse_sig(s: &str) -> Result<[u8; 64], IndexError> {
    let bytes = hex::decode(s).map_err(|e| IndexError::Hex(e.to_string()))?;
    let got = bytes.len();
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| IndexError::BadSignatureLen { got })
}

fn put_str(out: &mut Vec<u8>, s: &str) -> Result<(), IndexError> {
    let len = u32::try_from(s.len()).map_err(|_| IndexError::TooLong(s.len()))?;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(s.as_bytes());
    Ok(())
}

/// Machine string for an [`Arch`], matching its TOML/JSON serde form.
pub fn arch_wire(arch: Arch) -> &'static str {
    match arch {
        Arch::X86_64 => "x86_64",
        Arch::AArch64 => "aarch64",
    }
}

/// Reconstruct an [`IsaLevel`] from an arch + `wire_label` — the
/// inverse of [`IsaLevel::wire_label`], used reading an entry back from JSON.
pub fn parse_isa(arch: Arch, label: &str) -> Option<IsaLevel> {
    match arch {
        Arch::X86_64 => [
            X86_64Level::Baseline,
            X86_64Level::V2,
            X86_64Level::V3,
            X86_64Level::V4,
        ]
        .into_iter()
        .find(|l| l.wire_label() == label)
        .map(IsaLevel::X86_64),
        Arch::AArch64 => [
            AArch64Level::Armv8A,
            AArch64Level::Armv8_2A,
            AArch64Level::Armv8_4A,
            AArch64Level::Armv9A,
        ]
        .into_iter()
        .find(|l| l.wire_label() == label)
        .map(IsaLevel::AArch64),
    }
}

/// A `<name>/<semver>` reference to a runtime ABI bundle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeRef {
    pub name: String,
    pub version: semver::Version,
}

impl RuntimeRef {
    /// Parse `tilo-runtime/1.2.0`.
    pub fn parse(s: &str) -> Result<Self, IndexError> {
        let (name, ver) = s
            .rsplit_once('/')
            .ok_or_else(|| IndexError::BadRuntimeRef(s.to_string()))?;
        if name.is_empty() {
            return Err(IndexError::BadRuntimeRef(s.to_string()));
        }
        let version =
            semver::Version::parse(ver).map_err(|e| IndexError::BadSemver(s.to_string(), e))?;
        Ok(Self {
            name: name.to_string(),
            version,
        })
    }

    /// `tilo-runtime/1.2.0` wire form.
    pub fn to_wire(&self) -> String {
        format!("{}/{}", self.name, self.version)
    }

    /// Whether `available` can host a build requiring `self`: same
    /// runtime name, same major, and `available >= self`.
    pub fn satisfied_by(&self, available: &RuntimeRef) -> bool {
        self.name == available.name
            && available.version.major == self.version.major
            && available.version >= self.version
    }
}

/// What the local host can run: arch, actual CPU ISA level, and installed runtime ABIs.
#[derive(Clone, Debug)]
pub struct HostProfile {
    pub arch: Arch,
    pub isa: IsaLevel,
    pub runtimes: Vec<RuntimeRef>,
}

impl HostProfile {
    /// Detect the current host's arch + ISA level; `runtimes` starts
    /// empty (caller populates from installed bundles).
    pub fn detect() -> Self {
        #[cfg(target_arch = "x86_64")]
        {
            let level =
                if is_x86_feature_detected!("avx512f") && is_x86_feature_detected!("avx512bw") {
                    X86_64Level::V4
                } else if is_x86_feature_detected!("avx2")
                    && is_x86_feature_detected!("bmi2")
                    && is_x86_feature_detected!("fma")
                {
                    X86_64Level::V3
                } else if is_x86_feature_detected!("sse4.2") && is_x86_feature_detected!("popcnt") {
                    X86_64Level::V2
                } else {
                    X86_64Level::Baseline
                };
            return Self {
                arch: Arch::X86_64,
                isa: IsaLevel::X86_64(level),
                runtimes: Vec::new(),
            };
        }
        #[cfg(target_arch = "aarch64")]
        {
            let level = if std::arch::is_aarch64_feature_detected!("sve2") {
                AArch64Level::Armv9A
            } else if std::arch::is_aarch64_feature_detected!("i8mm") {
                AArch64Level::Armv8_4A
            } else if std::arch::is_aarch64_feature_detected!("dotprod") {
                AArch64Level::Armv8_2A
            } else {
                AArch64Level::Armv8A
            };
            return Self {
                arch: Arch::AArch64,
                isa: IsaLevel::AArch64(level),
                runtimes: Vec::new(),
            };
        }
        #[allow(
            unreachable_code,
            reason = "the fallback remains type-checked on architectures whose cfg branch returns above"
        )]
        Self {
            arch: Arch::X86_64,
            isa: IsaLevel::X86_64(X86_64Level::Baseline),
            runtimes: Vec::new(),
        }
    }

    /// Register an installed runtime ABI so runtime-gated entries become selectable.
    #[must_use]
    pub fn with_runtime(mut self, r: RuntimeRef) -> Self {
        self.runtimes.push(r);
        self
    }
}

/// One published version of one app.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexEntry {
    /// Reverse-DNS app id, e.g. `so.tilo.editor`.
    pub app_id: String,
    /// Semver of this build.
    pub version: String,
    /// `tapp:<hex>` — blake3 of the signed-root (package identity).
    pub package_id: String,
    /// blake3 hex of the whole .tapp tar — the iroh-blobs content hash used to fetch and verify-on-arrival.
    pub tapp_hash: String,
    /// Size of the `.tapp` in bytes (advisory; UI + quota).
    pub size_bytes: u64,
    /// Target architecture.
    pub arch: Arch,
    /// Minimum ISA feature level (`IsaLevel::wire_label`), e.g. `x86_64-v3`.
    pub isa: String,
    /// `<runtime>/<semver>` this build binds to; `None` for a `kind = "runtime"` package or fully-static app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    /// Publisher's Ed25519 identity, `ed25519:<hex>`.
    pub publisher_identity: String,
    /// Publisher's Ed25519 signature over [`Self::signing_bytes`].
    pub publisher_signature: String,
    /// iroh `BlobTicket` (base32) naming the node + hash to fetch from; the hash inside MUST equal `tapp_hash`.
    pub ticket: String,
}

impl IndexEntry {
    /// The 32-byte tapp content hash.
    ///
    /// # Errors
    ///
    /// Returns an error if `tapp_hash` isn't valid hex or isn't 32 bytes long.
    pub fn tapp_hash_bytes(&self) -> Result<[u8; 32], IndexError> {
        let b = hex::decode(&self.tapp_hash).map_err(|e| IndexError::Hex(e.to_string()))?;
        b.as_slice()
            .try_into()
            .map_err(|_| IndexError::Hex("tapp_hash is not 32 bytes".into()))
    }

    pub fn parse_package_id(&self) -> Result<PackageId, IndexError> {
        self.package_id.parse().map_err(|e| {
            IndexError::BadIdentity(format!("package_id {:?}: {e:?}", self.package_id))
        })
    }

    /// The parsed publisher key bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if `publisher_identity` isn't `ed25519:<hex32>`.
    pub fn publisher_key(&self) -> Result<[u8; 32], IndexError> {
        parse_ed25519(&self.publisher_identity)
    }

    /// Parsed ISA level, or `None` if arch/label don't match a known level.
    pub fn isa_level(&self) -> Option<IsaLevel> {
        parse_isa(self.arch, &self.isa)
    }

    /// Canonical bytes the publisher signs. Binds every advertised
    /// field so a registry cannot relabel the bytes.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, IndexError> {
        let mut out = Vec::new();
        out.extend_from_slice(ENTRY_MAGIC);
        out.extend_from_slice(&INDEX_FORMAT.to_le_bytes());
        put_str(&mut out, &self.app_id)?;
        put_str(&mut out, &self.version)?;
        put_str(&mut out, &self.package_id)?;
        out.extend_from_slice(&self.tapp_hash_bytes()?);
        out.extend_from_slice(&self.size_bytes.to_le_bytes());
        put_str(&mut out, arch_wire(self.arch))?;
        put_str(&mut out, &self.isa)?;
        put_str(&mut out, self.runtime.as_deref().unwrap_or(""))?;
        Ok(out)
    }

    /// Fill `publisher_identity`+`publisher_signature` by signing [`Self::signing_bytes`] with `identity`.
    ///
    /// # Errors
    ///
    /// Returns an error if [`Self::signing_bytes`] fails, or `identity` fails to sign.
    pub fn sign(&mut self, identity: &SigningIdentity) -> Result<(), IndexError> {
        self.publisher_identity = format!("ed25519:{}", hex::encode(identity.public_key_bytes()));
        let preimage = self.signing_bytes()?;
        let sig = identity
            .sign_message(&preimage)
            .map_err(|e| IndexError::Sign(e.to_string()))?;
        self.publisher_signature = hex::encode(sig);
        Ok(())
    }

    /// Verify the publisher signature over this entry.
    pub fn verify_publisher(&self) -> Result<(), IndexError> {
        let key = self.publisher_key()?;
        let vk = VerifyingKey::from_bytes(&key).map_err(|_| IndexError::InvalidKey)?;
        let sig = Signature::from_bytes(&parse_sig(&self.publisher_signature)?);
        vk.verify(&self.signing_bytes()?, &sig)
            .map_err(|_| IndexError::EntrySignatureMismatch {
                app_id: self.app_id.clone(),
                version: self.version.clone(),
            })
    }

    fn semver(&self) -> Result<semver::Version, IndexError> {
        semver::Version::parse(&self.version)
            .map_err(|e| IndexError::BadSemver(self.version.clone(), e))
    }

    /// Whether this build can run on `host`: arch matches, host ISA rank
    /// meets the floor, and the runtime (if any) is available.
    pub fn runs_on(&self, host: &HostProfile) -> bool {
        if self.arch != host.arch {
            return false;
        }
        let Some(required) = self.isa_level() else {
            return false;
        };
        // Cross-arch rank compares panic in pkg; we already gated arch.
        if required.arch() != host.isa.arch() || required.rank() > host.isa.rank() {
            return false;
        }
        if let Some(rt) = &self.runtime {
            let Ok(req) = RuntimeRef::parse(rt) else {
                return false;
            };
            if !host.runtimes.iter().any(|a| req.satisfied_by(a)) {
                return false;
            }
        }
        true
    }
}

/// The full served document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryIndex {
    pub format: u32,
    pub registry_id: String,
    pub generated_unix: u64,
    /// Registry operator's Ed25519 identity `ed25519:<hex>`; empty until [`Self::sign`] runs.
    #[serde(default)]
    pub signer_identity: String,
    /// Signature over [`Self::signing_bytes`]. Empty until signed.
    #[serde(default)]
    pub signature: String,
    #[serde(default)]
    pub packages: Vec<IndexEntry>,
}

impl RegistryIndex {
    /// Empty, unsigned index for `registry_id`.
    pub fn new(registry_id: impl Into<String>) -> Self {
        Self {
            format: INDEX_FORMAT,
            registry_id: registry_id.into(),
            generated_unix: unix_now(),
            signer_identity: String::new(),
            signature: String::new(),
            packages: Vec::new(),
        }
    }

    /// Decode from the served JSON bytes.
    pub fn from_json(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }

    /// Encode to pretty JSON for serving / on-disk storage.
    pub fn to_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec_pretty(self)
    }

    /// Insert-or-replace an entry keyed by (`app_id`, version, arch, isa)
    /// and refresh `generated_unix`; re-sign afterwards.
    pub fn upsert(&mut self, entry: IndexEntry) {
        self.packages.retain(|e| {
            !(e.app_id == entry.app_id
                && e.version == entry.version
                && e.arch == entry.arch
                && e.isa == entry.isa)
        });
        self.packages.push(entry);
        self.generated_unix = unix_now();
    }

    /// Canonical bytes the registry operator signs — the whole document minus the signature itself.
    ///
    /// # Errors
    ///
    /// Returns an error if an entry's [`IndexEntry::signing_bytes`] fails,
    /// or the entry count / a field doesn't fit the wire format's u32 length field.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, IndexError> {
        let mut out = Vec::new();
        out.extend_from_slice(INDEX_MAGIC);
        out.extend_from_slice(&self.format.to_le_bytes());
        put_str(&mut out, &self.registry_id)?;
        out.extend_from_slice(&self.generated_unix.to_le_bytes());
        let packages_len = u32::try_from(self.packages.len())
            .map_err(|_| IndexError::TooLong(self.packages.len()))?;
        out.extend_from_slice(&packages_len.to_le_bytes());
        // Entries in a deterministic order (by the fields that make one
        // unique) so the signature is stable regardless of push order.
        let mut idx: Vec<&IndexEntry> = self.packages.iter().collect();
        idx.sort_by(|a, b| {
            (&a.app_id, &a.version, arch_wire(a.arch), &a.isa).cmp(&(
                &b.app_id,
                &b.version,
                arch_wire(b.arch),
                &b.isa,
            ))
        });
        for e in idx {
            out.extend_from_slice(&e.signing_bytes()?);
            put_str(&mut out, &e.publisher_identity)?;
            put_str(&mut out, &e.publisher_signature)?;
            put_str(&mut out, &e.ticket)?;
        }
        Ok(out)
    }

    /// Sign the index with the registry operator identity.
    ///
    /// # Errors
    ///
    /// Returns an error if [`Self::signing_bytes`] fails, or `identity` fails to sign.
    pub fn sign(&mut self, identity: &SigningIdentity) -> Result<(), IndexError> {
        self.signer_identity = format!("ed25519:{}", hex::encode(identity.public_key_bytes()));
        let preimage = self.signing_bytes()?;
        let sig = identity
            .sign_message(&preimage)
            .map_err(|e| IndexError::Sign(e.to_string()))?;
        self.signature = hex::encode(sig);
        Ok(())
    }

    /// Verify the index signature against `expected_key` (the pinned
    /// registry operator key). Fail-closed: unsigned or mismatched → `Err`.
    pub fn verify_signature(&self, expected_key: &[u8; 32]) -> Result<(), IndexError> {
        if self.signature.is_empty() || self.signer_identity.is_empty() {
            return Err(IndexError::Unsigned);
        }
        let embedded = parse_ed25519(&self.signer_identity)?;
        if &embedded != expected_key {
            return Err(IndexError::IndexSignatureMismatch(
                self.signer_identity.clone(),
            ));
        }
        let vk = VerifyingKey::from_bytes(expected_key).map_err(|_| IndexError::InvalidKey)?;
        let sig = Signature::from_bytes(&parse_sig(&self.signature)?);
        vk.verify(&self.signing_bytes()?, &sig)
            .map_err(|_| IndexError::IndexSignatureMismatch(self.signer_identity.clone()))
    }

    /// Pick the best entry for `app_id` on `host`: highest semver matching
    /// `req` (or all versions), tie-broken by the most-optimised ISA the
    /// host can run. Each candidate's publisher signature is verified;
    /// entries that fail are skipped (fail-closed).
    pub fn resolve_best(
        &self,
        app_id: &str,
        req: Option<&semver::VersionReq>,
        host: &HostProfile,
    ) -> Option<&IndexEntry> {
        let mut best: Option<(&IndexEntry, semver::Version)> = None;
        for e in &self.packages {
            if e.app_id != app_id {
                continue;
            }
            if !e.runs_on(host) {
                continue;
            }
            let Ok(ver) = e.semver() else { continue };
            if let Some(req) = req
                && !req.matches(&ver)
            {
                continue;
            }
            if e.verify_publisher().is_err() {
                continue;
            }
            let better = match &best {
                None => true,
                Some((cur, cur_ver)) => {
                    ver > *cur_ver
                        || (ver == *cur_ver
                            && e.isa_level()
                                .map_or(0, tilo_app_package::manifest::IsaLevel::rank)
                                > cur
                                    .isa_level()
                                    .map_or(0, tilo_app_package::manifest::IsaLevel::rank))
                }
            };
            if better {
                best = Some((e, ver));
            }
        }
        best.map(|(e, _)| e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_app_package::pack::SigningIdentity;

    fn sample_entry(app: &str, ver: &str, isa: &str) -> IndexEntry {
        IndexEntry {
            app_id: app.into(),
            version: ver.into(),
            package_id: format!("tapp:{}", hex::encode([7u8; 32])),
            tapp_hash: hex::encode([9u8; 32]),
            size_bytes: 1234,
            arch: Arch::X86_64,
            isa: isa.into(),
            runtime: None,
            publisher_identity: String::new(),
            publisher_signature: String::new(),
            ticket: "blobplaceholder".into(),
        }
    }

    fn x86_host(level: X86_64Level) -> HostProfile {
        HostProfile {
            arch: Arch::X86_64,
            isa: IsaLevel::X86_64(level),
            runtimes: Vec::new(),
        }
    }

    /// An index of `so.tilo.editor` entries, each signed by one publisher.
    fn index_of(entries: impl IntoIterator<Item = IndexEntry>) -> RegistryIndex {
        let publisher = SigningIdentity::generate();
        let mut idx = RegistryIndex::new("so.tilo.central");
        for mut e in entries {
            e.sign(&publisher).unwrap();
            idx.upsert(e);
        }
        idx
    }

    #[test]
    fn entry_and_index_signatures_verify_survive_json_and_catch_tampering() {
        let id = SigningIdentity::generate();
        let mut e = sample_entry("so.tilo.editor", "1.0.0", "x86_64-v3");
        e.sign(&id).unwrap();
        e.verify_publisher().unwrap();
        // Flip the advertised hash — signature must no longer verify.
        e.tapp_hash = hex::encode([1u8; 32]);
        assert!(e.verify_publisher().is_err());

        let registry = SigningIdentity::generate();
        let other = SigningIdentity::generate();
        let mut idx = index_of([sample_entry("so.tilo.editor", "1.0.0", "x86_64-v3")]);
        idx.sign(&registry).unwrap();
        idx.verify_signature(&registry.public_key_bytes()).unwrap();
        assert!(idx.verify_signature(&other.public_key_bytes()).is_err());

        let bytes = idx.to_json().unwrap();
        let back = RegistryIndex::from_json(&bytes).unwrap();
        back.verify_signature(&registry.public_key_bytes()).unwrap();
        assert_eq!(back, idx);
    }

    #[test]
    fn resolution_picks_the_best_runnable_isa_version_and_runtime() {
        let idx = index_of(
            [
                ("1.0.0", "x86_64"),
                ("1.0.0", "x86_64-v3"),
                ("1.0.0", "x86_64-v4"),
                ("0.9.0", "x86_64-v3"),
            ]
            .map(|(ver, isa)| sample_entry("so.tilo.editor", ver, isa)),
        );
        // A v3 host cannot run the v4 build; must pick 1.0.0 / v3.
        let best = idx
            .resolve_best("so.tilo.editor", None, &x86_host(X86_64Level::V3))
            .unwrap();
        assert_eq!(best.version, "1.0.0");
        assert_eq!(best.isa, "x86_64-v3");
        // A v4 host prefers the v4 build.
        let best4 = idx
            .resolve_best("so.tilo.editor", None, &x86_host(X86_64Level::V4))
            .unwrap();
        assert_eq!(best4.isa, "x86_64-v4");

        let idx = index_of(
            ["1.0.0", "1.2.0", "2.0.0"].map(|ver| sample_entry("so.tilo.editor", ver, "x86_64")),
        );
        let req = semver::VersionReq::parse("^1").unwrap();
        let best = idx
            .resolve_best(
                "so.tilo.editor",
                Some(&req),
                &x86_host(X86_64Level::Baseline),
            )
            .unwrap();
        assert_eq!(best.version, "1.2.0");

        let mut e = sample_entry("so.tilo.editor", "1.0.0", "x86_64");
        e.runtime = Some("tilo-runtime/1.2.0".into());
        let idx = index_of([e]);
        let host = x86_host(X86_64Level::Baseline);
        assert!(
            idx.resolve_best("so.tilo.editor", None, &host).is_none(),
            "no runtime installed"
        );
        let host = host.with_runtime(RuntimeRef::parse("tilo-runtime/1.5.0").unwrap());
        assert!(
            idx.resolve_best("so.tilo.editor", None, &host).is_some(),
            "compatible runtime"
        );
        let host = x86_host(X86_64Level::Baseline)
            .with_runtime(RuntimeRef::parse("tilo-runtime/2.0.0").unwrap());
        assert!(
            idx.resolve_best("so.tilo.editor", None, &host).is_none(),
            "different major"
        );
    }
}
