//! Closed installed Home and applications consumer contracts. Digests bind
//! artifacts, not execution.
//!
//! Applications qualification is exclusively the owned-disposable factory in
//! the prepared setup helper. Read-only checks revalidate its protected native
//! receipt and signed source cohort; they never rerun capture or manufacture a
//! successful qualification.
use super::{field, invalid, json, require};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    ffi::OsString,
    fs::{File, Metadata, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};

pub const HELPER: &str = "/usr/libexec/tilo-prepared-setup";
pub const HOME: &str = "home-consumer-capabilities.json";
pub const APPS: &str = "applications-consumer-capabilities.json";
pub const NATIVE: &str = "applications-consumer-native-qualification.json";
pub const ORIGINAL: &str = "original-source-consumer-capabilities.json";
pub const TOOLS: [&str; 4] = [
    "/usr/bin/flatpak",
    "/usr/bin/ostree",
    "/usr/bin/unshare",
    "/usr/bin/setpriv",
];
const HOME_KEYS: [&str; 7] = [
    "schema",
    "archive_protocol",
    "seed_protocol",
    "firstboot_user_sha256",
    "seed_helper_sha256",
    "prepared_helper_sha256",
    "tar_sha256",
];
const HOME_PROTOCOLS: [(&str, i64); 3] =
    [("schema", 1), ("archive_protocol", 2), ("seed_protocol", 1)];
const APPS_KEYS: [&str; 5] = [
    "schema",
    "subset_protocol",
    "prepared_helper_sha256",
    "qualification_script_sha256",
    "native_qualification_sha256",
];
const APPS_PROTOCOLS: [(&str, i64); 2] = [("schema", 1), ("subset_protocol", 2)];
const NATIVE_KEYS: [&str; 10] = [
    "schema",
    "subset_protocol",
    "source_protocol",
    "prepared_helper_sha256",
    "tools",
    "source_read_claim_sha256",
    "source_completion_receipt_sha256",
    "setup_ciphertext_sha256",
    "applications_ciphertext_sha256",
    "receiving",
];
/// Rootless admission checks stay finite; only the root producer gets the
/// long budget the actual capture and receiving qualification needs.
const CHECK_TIMEOUT: Duration = Duration::from_secs(120);
const PRODUCER_TIMEOUT: Duration = Duration::from_secs(1800);

/// Probe executor: command, stdout/stderr bound, deadline.
pub type Runner<'a> = dyn FnMut(&[OsString], usize, Duration) -> io::Result<Vec<u8>> + 'a;

/// The production executor.
pub fn invoke(command: &[OsString], limit: usize, timeout: Duration) -> io::Result<Vec<u8>> {
    super::invoke::run_fixed(command, limit, timeout)
}

fn decode(data: &[u8], limit: usize) -> io::Result<Value> {
    require(
        !data.is_empty() && data.len() <= limit,
        "consumer response exceeds bound",
    )?;
    json::strict(data)
}

