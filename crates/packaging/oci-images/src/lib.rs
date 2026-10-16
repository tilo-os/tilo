//! `tilo-oci-images` — environment images from the OCI registry CI
//! publishes them to.
//!
//! CI seals `env-runtime` and catalog `tool` images into signed tapps and
//! publishes them as OCI images whose layers hold the `.tapp` files
//! (`registry.tilo.so/tilo/env-runtime:set-<fingerprint>`,
//! `registry.tilo.so/tilo/env-tools:<tag>`). They are not in the app
//! registry: they are not apps, and its review would cap and list them.
//! [`OciImages`] is a [`PackageFetcher`] over those repositories, for a node
//! that did not ship the images in its system store — a relay, or a node of
//! another architecture.
//!
//! Nothing here is trusted. A layer is checked against its digest, the
//! tapps in it are cached by the id their header claims, and the package
//! store that asked checks the package hashes to the id it wanted and is
//! signed by a publisher it trusts. Manifests already scanned are
//! remembered, so each published set is downloaded once.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use sha2::Digest;
use tilo_app_package::{BlobFetcher, BlobHash, BlobSink, PackageFetcher, PackageId};

/// No published layer is larger than this.
const MAX_LAYER_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// The most bytes one `.tapp` may occupy once unpacked, and the most a cached
/// one may be when it is read back whole. The compressed layer is bounded by
/// `MAX_LAYER_BYTES`, but what it inflates to is the registry's word, and a
/// cached package is read into memory before anything verifies it.
const MAX_TAPP_BYTES: u64 = 1024 * 1024 * 1024;
/// Manifests and token responses are small.
const MAX_DOCUMENT_BYTES: usize = 1024 * 1024;
/// Tags scanned in one repository named without a tag.
const MAX_SCANNED_TAGS: usize = 64;

const MANIFEST_TYPES: &str = "application/vnd.oci.image.index.v1+json, \
     application/vnd.docker.distribution.manifest.list.v2+json, \
     application/vnd.oci.image.manifest.v1+json, \
     application/vnd.docker.distribution.manifest.v2+json";

/// One OCI repository, optionally pinned to a tag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OciSource {
    /// Registry host, e.g. `registry.tilo.so`.
    pub registry: String,
    /// Repository, e.g. `tilo/env-runtime`.
    pub repository: String,
    /// A tag; `None` scans the repository's tags.
    pub tag: Option<String>,
}

impl OciSource {
    /// Parse `host/repo[:tag]`.
    ///
    /// # Errors
    ///
    /// Returns an error for a reference without a host and a repository.
    pub fn parse(reference: &str) -> Result<Self> {
        let (registry, rest) = reference
            .split_once('/')
            .ok_or_else(|| anyhow!("{reference:?} names no registry host"))?;
        let (repository, tag) = match rest.rsplit_once(':') {
            Some((repository, tag)) if !tag.contains('/') => {
                (repository.to_owned(), Some(tag.to_owned()))
            }
            _ => (rest.to_owned(), None),
        };
        if registry.is_empty() || repository.is_empty() {
            bail!("{reference:?} names no repository");
        }
        Ok(Self {
            registry: registry.to_owned(),
            repository,
            tag,
        })
    }
}

/// Environment images fetched from OCI repositories, cached on disk.
#[derive(Clone, Debug)]
pub struct OciImages {
    sources: Vec<OciSource>,
    cache: PathBuf,
    scheme: &'static str,
}

impl OciImages {
    /// Fetch from `sources`, caching tapps and scanned manifests in `cache`.
    pub fn new(sources: Vec<OciSource>, cache: PathBuf) -> Self {
        Self {
            sources,
            cache,
            scheme: "https",
        }
    }

    /// The registry CI publishes environment images to.
    pub fn published(cache: PathBuf) -> Self {
        Self::new(
            vec![
                OciSource {
                    registry: "registry.tilo.so".into(),
                    repository: "tilo/env-runtime".into(),
                    tag: None,
                },
                OciSource {
                    registry: "registry.tilo.so".into(),
                    repository: "tilo/env-tools".into(),
                    tag: None,
                },
            ],
            cache,
        )
    }

    /// Speak plain HTTP (a local test registry).
    #[must_use]
    pub fn insecure(mut self) -> Self {
        self.scheme = "http";
        self
    }

