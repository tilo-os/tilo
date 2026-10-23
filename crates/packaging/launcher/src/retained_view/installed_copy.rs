//! Installed original-copy archives on the existing Shell/root-helper gate.
//! Public declarations are shape only. The fixed root helper must retain the
//! independently reviewed operation authority and quiescent output transaction.
//! No mounted source, block device or writable archive is delivered to the user.
use super::{
    Arc, AsRawFd, Duration, ExecutingPeer, File, FromRawFd, Handoff, LIMIT, MetadataExt, OwnedFd,
    RetainedLaunchGate, RootConsumerChannel, RootRole, SEALS, ServiceAuthority,
    ServiceCancellation, UnixStream, Write, check_executing_image, io, own_helper,
    protected_helper, protected_program, receive_fd, receive_json, receive_json_bound, refused,
    send_json,
};
use std::os::fd::AsFd;

static UNKNOWN_DELIVERY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ArchiveFacts {
    dev: u64,
    ino: u64,
    len: u64,
    mtime: [i64; 2],
    ctime: [i64; 2],
}
impl ArchiveFacts {
    fn measure(file: &File) -> io::Result<Self> {
        let metadata = file.metadata()?;
        let flags = rustix::fs::fcntl_getfl(file)?;
        if !metadata.is_file()
            || metadata.uid() != 0
            || metadata.mode() & 0o777 != 0o600
            || metadata.nlink() != 1
            || metadata.len() == 0
            || flags.contains(rustix::fs::OFlags::PATH)
            || flags.intersection(rustix::fs::OFlags::ACCMODE) != rustix::fs::OFlags::RDONLY
        {
            return Err(refused(
                "installed Copy archive is not private read-only output",
            ));
        }
        Ok(Self {
            dev: metadata.dev(),
            ino: metadata.ino(),
            len: metadata.len(),
            mtime: [metadata.mtime(), metadata.mtime_nsec()],
            ctime: [metadata.ctime(), metadata.ctime_nsec()],
        })
    }
    fn matches(&self, file: &File) -> io::Result<()> {
        if &Self::measure(file)? != self {
            return Err(refused("installed Copy held archive changed"));
        }
        Ok(())
    }
}

/// Operation metadata selected by the protected root transaction. `payload`
/// carries the canonical authenticated archive expectations; it is not a source
/// grant and must be interpreted by the installed receiver, not the launcher.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CopyDeclaration {
    pub operation: String,
    pub setup_binding: String,
    pub person: [u8; 32],
    pub read_claim: [u8; 32],
    pub target_uid: u32,
    pub payload: Vec<u8>,
}
impl CopyDeclaration {
    fn validate(&self) -> io::Result<()> {
        if uuid::Uuid::parse_str(&self.operation)
            .map_err(io::Error::other)?
            .to_string()
            != self.operation
            || self.setup_binding.is_empty()
            || self.setup_binding.len() > 256
            || self.setup_binding.chars().any(char::is_control)
            || self.person == [0; 32]
            || self.read_claim == [0; 32]
            || self.target_uid == 0
            || self.payload.is_empty()
            || self.payload.len() > 8192
        {
            return Err(refused("installed Copy declaration is incomplete"));
        }
        Ok(())
    }
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct WireReceipt {
    schema: u32,
    declaration: CopyDeclaration,
    home: Option<ArchiveFacts>,
    applications: Option<ArchiveFacts>,
}
impl WireReceipt {
    fn validate(&self) -> io::Result<()> {
        self.declaration.validate()?;
        if self.schema != 1 || (self.home.is_none() && self.applications.is_none()) {
            return Err(refused("installed Copy receipt purpose differs"));
        }
        Ok(())
    }
}

fn check_secret(file: &File) -> io::Result<()> {
    let metadata = file.metadata()?;
    let seals = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) };
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.mode() & 0o777 != 0o600
        || metadata.nlink() != 0
        || !(16..=128).contains(&metadata.len())
        || flags < 0
        || flags & libc::O_PATH != 0
        || flags & libc::O_ACCMODE != libc::O_RDONLY
        || seals < 0
        || seals & SEALS != SEALS
    {
        return Err(refused(
            "installed Copy secret lacks sealed anonymous root custody",
        ));
    }
    Ok(())
}

