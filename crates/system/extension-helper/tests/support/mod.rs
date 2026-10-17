//! A Tilo root in a temporary directory, with a fake network.
//!
//! Artifacts are small JSON stand-ins for RPMs (the fake `rpm_header` and
//! `extract_rpm` read them); the network is a map of URLs; the signature
//! check is a keyed digest so that "signed by the trusted key", "signed by
//! another key" and "bytes changed after signing" stay distinct. The end to
//! end suite keeps the real rpm, rpm2archive and P-256 check instead.
#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::arithmetic_side_effects,
    clippy::panic,
    reason = "a test fixture: each integration target uses the part it needs, and any I/O error fails the test"
)]

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Cursor, Read};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tilo_extension::{Error, Header, Host, HttpError, Layout, Response, Result, System};

pub const BASE_PACKAGES: [&str; 5] = [
    "gstreamer1 0:1.26.5-1.fc44.x86_64",
    "libavcodec-free 0:7.1.1-3.fc44.x86_64",
    "mesa-dri-drivers 0:25.2.4-1.fc44.x86_64",
    "glibc 0:2.42-4.fc44.x86_64",
    "gpg-pubkey 0:d651ff2e-5dadbbc1.(none)",
];
pub const TRUSTED: &[u8] = b"tilo release key";
pub const ROGUE: &[u8] = b"somebody else";
pub const RPMFUSION: &str = "https://download1.rpmfusion.org/free/fedora/updates/44/x86_64";

pub fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

pub fn catalogue_dir() -> PathBuf {
    repo().join("os/usr/share/tilo/system-extensions/sysext.d")
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

pub fn sign(key: &[u8], data: &[u8]) -> Vec<u8> {
    sha256_hex(&[key, data].concat()).into_bytes()
}

/// A stand-in RPM: its header and payload as JSON.
pub fn rpm(name: &str, version: &str) -> (String, Vec<u8>) {
    rpm_with(
        name,
        version,
        &[(
            &format!("usr/lib64/{name}.so"),
            &format!("{name} {version}"),
        )],
    )
}

pub fn rpm_with(name: &str, version: &str, files: &[(&str, &str)]) -> (String, Vec<u8>) {
    let nvra = format!("{name}-{version}-1.fc44.x86_64");
    let files: serde_json::Map<String, Value> = files
        .iter()
        .map(|(k, v)| ((*k).to_owned(), json!(v)))
        .collect();
    let data = serde_json::to_vec(&json!({"name": name, "nvra": nvra, "files": files})).unwrap();
    (nvra, data)
}

struct Body {
    data: Cursor<Vec<u8>>,
    status: u16,
}

impl Read for Body {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.data.read(buf)
    }
}

impl Response for Body {
    fn status(&self) -> u16 {
        self.status
    }
}

/// The real signature check, with a tilo-crypto that is not at /usr/bin.
struct RealVerify(PathBuf);

impl Host for RealVerify {
    fn tilo_crypto(&self) -> PathBuf {
        self.0.clone()
    }
}

/// What the helper sees of the machine.
#[derive(Default)]
pub struct Fake {
    pub packages: RefCell<Vec<String>>,
    pub web: RefCell<BTreeMap<String, Vec<u8>>>,
    pub requests: RefCell<Vec<(String, u64)>>,
    pub online: RefCell<String>,
    pub merges: Cell<u32>,
    pub unmerges: Cell<u32>,
    pub merged: RefCell<Vec<String>>,
    pub masked: Cell<bool>,
    /// Renames out of the download cache fail as if it were another mount.
    pub cross_device: Cell<bool>,
    /// Real rpm, rpm2archive and a real P-256 check through this tilo-crypto.
    pub real_tools: Option<PathBuf>,
    pub rpm_queries: Cell<u32>,
}

impl Host for Fake {
    fn rpm_query_all(&self, _layout: &Layout) -> Result<Vec<String>> {
        self.rpm_queries.set(self.rpm_queries.get() + 1);
        Ok(self.packages.borrow().clone())
    }

    fn http_open(&self, url: &str, offset: u64) -> Result<Box<dyn Response>, HttpError> {
        self.requests.borrow_mut().push((url.to_owned(), offset));
        let web = self.web.borrow();
        let Some(data) = web.get(url) else {
            return Err(HttpError::Status {
                code: 404,
                reason: "Not Found".to_owned(),
            });
        };
        let start = usize::try_from(offset).unwrap().min(data.len());
        Ok(Box::new(Body {
            data: Cursor::new(data[start..].to_vec()),
            status: if offset > 0 { 206 } else { 200 },
        }))
    }