    fn cached(&self, id: &PackageId) -> PathBuf {
        self.cache
            .join("tapps")
            .join(format!("{}.tapp", id.to_hex()))
    }

    /// The tapp of `id`, from the cache or the sources.
    ///
    /// # Errors
    ///
    /// Returns an error when a source cannot be asked or serves bytes that
    /// do not match their digest.
    pub async fn fetch(&self, id: &PackageId) -> Result<Option<Vec<u8>>> {
        let path = self.cached(id);
        if let Some(bytes) = read_if_present(&path)? {
            return Ok(Some(bytes));
        }
        let client = tilo_net::http::client()
            .build()
            .context("build OCI HTTP client")?;
        for source in &self.sources {
            let mut session = Session {
                client: &client,
                images: self,
                source,
                token: None,
            };
            session.scan().await?;
            if let Some(bytes) = read_if_present(&path)? {
                return Ok(Some(bytes));
            }
        }
        Ok(None)
    }

    fn scanned_path(&self) -> PathBuf {
        self.cache.join("scanned")
    }

    fn scanned(&self) -> BTreeSet<String> {
        std::fs::read_to_string(self.scanned_path())
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn mark_scanned(&self, digest: &str) -> Result<()> {
        std::fs::create_dir_all(&self.cache)?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.scanned_path())?;
        writeln!(file, "{digest}")?;
        Ok(())
    }

    /// Unpack every `.tapp` in a layer into the cache under the id its
    /// header claims.
    fn unpack_layer(&self, layer: &Path, media_type: &str) -> Result<usize> {
        self.unpack_layer_bounded(layer, media_type, MAX_TAPP_BYTES)
    }

    /// [`Self::unpack_layer`] with the per-package ceiling as a parameter.
    /// An entry past it — declared or inflated — is dropped, not truncated:
    /// a partial package would fail verification anyway, and the rest of the
    /// layer is still worth unpacking.
    fn unpack_layer_bounded(
        &self,
        layer: &Path,
        media_type: &str,
        max_tapp_bytes: u64,
    ) -> Result<usize> {
        let file = std::fs::File::open(layer)?;
        let reader: Box<dyn Read> = if media_type.ends_with("gzip") {
            Box::new(flate2::read::GzDecoder::new(file))
        } else {
            Box::new(file)
        };
        let tapps = self.cache.join("tapps");
        std::fs::create_dir_all(&tapps)?;
        let mut archive = tar::Archive::new(reader);
        let mut unpacked = 0usize;
        for entry in archive.entries()? {
            let mut entry = entry?;
            let is_tapp = entry.header().entry_type().is_file()
                && entry
                    .path()?
                    .extension()
                    .is_some_and(|extension| extension == "tapp");
            if !is_tapp || entry.size() > max_tapp_bytes {
                continue;
            }
            let staging = tapps.join(format!(".incoming-{}", std::process::id()));
            let written = {
                let mut out = std::fs::File::create(&staging)?;
                // The header's size is not trusted either: inflate at most
                // one byte past the ceiling, and that byte means over.
                let written = std::io::copy(
                    &mut (&mut entry).take(max_tapp_bytes.saturating_add(1)),
                    &mut out,
                )?;
                out.sync_all()?;
                written
            };
            if written > max_tapp_bytes {
                std::fs::remove_file(&staging)?;
                continue;
            }
            match claimed_id(&staging) {
                Some(id) => {
                    std::fs::rename(&staging, tapps.join(format!("{}.tapp", id.to_hex())))?;
                    unpacked = unpacked.saturating_add(1);
                }
                None => std::fs::remove_file(&staging)?,
            }
        }
        Ok(unpacked)
    }

    /// Run `work` on a private current-thread runtime, as the package store
    /// calls in from synchronous code.
    fn block_on<T>(work: impl std::future::Future<Output = Result<T>>) -> std::io::Result<T> {
        if tokio::runtime::Handle::try_current().is_ok() {
            return Err(std::io::Error::other(
                "image fetch must not run on an async runtime thread",
            ));
        }
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(work)
            .map_err(std::io::Error::other)
    }
}