fn peer_stopped(peer: &ExecutingPeer) -> io::Result<bool> {
    let mut poll = libc::pollfd {
        fd: peer.pidfd().as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let count = unsafe { libc::poll(&raw mut poll, 1, 0) };
    if count < 0 {
        return Err(io::Error::last_os_error());
    }
    if poll.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
        return Err(refused("Copy receiver kernel death cannot be observed"));
    }
    Ok(count > 0 && poll.revents & libc::POLLIN != 0)
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CopyRecipientObservation {
    pub pid: u32,
    pub start: u64,
    pub helper_dev: u64,
    pub helper_ino: u64,
}
/// Endorsed by the real pinned Shell on its already authenticated root channel.
/// This is actual owned-child selection, distinct from the owner's person proof.
pub struct CopyRecipient {
    shell: Arc<ExecutingPeer>,
    shell_image: File,
    child_pidfd: OwnedFd,
    expected: CopyRecipientObservation,
}
impl CopyRecipient {
    fn check(&self, peer: &ExecutingPeer) -> io::Result<()> {
        check_executing_image(&self.shell, &self.shell_image)?;
        let current = protected_program("/usr/bin/shell")?.metadata()?;
        let held = self.shell_image.metadata()?;
        let helper = protected_helper()?;
        let metadata = helper.metadata()?;
        if (current.dev(), current.ino()) != (held.dev(), held.ino())
            || self.expected.pid < 2
            || self.expected.start == 0
            || peer.uid() != self.shell.uid()
            || peer.pid() != self.expected.pid
            || peer.start_time()? != self.expected.start
            || !peer.belongs_to(self.shell.pid(), self.shell.start_time()?)
            || (metadata.dev(), metadata.ino())
                != (self.expected.helper_dev, self.expected.helper_ino)
        {
            return Err(refused(
                "installed Copy recipient is not Shell's exact owned importer",
            ));
        }
        peer.matches_transferred_pidfd(std::os::fd::AsFd::as_fd(&self.child_pidfd))?;
        check_executing_image(peer, &helper)
    }
}
impl RootConsumerChannel {
    /// # Errors
    /// Refuses a non-helper caller or changed pinned Shell, importer identity,
    /// executable or transferred pidfd.
    pub fn verify_installed_copy_recipient(&self, recipient: &CopyRecipient) -> io::Result<()> {
        own_helper()?;
        recipient.check(&self.peer)
    }
    /// Cleanup-only shutdown of this already authenticated private endpoint.
    pub fn stop_installed_copy_transport(&mut self) -> io::Result<()> {
        own_helper()?;
        self.stream.shutdown(std::net::Shutdown::Both)
    }
    /// # Errors
    /// Refuses a retired owner or non-helper caller; reports cancellation,
    /// serialization failures and failed or timed-out status transfers.
    pub fn send_installed_copy_status(
        &mut self,
        event: &CopyOwnerEvent,
        cancellation: &ServiceCancellation,
    ) -> io::Result<()> {
        own_helper()?;
        if !self.peer.current() {
            return Err(refused("installed Copy owner retired"));
        }
        let mut io = Handoff::new(&mut self.stream, cancellation, Duration::from_secs(10))?;
        send_json(&mut io, event)?;
        if !self.peer.current() {
            return Err(refused("installed Copy owner retired"));
        }
        Ok(())
    }
    /// # Errors
    /// Refuses non-helper callers, retired owners or budgets above one second;
    /// reports polling, cancellation, framing and command decoding errors.
    pub fn receive_installed_copy_command(
        &mut self,
        budget: Duration,
        cancellation: &ServiceCancellation,
    ) -> io::Result<Option<CopyOwnerCommand>> {
        own_helper()?;
        if budget > Duration::from_secs(1) || !self.peer.current() {
            return Err(refused("installed Copy owner command unavailable"));
        }
        let mut fd = libc::pollfd {
            fd: self.stream.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let count = unsafe {
            libc::poll(
                &raw mut fd,
                1,
                i32::try_from(budget.as_millis()).map_err(io::Error::other)?,
            )
        };
        if count < 0 {
            return Err(io::Error::last_os_error());
        }
        if !self.peer.current() {
            return Err(refused("installed Copy owner retired"));
        }
        if count == 0 {
            return Ok(None);
        }
        let mut io = Handoff::new(&mut self.stream, cancellation, Duration::from_secs(10))?;
        let command = receive_json_bound(&mut io, LIMIT)?;
        if !self.peer.current() {
            return Err(refused("installed Copy owner retired"));
        }
        Ok(Some(command))
    }
    /// Seals the already protected operation secret for the private receiver.
    /// No source, recipient or delivery authority is minted by this helper.
    /// # Errors
    /// Refuses non-helper callers or invalid secret lengths; reports failures
    /// creating, protecting, writing, sealing or reopening the anonymous file.
    pub fn seal_installed_copy_secret(secret: &[u8]) -> io::Result<File> {
        own_helper()?;
        if !(32..=128).contains(&secret.len()) {
            return Err(refused("installed Copy secret size invalid"));
        }
        let raw = unsafe {
            libc::memfd_create(
                c"tilo-installed-copy-secret".as_ptr(),
                libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
            )
        };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut file = unsafe { File::from_raw_fd(raw) };
        if unsafe { libc::fchmod(file.as_raw_fd(), 0o600) } < 0 {
            return Err(io::Error::last_os_error());
        }
        file.write_all(secret)?;
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, SEALS) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let readonly = File::open(format!("/proc/self/fd/{}", file.as_raw_fd()))?;
        check_secret(&readonly)?;
        Ok(readonly)
    }
    /// Receives only the actual private importer endpoint endorsed by the
    /// independently authenticated Shell owner. A path/PID is never an endpoint.
    /// # Errors
    /// Refuses retired owners or endpoints that are not connected Unix streams;
    /// reports cancellation, malformed handoffs, socket and acknowledgment failures.
    pub fn receive_installed_copy_endpoint(
        &mut self,
        cancellation: &ServiceCancellation,
    ) -> io::Result<UnixStream> {
        own_helper()?;
        if !self.peer.current() {
            return Err(refused("installed Copy owner retired"));
        }
        let mut io = Handoff::new(&mut self.stream, cancellation, Duration::from_secs(30))?;
        let (bytes, descriptor) = io.retry(libc::POLLIN, |stream| {
            runtime_portal::fd_transport::recv_with_fd(stream.as_fd())
        })?;
        if bytes != [84] {
            return Err(refused("installed Copy endpoint order differs"));
        }
        let endpoint =
            UnixStream::from(descriptor.ok_or_else(|| refused("installed Copy endpoint missing"))?);
        let mut kind: libc::c_int = 0;
        let mut size =
            libc::socklen_t::try_from(std::mem::size_of_val(&kind)).map_err(io::Error::other)?;
        if unsafe {
            libc::getsockopt(
                endpoint.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_TYPE,
                (&raw mut kind).cast(),
                &raw mut size,
            )
        } != 0
            || kind != libc::SOCK_STREAM
        {
            return Err(refused(
                "installed Copy endpoint is not a connected Unix stream",
            ));
        }
        endpoint.peer_addr()?;
        io.write_all(&[84])?;
        if !self.peer.current() {
            return Err(refused("installed Copy owner retired"));
        }
        Ok(endpoint)
    }
}

impl RootConsumerChannel {
    /// `owner` is the actual Shell peer from the independently certified person
    /// proof. This channel's QPE must match that task, not merely its UID.
    /// # Errors
    /// Refuses mismatched or retired Shell owners and unpinned executables;
    /// reports cancellation and malformed or failed recipient/pidfd transfers.
    pub fn receive_installed_copy_recipient(
        &mut self,
        owner: &ExecutingPeer,
        cancellation: &ServiceCancellation,
    ) -> io::Result<CopyRecipient> {
        own_helper()?;
        if self.peer.pid() != owner.pid()
            || self.peer.start_time()? != owner.start_time()?
            || self.peer.uid() != owner.uid()
            || !owner.current()
        {
            return Err(refused(
                "Copy recipient endorsement differs from certified Shell",
            ));
        }
        let shell_image = protected_program("/usr/bin/shell")?;
        check_executing_image(&self.peer, &shell_image)?;
        let (expected, child_pidfd) = {
            let mut io = Handoff::new(&mut self.stream, cancellation, Duration::from_secs(10))?;
            let expected = receive_json_bound(&mut io, 4096)?;
            let pidfd = receive_fd(&mut io, 83)?;
            (expected, pidfd)
        };
        check_executing_image(&self.peer, &shell_image)?;
        if !owner.current() {
            return Err(refused("Copy owner stopped"));
        }
        Ok(CopyRecipient {
            shell: self.peer.clone(),
            shell_image,
            child_pidfd,
            expected,
        })
    }
}

struct IssueCustody {
    channel: RootConsumerChannel,
    recipient: CopyRecipient,
    authority: Arc<dyn ServiceAuthority>,
    receipt: WireReceipt,
    home: Option<File>,
    applications: Option<File>,
    secret: File,
    cancellation: ServiceCancellation,
    possible_delivery: bool,
    delivered: bool,
    unusable: bool,
}
impl IssueCustody {
    fn check(&self) -> io::Result<()> {
        own_helper()?;
        self.authority.check()?;
        if self.cancellation.is_cancelled() || !self.channel.peer.current() || self.unusable {
            return Err(refused("installed Copy peer or authority retired"));
        }
        self.recipient.check(&self.channel.peer)?;
        self.receipt.validate()?;
        if self.channel.peer.uid() != self.receipt.declaration.target_uid {
            return Err(refused("installed Copy receiver owner differs"));
        }
        for (facts, file) in [
            (&self.receipt.home, &self.home),
            (&self.receipt.applications, &self.applications),
        ] {
            match (facts, file) {
                (Some(facts), Some(file)) => facts.matches(file)?,
                (None, None) => {}
                _ => return Err(refused("installed Copy archive selection differs")),
            }
        }
        check_secret(&self.secret)?;
        self.authority.check()
    }
}

/// Root retains this before ANY archive or secret descriptor can be delivered.
/// A transport failure conservatively retains all source/output authority until
/// the actual QPE receiver task has stopped. An acknowledgment is not task death.
pub struct InstalledCopyIssue {
    custody: Option<Box<IssueCustody>>,
}
impl RootConsumerChannel {
    /// Shape-only metadata cannot create this channel: its constructor still
    /// requires mutual QPE from the actual installed root helper. The supplied
    /// authority must be the protected Copy transaction, not a wire declaration.
    /// # Errors
    /// Refuses invalid declarations, archive or secret custody, recipient identity,
    /// cancelled operations and revoked transaction authority.
    pub fn prepare_installed_copy(
        self,
        declaration: CopyDeclaration,
        recipient: CopyRecipient,
        archives: (Option<File>, Option<File>),
        secret: File,
        authority: Arc<dyn ServiceAuthority>,
        cancellation: ServiceCancellation,
    ) -> io::Result<InstalledCopyIssue> {
        let (home, applications) = archives;
        let receipt = WireReceipt {
            schema: 1,
            declaration,
            home: home.as_ref().map(ArchiveFacts::measure).transpose()?,
            applications: applications
                .as_ref()
                .map(ArchiveFacts::measure)
                .transpose()?,
        };
        let custody = Box::new(IssueCustody {
            channel: self,
            recipient,
            authority,
            receipt,
            home,
            applications,
            secret,
            cancellation,
            possible_delivery: false,
            delivered: false,
            unusable: false,
        });
        custody.check()?;
        Ok(InstalledCopyIssue {
            custody: Some(custody),
        })
    }
}
impl InstalledCopyIssue {
    /// The helper must remain alive after abandonment with unobserved receiver death.
    pub fn retirement_pending() -> bool {
        UNKNOWN_DELIVERY.load(std::sync::atomic::Ordering::Acquire)
    }
    fn custody(&self) -> io::Result<&IssueCustody> {
        self.custody
            .as_deref()
            .ok_or_else(|| refused("installed Copy issue retired"))
    }
    /// Returns the actual retained gate declaration after reattesting its custody.
    /// # Errors
    /// Refuses retired issue custody, revoked authority or changed held evidence.
    pub fn declaration(&self) -> io::Result<&CopyDeclaration> {
        let custody = self.custody()?;
        custody.check()?;
        Ok(&custody.receipt.declaration)
    }
    /// Sends bounded expectations and read-only files through this exact gate.
    /// Every timeout/partial frame poisons further transport; retirement remains
    /// possible and retains all authority until the receiver's kernel task exits.
    /// # Errors
    /// Refuses retired or already attempted delivery and invalid custody; reports
    /// cancellation, descriptor transfer, framing and acknowledgment failures.
    /// Failed transport remains unusable while custody is retained.
    pub fn deliver(&mut self) -> io::Result<()> {
        let custody = self
            .custody
            .as_mut()
            .ok_or_else(|| refused("Copy issue retired"))?;
        custody.check()?;
        if custody.delivered || custody.possible_delivery {
            return Err(refused("installed Copy delivery already attempted"));
        }
        let result = (|| {
            let mut io = Handoff::new(
                &mut custody.channel.stream,
                &custody.cancellation,
                Duration::from_secs(30),
            )?;
            send_json(&mut io, &custody.receipt)?;
            for (index, file) in [
                (80, Some(&custody.secret)),
                (81, custody.home.as_ref()),
                (82, custody.applications.as_ref()),
            ] {
                let Some(file) = file else { continue };
                custody.authority.check()?;
                if !custody.channel.peer.current() {
                    return Err(refused("Copy receiver stopped"));
                }
                // Conservative BEFORE syscall: interrupted sendmsg may have
                // delivered a descriptor even without its acknowledgment.
                custody.possible_delivery = true;
                io.retry(libc::POLLOUT, |stream| {
                    runtime_portal::fd_transport::send_with_fd(
                        stream.as_fd(),
                        &[index],
                        file.as_fd(),
                    )
                })?;
                let mut ack = [0];
                io.read_exact(&mut ack)?;
                if ack != [1] {
                    return Err(refused("Copy descriptor acknowledgment differs"));
                }
            }
            Ok(())
        })();
        if result.is_err() {
            custody.unusable = true;
        } else {
            custody.delivered = true;
        }
        result?;
        custody.check()
    }
    /// Same authenticated stream only; serialized commands/events remain the
    /// installed app's closed semantic vocabulary. Partial frames are terminal.
    /// # Errors
    /// Refuses retired or invalid custody and unavailable delivery; reports
    /// cancellation, serialization and failed or timed-out frame transfers.
    /// Transport failure makes further communication unusable.
    pub fn send<T: serde::Serialize>(&mut self, value: &T) -> io::Result<()> {
        let custody = self
            .custody
            .as_mut()
            .ok_or_else(|| refused("Copy issue retired"))?;
        custody.check()?;
        if !custody.delivered {
            return Err(refused("Copy archives not delivered"));
        }
        let result = Handoff::new(
            &mut custody.channel.stream,
            &custody.cancellation,
            Duration::from_secs(10),
        )
        .and_then(|mut io| send_json(&mut io, value));
        if result.is_err() {
            custody.unusable = true;
        }
        result?;
        custody.check()
    }
    /// # Errors
    /// Refuses invalid or retired custody; reports cancellation, malformed frames,
    /// decoding errors and failed or timed-out transfers. Transport failures
    /// make further communication unusable.
    pub fn receive<T: serde::de::DeserializeOwned>(&mut self) -> io::Result<T> {
        let custody = self
            .custody
            .as_mut()
            .ok_or_else(|| refused("Copy issue retired"))?;
        custody.check()?;
        if !custody.delivered {
            return Err(refused("Copy archives not delivered"));
        }
        let result = Handoff::new(
            &mut custody.channel.stream,
            &custody.cancellation,
            Duration::from_secs(30),
        )
        .and_then(|mut io| receive_json(&mut io));
        if result.is_err() {
            custody.unusable = true;
        }
        let value = result?;
        custody.check()?;
        Ok(value)
    }
    /// Idle polling does not consume a frame or impose an import deadline.
    /// Once any frame starts, its bounded complete-frame budget is terminal.
    /// # Errors
    /// Refuses budgets above one second, invalid custody or undelivered archives;
    /// reports polling, cancellation and complete-frame transfer or decoding errors.
    pub fn receive_timeout<T: serde::de::DeserializeOwned>(
        &mut self,
        budget: Duration,
    ) -> io::Result<Option<T>> {
        if budget > Duration::from_secs(1) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let custody = self
            .custody
            .as_mut()
            .ok_or_else(|| refused("Copy issue retired"))?;
        custody.check()?;
        if !custody.delivered {
            return Err(refused("Copy archives not delivered"));
        }
        let mut descriptor = libc::pollfd {
            fd: custody.channel.stream.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe {
            libc::poll(
                &raw mut descriptor,
                1,
                i32::try_from(budget.as_millis()).map_err(io::Error::other)?,
            )
        };
        if ready < 0 {
            return Err(io::Error::last_os_error());
        }
        custody.check()?;
        if ready == 0 {
            return Ok(None);
        }
        let result = Handoff::new(
            &mut custody.channel.stream,
            &custody.cancellation,
            Duration::from_secs(10),
        )
        .and_then(|mut io| receive_json(&mut io));
        if result.is_err() {
            custody.unusable = true;
        }
        let value = result?;
        custody.check()?;
        Ok(Some(value))
    }
    /// Cleanup only: stop the exact private receiver channel even when source
    /// authorization has been revoked. This creates no file/source permission
    /// and retains every possibly delivered descriptor until actual task death.
    /// # Errors
    /// Refuses a non-helper caller or retired custody; reports socket shutdown failure.
    pub fn cancel_receiver(&mut self) -> io::Result<()> {
        own_helper()?;
        let custody = self
            .custody
            .as_mut()
            .ok_or_else(|| refused("Copy issue retired"))?;
        custody.unusable = true;
        custody.channel.stream.shutdown(std::net::Shutdown::Both)
    }
    /// Only actual authenticated task death releases possibly delivered output.
    /// Returns false without consuming custody when the receiver is still live.
    /// # Errors
    /// Refuses non-helper callers or retired custody; reports failure to observe
    /// the receiver's kernel task. Unknown death never releases custody.
    pub fn retire(&mut self) -> io::Result<bool> {
        let custody = self.custody()?;
        own_helper()?;
        if custody.possible_delivery && !peer_stopped(&custody.channel.peer)? {
            return Ok(false);
        }
        self.custody.take();
        Ok(true)
    }
}
impl Drop for InstalledCopyIssue {
    fn drop(&mut self) {
        if let Some(custody) = self.custody.take() {
            // Signal cancellation before parking custody. Retaining an open
            // channel after withdrawal would strand the live receiver forever.
            let _ = custody.channel.stream.shutdown(std::net::Shutdown::Both);
            if custody.possible_delivery && !matches!(peer_stopped(&custody.channel.peer), Ok(true))
            {
                // Never silently withdraw source/output custody on an unknown
                // delivery or receiver state. The helper supervisor normally
                // retains the issue and performs explicit retirement instead.
                UNKNOWN_DELIVERY.store(true, std::sync::atomic::Ordering::Release);
                let _ = Box::leak(custody);
            }
        }
    }
}

/// Actual root task + pinned Shell gate, not a deserializable receive grant.
/// Secret contents are never put in the receipt or diagnostic transport.
pub struct ReceivedInstalledCopy {
    role: RootRole,
    stream: UnixStream,
    receipt: WireReceipt,
    home: Option<File>,
    applications: Option<File>,
    secret: File,
    cancellation: ServiceCancellation,
    unusable: bool,
}
impl RetainedLaunchGate {
    /// # Errors
    /// Refuses unauthenticated root peers, mismatched target owners and invalid
    /// archive/secret evidence; reports cancellation and failed handoff transfers.
    pub fn receive_installed_copy(
        self,
        cancellation: ServiceCancellation,
    ) -> io::Result<ReceivedInstalledCopy> {
        let (role, mut stream) = RootRole::authenticate(self, &cancellation)?;
        let (receipt, secret, home, applications) = {
            let mut io = Handoff::new(&mut stream, &cancellation, Duration::from_secs(30))?;
            let receipt: WireReceipt = receive_json(&mut io)?;
            receipt.validate()?;
            if receipt.declaration.target_uid != rustix::process::getuid().as_raw() {
                return Err(refused("Copy receipt belongs to another owner"));
            }
            role.check()?;
            let secret = File::from(receive_fd(&mut io, 80)?);
            check_secret(&secret)?;
            let home = receipt
                .home
                .as_ref()
                .map(|_| receive_fd(&mut io, 81).map(File::from))
                .transpose()?;
            let applications = receipt
                .applications
                .as_ref()
                .map(|_| receive_fd(&mut io, 82).map(File::from))
                .transpose()?;
            (receipt, secret, home, applications)
        };
        let received = ReceivedInstalledCopy {
            role,
            stream,
            receipt,
            secret,
            home,
            applications,
            cancellation,
            unusable: false,
        };
        received.check()?;
        Ok(received)
    }
}
/// Cancellation observation only; it never certifies archive adoption or
/// receiver retirement. Retains the exact authenticated socket and root pidfd.
pub struct InstalledCopyStopFence {
    stream: UnixStream,
    root: OwnedFd,
    cancellation: ServiceCancellation,
}
impl InstalledCopyStopFence {
    /// # Errors
    /// Returns the kernel polling error if task or channel cancellation cannot be observed.
    pub fn cancelled(&self) -> io::Result<bool> {
        if self.cancellation.is_cancelled() {
            return Ok(true);
        }
        let mut fds = [
            libc::pollfd {
                fd: self.root.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: self.stream.as_raw_fd(),
                events: libc::POLLRDHUP,
                revents: 0,
            },
        ];
        if unsafe { libc::poll(fds.as_mut_ptr(), 2, 0) } < 0 {
            return Err(io::Error::last_os_error());
        }
        // Failed observation refuses operation; this boolean never proves the
        // original/target process has retired. RDHUP also works with queued data.
        Ok(fds[0].revents != 0
            || fds[1].revents & (libc::POLLRDHUP | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL)
                != 0)
    }
}
impl ReceivedInstalledCopy {
    pub fn stop_fence(&self) -> io::Result<InstalledCopyStopFence> {
        self.check()?;
        Ok(InstalledCopyStopFence {
            stream: self.stream.try_clone()?,
            root: self.role.peer.pidfd().try_clone_to_owned()?,
            cancellation: self.cancellation.clone(),
        })
    }
    /// # Errors
    /// Refuses retired root authority, cancelled or unusable transport, invalid
    /// declarations and changed archive or sealed-secret evidence.
    pub fn check(&self) -> io::Result<()> {
        self.role.check()?;
        if self.cancellation.is_cancelled() || self.unusable {
            return Err(refused("Copy receive channel retired"));
        }
        self.receipt.validate()?;
        for (facts, file) in [
            (&self.receipt.home, &self.home),
            (&self.receipt.applications, &self.applications),
        ] {
            if let (Some(facts), Some(file)) = (facts, file) {
                facts.matches(file)?;
            }
        }
        check_secret(&self.secret)
    }
    pub fn declaration(&self) -> &CopyDeclaration {
        &self.receipt.declaration
    }
    pub fn clone_home(&self) -> io::Result<Option<File>> {
        self.check()?;
        self.home.as_ref().map(File::try_clone).transpose()
    }
    pub fn clone_applications(&self) -> io::Result<Option<File>> {
        self.check()?;
        self.applications.as_ref().map(File::try_clone).transpose()
    }
    /// # Errors
    /// Refuses invalid receive custody or returns the sealed-secret descriptor duplication error.
    pub fn secret(&self) -> io::Result<File> {
        self.check()?;
        self.secret.try_clone()
    }
    /// # Errors
    /// Refuses retired or invalid custody and unavailable delivery; reports
    /// cancellation, serialization and failed or timed-out frame transfers.
    /// Transport failure makes further communication unusable.
    pub fn send<T: serde::Serialize>(&mut self, value: &T) -> io::Result<()> {
        self.check()?;
        let result = Handoff::new(
            &mut self.stream,
            &self.cancellation,
            Duration::from_secs(10),
        )
        .and_then(|mut io| send_json(&mut io, value));
        if result.is_err() {
            self.unusable = true;
        }
        result?;
        self.check()
    }
    /// No bytes are consumed on idle timeout. Once a frame begins, its absolute
    /// ten-second deadline applies; partial/EOF failure poisons this channel.
    /// # Errors
    /// Refuses invalid or retired custody; reports cancellation, malformed frames,
    /// decoding errors and failed or timed-out transfers. Transport failures
    /// make further communication unusable.
    pub fn receive<T: serde::de::DeserializeOwned>(
        &mut self,
        budget: Duration,
    ) -> io::Result<Option<T>> {
        self.check()?;
        if budget > Duration::from_secs(1) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let mut fd = libc::pollfd {
            fd: self.stream.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let count = unsafe {
            libc::poll(
                &raw mut fd,
                1,
                i32::try_from(budget.as_millis()).map_err(io::Error::other)?,
            )
        };
        if count < 0 {
            return Err(io::Error::last_os_error());
        }
        self.check()?;
        if count == 0 {
            return Ok(None);
        }
        let result = Handoff::new(
            &mut self.stream,
            &self.cancellation,
            Duration::from_secs(10),
        )
        .and_then(|mut io| receive_json(&mut io));
        if result.is_err() {
            self.unusable = true;
        }
        let value = result?;
        self.check()?;
        Ok(Some(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn oversized_archive_receipt_never_writes_transport() {
        let (mut sender, mut receiver) = UnixStream::pair().expect("pair");
        receiver.set_nonblocking(true).expect("nonblocking");
        let cancel = ServiceCancellation::default();
        let mut io = Handoff::new(&mut sender, &cancel, Duration::from_secs(1)).expect("handoff");
        assert!(send_json(&mut io, &vec![0u8; LIMIT]).is_err());
        let mut byte = [0];
        assert_eq!(
            receiver.read(&mut byte).expect_err("no frame").kind(),
            io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn archive_and_secret_admission_refuse_directory_descriptors() {
        let directory = File::open("/").expect("directory");
        assert!(ArchiveFacts::measure(&directory).is_err());
        assert!(check_secret(&directory).is_err());
    }

    #[test]
    fn closed_receipt_refuses_extra_fields_and_old_schema() {
        let declaration = CopyDeclaration {
            operation: uuid::Uuid::new_v4().to_string(),
            setup_binding: "ab".repeat(32),
            person: [1; 32],
            read_claim: [2; 32],
            target_uid: 1000,
            payload: vec![1],
        };
        let mut value = serde_json::to_value(&declaration).expect("encode");
        value
            .as_object_mut()
            .expect("object")
            .insert("source_path".into(), serde_json::json!("/"));
        assert!(serde_json::from_value::<CopyDeclaration>(value).is_err());
        let receipt = WireReceipt {
            schema: 0,
            declaration,
            home: None,
            applications: None,
        };
        assert!(receipt.validate().is_err());
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "command", rename_all = "kebab-case", deny_unknown_fields)]
pub enum CopyOwnerCommand {
    Cancel {},
    Retry {},
    ResumeCleanup {
        operation: String,
    },
    AcknowledgeCleanup {
        operation: String,
        metadata_quarantined: bool,
    },
    AcknowledgeComplete {
        operation: String,
        home_cipher_sha256: Option<String>,
        applications_cipher_sha256: Option<String>,
    },
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "event", rename_all = "kebab-case", deny_unknown_fields)]
pub enum CopyOwnerEvent {
    Copying {
        operation: String,
    },
    Verifying {
        operation: String,
    },
    Adopting {
        operation: String,
    },
    RestoringApplications {
        operation: String,
    },
    Complete {
        operation: String,
        home_cipher_sha256: Option<String>,
        applications_cipher_sha256: Option<String>,
    },
    CleanupPending {
        operation: String,
        message: String,
        resumable: bool,
    },
    CleanupFinished {
        operation: String,
        metadata_quarantined: bool,
    },
    Cancelled {
        operation: String,
    },
    Failed {
        operation: String,
        message: String,
    },
    RetirementPending {
        operation: String,
        message: String,
    },
}
impl CopyOwnerEvent {
    pub fn operation(&self) -> &str {
        match self {
            Self::Copying { operation }
            | Self::Verifying { operation }
            | Self::Adopting { operation }
            | Self::RestoringApplications { operation }
            | Self::Complete { operation, .. }
            | Self::CleanupPending { operation, .. }
            | Self::CleanupFinished { operation, .. }
            | Self::Cancelled { operation }
            | Self::Failed { operation, .. }
            | Self::RetirementPending { operation, .. } => operation,
        }
    }
}
#[cfg(test)]
mod owner_protocol_tests {
    use super::*;
    #[test]
    fn owner_commands_refuse_receiver_authority_and_unknown_fields() {
        for input in [
            r#"{"command":"cancel","uid":0}"#,
            r#"{"command":"retry","source":"/home"}"#,
            r#"{"command":"adopt"}"#,
        ] {
            assert!(serde_json::from_str::<CopyOwnerCommand>(input).is_err());
        }
        assert!(serde_json::from_str::<CopyOwnerCommand>(r#"{"command":"cancel"}"#).is_ok());
    }
}