    fn verify_signature(
        &self,
        layout: &Layout,
        data: &[u8],
        signature: &[u8],
        workdir: &Path,
    ) -> Result<()> {
        if let Some(crypto) = &self.real_tools {
            return RealVerify(crypto.clone()).verify_signature(layout, data, signature, workdir);
        }
        if signature != sign(TRUSTED, data) {
            return Err(Error::pack(
                "the manifest's signature is not valid for this image; nothing was changed",
            ));
        }
        Ok(())
    }

    fn connectivity(&self) -> String {
        self.online.borrow().clone()
    }

    fn rpm_header(&self, path: &Path) -> Result<Header> {
        if self.real_tools.is_some() {
            return System.rpm_header(path);
        }
        let value: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        Ok(Header {
            name: value["name"].as_str().unwrap().to_owned(),
            nvra: value["nvra"].as_str().unwrap().to_owned(),
        })
    }

    fn extract_rpm(&self, path: &Path, tree: &Path, ld_conf: &mut Vec<String>) -> Result<()> {
        if self.real_tools.is_some() {
            return System.extract_rpm(path, tree, ld_conf);
        }
        let value: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        for (name, text) in value["files"].as_object().unwrap() {
            let target = tree.join(name);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(&target, text.as_str().unwrap()).unwrap();
        }
        Ok(())
    }

    fn sysext_refresh(&self, layout: &Layout) -> Result<()> {
        self.merges.set(self.merges.get() + 1);
        let mut names: Vec<String> = fs::read_dir(layout.extensions())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| !n.starts_with('.'))
            .collect();
        names.sort();
        *self.merged.borrow_mut() = names;
        Ok(())
    }

    fn sysext_unmerge(&self, _layout: &Layout) -> Result<()> {
        self.unmerges.set(self.unmerges.get() + 1);
        self.merged.borrow_mut().clear();
        Ok(())
    }

    fn sysext_masked(&self) -> Result<bool> {
        Ok(self.masked.get())
    }

    fn start_activation_unit(&self) -> Result<()> {
        Err(Error::pack("a test root never asks PID 1 to activate"))
    }

    fn relabel(&self, _tree: &Path) -> Result<()> {
        Ok(())
    }

    fn machine(&self) -> String {
        "x86_64".to_owned()
    }

    fn euid(&self) -> u32 {
        0
    }

    fn replace(&self, from: &Path, to: &Path) -> io::Result<()> {
        if self.cross_device.get() && from.to_string_lossy().contains("partial") {
            return Err(io::Error::from_raw_os_error(
                rustix::io::Errno::XDEV.raw_os_error(),
            ));
        }
        fs::rename(from, to)
    }
}

/// How to publish a manifest.
pub struct Publish {
    pub fingerprint: Option<String>,
    pub key: &'static [u8],
    pub revision: Option<u64>,
    pub version_id: &'static str,
}

impl Default for Publish {
    fn default() -> Self {
        Self {
            fingerprint: None,
            key: TRUSTED,
            revision: None,
            version_id: "44",
        }
    }
}

/// A Tilo root in a temporary directory with a fake network.
pub struct Machine {
    _tmp: tempfile::TempDir,
    pub root: PathBuf,
    pub layout: Layout,
    pub host: Fake,
    revision: Cell<u64>,
}

impl Machine {
    pub fn new() -> Self {
        Self::with_host(Fake::default())
    }

    pub fn with_host(host: Fake) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let layout = Layout::new(&root);
        fs::create_dir_all(root.join("usr/lib")).unwrap();
        fs::write(
            root.join("usr/lib/os-release"),
            "ID=fedora\nVERSION_ID=\"44\"\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("usr/share/tilo")).unwrap();
        fs::write(layout.trust_key(), "trusted\n").unwrap();
        fs::create_dir_all(layout.catalogue_dir()).unwrap();
        fs::copy(
            catalogue_dir().join("media-codecs.json"),
            layout.catalogue_dir().join("media-codecs.json"),
        )
        .unwrap();
        *host.packages.borrow_mut() = BASE_PACKAGES.iter().map(|s| (*s).to_owned()).collect();
        "full".clone_into(&mut host.online.borrow_mut());
        host.masked.set(true);
        Self {
            _tmp: tmp,
            root,
            layout,
            host,
            revision: Cell::new(1),
        }
    }

    /// Make this an image that carries the release root, as one built after
    /// the root ceremony does. The fake signature check stays keyed; the end
    /// to end suite puts real trust material here.
    pub fn certify(&self) {
        fs::create_dir_all(self.layout.release_root().parent().unwrap()).unwrap();
        fs::write(self.layout.release_root(), "root\n").unwrap();
        fs::write(self.layout.rekor_key(), "rekor\n").unwrap();
        fs::write(self.layout.release_trust(), "TILO_RELEASE_EPOCH=1\n").unwrap();
    }

    pub fn fingerprint(&self) -> String {
        tilo_extension::base_packages_sha256(&self.host.packages.borrow())
    }