/// The package id a tapp's header claims, read from its first entry.
fn claimed_id(path: &Path) -> Option<PackageId> {
    #[derive(serde::Deserialize)]
    struct Header {
        package_id: String,
    }
    let mut archive = tar::Archive::new(std::fs::File::open(path).ok()?);
    let mut entry = archive.entries().ok()?.next()?.ok()?;
    if entry.path().ok()?.as_os_str() != "tapp.toml" {
        return None;
    }
    let mut text = String::new();
    entry.by_ref().take(4096).read_to_string(&mut text).ok()?;
    let header: Header = toml::from_str(&text).ok()?;
    header.package_id.parse().ok()
}

fn read_if_present(path: &Path) -> Result<Option<Vec<u8>>> {
    read_if_present_within(path, MAX_TAPP_BYTES)
}

/// Reads a cached package whole, refusing one larger than `max_bytes`
/// before allocating for it.
fn read_if_present_within(path: &Path, max_bytes: u64) -> Result<Option<Vec<u8>>> {
    let len = match std::fs::metadata(path) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("stat {}", path.display())),
    };
    if len > max_bytes {
        bail!(
            "{} is {len} bytes, past the {max_bytes}-byte package ceiling",
            path.display()
        );
    }
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

/// The OCI platform architecture of this machine.
fn oci_architecture() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    }
}

struct Session<'a> {
    client: &'a reqwest::Client,
    images: &'a OciImages,
    source: &'a OciSource,
    token: Option<String>,
}

