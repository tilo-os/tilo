//! Root-service bootstrap for one SAME-source original Home projection.
//! Credentials become effective only inside the completed canonical namespace.
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{self, Read, Seek, Write},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
};
pub(in crate::owned_service) mod credentials;
mod delayed_filter;
mod roots;
mod workspace;
pub(super) use roots::reserved_fd;
pub use roots::{
    OriginalMountedBinding, OriginalMountedIdentity, OriginalMountedInput,
    VerifiedOriginalMountedRoots, original_mounted_table_sha256, verify_original_mounted_roots,
};
pub(crate) const BOOTSTRAP_FD: RawFd = 49;
pub(crate) const CLAIM_FD: RawFd = 50;
const MAX_BYTES: u64 = 1024 * 1024;
const GROUPS_LIMIT: usize = 65_536;
const SEALS: i32 = libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
const NS: [&str; 5] = ["mnt", "pid", "net", "ipc", "user"];
fn refused(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}
#[derive(Debug)]
pub(super) enum Stage {
    ReceiveBootstrap,
    VerifyNamespaceAndHome,
    ReceiveBaseline,
    ProjectMountedRoots,
    AttachMountedRoot,
    RemountReadonly,
    InstallBaseline,
    ObserveMountedRoots,
    CloseAmbientDescriptors,
    PublishMountedReceipt,
    PublishClaim,
    DropCredentials,
}
pub(super) fn phase<T>(stage: Stage, result: io::Result<T>) -> io::Result<T> {
    if let Err(error) = &result {
        // Never print provider prose, paths, root placement or source contents.
        eprintln!(
            "tilo-sandbox-exec: original-source stage={stage:?} kind={:?} errno={:?}",
            error.kind(),
            error.raw_os_error()
        );
    }
    result
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Facts {
    schema: u32,
    read_claim_digest: [u8; 32],
    uid: u32,
    gid: u32,
    groups: Vec<u32>,
    source: (u64, u64),
    namespaces: Vec<(u64, u64)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    roots: Vec<roots::Projection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    baseline_filter: Option<delayed_filter::Binding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mounted_table_sha256: Option<[u8; 32]>,
}
impl Facts {
    fn projection_filter_binding(&self) -> io::Result<Option<&delayed_filter::Binding>> {
        match (self.roots.is_empty(), self.baseline_filter.as_ref()) {
            (true, None) => Ok(None),
            (false, Some(binding)) => {
                binding.validate()?;
                Ok(Some(binding))
            }
            _ => Err(refused("Original projection baseline admission differs")),
        }
    }
    fn validate(&self) -> io::Result<()> {
        if self.schema != 2
            || self.read_claim_digest == [0; 32]
            || self.uid == 0
            || self.uid == u32::MAX
            || self.gid == u32::MAX
            || self.groups.len() > GROUPS_LIMIT
            || self.groups.contains(&u32::MAX)
            || self
                .groups
                .windows(2)
                .any(|p| p.first().zip(p.get(1)).is_none_or(|(a, b)| a >= b))
            || self.namespaces.len() != NS.len()
        {
            return Err(refused("Original-source credential token is not canonical"));
        }
        if let Some(binding) = &self.baseline_filter {
            binding.validate()?;
        }
        roots::validate(&self.roots)?;
        if let Some(expected) = self.mounted_table_sha256 {
            let actual = roots::original_mounted_table_sha256(
                &self
                    .roots
                    .iter()
                    .map(|root| roots::OriginalMountedIdentity {
                        placement: root.placement.clone(),
                        identity: root.identity,
                    })
                    .collect::<Vec<_>>(),
            )?;
            if actual != expected || expected == [0; 32] {
                return Err(refused("Original protected mounted table differs"));
            }
        }
        Ok(())
    }
}
pub(super) struct Bootstrap {
    token: File,
    facts: Facts,
    roots: Option<roots::Inputs>,
    workspace: std::sync::Mutex<Option<workspace::Workspace>>,
}
/// Private root runner lease; it cannot be minted from caller policy or JSON.
pub(crate) struct FilterLaunch {
    token: File,
    filter: delayed_filter::Filter,
    roots: roots::DetachedRoots,
}
impl FilterLaunch {
    pub(crate) fn descriptor_pairs(&self) -> Vec<(RawFd, RawFd)> {
        let mut pairs = vec![
            (self.token.as_raw_fd(), BOOTSTRAP_FD),
            (
                self.filter.descriptor(),
                crate::services::BASELINE_SECCOMP_CHILD_FD,
            ),
        ];
        pairs.extend(self.roots.pairs());
        pairs
    }
    pub(crate) fn splice_argv(&self, argv: &mut Vec<String>) -> io::Result<()> {
        root()?;
        let expected = ["--add-seccomp-fd", "40", "--add-seccomp-fd", "41"];
        if argv.len() < expected.len() || argv.iter().take(4).map(String::as_str).ne(expected) {
            return Err(refused("Canonical original baseline prefix differs"));
        }
        // The compatibility filter stays active. The exact full baseline is
        // held sealed until the signed shim completes its confined projection.
        argv.drain(2..4);
        Ok(())
    }
    pub(crate) fn splice_pairs(&self, pairs: &mut [(RawFd, RawFd)]) -> io::Result<()> {
        root()?;
        self.roots.check()?;
        let replacements = self.descriptor_pairs();
        if replacements
            .iter()
            .any(|(_, target)| pairs.iter().filter(|(_, fd)| fd == target).count() != 1)
        {
            return Err(refused("Canonical original baseline descriptors differ"));
        }
        for (source, target) in replacements {
            pairs
                .iter_mut()
                .find(|(_, fd)| *fd == target)
                .expect("validated unique descriptor")
                .0 = source;
        }
        Ok(())
    }
}
impl Bootstrap {
    pub(super) fn admit(
        source: &File,
        uid: u32,
        gid: u32,
        groups: &[u32],
        read_claim_digest: [u8; 32],
    ) -> io::Result<Self> {
        root()?;
        let metadata = source.metadata()?;
        if !metadata.is_dir() || metadata.uid() != uid {
            return Err(refused("Original source owner differs from admitted facts"));
        }
        readonly(source)?;
        let facts = Facts {
            schema: 2,
            read_claim_digest,
            uid,
            gid,
            groups: groups.to_vec(),
            source: (metadata.dev(), metadata.ino()),
            namespaces: namespaces()?,
            roots: Vec::new(),
            baseline_filter: None,
            mounted_table_sha256: None,
        };
        facts.validate()?;
        let token = seal_facts(&facts)?;
        let result = Self {
            token,
            facts,
            roots: None,
            workspace: std::sync::Mutex::new(None),
        };
        result.check(source)?;
        Ok(result)
    }
    pub(super) fn check(&self, source: &File) -> io::Result<()> {
        root()?;
        self.facts.validate()?;
        let metadata = source.metadata()?;
        if !metadata.is_dir()
            || metadata.uid() != self.facts.uid
            || (metadata.dev(), metadata.ino()) != self.facts.source
            || namespaces()? != self.facts.namespaces
        {
            return Err(refused("Original-source admission changed before launch"));
        }
        readonly(source)?;
        check_token(&self.token)?;
        if let Some(roots) = &self.roots {
            roots.check()?;
        }
        if let Some(workspace) = self
            .workspace
            .lock()
            .map_err(|_| refused("Worker custody lock failed"))?
            .as_ref()
        {
            workspace.check()?;
        }
        Ok(())
    }
    pub(super) fn check_inventory_scratch(
        &self,
        scratch: &super::inventory_scratch::Scratch,
    ) -> io::Result<()> {
        if let Some(roots) = &self.roots {
            roots.check_inventory_scratch(scratch)?;
        }
        Ok(())
    }
    pub(super) fn with_roots(mut self, inputs: Vec<OriginalMountedInput>) -> io::Result<Self> {
        if self.roots.is_some() {
            return Err(refused("Original root inputs already attached"));
        }
        let roots = roots::Inputs::admit(inputs, self.facts.source.0)?;
        self.facts.roots = roots.projections();
        self.facts.validate()?;
        self.token = seal_facts(&self.facts)?;
        self.roots = Some(roots);
        Ok(self)
    }
    pub(super) fn filter_for_launch(&self, baseline: &OwnedFd) -> io::Result<Option<FilterLaunch>> {
        root()?;
        self.facts.validate()?;
        let retained = self
            .roots
            .as_ref()
            .map(roots::Inputs::projections)
            .unwrap_or_default();
        if retained != self.facts.roots {
            return Err(refused(
                "Original root custody differs from bootstrap facts",
            ));
        }
        if self.facts.baseline_filter.is_some() {
            return Err(refused("Original baseline is already attached"));
        }
        if self.facts.roots.is_empty() {
            return Ok(None);
        }
        if let Some(roots) = &self.roots {
            roots.check()?;
        }
        let roots = self
            .roots
            .as_ref()
            .ok_or_else(|| refused("Original root custody is absent"))?
            .prepare_launch()?;
        let filter = delayed_filter::Filter::capture(baseline)?;
        let mut facts = self.facts.clone();
        facts.roots = roots.projections();
        facts.baseline_filter = Some(filter.binding());
        facts.validate()?;
        Ok(Some(FilterLaunch {
            token: seal_facts(&facts)?,
            filter,
            roots,
        }))
    }
    pub(super) fn with_bound_roots(
        mut self,
        inputs: Vec<OriginalMountedInput>,
        binding: &OriginalMountedBinding,
    ) -> io::Result<Self> {
        if self.roots.is_some() || self.facts.read_claim_digest != binding.read_claim_digest {
            return Err(refused(
                "Protected mounted roots differ from original read claim",
            ));
        }
        let roots = roots::Inputs::admit_bound(inputs, self.facts.source.0, binding.table_sha256)?;
        self.facts.roots = roots.projections();
        self.facts.mounted_table_sha256 = Some(binding.table_sha256);
        self.facts.validate()?;
        self.token = seal_facts(&self.facts)?;
        self.roots = Some(roots);
        Ok(self)
    }
    pub(super) fn root_pairs(&self) -> Vec<(RawFd, RawFd)> {
        self.roots
            .as_ref()
            .map(roots::Inputs::pairs)
            .unwrap_or_default()
    }
    pub(super) fn prepare_workspace(&self, root: &std::path::Path) -> io::Result<()> {
        let mut held = self
            .workspace
            .lock()
            .map_err(|_| refused("Worker custody lock failed"))?;
        if held.is_some() {
            return Err(refused("Worker workspace was already prepared"));
        }
        *held = Some(workspace::prepare(root, &self.facts)?);
        Ok(())
    }
    pub(super) fn sandbox_account_identity(&self) -> (u32, u32) {
        (self.facts.uid, self.facts.gid)
    }
    pub(super) fn read_claim_digest(&self) -> [u8; 32] {
        self.facts.read_claim_digest
    }
    pub(super) fn descriptor(&self) -> RawFd {
        self.token.as_raw_fd()
    }
    pub(super) fn splice(&self, argv: &mut Vec<String>) -> io::Result<()> {
        root()?;
        let pos = argv
            .iter()
            .position(|a| a == "--unshare-all")
            .ok_or_else(|| refused("Canonical namespace plan is absent"))?;
        if argv
            .iter()
            .any(|a| a == "--share-net" || a == "--unshare-user" || a == "--uid" || a == "--gid")
        {
            return Err(refused("Original-source namespace plan was changed"));
        }
        // Preserve numeric source credentials in the ORIGINAL user namespace.
        // Mount/PID/network/IPC isolation remains mandatory. Only the signed
        // shim receives SETUID/SETGID, which it clears before executing worker.
        argv.splice(
            pos..pos + 1,
            [
                "--unshare-pid",
                "--unshare-net",
                "--unshare-ipc",
                "--unshare-uts",
                "--unshare-cgroup-try",
                "--cap-drop",
                "ALL",
                "--cap-add",
                "CAP_SETUID",
                "--cap-add",
                "CAP_SETGID",
            ]
            .map(String::from),
        );
        if self.roots.is_some() {
            // Signed root shim only, cleared with all capabilities before worker.
            let capability = argv
                .iter()
                .position(|arg| arg == "CAP_SETGID")
                .ok_or_else(|| refused("Original credential capabilities are absent"))?;
            argv.splice(
                capability + 1..capability + 1,
                ["--cap-add".into(), "CAP_SYS_ADMIN".into()],
            );
        }
        let pos = argv
            .iter()
            .position(|a| a == "__sandbox-exec")
            .ok_or_else(|| refused("Signed sandbox shim is absent"))?;
        argv.splice(
            pos + 1..pos + 1,
            ["--original-source-fd".into(), BOOTSTRAP_FD.to_string()],
        );
        // These are empty, root-owned directories in bwrap's fresh filesystem,
        // created BEFORE any data or host binds. Private data copies otherwise
        // make their synthetic parents0700, blocking the later source UID.
        // No host path, original source leaf or delegated app leaf is chmodded.
        let mut traversal = Vec::new();
        for path in [
            "/etc",
            "/etc/alternatives",
            "/run/tilo-runtime",
            "/var",
            "/var/tilo-apps",
            "/usr/share",
            "/usr/share/X11",
        ] {
            traversal.extend(["--perms".into(), "0755".into(), "--dir".into(), path.into()]);
        }
        argv.splice(0..0, traversal);
        Ok(())
    }
}
fn root() -> io::Result<()> {
    super::check_credentials(0)
}
fn namespaces() -> io::Result<Vec<(u64, u64)>> {
    NS.iter()
        .map(|name| {
            let metadata = std::fs::metadata(format!("/proc/self/ns/{name}"))?;
            Ok((metadata.dev(), metadata.ino()))
        })
        .collect()
}
fn readonly(directory: &File) -> io::Result<()> {
    let mut flags: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatvfs(directory.as_raw_fd(), &raw mut flags) } < 0 {
        return Err(io::Error::last_os_error());
    }
    if flags.f_flag & libc::ST_RDONLY == 0 {
        return Err(refused(
            "Original source projection is not kernel read-only",
        ));
    }
    Ok(())
}
fn check_token(token: &File) -> io::Result<()> {
    let metadata = token.metadata()?;
    let seals = unsafe { libc::fcntl(token.as_raw_fd(), libc::F_GET_SEALS) };
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.nlink() != 0
        || metadata.len() > MAX_BYTES
        || seals < 0
        || seals & SEALS != SEALS
    {
        return Err(refused(
            "Original-source token is not immutable root custody",
        ));
    }
    Ok(())
}
fn receive() -> io::Result<Facts> {
    root()?;
    let raw = unsafe { libc::fcntl(BOOTSTRAP_FD, libc::F_DUPFD_CLOEXEC, 128) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut token = unsafe { File::from_raw_fd(raw) };
    check_token(&token)?;
    token.rewind()?;
    let mut bytes = Vec::new();
    token.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(refused("Original-source token exceeds bound"));
    }
    let facts: Facts = serde_json::from_slice(&bytes)?;
    facts.validate()?;
    Ok(facts)
}
fn validate_namespace(facts: &Facts) -> io::Result<()> {
    for ((name, original), current) in NS.iter().zip(&facts.namespaces).zip(namespaces()?) {
        let changed = current != *original;
        if (*name == "user" && changed) || (*name != "user" && !changed) {
            return Err(refused(
                "Original credentials require completed private namespaces",
            ));
        }
    }
    // O_PATH performs only identity/readonly metadata checks. Root does not
    // read or traverse source contents before the confined credential drop.
    let source = File::options()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(super::SOURCE_PATH)?;
    let metadata = source.metadata()?;
    if !metadata.is_dir()
        || metadata.uid() != facts.uid
        || (metadata.dev(), metadata.ino()) != facts.source
    {
        return Err(refused("Projected source differs from admitted held Home"));
    }
    readonly(&source)
}
/// Signed shim only: namespace reattestation and fd closure precede source
/// credential application. Ambient parent credentials never change.
pub(crate) fn enter(keep: &mut Vec<i32>) -> io::Result<()> {
    let facts = phase(Stage::ReceiveBootstrap, receive())?;
    phase(Stage::VerifyNamespaceAndHome, validate_namespace(&facts))?;
    if keep.contains(&BOOTSTRAP_FD)
        || keep.contains(&CLAIM_FD)
        || keep.contains(&crate::services::BASELINE_SECCOMP_CHILD_FD)
    {
        return Err(refused("Bootstrap cannot reach worker"));
    }
    if keep.iter().any(|fd| roots::reserved_fd(*fd)) {
        return Err(refused("Original root descriptors cannot reach worker"));
    }
    let filter = facts
        .projection_filter_binding()?
        .map(|binding| {
            phase(
                Stage::ReceiveBaseline,
                delayed_filter::Filter::receive(binding),
            )
        })
        .transpose()?;
    if !facts.roots.is_empty() {
        phase(
            Stage::ProjectMountedRoots,
            credentials::with_source_filesystem_access(&facts, |access| {
                roots::project(&facts.roots, access)
            }),
        )?;
    }
    // No worker, claim or source credential drop is reachable before the
    // complete canonical baseline is installed on all shim threads.
    if let Some(filter) = filter {
        phase(Stage::InstallBaseline, filter.install())?;
    }
    let observed = if facts.mounted_table_sha256.is_some() {
        phase(
            Stage::ObserveMountedRoots,
            credentials::with_source_filesystem_access(&facts, |_access| roots::observe(&facts)),
        )?
    } else {
        None
    };
    phase(
        Stage::CloseAmbientDescriptors,
        crate::landlock_shim::close_ambient_fds(keep),
    )?;
    phase(Stage::PublishMountedReceipt, roots::publish(observed))?;
    if facts.mounted_table_sha256.is_some() {
        keep.push(51);
    }
    phase(Stage::PublishClaim, publish_claim(facts.read_claim_digest))?;
    keep.push(CLAIM_FD);
    phase(Stage::DropCredentials, credentials::drop_to(&facts))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn original_source_shim_refuses_ambient_namespace_before_credential_change() {
        let before = (unsafe { libc::getuid() }, unsafe { libc::getgid() });
        let facts = Facts {
            schema: 2,
            read_claim_digest: [7; 32],
            uid: 1000,
            gid: 1001,
            groups: vec![1002],
            source: (0, 0),
            namespaces: namespaces().unwrap(),
            roots: Vec::new(),
            baseline_filter: None,
            mounted_table_sha256: None,
        };
        assert!(validate_namespace(&facts).is_err());
        assert_eq!(
            before,
            (unsafe { libc::getuid() }, unsafe { libc::getgid() })
        );
    }
    #[test]
    fn original_source_token_refuses_named_or_unsealed_input() {
        let ordinary = tempfile::tempfile().unwrap();
        assert!(check_token(&ordinary).is_err());
        let raw =
            unsafe { libc::memfd_create(c"invalid-original-source".as_ptr(), libc::MFD_CLOEXEC) };
        assert!(raw >= 0);
        let unsealed = unsafe { File::from_raw_fd(raw) };
        assert!(check_token(&unsealed).is_err());
    }
}

