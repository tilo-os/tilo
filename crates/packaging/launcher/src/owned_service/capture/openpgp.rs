//! Canonically discovered signed OSTree/OpenPGP subprocess closure.
use super::{CaptureAdmission, executable};
use serde::Deserialize;
use std::{
    collections::{BTreeSet, HashSet, VecDeque},
    fs::File,
    io::{self, Read},
    os::fd::{AsRawFd, RawFd},
    path::{Component, Path},
};
const TABLE: &str = "lib/tilo-capture-openpgp.json";
const BASE_FD: RawFd = 1536;
pub(super) fn reserved_fd(fd: RawFd) -> bool {
    (BASE_FD..=BASE_FD + 260).contains(&fd)
}
const LIB_PATH: &str = "/run/tilo/migration/tools/lib";
const BOOT_ID: &str = "/proc/sys/kernel/random/boot_id";
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Table {
    schema: u32,
    openpgp_version: String,
    libexecdir: String,
    helpers: Vec<Helper>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Helper {
    role: Role,
    target: String,
    bundle: String,
}
#[derive(Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
enum Role {
    Ostree,
    Openpgp,
    Gpgconf,
    Agent,
    Keyboxd,
}
struct Entry {
    target: String,
    file: File,
    child_fd: RawFd,
    executable: bool,
}
pub(super) struct OpenPgp {
    entries: Vec<Entry>,
}
fn absolute(value: &str) -> bool {
    value.starts_with('/')
        && value.len() <= 4096
        && !value.bytes().any(|byte| byte < 32 || byte == 127)
        && Path::new(value)
            .components()
            .all(|item| matches!(item, Component::RootDir | Component::Normal(_)))
        && !value.contains("//")
        && !value.ends_with('/')
        && !value.split('/').any(|part| part == "." || part == "..")
}
impl Table {
    fn validate(&self) -> io::Result<()> {
        let mut roles = BTreeSet::new();
        let mut targets = BTreeSet::new();
        if self.schema != 1
            || !(4..=5).contains(&self.helpers.len())
            || self.openpgp_version.is_empty()
            || self.openpgp_version.len() > 128
            || self
                .openpgp_version
                .bytes()
                .any(|byte| byte < 32 || byte == 127)
            || !absolute(&self.libexecdir)
        {
            return Err(io::ErrorKind::InvalidData.into());
        }
        for helper in &self.helpers {
            if !roles.insert(&helper.role)
                || !targets.insert(&helper.target)
                || !absolute(&helper.target)
                || !(helper.target.starts_with("/usr/bin/")
                    || helper.target.starts_with("/usr/libexec/"))
                || !helper.bundle.starts_with("bin/")
                || helper.bundle.len() > 256
                || helper
                    .bundle
                    .bytes()
                    .any(|byte| !(byte.is_ascii_alphanumeric() || b"/._-".contains(&byte)))
                || Path::new(&helper.bundle).components().count() != 2
                || !Path::new(&helper.bundle)
                    .components()
                    .all(|item| matches!(item, Component::Normal(_)))
                || helper.bundle.contains("//")
                || helper
                    .bundle
                    .split('/')
                    .any(|part| part == "." || part == "..")
                || (helper.role == Role::Ostree && helper.target != "/usr/bin/ostree")
            {
                return Err(io::ErrorKind::InvalidData.into());
            }
        }
        if ![Role::Ostree, Role::Openpgp, Role::Gpgconf, Role::Agent]
            .iter()
            .all(|role| roles.contains(role))
        {
            return Err(io::ErrorKind::InvalidData.into());
        }
        Ok(())
    }
}
impl OpenPgp {
    pub(super) fn admit(admission: &CaptureAdmission) -> io::Result<Self> {
        let bundle = runtime_bundle::verify_bundle_with(
            &admission.runtime,
            &runtime_bundle::VerifyOptions {
                require_publisher: Some(admission.publisher.clone()),
                require_signature: true,
                require_arch: Some(std::env::consts::ARCH.into()),
            },
        )
        .map_err(io::Error::other)?;
        if bundle.root_hash != admission.runtime_root_hash {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let mut metadata = executable::pin_file(&admission.runtime, &bundle, TABLE, false)?;
        let mut bytes = Vec::new();
        Read::by_ref(&mut metadata)
            .take(32 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 32 * 1024 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let table: Table = serde_json::from_slice(&bytes)?;
        table.validate()?;
        let expected_loader = crate::loader_preflight::read_interpreter(Path::new(&format!(
            "/proc/self/fd/{}",
            admission.bwrap.as_raw_fd()
        )))?
        .ok_or_else(|| io::Error::other("Signed sandbox interpreter missing"))?;
        let mut entries = Vec::new();
        let mut pending = VecDeque::new();
        let mut total = 0u64;
        for helper in table.helpers {
            let file = executable::pin(&admission.runtime, &bundle, &helper.bundle)?;
            let path = format!("/proc/self/fd/{}", file.as_raw_fd());
            if crate::loader_preflight::read_interpreter(Path::new(&path))?.as_deref()
                != Some(expected_loader.as_str())
            {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            let dynamic = crate::loader_preflight::read_dynamic_section(Path::new(&path))?
                .ok_or_else(|| io::Error::other("Signed OpenPGP helper is not dynamic ELF"))?;
            pending.extend(dynamic.needed);
            Self::push(&mut entries, helper.target, file, true, &mut total)?;
        }
        let mut seen = HashSet::new();
        while let Some(name) = pending.pop_front() {
            if !seen.insert(name.clone()) {
                continue;
            }
            if seen.len() > 256
                || name.is_empty()
                || Path::new(&name).components().count() != 1
                || !matches!(
                    Path::new(&name).components().next(),
                    Some(Component::Normal(_))
                )
            {
                return Err(io::ErrorKind::InvalidData.into());
            }
            let file =
                executable::pin_file(&admission.runtime, &bundle, &format!("lib/{name}"), false)?;
            let path = format!("/proc/self/fd/{}", file.as_raw_fd());
            let dynamic = crate::loader_preflight::read_dynamic_section(Path::new(&path))?
                .ok_or_else(|| io::Error::other("Signed OpenPGP dependency is not dynamic ELF"))?;
            pending.extend(dynamic.needed);
            Self::push(
                &mut entries,
                format!("{LIB_PATH}/{name}"),
                file,
                false,
                &mut total,
            )?;
        }
        Ok(Self { entries })
    }
    fn push(
        entries: &mut Vec<Entry>,
        target: String,
        file: File,
        executable: bool,
        total: &mut u64,
    ) -> io::Result<()> {
        *total = total
            .checked_add(file.metadata()?.len())
            .ok_or(io::ErrorKind::InvalidData)?;
        if *total > 256 * 1024 * 1024 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let child_fd = BASE_FD
            .checked_add(i32::try_from(entries.len()).map_err(io::Error::other)?)
            .ok_or(io::ErrorKind::InvalidData)?;
        entries.push(Entry {
            target,
            file,
            child_fd,
            executable,
        });
        Ok(())
    }
    pub(super) fn deduplicate_archive(
        &mut self,
        archive: &super::archive::Archive,
    ) -> io::Result<()> {
        let mut redundant = HashSet::new();
        for entry in &self.entries {
            if entry.executable {
                continue;
            }
            for (name, file) in &archive.libraries {
                if entry.target != format!("{LIB_PATH}/{name}") {
                    continue;
                }
                if !same_bytes(&entry.file, file)? {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                redundant.insert(entry.target.clone());
            }
        }
        self.entries
            .retain(|entry| !redundant.contains(&entry.target));
        Ok(())
    }
    pub(super) fn descriptor_pairs(&self) -> Vec<(RawFd, RawFd)> {
        self.entries
            .iter()
            .map(|entry| (entry.file.as_raw_fd(), entry.child_fd))
            .collect()
    }
    pub(super) fn splice(&self, argv: &mut Vec<String>, ro_paths: &mut Vec<String>) {
        // OSTree uses the kernel boot ID for repository locking. The existing
        // private procfs supplies it; grant only this file, only after signed
        // original-source OSTree/OpenPGP admission. No ambient mount is added.
        ro_paths.push(BOOT_ID.into());
        let mut mounts = vec!["--dir".into(), LIB_PATH.into()];
        for entry in &self.entries {
            if let Some(parent) = Path::new(&entry.target).parent() {
                mounts.extend(["--dir".into(), parent.to_string_lossy().into_owned()]);
            }
            mounts.extend([
                "--perms".into(),
                if entry.executable { "0555" } else { "0444" }.into(),
                "--ro-bind-data".into(),
                entry.child_fd.to_string(),
                entry.target.clone(),
            ]);
            ro_paths.push(entry.target.clone());
        }
        argv.splice(0..0, mounts);
    }
}
fn same_bytes(left: &File, right: &File) -> io::Result<bool> {
    use std::os::unix::fs::FileExt;
    let size = left.metadata()?.len();
    if size != right.metadata()?.len() || size > 256 * 1024 * 1024 {
        return Ok(false);
    }
    let mut offset = 0;
    let mut a = [0u8; 8192];
    let mut b = [0u8; 8192];
    while offset < size {
        let count =
            usize::try_from((size - offset).min(a.len() as u64)).map_err(io::Error::other)?;
        left.read_exact_at(&mut a[..count], offset)?;
        right.read_exact_at(&mut b[..count], offset)?;
        if a[..count] != b[..count] {
            return Ok(false);
        }
        offset += count as u64;
    }
    Ok(true)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn openpgp_library_dedup_checks_exact_bytes_before_omitting_mount() {
        use std::io::Write;
        let mut tar = tempfile::tempfile().unwrap();
        tar.write_all(b"library-one").unwrap();
        let mut same = tempfile::tempfile().unwrap();
        same.write_all(b"library-one").unwrap();
        let mut changed = tempfile::tempfile().unwrap();
        changed.write_all(b"library-two").unwrap();
        assert!(same_bytes(&tar, &same).unwrap());
        assert!(!same_bytes(&tar, &changed).unwrap());
        assert!(reserved_fd(1536));
        assert!(reserved_fd(1796));
        assert!(!reserved_fd(1535));
        assert!(!reserved_fd(1797));
    }
    #[test]
    fn signed_ostree_policy_grants_only_exact_readonly_boot_id() {
        let helper = OpenPgp {
            entries: Vec::new(),
        };
        let mut argv = Vec::new();
        let mut read_only = Vec::new();
        helper.splice(&mut argv, &mut read_only);
        assert_eq!(read_only, [BOOT_ID]);
        assert!(!argv.iter().any(|arg| arg.starts_with("/proc")));
        assert!(!read_only.iter().any(|arg| matches!(
            arg.as_str(),
            "/proc" | "/proc/sys" | "/proc/sys/kernel" | "/proc/sys/kernel/random"
        )));
    }
    fn table() -> serde_json::Value {
        serde_json::json!({"schema":1,"openpgp_version":"2.4.7","libexecdir":"/usr/libexec","helpers":[{"role":"ostree","target":"/usr/bin/ostree","bundle":"bin/ostree"},{"role":"openpgp","target":"/usr/bin/gpg2","bundle":"bin/gpg"},{"role":"gpgconf","target":"/usr/bin/gpgconf","bundle":"bin/gpgconf"},{"role":"agent","target":"/usr/bin/gpg-agent","bundle":"bin/gpg-agent"}]})
    }
    #[test]
    fn openpgp_table_refuses_ambient_or_redirected_helper_authority() {
        let good: Table = serde_json::from_value(table()).unwrap();
        good.validate().unwrap();
        for target in [
            "/tmp/gpg",
            "/usr/bin/../secret",
            "/usr/bin//gpg",
            "/usr/bin/gpg\n",
        ] {
            let mut raw = table();
            raw["helpers"][1]["target"] = target.into();
            let parsed: Table = serde_json::from_value(raw).unwrap();
            assert!(parsed.validate().is_err());
        }
        let mut raw = table();
        raw["helpers"][1]["role"] = "ostree".into();
        let parsed: Table = serde_json::from_value(raw).unwrap();
        assert!(parsed.validate().is_err());
        let mut raw = table();
        raw["helpers"][1]["bundle"] = "bin/../../unsigned".into();
        let parsed: Table = serde_json::from_value(raw).unwrap();
        assert!(parsed.validate().is_err());
        let mut raw = table();
        raw["helpers"][1]["override"] = true.into();
        assert!(serde_json::from_value::<Table>(raw).is_err());
    }
}
