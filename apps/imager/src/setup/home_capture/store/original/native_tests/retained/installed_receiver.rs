//! Root-only producer attachment to the existing owned receiver namespace.
//! Public observations never construct a live retained source authority.
use super::*;
use serde::Deserialize;
use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::os::fd::FromRawFd;
use std::os::unix::fs::OpenOptionsExt;
const SLOT: i32 = 87;
const MARKER: &str = "/run/tilo-retained-session-source-qualification";
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Guard {
    schema: u32,
    receiver_sha256: String,
    bridge_sha256: String,
    #[serde(default)]
    registration_main_sha256: Option<String>,
}
pub(super) struct Receiver {
    stream: UnixStream,
    _receiver: File,
    _namespace: File,
    registration_main: Option<Arc<crate::setup::qualification_registration::AdmittedMain>>,
}
pub(super) struct SelectedPackage {
    _directory: tempfile::TempDir,
    pub(super) sealed: PathBuf,
    pub(super) review: PreparedSetupReview,
}
fn denied() -> std::io::Error {
    std::io::ErrorKind::PermissionDenied.into()
}
fn hash(file: &File) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::FileExt;
    let before = file.metadata()?;
    if !before.is_file()
        || before.uid() != 0
        || before.mode() & 0o022 != 0
        || before.len() == 0
        || before.len() > 512 * 1024 * 1024
    {
        return Err(denied());
    }
    let mut h = Sha256::new();
    let mut buffer = [0; 65536];
    let mut offset = 0;
    loop {
        let n = file.read_at(&mut buffer, offset)?;
        if n == 0 {
            break;
        }
        offset += n as u64;
        if offset > before.len() {
            return Err(denied());
        }
        h.update(&buffer[..n]);
    }
    let after = file.metadata()?;
    if offset != before.len()
        || (
            before.dev(),
            before.ino(),
            before.len(),
            before.mtime(),
            before.mtime_nsec(),
            before.ctime(),
            before.ctime_nsec(),
        ) != (
            after.dev(),
            after.ino(),
            after.len(),
            after.mtime(),
            after.mtime_nsec(),
            after.ctime(),
            after.ctime_nsec(),
        )
    {
        return Err(denied());
    }
    Ok(hex::encode(h.finalize()))
}
impl Receiver {
    pub(super) fn from_inherited() -> std::io::Result<Option<Self>> {
        let Some(slot) = std::env::var_os("TILO_RETAINED_RECEIVER_CONTROL_FD") else {
            return Ok(None);
        };
        if slot != "87"
            || rustix::process::getuid().as_raw() != 0
            || rustix::process::geteuid().as_raw() != 0
            || unsafe { libc::getppid() } != 1
        {
            return Err(denied());
        }
        for fd in [SLOT, 88, 89] {
            if unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
                return Err(denied());
            }
        }
        // SAFETY: closed qualification bootstrap transfers these reserved slots once.
        let (stream, receiver, namespace) = unsafe {
            (
                UnixStream::from_raw_fd(SLOT),
                File::from_raw_fd(88),
                File::from_raw_fd(89),
            )
        };
        for fd in [SLOT, 88, 89] {
            if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        let mut peer: libc::ucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&raw mut peer).cast(),
                &raw mut len,
            )
        } != 0
            || len as usize != std::mem::size_of::<libc::ucred>()
            || peer.pid != 1
            || peer.uid != 0
            || peer.gid != 0
        {
            return Err(denied());
        }
        if unsafe { libc::ioctl(namespace.as_raw_fd(), 0xb703) } != libc::CLONE_NEWPID {
            return Err(denied());
        }
        let actual = File::open("/proc/self/ns/pid")?.metadata()?;
        let held = namespace.metadata()?;
        if (actual.dev(), actual.ino()) != (held.dev(), held.ino()) {
            return Err(denied());
        }
        let marker = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(MARKER)?;
        let facts = marker.metadata()?;
        if !facts.is_file()
            || facts.uid() != 0
            || facts.mode() & 0o7777 != 0o444
            || facts.nlink() != 1
            || facts.len() > 512
        {
            return Err(denied());
        }
        let mut bytes = Vec::new();
        marker.take(513).read_to_end(&mut bytes)?;
        let guard: Guard = serde_json::from_slice(&bytes).map_err(std::io::Error::other)?;
        if guard.schema != 1
            || hash(&receiver)? != guard.receiver_sha256
            || hash(&File::open("/proc/self/exe")?)? != guard.bridge_sha256
            || hash(&File::open("/proc/1/exe")?)? != guard.receiver_sha256
        {
            return Err(denied());
        }
        stream.set_read_timeout(Some(std::time::Duration::from_secs(120)))?;
        stream.set_write_timeout(Some(std::time::Duration::from_secs(10)))?;
        let registration_main = guard
            .registration_main_sha256
            .as_ref()
            .map(|_| crate::setup::qualification_registration::AdmittedMain::from_receiver())
            .transpose()?;
        Ok(Some(Self {
            stream,
            _receiver: receiver,
            _namespace: namespace,
            registration_main,
        }))
    }
    pub(super) fn has_selected_registration(&self) -> bool {
        self.registration_main.is_some()
    }
    /// The receiver owns source PAM/capture and sends this capsule only after
    /// observing source account, service and proof cleanup. The package itself
    /// still passes the normal password, protected declaration and publication
    /// checks below; this transport never constructs an installed authority.
    pub(super) fn selected_package(&mut self, home: &Path) -> std::io::Result<SelectedPackage> {
        self.registration_main
            .as_ref()
            .ok_or_else(denied)?
            .check()?;
        let original = PinnedDirectory::open_absolute(home)?;
        runtime_portal::fd_transport::send_with_fd(
            self.stream.as_fd(),
            &[75],
            original.file.as_fd(),
        )?;
        let mut ack = [0];
        self.stream.read_exact(&mut ack)?;
        if ack != [75] {
            return Err(denied());
        }
        drop(original);
        let (tag, disk) = runtime_portal::fd_transport::recv_with_fd(self.stream.as_fd())?;
        if tag != [74] {
            return Err(denied());
        }
        let disk = File::from(disk.ok_or_else(denied)?);
        let installation = super::native_staging::review(&disk, home)?;
        let bytes = serde_json::to_vec(&installation).map_err(std::io::Error::other)?;
        if bytes.len() > 65536 {
            return Err(denied());
        }
        self.stream.write_all(&[77])?;
        self.stream.write_all(&(bytes.len() as u32).to_le_bytes())?;
        self.stream.write_all(&bytes)?;
        drop(disk);
        self.stream
            .set_read_timeout(Some(std::time::Duration::from_secs(1200)))?;
        let (tag, descriptor) = runtime_portal::fd_transport::recv_with_fd(self.stream.as_fd())?;
        if tag != [76] {
            return Err(denied());
        }
        let source = File::from(descriptor.ok_or_else(denied)?);
        let before = source.metadata()?;
        const LIMIT: u64 = 32 * 1024 * 1024 + 16384;
        if !before.is_file()
            || before.uid() != 0
            || before.mode() & 0o077 != 0
            || before.nlink() != 1
            || !(48..=LIMIT).contains(&before.len())
        {
            return Err(denied());
        }
        let directory = tempfile::Builder::new()
            .prefix("tilo-selected-capsule-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in("/var/tmp")?;
        let sealed = directory.path().join("prepared.sealed");
        let mut target = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&sealed)?;
        use std::os::unix::fs::FileExt;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        let mut offset = 0;
        let mut bytes = [0; 65536];
        loop {
            if std::time::Instant::now() >= deadline {
                return Err(std::io::ErrorKind::TimedOut.into());
            }
            let n = source.read_at(&mut bytes, offset)?;
            if n == 0 {
                break;
            }
            offset += n as u64;
            if offset > before.len() {
                return Err(denied());
            }
            target.write_all(&bytes[..n])?;
        }
        let after = source.metadata()?;
        let stamp = |m: &std::fs::Metadata| {
            (
                m.dev(),
                m.ino(),
                m.len(),
                m.uid(),
                m.gid(),
                m.mode(),
                m.mtime(),
                m.mtime_nsec(),
                m.ctime(),
                m.ctime_nsec(),
            )
        };
        if offset != before.len() || stamp(&before) != stamp(&after) {
            return Err(denied());
        }
        target.sync_all()?;
        drop(target);
        let review = PreparedSetupReview::open(
            &sealed,
            super::PASSWORD,
            &directory.path().join("quarantine"),
            &AtomicBool::new(false),
        )?;
        if review
            .manifest()
            .pending_computer_applications_scope()?
            .is_none()
            || review.manifest().portable_home.is_some()
            || review.manifest().completed_home.is_some()
            || review.manifest().portable_flatpak.is_some()
            || review.manifest().completed_applications_only
        {
            return Err(denied());
        }
        self.stream.write_all(&[76])?;
        Ok(SelectedPackage {
            _directory: directory,
            sealed,
            review,
        })
    }
    pub(super) fn publish(
        mut self,
        review: &PreparedSetupReview,
        identity_public_key: &[u8; 32],
        plan: &Plan,
        sealed: &Path,
        password: &[u8],
    ) -> std::io::Result<()> {
        // The namespace owner provides a fresh canonical target. Publish and
        // reopen through production paths; never substitute open_fixture.
        review.publish_enrolled_system(Path::new("/var/lib/tilo"), password)?;
        let installed =
            prepared_setup::AuthenticatedInstalledRetained::open_installed(&plan.operation_id)?;
        if installed.manifest().identity_public_key != *identity_public_key
            || installed
                .manifest()
                .retained_home
                .as_ref()
                .is_none_or(|p| p.as_plan() != plan)
        {
            return Err(denied());
        }
        installed.revalidate()?;
        let package = File::open(sealed)?;
        let capsule_sha = hash(&package)?;
        let report=serde_json::to_vec(&serde_json::json!({"schema":1,"operation_id":plan.operation_id,"identity_public_key":*identity_public_key,"setup_binding":review.manifest().setup_binding,"source_plan":serde_json::from_slice::<serde_json::Value>(&plan.bytes()?).map_err(std::io::Error::other)?,"sealed_bytes":package.metadata()?.len(),"ciphertext_sha256":capsule_sha,"document":"receiver-document.txt"})).map_err(std::io::Error::other)?;
        if report.len() > 65536 {
            return Err(denied());
        }
        self.stream
            .write_all(&(report.len() as u32).to_le_bytes())?;
        self.stream.write_all(&report)?;
        runtime_portal::fd_transport::send_with_fd(self.stream.as_fd(), &[83], package.as_fd())?;
        let mut ack = [0];
        self.stream.read_exact(&mut ack)?;
        if ack != [83] {
            return Err(denied());
        }
        // ACK83 is transport only. Keep this source fixture's actual stack,
        // protected records and capsule custody alive for the whole journey.
        // The original namespace init sends final84 only after its normal
        // action/status and source-owner retirement observations complete.
        // Lost transport cannot prove that completion: retain custody until
        // the outer owned PID namespace is actually retired.
        installed.revalidate()?;
        loop {
            match self.stream.read_exact(&mut ack) {
                Ok(()) if ack == [84] => break,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    continue;
                }
                Ok(()) | Err(_) => loop {
                    std::thread::park_timeout(std::time::Duration::from_millis(50));
                },
            }
        }
        installed.revalidate()?;
        // Normal app/source owners must actually be gone before qualification
        // opens a fresh observation epoch. Do not mistake a killed Shell for
        // observed mount retirement or inspect a stale old pathname.
        let source_partition = partition(plan);
        let daemon = block_devices::udisks::Udisks::system().map_err(std::io::Error::other)?;
        if !daemon
            .filesystem(&source_partition)
            .and_then(|proxy| proxy.get_property::<Vec<Vec<u8>>>("MountPoints"))
            .map_err(std::io::Error::other)?
            .is_empty()
        {
            loop {
                std::thread::park_timeout(std::time::Duration::from_millis(50));
            }
        }
        let authority = RetainedSourceAuthority::review_installed(
            Arc::clone(&installed),
            identity_public_key,
            &AtomicBool::new(false),
        )?;
        let projection = authority.projection(&AtomicBool::new(false))?;
        projection.revalidate(&AtomicBool::new(false))?;
        runtime_portal::fd_transport::send_with_fd(
            self.stream.as_fd(),
            &[85],
            projection.directory().as_fd(),
        )?;
        // UID1000 receiver alone opens and reads the leaf. Root neither reads
        // unreadable content nor lends an original-user app Write capability.
        loop {
            match self.stream.read_exact(&mut ack) {
                Ok(()) if ack == [85] => break,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    continue;
                }
                Ok(()) | Err(_) => loop {
                    std::thread::park_timeout(std::time::Duration::from_millis(50));
                },
            }
        }
        projection.revalidate(&AtomicBool::new(false))?;
        authority.revoke();
        drop(projection);
        authority.retire()?;
        if !daemon
            .filesystem(&source_partition)
            .and_then(|proxy| proxy.get_property::<Vec<Vec<u8>>>("MountPoints"))
            .map_err(std::io::Error::other)?
            .is_empty()
        {
            loop {
                std::thread::park_timeout(std::time::Duration::from_millis(50));
            }
        }
        self.stream.write_all(&[84])?;
        Ok(())
    }
}