    pub fn manifest_url(&self, fingerprint: Option<&str>) -> String {
        format!(
            "https://code.tilo.so/api/packages/tilo/generic/tilo-extension-media-codecs/x86_64-{}/manifest.json",
            fingerprint.map_or_else(|| self.fingerprint(), str::to_owned)
        )
    }

    pub fn artifact(&self, payload: (String, Vec<u8>)) -> Value {
        let (nvra, _) = &payload;
        let url = format!("{RPMFUSION}/{}/{nvra}.rpm", &nvra[..1]);
        self.artifact_at(payload, &url)
    }

    pub fn artifact_at(&self, payload: (String, Vec<u8>), url: &str) -> Value {
        let (nvra, data) = payload;
        let entry = json!({
            "package": nvra, "urls": [url], "size": data.len(), "sha256": sha256_hex(&data),
            "publisher": "RPM Fusion", "license": "GPL-2.0-or-later",
        });
        self.host.web.borrow_mut().insert(url.to_owned(), data);
        entry
    }

    pub fn manifest(&self, codecs: &[Value], mesa: Option<&[Value]>) -> Vec<u8> {
        self.manifest_with(codecs, mesa, &Publish::default())
    }

    pub fn manifest_with(
        &self,
        codecs: &[Value],
        mesa: Option<&[Value]>,
        publish: &Publish,
    ) -> Vec<u8> {
        let mut parts = vec![json!({
            "name": "tilo-codecs", "format": "rpm", "state": "available",
            "requires_base": {"gstreamer1": "0:1.26.5-1.fc44", "libavcodec-free": "0:7.1.1-3.fc44"},
            "artifacts": codecs,
        })];
        parts.push(match mesa {
            None => json!({
                "name": "tilo-codecs-mesa", "format": "rpm", "state": "waiting",
                "message": "Hardware H.264/HEVC is waiting for a matching Mesa build.",
                "requires_base": {"mesa-dri-drivers": "0:25.2.4-1.fc44"}, "artifacts": [],
            }),
            Some(mesa) => json!({
                "name": "tilo-codecs-mesa", "format": "rpm", "state": "available",
                "requires_base": {"mesa-dri-drivers": "0:25.2.4-1.fc44"}, "artifacts": mesa,
            }),
        });
        let revision = publish.revision.unwrap_or_else(|| {
            let next = self.revision.get();
            self.revision.set(next + 1);
            next
        });
        let fingerprint = publish
            .fingerprint
            .clone()
            .unwrap_or_else(|| self.fingerprint());
        let document = json!({
            "type": tilo_extension::MANIFEST_TYPE, "schema": 1, "id": "media-codecs", "revision": revision,
            "created": "2026-10-03T12:00:00Z",
            "base": {"os_id": "fedora", "version_id": publish.version_id, "architecture": "x86_64",
                     "packages_sha256": fingerprint,
                     "image": format!("registry.tilo.so/tilo/tilo@sha256:{}", "a".repeat(64))},
            "parts": parts,
        });
        let data = serde_json::to_vec_pretty(&document).unwrap();
        self.publish(&data, publish.key, publish.fingerprint.as_deref());
        data
    }

    pub fn publish(&self, data: &[u8], key: &[u8], fingerprint: Option<&str>) {
        let url = self.manifest_url(fingerprint);
        let mut web = self.host.web.borrow_mut();
        web.insert(
            format!("{url}{}", self.layout.signature_suffix()),
            sign(key, data),
        );
        web.insert(url, data.to_vec());
    }

    pub fn run(&self, verb: &str) -> i32 {
        self.run_on(verb, "media-codecs")
    }

    pub fn run_on(&self, verb: &str, ident: &str) -> i32 {
        let argument = if verb == "activate" {
            "activate".to_owned()
        } else {
            format!("{verb}:{ident}")
        };
        tilo_extension::main(&self.host, &self.layout, &[argument])
    }

    pub fn status(&self) -> Value {
        serde_json::from_slice(&fs::read(self.layout.status("media-codecs")).unwrap()).unwrap()
    }

    pub fn part(&self, name: &str) -> PathBuf {
        self.layout.extensions().join(name)
    }

    pub fn codecs(&self) -> PathBuf {
        self.part("tilo-codecs")
    }

    pub fn release(&self, name: &str) -> BTreeMap<String, String> {
        let path = self.part(name).join(format!(
            "usr/lib/extension-release.d/extension-release.{name}"
        ));
        tilo_extension::parse_os_release(&fs::read_to_string(path).unwrap())
    }

    pub fn rpm_requests(&self) -> Vec<String> {
        self.host
            .requests
            .borrow()
            .iter()
            .map(|(url, _)| url.clone())
            .filter(|u| Path::new(u).extension().is_some_and(|e| e == "rpm"))
            .collect()
    }
}