fn hex_digest(value: &Value) -> bool {
    value.as_str().is_some_and(|text| {
        text.len() == 64
            && text
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

fn int_is(value: Option<&Value>, expected: i64) -> bool {
    value.and_then(Value::as_i64) == Some(expected)
}

fn shape<'v>(value: &'v Value, keys: &[&str], protocols: &[(&str, i64)]) -> io::Result<&'v Value> {
    let fields = value
        .as_object()
        .filter(|fields| {
            fields.keys().map(String::as_str).collect::<BTreeSet<_>>()
                == keys.iter().copied().collect()
        })
        .ok_or_else(|| invalid("unknown or missing consumer fields"))?;
    for (key, expected) in protocols {
        require(
            int_is(fields.get(*key), *expected),
            "unsupported consumer protocol",
        )?;
    }
    for key in keys.iter().filter(|key| key.ends_with("_sha256")) {
        require(
            fields.get(*key).is_some_and(hex_digest),
            "invalid consumer digest",
        )?;
    }
    Ok(value)
}

/// The fixed installed namespace. `root` and `owner` are test seams only; the
/// installed entry points always use [`Store::installed`].
pub struct Store {
    root: PathBuf,
    owner: u32,
}

type Stamp = (u64, u64, u32, u32, u32, u64, u64, i64, i64, i64, i64);
fn stamp(info: &Metadata) -> Stamp {
    (
        info.dev(),
        info.ino(),
        info.mode(),
        info.uid(),
        info.gid(),
        info.nlink(),
        info.size(),
        info.mtime(),
        info.mtime_nsec(),
        info.ctime(),
        info.ctime_nsec(),
    )
}

impl Store {
    #[must_use]
    pub fn installed() -> Self {
        Self::new(PathBuf::from("/"), 0)
    }

    #[must_use]
    pub fn new(root: PathBuf, owner: u32) -> Self {
        Self { root, owner }
    }

    /// Map a fixed absolute image path into this namespace.
    pub fn path(&self, name: &str) -> io::Result<PathBuf> {
        let relative = name
            .strip_prefix('/')
            .filter(|rest| rest.split('/').all(|part| !matches!(part, "" | "." | "..")))
            .ok_or_else(|| invalid("invalid fixed artifact path"))?;
        Ok(self.root.join(relative))
    }

    /// Check every namespace edge, the root included: owned, not foreign
    /// writable, and never a redirecting symlink.
    fn trusted(&self, path: &Path) -> io::Result<Metadata> {
        let relative = path
            .strip_prefix(&self.root)
            .map_err(|_| invalid("artifact outside installed root"))?;
        let mut edge = self.root.clone();
        let mut info = std::fs::symlink_metadata(&edge)?;
        let check = |info: &Metadata| {
            require(
                info.uid() == self.owner
                    && info.mode() & 0o022 == 0
                    && !info.file_type().is_symlink(),
                "consumer artifact is not image-owned",
            )
        };
        check(&info)?;
        for part in relative.components() {
            edge.push(part);
            info = std::fs::symlink_metadata(&edge)?;
            check(&info)?;
        }
        Ok(info)
    }

    fn open(path: &Path) -> io::Result<File> {
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
    }

    fn read(&self, name: &str, limit: u64) -> io::Result<Vec<u8>> {
        let path = self.path(name)?;
        let before = self.trusted(&path)?;
        require(
            before.is_file() && before.nlink() == 1 && before.size() > 0 && before.size() <= limit,
            "consumer artifact is not bounded regular executable/data",
        )?;
        let file = Self::open(&path)?;
        require(
            stamp(&before) == stamp(&file.metadata()?),
            "consumer replaced",
        )?;
        let mut data = Vec::new();
        (&file)
            .take(limit.saturating_add(1))
            .read_to_end(&mut data)?;
        require(
            data.len() as u64 == before.size()
                && stamp(&before) == stamp(&file.metadata()?)
                && stamp(&before) == stamp(&self.trusted(&path)?),
            "consumer changed",
        )?;
        Ok(data)
    }

    /// Stream an installed executable (or data file) into its SHA-256.
    pub fn digest(&self, name: &str, limit: u64, executable: bool) -> io::Result<String> {
        let path = self.path(name)?;
        let before = self.trusted(&path)?;
        require(
            before.is_file()
                && before.size() > 0
                && before.size() <= limit
                && (!executable || before.mode() & 0o111 != 0),
            "invalid installed executable",
        )?;
        let mut file = Self::open(&path)?;
        require(
            stamp(&before) == stamp(&file.metadata()?),
            "consumer replaced",
        )?;
        let digest = super::sha256_hex(&mut file)?;
        require(
            stamp(&before) == stamp(&file.metadata()?)
                && stamp(&before) == stamp(&self.trusted(&path)?),
            "consumer changed",
        )?;
        Ok(digest)
    }

    fn executable(&self, name: &str) -> io::Result<String> {
        self.digest(name, 1 << 30, true)
    }

    fn receipt(&self, name: &str, limit: u64) -> io::Result<Value> {
        decode(
            &self.read(&format!("/usr/share/tilo/{name}"), limit)?,
            usize::try_from(limit).unwrap_or(usize::MAX),
        )
    }

    fn exists(&self, name: &str) -> io::Result<bool> {
        Ok(std::fs::symlink_metadata(self.path(&format!("/usr/share/tilo/{name}"))?).is_ok())
    }

    /// Only the namespace owner (root for the installed image) may publish.
    fn owner_acts(&self) -> bool {
        rustix::process::geteuid().as_raw() == self.owner
    }

    /// Atomically publish a new receipt; an existing one is never replaced.
    fn publish(&self, name: &str, data: &[u8], limit: usize) -> io::Result<()> {
        require(self.owner_acts(), "consumer publication requires root")?;
        require(
            !data.is_empty() && data.len() <= limit,
            "consumer proof exceeds public bound",
        )?;
        let parent = self.path("/usr/share/tilo")?;
        self.trusted(&parent)?;
        let target = parent.join(name);
        require(
            std::fs::symlink_metadata(&target).is_err(),
            "consumer receipt already exists; refusing replacement",
        )?;
        let mut temporary = tempfile::Builder::new()
            .prefix(".data-consumer-")
            .tempfile_in(&parent)?;
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o644))?;
        temporary.write_all(data)?;
        temporary.flush()?;
        temporary.as_file().sync_all()?;
        // linkat without AT_SYMLINK_FOLLOW: atomic and fails if the name exists.
        std::fs::hard_link(temporary.path(), &target)?;
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(&parent)?
            .sync_all()
        // Dropping `temporary` unlinks its private name.
    }
}