// Only the already-verified signed shim emits this digest-only descriptor.
fn publish_claim(digest: [u8; 32]) -> io::Result<()> {
    let raw = unsafe {
        libc::memfd_create(
            c"tilo-original-claim".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut token = unsafe { File::from_raw_fd(raw) };
    token.write_all(&digest)?;
    token.rewind()?;
    if unsafe { libc::fcntl(raw, libc::F_ADD_SEALS, SEALS) } < 0 {
        return Err(io::Error::last_os_error());
    }
    if raw == CLAIM_FD {
        if unsafe { libc::fcntl(raw, libc::F_SETFD, 0) } < 0 {
            return Err(io::Error::last_os_error());
        }
        std::mem::forget(token);
    } else if unsafe { libc::dup3(raw, CLAIM_FD, 0) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
/// Compare an original request with the digest inherited from the signed shim.
/// A serialized claim, environment variable or ordinary file cannot authorize it.
pub fn verify_original_read_claim(expected: [u8; 32]) -> io::Result<()> {
    if expected == [0; 32] {
        return Err(refused("Original read claim is empty"));
    }
    let raw = unsafe { libc::fcntl(CLAIM_FD, libc::F_DUPFD_CLOEXEC, 128) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let token = unsafe { File::from_raw_fd(raw) };
    verify_claim_token(&token, expected)
}
fn verify_claim_token(token: &File, expected: [u8; 32]) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    check_token(token)?;
    if token.metadata()?.len() != 32 {
        return Err(refused("Original read claim length differs"));
    }
    let mut actual = [0; 32];
    token.read_exact_at(&mut actual, 0)?;
    if actual != expected {
        return Err(refused("Original read request differs from root admission"));
    }
    Ok(())
}

#[cfg(test)]
mod claim_tests {
    use super::*;
    #[test]
    fn original_read_claim_refuses_named_and_unsealed_descriptors() {
        let mut ordinary = tempfile::tempfile().unwrap();
        ordinary.write_all(&[7; 32]).unwrap();
        assert!(verify_claim_token(&ordinary, [7; 32]).is_err());
        let raw = unsafe { libc::memfd_create(c"unsealed-claim".as_ptr(), libc::MFD_CLOEXEC) };
        assert!(raw >= 0);
        let mut unsealed = unsafe { File::from_raw_fd(raw) };
        unsealed.write_all(&[7; 32]).unwrap();
        assert!(verify_claim_token(&unsealed, [7; 32]).is_err());
    }
}

fn seal_facts(facts: &Facts) -> io::Result<File> {
    let bytes = serde_json::to_vec(&facts)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(refused("Original-source token exceeds bound"));
    }
    // No named path can replace this root-authored inherited descriptor.
    let raw = unsafe {
        libc::memfd_create(
            c"tilo-original-source".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut token = unsafe { File::from_raw_fd(raw) };
    token.write_all(&bytes)?;
    token.rewind()?;
    if unsafe { libc::fcntl(token.as_raw_fd(), libc::F_ADD_SEALS, SEALS) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(token)
}
