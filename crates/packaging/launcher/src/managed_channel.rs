//! One-use kernel-authenticated startup for the enrolled collaboration descriptor.
//! Supervisor custody is separate from the executing agent and its durable binding.
pub use super::{AdmittedManagedConnection, ChannelProcessFacts, VerifiedManagedProcess};
use super::{
    Arc, KernelProcess, ManagedBinding, Mutex, Path, ProcessIdentity, ProcessRecord,
    VerifiedLaunchAuthority, VerifiedManagedSupervisor,
};
use rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    net::sockopt::set_socket_passcred,
    process::{Pid, PidfdFlags, pidfd_open},
};
use std::{
    fs::File,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{fs::MetadataExt, net::UnixStream},
    },
    time::Duration,
};

/// Installed canonical owner callback. Success means the exact actual PID/start
/// binding is durable; any error after spawn is an uncertain launch, never a retry.
pub trait ManagedProcessAuthority {
    /// Durably bind the exact admitted agent process.
    ///
    /// # Errors
    ///
    /// Returns a description when the binding cannot be made durable; the launch is then
    /// uncertain.
    fn bind_process(&self, process: &VerifiedManagedProcess) -> Result<(), String>;
}
struct Executable {
    file: File,
    digest: tilo_app_package::BlobHash,
    /// Last full digest match and the exact file metadata it covered.
    hashed: std::sync::Mutex<Option<(std::time::Instant, [i64; 7])>>,
}
/// Longest interval a digest match of the executing image is reused while
/// its inode, length, mtime and ctime are unchanged (an executing image
/// cannot be written on Linux: ETXTBSY; any write or chmod changes ctime).
const EXECUTABLE_RECHECK: Duration = Duration::from_secs(2);
fn image_stamp(metadata: &std::fs::Metadata) -> [i64; 7] {
    [
        metadata.dev().cast_signed(),
        metadata.ino().cast_signed(),
        metadata.len().cast_signed(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    ]
}
impl Executable {
    fn capture(path: &Path, digest: tilo_app_package::BlobHash) -> Result<Self, String> {
        let mut file = File::open(path).map_err(|e| e.to_string())?;
        let mut bytes = vec![];
        (&mut file)
            .take(256 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > 256 * 1024 * 1024 {
            return Err("managed executable too large".into());
        }
        if tilo_app_package::BlobHash::of(&bytes) != digest {
            return Err("installed executable changed after verification".into());
        }
        Ok(Self {
            file,
            digest,
            hashed: std::sync::Mutex::new(None),
        })
    }
    fn verify(&self, pid: u32) -> Result<(), String> {
        // Opening /proc/PID/exe names the kernel's executable object, not a
        // process-supplied pathname. Retained file keeps the installed inode live.
        let path = format!("/proc/{pid}/exe");
        let mut actual = File::open(path).map_err(|e| format!("executing image: {e}"))?;
        let expected = self.file.metadata().map_err(|e| e.to_string())?;
        let metadata = actual.metadata().map_err(|e| e.to_string())?;
        if expected.dev() != metadata.dev() || expected.ino() != metadata.ino() {
            return Err("sender is not the installed executable object".into());
        }
        let stamp = image_stamp(&metadata);
        let reusable = self
            .hashed
            .lock()
            .map_err(|_| "executable verification cache poisoned")?
            .is_some_and(|(at, hashed)| at.elapsed() < EXECUTABLE_RECHECK && hashed == stamp);
        if reusable {
            return recheck_executable_object(pid, &expected);
        }
        let began = std::time::Instant::now();
        let mut bytes = vec![];
        (&mut actual)
            .take(256 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > 256 * 1024 * 1024 {
            return Err("executing image too large".into());
        }
        if tilo_app_package::BlobHash::of(&bytes) != self.digest {
            return Err("executing image bytes changed".into());
        }
        // Retained only when the image metadata was unchanged across the read.
        let after = actual.metadata().map_err(|e| e.to_string())?;
        if let Ok(mut hashed) = self.hashed.lock() {
            *hashed = (image_stamp(&after) == stamp).then_some((began, stamp));
        }
        // exec preserves PID/start and leaves an already-open executable FD
        // naming the prior image. Recheck the current kernel object after the
        // digest read; this remains point-in-time evidence, refreshed per use.
        recheck_executable_object(pid, &expected)
    }
}
fn recheck_executable_object(pid: u32, verified: &std::fs::Metadata) -> Result<(), String> {
    let current = File::open(format!("/proc/{pid}/exe"))
        .and_then(|file| file.metadata())
        .map_err(|error| format!("current executing image: {error}"))?;
    if (current.dev(), current.ino()) != (verified.dev(), verified.ino()) {
        return Err("executing image changed during verification".into());
    }
    Ok(())
}
pub(super) struct ProcessEvidence {
    pub(super) supervisor: ProcessIdentity,
    pub(super) channel_id: String,
    executable: Arc<Executable>,
    pidfd: OwnedFd,
}
impl ProcessEvidence {
    pub(super) fn revalidate(&self, process: ProcessIdentity) -> Result<(), String> {
        let mut pidfd = [PollFd::new(&self.pidfd, PollFlags::IN)];
        // A pidfd pins the original kernel task even after numeric PID reuse.
        if poll(&mut pidfd, Some(&Timespec::default())).map_err(|e| e.to_string())? != 0
            || !process.is_current()
            || !self.supervisor.is_current()
        {
            return Err("agent or supervisor process exited/changed".into());
        }
        verify_descendant(process, self.supervisor)?;
        self.executable.verify(process.pid)?;
        if !process.is_current() || !self.supervisor.is_current() {
            return Err("agent or supervisor changed during executable verification".into());
        }
        verify_descendant(process, self.supervisor)
    }
}
fn verify_descendant(process: ProcessIdentity, supervisor: ProcessIdentity) -> Result<(), String> {
    if process == supervisor {
        return Err("supervisor is not an executing agent".into());
    }
    let mut pid = process.pid;
    for _ in 0..32 {
        let current = ProcessIdentity::read(pid)?;
        if current == supervisor {
            return Ok(());
        }
        let stat =
            std::fs::read_to_string(format!("/proc/{pid}/stat")).map_err(|e| e.to_string())?;
        pid = stat
            .rsplit_once(") ")
            .and_then(|(_, rest)| rest.split_whitespace().nth(1))
            .ok_or("invalid process ancestry")?
            .parse()
            .map_err(|_| "invalid parent PID")?;
        if pid == 0 {
            break;
        }
    }
    Err("agent is not a current descendant of this exact supervisor".into())
}
/// Neither Clone nor Serde: exactly this connected descriptor owns its challenge.
pub struct PendingManagedConnection {
    socket: UnixStream,
    challenge: [u8; 32],
    binding: ManagedBinding,
    executable: Arc<Executable>,
    channel_id: String,
}

/// Cancellation of exactly one connected startup descriptor. This cannot
/// read/write protocol data, identify a process, or grant launch authority.
/// Shutting it down also wakes an in-flight blocking descriptor challenge.
pub struct ManagedConnectionCancellation(UnixStream);

impl ManagedConnectionCancellation {
    /// Interrupt this connection only; the owning launcher must still stop
    /// its actual children and retire canonical session custody.
    ///
    /// # Errors
    /// Reports a kernel failure to shut down the retained connected socket.
    pub fn cancel(&self) -> std::io::Result<()> {
        self.0.shutdown(std::net::Shutdown::Both)
    }
}

impl PendingManagedConnection {
    pub fn channel_id(&self) -> &str {
        &self.channel_id
    }

    /// Retain cancellation before moving the one-use connection into a worker.
    /// No protocol endpoint or process authority is exposed by this handle.
    ///
    /// # Errors
    /// Refuses a kernel failure to duplicate the retained socket descriptor.
    pub fn cancellation(&self) -> std::io::Result<ManagedConnectionCancellation> {
        self.socket.try_clone().map(ManagedConnectionCancellation)
    }
    /// Allocate this attempt's one connected descriptor pair and challenge.
    ///
    /// # Errors
    ///
    /// Returns a description when a channel was already allocated, the permit or package no
    /// longer verifies, or the socket cannot be configured.
    pub fn pair(permit: &mut VerifiedLaunchAuthority) -> Result<(Self, UnixStream), String> {
        if permit.channel_id.is_some() {
            return Err("managed channel already allocated for this attempt".into());
        }
        permit.revalidate()?;
        let owner = permit.owner.upgrade().ok_or("runtime owner lost")?;
        let binding = &permit.binding;
        let package = &owner.package;
        let channel_id = uuid::Uuid::new_v4().to_string();
        permit.channel_id = Some(channel_id.clone());
        let (manifest, digest) = package.verified_manifest(&owner.trust)?;
        let executable = Arc::new(Executable::capture(
            &package
                .payload_root()
                .join(manifest.entrypoint.cli_bin().ok_or("CLI entry missing")?),
            digest,
        )?);
        let (socket, client) = UnixStream::pair().map_err(|e| e.to_string())?;
        set_socket_passcred(&socket, true).map_err(|e| e.to_string())?;
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .map_err(|e| e.to_string())?;
        socket
            .set_write_timeout(Some(Duration::from_secs(10)))
            .map_err(|e| e.to_string())?;
        let mut challenge = [0; 32];
        challenge[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        challenge[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        Ok((
            Self {
                socket,
                challenge,
                binding: binding.clone(),
                executable,
                channel_id,
            },
            client,
        ))
    }
    /// Admit the agent and wrap its proof with the exact admitted socket.
    ///
    /// # Errors
    ///
    /// As [`PendingManagedConnection::admit`].
    pub fn admit_connection(
        self,
        supervisor: &VerifiedManagedSupervisor,
        authority: &dyn ManagedProcessAuthority,
    ) -> Result<AdmittedManagedConnection, String> {
        let (process, stream) = self.admit(supervisor, authority)?;
        Ok(AdmittedManagedConnection { process, stream })
    }
    /// Authenticate the kernel sender, verify its executable and ancestry, and
    /// durably bind it once.
    ///
    /// # Errors
    ///
    /// Returns a description when custody, the challenge, the sender's identity or executable,
    /// or the durable binding fails; after spawn such a failure is an uncertain launch.
    pub fn admit(
        mut self,
        supervisor: &VerifiedManagedSupervisor,
        authority: &dyn ManagedProcessAuthority,
    ) -> Result<(VerifiedManagedProcess, UnixStream), String> {
        supervisor.revalidate(&self.binding, supervisor.process())?;
        self.socket
            .write_all(&self.challenge)
            .map_err(|e| e.to_string())?;
        let (answer, pid) = receive_credentials(&self.socket)?;
        if answer != self.challenge {
            return Err("descriptor challenge mismatch".into());
        }
        let process = ProcessIdentity::read(pid)?;
        let pid = Pid::from_raw(pid.cast_signed()).ok_or("agent PID out of range")?;
        let pidfd = pidfd_open(pid, PidfdFlags::empty())
            .map_err(|e| format!("agent pidfd unavailable: {e}"))?;
        let evidence = ProcessEvidence {
            supervisor: supervisor.process(),
            executable: self.executable,
            channel_id: self.channel_id,
            pidfd,
        };
        evidence.revalidate(process)?;
        {
            let mut attempted = supervisor
                .record
                .admission_attempted
                .lock()
                .map_err(|_| "agent binding unavailable")?;
            if *attempted {
                return Err("agent binding already attempted; reconcile original launch".into());
            }
            *attempted = true;
        }
        // No registry lock spans this canonical durable barrier.
        supervisor.revalidate(&self.binding, supervisor.process())?;
        evidence.revalidate(process)?;
        let registry = supervisor
            .registry
            .upgrade()
            .ok_or("launch lifecycle ended")?;
        let record = Arc::new(ProcessRecord {
            binding: self.binding,
            runtime: supervisor.record.runtime.clone(),
            cancelled: supervisor.record.cancelled.clone(),
            kernel: KernelProcess::open(process)?,
            admission_attempted: Mutex::new(true),
            agent: Some(evidence),
        });
        {
            let mut records = registry
                .lock()
                .map_err(|_| "process registry unavailable")?;
            if records.contains_key(&process) {
                return Err("actual agent process already bound".into());
            }
            if !records
                .get(&supervisor.process())
                .is_some_and(|r| Arc::ptr_eq(r, &supervisor.record))
            {
                return Err("supervisor retired during admission".into());
            }
            records.insert(process, record.clone());
        }
        let proof = VerifiedManagedProcess(VerifiedManagedSupervisor {
            registry: Arc::downgrade(&registry),
            record,
            process,
        });
        proof.revalidate(proof.binding(), process)?;
        if let Err(error) = authority.bind_process(&proof) {
            if let Ok(mut records) = registry.lock() {
                records.remove(&process);
            }
            return Err(error);
        }
        proof.revalidate(proof.binding(), process)?;
        // The SDK cannot start Cap'n Proto until after the barrier and recheck.
        if let Err(error) = self.socket.write_all(&[1]) {
            if let Ok(mut records) = registry.lock() {
                records.remove(&process);
            }
            return Err(format!("durably bound agent readiness uncertain: {error}"));
        }
        self.socket
            .set_read_timeout(None)
            .map_err(|e| e.to_string())?;
        self.socket
            .set_write_timeout(None)
            .map_err(|e| e.to_string())?;
        Ok((proof, self.socket))
    }
}
// All installed SCM_RIGHTS descriptors become owned before any validation can return.
// Holding the chunk keeps them close-on-exec until RAII closes them on every path.
struct CredentialChunk {
    count: usize,
    credential: Option<libc::ucred>,
    rights: Vec<std::os::fd::OwnedFd>,
    invalid: bool,
    truncated: bool,
}
fn receive_credential_chunk(
    socket: &UnixStream,
    answer: &mut [u8],
) -> Result<CredentialChunk, String> {
    const WIDTH: usize = std::mem::size_of::<libc::c_int>();
    let mut iov = libc::iovec {
        iov_base: answer.as_mut_ptr().cast(),
        iov_len: answer.len(),
    };
    let mut control = [0usize; 16];
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &raw mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    msg.msg_controllen = std::mem::size_of_val(&control);
    let count = unsafe { libc::recvmsg(socket.as_raw_fd(), &raw mut msg, libc::MSG_CMSG_CLOEXEC) };
    if count < 0 {
        return Err(format!(
            "kernel credential read failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    let mut received = CredentialChunk {
        count: count.unsigned_abs(),
        credential: None,
        rights: Vec::new(),
        invalid: false,
        truncated: msg.msg_flags & (libc::MSG_CTRUNC | libc::MSG_TRUNC) != 0,
    };
    // recvmsg supplies kernel-validated headers. Bound each payload defensively;
    // never stop after a rejected credential before owning later rights headers.
    let used = msg.msg_controllen.min(std::mem::size_of_val(&control));
    unsafe {
        let mut header = libc::CMSG_FIRSTHDR(&raw const msg);
        while !header.is_null() {
            let offset = (header as usize).saturating_sub(msg.msg_control as usize);
            let base = libc::CMSG_LEN(0) as usize;
            if offset.checked_add(base).is_none_or(|end| end > used) {
                received.invalid = true;
                break;
            }
            let length = (*header).cmsg_len;
            if length < base {
                received.invalid = true;
                break;
            }
            // `offset + base <= used` and `length >= base` were checked above.
            let available = used.saturating_sub(offset);
            let payload = length.min(available).saturating_sub(base);
            if length > available {
                received.invalid = true;
            }
            if (*header).cmsg_level == libc::SOL_SOCKET && (*header).cmsg_type == libc::SCM_RIGHTS {
                received.invalid = true;
                if !payload.is_multiple_of(WIDTH) {
                    received.invalid = true;
                }
                for index in 0..payload / WIDTH {
                    let fd = libc::c_int::from_ne_bytes(std::ptr::read_unaligned(
                        libc::CMSG_DATA(header)
                            .add(index.saturating_mul(WIDTH))
                            .cast::<[u8; WIDTH]>(),
                    ));
                    if fd >= 0 {
                        received.rights.push(std::os::fd::OwnedFd::from_raw_fd(fd));
                    } else {
                        received.invalid = true;
                    }
                }
            } else if (*header).cmsg_level == libc::SOL_SOCKET
                && (*header).cmsg_type == libc::SCM_CREDENTIALS
            {
                if payload != std::mem::size_of::<libc::ucred>() || received.credential.is_some() {
                    received.invalid = true;
                } else {
                    received.credential = Some(std::ptr::read_unaligned(
                        libc::CMSG_DATA(header).cast::<libc::ucred>(),
                    ));
                }
            } else {
                received.invalid = true;
            }
            if length > available {
                break;
            }
            header = libc::CMSG_NXTHDR(&raw const msg, header);
        }
    }
    Ok(received)
}
fn receive_credentials(socket: &UnixStream) -> Result<([u8; 32], u32), String> {
    let mut answer = [0; 32];
    let mut offset = 0;
    let mut sender = None;
    while offset < answer.len() {
        let remaining = answer
            .get_mut(offset..)
            .ok_or("kernel credential handshake overran")?;
        let received = receive_credential_chunk(socket, remaining)?;
        if received.count == 0
            || received.truncated
            || received.invalid
            || !received.rights.is_empty()
        {
            return Err(
                "kernel credential handshake incomplete or unexpected ancillary data".into(),
            );
        }
        let credential = received
            .credential
            .ok_or("kernel sender credential missing")?;
        if credential.pid <= 0 || credential.uid != rustix::process::geteuid().as_raw() {
            return Err("wrong kernel sender identity".into());
        }
        let pid = credential.pid.unsigned_abs();
        if sender.is_some_and(|prior| prior != pid) {
            return Err("descriptor sender changed".into());
        }
        sender = Some(pid);
        offset = offset.saturating_add(received.count);
    }
    Ok((answer, sender.ok_or("sender absent")?))
}

#[cfg(test)]
#[test]
fn enrolled_current_executable_recheck_detects_exec_with_same_start() {
    use std::process::{Command, Stdio};
    struct Reap(std::process::Child);
    impl Drop for Reap {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut cleanup = Reap(
        Command::new("/bin/sh")
            .args(["-c", "printf ready; read -r answer; exec /usr/bin/sleep 30"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let child = &mut cleanup.0;
    let mut ready = [0; 5];
    child
        .stdout
        .as_mut()
        .unwrap()
        .read_exact(&mut ready)
        .unwrap();
    assert_eq!(&ready, b"ready");
    let identity = ProcessIdentity::read(child.id()).unwrap();
    let opened = File::open(format!("/proc/{}/exe", child.id())).unwrap();
    let verified_object = opened.metadata().unwrap();
    recheck_executable_object(child.id(), &verified_object).unwrap();
    child.stdin.as_mut().unwrap().write_all(b"exec\n").unwrap();
    let expected = std::fs::metadata("/usr/bin/sleep").unwrap();
    let deadline = tilo_test_env::hang_deadline();
    let changed = loop {
        let current = std::fs::metadata(format!("/proc/{}/exe", child.id())).unwrap();
        if (current.dev(), current.ino()) == (expected.dev(), expected.ino()) {
            break true;
        }
        if std::time::Instant::now() >= deadline {
            break false;
        }
        std::thread::yield_now();
    };
    let same_start = identity.is_current();
    let result = recheck_executable_object(child.id(), &verified_object);
    let retained = opened.metadata().unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(changed, "fixture must execute a real replacement image");
    assert!(same_start, "exec preserves process PID/start");
    assert_eq!(
        (retained.dev(), retained.ino()),
        (verified_object.dev(), verified_object.ino()),
        "old open executable FD still names its prior kernel object"
    );
    assert!(
        result.is_err(),
        "a hash of the prior open executable cannot attest the current image after exec"
    );
}

#[cfg(test)]
mod received_rights_tests {
    use super::*;

    fn socket_pair(passcred: bool) -> (UnixStream, UnixStream) {
        let (receiver, sender) = UnixStream::pair().unwrap();
        receiver
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        set_socket_passcred(&receiver, passcred).unwrap();
        (receiver, sender)
    }
    fn send_rights(sender: &UnixStream, file: &File, count: usize) {
        use rustix::net::{SendAncillaryBuffer, SendAncillaryMessage, SendFlags, sendmsg};
        use std::os::fd::AsFd;
        let rights = vec![file.as_fd(); count];
        let mut space =
            vec![std::mem::MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(count))];
        let mut control = SendAncillaryBuffer::new(&mut space);
        assert!(control.push(SendAncillaryMessage::ScmRights(&rights)));
        let sent = sendmsg(
            sender,
            &[std::io::IoSlice::new(&[37; 32])],
            &mut control,
            SendFlags::NOSIGNAL,
        );
        assert_eq!(sent.unwrap(), 32);
    }
    fn descriptor_count(file: &File) -> usize {
        let expected = file.metadata().unwrap();
        std::fs::read_dir("/proc/self/fd")
            .expect("Linux /proc FD inventory required")
            .map(|entry| entry.unwrap().path())
            .filter_map(|path| std::fs::metadata(path).ok())
            .filter(|actual| (actual.dev(), actual.ino()) == (expected.dev(), expected.ino()))
            .count()
    }
    fn assert_not_inherited(path: &Path) {
        let child = std::process::Command::new("/bin/sh")
            .args([
                "-c",
                "test -d /proc/self/fd && test -x /usr/bin/readlink || exit 97; for fd in /proc/self/fd/*; do /usr/bin/readlink \"$fd\" 2>/dev/null || :; done",
            ])
            .output()
            .expect("real Linux exec is required for FD inheritance qualification");
        assert!(child.status.success());
        assert!(
            !String::from_utf8(child.stdout)
                .unwrap()
                .lines()
                .any(|line| line == path.to_str().unwrap()),
            "received SCM_RIGHTS descriptor escaped into another exec"
        );
    }
    #[test]
    fn managed_received_rights_are_cloexec_while_cleanup_guard_is_alive() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let (receiver, sender) = socket_pair(true);
        let before = descriptor_count(file.as_file());
        send_rights(&sender, file.as_file(), 1);
        let mut answer = [0; 32];
        let chunk = receive_credential_chunk(&receiver, &mut answer).unwrap();
        assert_eq!(chunk.rights.len(), 1);
        assert_eq!(descriptor_count(file.as_file()), before + 1);
        assert!(
            rustix::io::fcntl_getfd(&chunk.rights[0])
                .unwrap()
                .contains(rustix::io::FdFlags::CLOEXEC),
            "recvmsg must atomically set CLOEXEC"
        );
        assert_not_inherited(file.path());
        drop(chunk);
        assert_eq!(descriptor_count(file.as_file()), before);
    }
    #[test]
    fn managed_received_rights_are_rejected_and_closed_even_when_truncated() {
        let file = tempfile::NamedTempFile::new().unwrap();
        for (count, passcred) in [(1, true), (128, true), (1, false), (128, false)] {
            let (receiver, sender) = socket_pair(passcred);
            let before = descriptor_count(file.as_file());
            send_rights(&sender, file.as_file(), count);
            let result = receive_credentials(&receiver);
            let after = descriptor_count(file.as_file());
            assert_eq!(
                after, before,
                "normal/truncated SCM_RIGHTS must close every installed descriptor"
            );
            assert_not_inherited(file.path());
            assert!(
                result.is_err(),
                "unexpected ancillary control must reject the challenge"
            );
        }
        let (receiver, mut sender) = socket_pair(true);
        sender.write_all(&[37; 32]).unwrap();
        assert_eq!(
            receive_credentials(&receiver).unwrap(),
            ([37; 32], std::process::id()),
            "credential-only challenge remains valid"
        );
    }

    #[test]
    fn cancelling_one_descriptor_wakes_its_challenge_without_affecting_another() {
        let (receiver, mut sender) = socket_pair(true);
        let cancellation = ManagedConnectionCancellation(receiver.try_clone().unwrap());
        let (ready, waiting) = std::sync::mpsc::channel();
        let (finished, result) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            ready.send(()).unwrap();
            finished.send(receive_credentials(&receiver)).unwrap();
        });
        waiting.recv_timeout(Duration::from_secs(1)).unwrap();
        cancellation.cancel().unwrap();
        assert!(
            result
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .is_err(),
            "cancellation must wake and refuse the real credential receive"
        );
        worker.join().unwrap();
        assert!(sender.write_all(&[37; 32]).is_err());
        let (other_receiver, mut other_sender) = socket_pair(true);
        other_sender.write_all(&[41; 32]).unwrap();
        assert_eq!(
            receive_credentials(&other_receiver).unwrap(),
            ([41; 32], std::process::id()),
            "cancelling a prior connection must not affect another agent"
        );
    }
}