fn run(runner: &mut Runner<'_>, command: &[OsString], limit: usize) -> io::Result<Vec<u8>> {
    runner(command, limit, CHECK_TIMEOUT)
}

pub(super) fn home_facts(store: &Store) -> io::Result<Value> {
    Ok(json!({
        "schema": 1, "archive_protocol": 2, "seed_protocol": 1,
        "prepared_helper_sha256": store.executable(HELPER)?,
        "firstboot_user_sha256": store.digest("/usr/libexec/tilo-firstboot-user", 1 << 20, true)?,
        "seed_helper_sha256": store.digest("/usr/libexec/tilo-firstboot-home-seeds", 64 << 20, true)?,
        "tar_sha256": store.digest("/usr/bin/tar", 64 << 20, true)?,
    }))
}

/// Produce (root) or read-only check (rootless) the installed Home consumers.
pub fn home(store: &Store, check: bool, runner: &mut Runner<'_>) -> io::Result<Value> {
    // The verifier is the helper itself, already bound by its digest; this
    // pins the compiled firstboot files helper the probe relies on.
    let infrastructure = || store.digest("/usr/libexec/tilo-firstboot-files", 64 << 20, true);
    let pinned = infrastructure()?;
    let before = home_facts(store)?;
    let helper = store.path(HELPER)?;
    let probe = decode(
        &run(
            runner,
            &[helper.into(), "--home-consumer-capability".into()],
            16384,
        )?,
        4096,
    )?;
    let actual = shape(&probe, &HOME_KEYS, &HOME_PROTOCOLS)?.clone();
    require(
        actual == before,
        "installed Home probe disagrees with exact artifacts",
    )?;
    let seeds = store.path("/usr/libexec/tilo-firstboot-home-seeds")?;
    let seed = decode(
        &run(runner, &[seeds.into(), "--capabilities".into()], 16384)?,
        4096,
    )?;
    shape(
        &seed,
        &["schema", "seed_protocol"],
        &[("schema", 1), ("seed_protocol", 1)],
    )?;
    require(
        home_facts(store)? == before && infrastructure()? == pinned,
        "Home artifacts changed during probe",
    )?;
    if check {
        let receipt = store.receipt(HOME, 4096)?;
        require(
            shape(&receipt, &HOME_KEYS, &HOME_PROTOCOLS)? == &actual,
            "installed Home receipt differs from actual consumers",
        )?;
    } else {
        let mut bytes = json::canonical(&actual);
        bytes.push(b'\n');
        store.publish(HOME, &bytes, 4096)?;
    }
    Ok(actual)
}

pub(super) struct AppsArtifacts {
    pub(super) helper: String,
    pub(super) tools: Value,
}

pub(super) fn apps_artifacts(store: &Store) -> io::Result<AppsArtifacts> {
    let mut tools = Map::new();
    for name in TOOLS {
        tools.insert(name.into(), store.executable(name)?.into());
    }
    Ok(AppsArtifacts {
        helper: store.executable(HELPER)?,
        tools: Value::Object(tools),
    })
}

impl PartialEq for AppsArtifacts {
    fn eq(&self, other: &Self) -> bool {
        self.helper == other.helper && self.tools == other.tools
    }
}

fn native_proof(native: &Value, capability: &Value, facts: &AppsArtifacts) -> io::Result<()> {
    shape(
        native,
        &NATIVE_KEYS,
        &[
            ("schema", 1),
            ("subset_protocol", 2),
            ("source_protocol", 6),
        ],
    )?;
    require(
        field(native, "prepared_helper_sha256") == facts.helper.as_str()
            && *field(native, "tools") == facts.tools,
        "native qualification artifact mismatch",
    )?;
    let report = shape(
        field(native, "receiving"),
        &[
            "schema",
            "subset_protocol",
            "deployments",
            "install_commands",
            "commits",
            "selected_data_sha256",
        ],
        &[
            ("schema", 1),
            ("subset_protocol", 2),
            ("deployments", 2),
            ("install_commands", 2),
        ],
    )?;
    let commits = field(report, "commits").as_object();
    require(
        commits.is_some_and(|commits| {
            commits.keys().map(String::as_str).collect::<BTreeSet<_>>()
                == BTreeSet::from([
                    "app/org.tilo.Fixture/x86_64/stable",
                    "runtime/org.tilo.FixtureRuntime/x86_64/stable",
                ])
                && commits.values().all(hex_digest)
        }),
        "receiving qualification did not restore and retry",
    )?;
    require(
        field(capability, "native_qualification_sha256")
            == hex::encode(Sha256::digest(json::canonical(native))).as_str(),
        "native qualification receipt digest mismatch",
    )
}