impl Session<'_> {
    fn url(&self, path: &str) -> String {
        format!(
            "{}://{}/v2/{}/{path}",
            self.images.scheme, self.source.registry, self.source.repository
        )
    }

    /// GET `url`, answering one bearer-token challenge.
    async fn get(&mut self, url: &str, accept: Option<&str>) -> Result<reqwest::Response> {
        for _ in 0..2 {
            let mut request = self.client.get(url);
            if let Some(accept) = accept {
                request = request.header("Accept", accept);
            }
            if let Some(token) = &self.token {
                request = request.bearer_auth(token);
            }
            let response = request.send().await.with_context(|| format!("GET {url}"))?;
            if response.status().as_u16() != 401 || self.token.is_some() {
                return response
                    .error_for_status()
                    .with_context(|| format!("GET {url}"));
            }
            let challenge = response
                .headers()
                .get("www-authenticate")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
                .ok_or_else(|| anyhow!("{url} wants authentication without a challenge"))?;
            self.token = Some(self.authenticate(&challenge).await?);
        }
        bail!("{url} refused the registry's own token")
    }

    async fn authenticate(&self, challenge: &str) -> Result<String> {
        #[derive(serde::Deserialize)]
        struct Token {
            token: Option<String>,
            access_token: Option<String>,
        }
        let parameters = challenge
            .strip_prefix("Bearer ")
            .ok_or_else(|| anyhow!("unsupported registry challenge {challenge:?}"))?;
        let field = |name: &str| {
            parameters.split(',').find_map(|part| {
                let (key, value) = part.trim().split_once('=')?;
                (key == name).then(|| value.trim_matches('"').to_owned())
            })
        };
        let realm = field("realm").ok_or_else(|| anyhow!("challenge names no realm"))?;
        let separator = if realm.contains('?') { '&' } else { '?' };
        let mut url = format!(
            "{realm}{separator}scope=repository:{}:pull",
            self.source.repository
        );
        if let Some(service) = field("service") {
            url.push_str("&service=");
            url.push_str(&service);
        }
        let body = self
            .client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("GET {realm}"))?
            .error_for_status()
            .with_context(|| format!("GET {realm}"))?
            .bytes()
            .await?;
        let token: Token = serde_json::from_slice(body.get(..MAX_DOCUMENT_BYTES).unwrap_or(&body))?;
        token
            .token
            .or(token.access_token)
            .ok_or_else(|| anyhow!("{realm} issued no token"))
    }

    async fn document(&mut self, url: &str, accept: Option<&str>) -> Result<serde_json::Value> {
        let body = self.get(url, accept).await?.bytes().await?;
        if body.len() > MAX_DOCUMENT_BYTES {
            bail!("{url} is larger than any registry document");
        }
        Ok(serde_json::from_slice(&body)?)
    }

    /// Scan the source's tags (or its one tag) for tapps.
    async fn scan(&mut self) -> Result<()> {
        let tags = if let Some(tag) = &self.source.tag {
            vec![tag.clone()]
        } else {
            {
                let list = self.document(&self.url("tags/list"), None).await?;
                let mut tags: Vec<String> = list
                    .pointer("/tags")
                    .and_then(serde_json::Value::as_array)
                    .map(|tags| {
                        tags.iter()
                            .filter_map(|tag| tag.as_str().map(str::to_owned))
                            .filter(|tag| !tag.starts_with("candidate-"))
                            .collect()
                    })
                    .unwrap_or_default();
                tags.sort();
                tags.reverse();
                tags.truncate(MAX_SCANNED_TAGS);
                tags
            }
        };
        let mut scanned = self.images.scanned();
        for tag in tags {
            let manifest = self.platform_manifest(&tag).await?;
            let Some((digest, manifest)) = manifest else {
                continue;
            };
            if scanned.contains(&digest) {
                continue;
            }
            for layer in manifest
                .pointer("/layers")
                .and_then(serde_json::Value::as_array)
                .cloned()
                .unwrap_or_default()
            {
                let (Some(layer_digest), media_type) = (
                    layer.pointer("/digest").and_then(serde_json::Value::as_str),
                    layer
                        .pointer("/mediaType")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or(""),
                ) else {
                    continue;
                };
                let path = self.download_layer(layer_digest).await?;
                let unpacked = self.images.unpack_layer(&path, media_type);
                let _ = std::fs::remove_file(&path);
                unpacked?;
            }
            self.images.mark_scanned(&digest)?;
            scanned.insert(digest);
        }
        Ok(())
    }

    /// The manifest of `tag` for this machine's architecture, and its
    /// digest (the cache key of what was scanned).
    async fn platform_manifest(
        &mut self,
        tag: &str,
    ) -> Result<Option<(String, serde_json::Value)>> {
        let top = self
            .document(&self.url(&format!("manifests/{tag}")), Some(MANIFEST_TYPES))
            .await?;
        let Some(entries) = top
            .pointer("/manifests")
            .and_then(serde_json::Value::as_array)
        else {
            let digest = format!("{}:{tag}", self.source.repository);
            return Ok(Some((digest, top)));
        };
        let chosen = entries
            .iter()
            .find(|entry| {
                entry
                    .pointer("/platform/architecture")
                    .and_then(serde_json::Value::as_str)
                    == Some(oci_architecture())
            })
            .or_else(|| entries.first());
        let Some(digest) = chosen
            .and_then(|entry| entry.pointer("/digest").and_then(serde_json::Value::as_str))
            .map(str::to_owned)
        else {
            return Ok(None);
        };
        let manifest = self
            .document(
                &self.url(&format!("manifests/{digest}")),
                Some(MANIFEST_TYPES),
            )
            .await?;
        Ok(Some((digest, manifest)))
    }

    /// Download one layer to the cache, checked against its digest.
    async fn download_layer(&mut self, digest: &str) -> Result<PathBuf> {
        let expected = digest
            .strip_prefix("sha256:")
            .ok_or_else(|| anyhow!("unsupported layer digest {digest}"))?
            .to_owned();
        std::fs::create_dir_all(&self.images.cache)?;
        let path = self.images.cache.join(format!(".layer-{expected}"));
        let url = self.url(&format!("blobs/{digest}"));
        let mut response = self.get(&url, None).await?;
        let mut file = std::fs::File::create(&path)?;
        let mut hasher = sha2::Sha256::new();
        let mut length = 0u64;
        while let Some(chunk) = response
            .chunk()
            .await
            .with_context(|| format!("read {url}"))?
        {
            length = length.saturating_add(chunk.len() as u64);
            if length > MAX_LAYER_BYTES {
                drop(file);
                let _ = std::fs::remove_file(&path);
                bail!("{url} exceeds the {MAX_LAYER_BYTES}-byte layer ceiling");
            }
            hasher.update(&chunk);
            file.write_all(&chunk)?;
        }
        file.sync_all()?;
        if hex::encode(hasher.finalize()) != expected {
            let _ = std::fs::remove_file(&path);
            bail!("{url} served bytes that do not hash to {digest}");
        }
        Ok(path)
    }
}

impl PackageFetcher for OciImages {
    fn fetch_package(&self, id: &PackageId) -> std::io::Result<Option<Vec<u8>>> {
        Self::block_on(self.fetch(id))
    }
}