/// Run the root factory, or read-only check (rootless) its published receipts.
#[allow(
    clippy::too_many_lines,
    reason = "one linear contract: each step gates the next"
)]
pub fn applications(store: &Store, check: bool, runner: &mut Runner<'_>) -> io::Result<Value> {
    let before = apps_artifacts(store)?;
    let helper = store.path(HELPER)?;
    let (capability, native, native_bytes, original_source) = if check {
        let capability = store.receipt(APPS, 4096)?;
        shape(&capability, &APPS_KEYS, &APPS_PROTOCOLS)?;
        let native_bytes = store.read(&format!("/usr/share/tilo/{NATIVE}"), 8192)?;
        let native = decode(&native_bytes, 8192)?;
        require(
            native_bytes == json::canonical(&native),
            "native qualification bytes are not canonical",
        )?;
        (capability, native, native_bytes, None)
    } else {
        require(
            store.owner_acts(),
            "native application qualification requires root",
        )?;
        for name in [APPS, NATIVE, ORIGINAL] {
            require(
                !store.exists(name)?,
                "native qualification must begin without previous receipts",
            )?;
        }
        // No path/receipt/boolean input: the factory checks disposable ownership,
        // real original-user grants, signed source6 cohort, target restore/retry
        // and retirement.
        let output = runner(
            &[
                helper.clone().into(),
                "--applications-consumer-capability".into(),
            ],
            8192,
            PRODUCER_TIMEOUT,
        )?;
        let envelope = decode(&output, 8192)?;
        let mut fields = envelope
            .as_object()
            .filter(|fields| {
                fields.keys().map(String::as_str).collect::<BTreeSet<_>>()
                    == BTreeSet::from(["capability", "native_qualification", "original_source"])
            })
            .cloned()
            .ok_or_else(|| invalid("invalid actual native qualification envelope"))?;
        let capability = fields.remove("capability").unwrap_or_default();
        shape(&capability, &APPS_KEYS, &APPS_PROTOCOLS)?;
        let native = fields.remove("native_qualification").unwrap_or_default();
        let native_bytes = json::canonical(&native);
        (
            capability,
            native,
            native_bytes,
            fields.remove("original_source"),
        )
    };
    require(
        field(&capability, "prepared_helper_sha256") == before.helper.as_str()
            && field(&capability, "qualification_script_sha256") == before.helper.as_str(),
        "application capability artifacts differ",
    )?;
    native_proof(&native, &capability, &before)?;
    require(
        apps_artifacts(store)? == before,
        "applications artifacts changed during qualification",
    )?;
    if let Some(original_source) = &original_source {
        // The app capability is the completion marker, published LAST. A crash
        // before it leaves admission unavailable; no read-only check repairs it.
        let mut bytes = json::canonical(original_source);
        bytes.push(b'\n');
        store.publish(ORIGINAL, &bytes, 8192)?;
    }
    let original = decode(
        &run(
            runner,
            &[
                helper.into(),
                "--reattest-original-source-consumer-capability".into(),
            ],
            16384,
        )?,
        16384,
    )?;
    require(
        original == store.receipt(ORIGINAL, 8192)?,
        "signed original consumer reattestation differs",
    )?;
    require(
        int_is(original.get("original_source_protocol"), 6)
            && original.get("prepared_helper_sha256") == Some(&Value::from(before.helper.as_str()))
            && int_is(
                original
                    .get("capture_pins")
                    .and_then(Value::as_object)
                    .and_then(|pins| pins.get("worker_protocol")),
                6,
            ),
        "applications require the exact reattested source6 cohort",
    )?;
    if check {
        require(
            store.receipt(APPS, 4096)? == capability
                && store.read(&format!("/usr/share/tilo/{NATIVE}"), 8192)? == native_bytes,
            "application receipts changed during reattestation",
        )?;
    }
    require(
        apps_artifacts(store)? == before,
        "applications artifacts changed during reattestation",
    )?;
    if !check {
        store.publish(NATIVE, &native_bytes, 8192)?;
        let mut bytes = json::canonical(&capability);
        bytes.push(b'\n');
        store.publish(APPS, &bytes, 4096)?;
    }
    Ok(capability)
}

/// Checked installed facts for the release signer's catalog or manifest.
///
/// Run in the exact final native target root before freezing its image. This
/// signs nothing: the fields travel in the existing authenticated catalog,
/// never as a freestanding trusted sidecar.
pub fn catalog_facts(store: &Store, runner: &mut Runner<'_>) -> io::Result<Value> {
    let mut facts = Map::new();
    facts.insert("home_archive".into(), home(store, true, runner)?);
    // An incomplete attempted native producer fails publication rather than
    // silently downgrading the release to Home-only support.
    if store.exists(APPS)? || store.exists(NATIVE)? {
        facts.insert("applications".into(), applications(store, true, runner)?);
        facts.insert("original_source".into(), store.receipt(ORIGINAL, 8192)?);
    }
    Ok(Value::Object(facts))
}