impl BlobFetcher for OciImages {
    /// Published images are full tapps: they carry every payload blob.
    fn fetch(&self, hashes: &[BlobHash], _sink: &mut dyn BlobSink) -> std::io::Result<()> {
        if hashes.is_empty() {
            return Ok(());
        }
        Err(std::io::Error::other(
            "published environment images carry their payload; none is fetched separately",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_name_a_registry_a_repository_and_maybe_a_tag() {
        assert_eq!(
            OciSource::parse("registry.tilo.so/tilo/env-runtime:set-ab").unwrap(),
            OciSource {
                registry: "registry.tilo.so".into(),
                repository: "tilo/env-runtime".into(),
                tag: Some("set-ab".into()),
            }
        );
        assert_eq!(
            OciSource::parse("localhost:5000/tilo/env-tools")
                .unwrap()
                .tag,
            None
        );
        assert!(OciSource::parse("no-repository").is_err());
    }

    #[test]
    fn layers_unpack_their_tapps_under_the_id_the_header_claims() {
        let dir = tempfile::tempdir().unwrap();
        let images = OciImages::new(Vec::new(), dir.path().join("cache"));
        let id = PackageId::from_bytes([7; 32]);
        let mut tapp = tar::Builder::new(Vec::new());
        let header = format!("package_id = \"{id}\"\nformat = 1\n");
        let mut entry = tar::Header::new_gnu();
        entry.set_size(header.len() as u64);
        entry.set_mode(0o644);
        entry.set_cksum();
        tapp.append_data(&mut entry, "tapp.toml", header.as_bytes())
            .unwrap();
        let tapp = tapp.into_inner().unwrap();

        let mut layer = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for (name, bytes) in [
            ("env-runtime/env-runtime-x86_64.tapp", tapp.as_slice()),
            ("env-runtime/SHA256SUMS", b"ignored".as_slice()),
        ] {
            let mut entry = tar::Header::new_gnu();
            entry.set_size(bytes.len() as u64);
            entry.set_mode(0o644);
            entry.set_cksum();
            layer.append_data(&mut entry, name, bytes).unwrap();
        }
        let layer = layer.into_inner().unwrap().finish().unwrap();
        let path = dir.path().join("layer");
        std::fs::write(&path, layer).unwrap();

        assert_eq!(
            images
                .unpack_layer(&path, "application/vnd.oci.image.layer.v1.tar+gzip")
                .unwrap(),
            1
        );
        assert_eq!(
            read_if_present(&images.cached(&id)).unwrap(),
            Some(tapp.clone())
        );
        images.mark_scanned("sha256:aa").unwrap();
        assert!(images.scanned().contains("sha256:aa"));

        // Phase `a_package_past_the_ceiling_is_dropped_not_unpacked`.
        // The same layer under a ceiling smaller than the package: nothing
        // is unpacked, and no staging file is left behind.
        {
            let dir = tempfile::tempdir().unwrap();
            let images = OciImages::new(Vec::new(), dir.path().join("cache"));
            let ceiling = u64::try_from(tapp.len()).expect("fits").saturating_sub(1);
            assert_eq!(
                images
                    .unpack_layer_bounded(
                        &path,
                        "application/vnd.oci.image.layer.v1.tar+gzip",
                        ceiling
                    )
                    .unwrap(),
                0
            );
            assert!(!images.cached(&id).exists());
            let leftovers: Vec<_> = std::fs::read_dir(dir.path().join("cache/tapps"))
                .unwrap()
                .flatten()
                .collect();
            assert!(
                leftovers.is_empty(),
                "no staging file may remain: {leftovers:?}"
            );
        }
        // Phase `a_cached_package_past_the_ceiling_is_refused_before_it_is_read`.
        {
            let cached = dir.path().join("big.tapp");
            std::fs::write(&cached, &tapp).unwrap();
            let len = u64::try_from(tapp.len()).expect("fits");
            assert!(read_if_present_within(&cached, len.saturating_sub(1)).is_err());
            assert_eq!(read_if_present_within(&cached, len).unwrap(), Some(tapp));
            assert_eq!(
                read_if_present_within(&dir.path().join("missing.tapp"), 1).unwrap(),
                None
            );
        }
    }
}
