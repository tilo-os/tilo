//! Owned selected-open process proof transport. No path or account authority is
//! accepted here: callers retain their granted objects until this handle drops.
use std::io;

/// An owned, deadline-bounded process-binding response channel. It proves only
/// process identity; the host still checks invocation and object authority.
#[cfg(target_os = "linux")]
pub struct Proof {
    remote: std::os::unix::net::UnixStream,
    control: std::os::unix::net::UnixStream,
    worker: Option<std::thread::JoinHandle<io::Result<()>>>,
}
/// Unsupported process-proof transport on targets without Linux task custody.
#[cfg(not(target_os = "linux"))]
pub struct Proof;
#[cfg(target_os = "linux")]
impl Proof {
    /// Start an owned responder with a total handshake deadline.
    pub fn start() -> io::Result<Self> {
        Self::start_with_timeout(std::time::Duration::from_secs(10))
    }
    fn start_with_timeout(timeout: std::time::Duration) -> io::Result<Self> {
        let deadline = std::time::Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| io::Error::other("selected open deadline overflow"))?;
        let (mut local, remote) = std::os::unix::net::UnixStream::pair()?;
        let control = local.try_clone()?;
        let fence = local.try_clone()?;
        let worker = std::thread::Builder::new()
            .name("files-selected-open-proof".into())
            .spawn(move || {
                // Bound the entire handshake, including peers that slowly progress
                // across process_binding::complete's individual read timeouts.
                let (done, finished) = std::sync::mpsc::channel();
                let watchdog = std::thread::Builder::new()
                    .name("files-selected-open-deadline".into())
                    .spawn(move || {
                        if finished
                            .recv_timeout(
                                deadline.saturating_duration_since(std::time::Instant::now()),
                            )
                            .is_err()
                        {
                            let _ = fence.shutdown(std::net::Shutdown::Both);
                        }
                    })?;
                let result = super::complete(&mut local);
                let _ = done.send(());
                watchdog
                    .join()
                    .map_err(|_| io::Error::other("selected open deadline worker failed"))?;
                result
            })?;
        Ok(Self {
            remote,
            control,
            worker: Some(worker),
        })
    }
    /// Borrow the wire descriptor while this proof remains held.
    /// # Errors
    /// Returns an error if the owned descriptor cannot fit the wire format.
    pub fn descriptor(&self) -> io::Result<u32> {
        use std::os::fd::AsRawFd as _;
        u32::try_from(self.remote.as_raw_fd())
            .map_err(|_| io::Error::other("selected open proof unavailable"))
    }
    pub fn finished(&self) -> bool {
        self.worker
            .as_ref()
            .is_none_or(std::thread::JoinHandle::is_finished)
    }
    /// Join the responder after `finished` reports completion.
    /// # Errors
    /// Refuses a failed, timed out, or already consumed handshake.
    pub fn result(&mut self) -> io::Result<()> {
        self.worker
            .take()
            .ok_or_else(|| io::Error::other("selected open proof unavailable"))?
            .join()
            .map_err(|_| io::Error::other("selected open proof unavailable"))?
            .map_err(|_| io::Error::other("selected open proof unavailable"))
    }
}
#[cfg(target_os = "linux")]
impl Drop for Proof {
    fn drop(&mut self) {
        let _ = self.control.shutdown(std::net::Shutdown::Both);
        let _ = self.remote.shutdown(std::net::Shutdown::Both);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
#[cfg(not(target_os = "linux"))]
impl Proof {
    /// Start a process proof responder.
    /// # Errors
    /// Always returns unsupported without Linux process custody.
    pub fn start() -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "selected open requires Linux process custody",
        ))
    }
    /// Borrow the proof descriptor.
    /// # Errors
    /// Always returns unsupported without Linux process custody.
    pub fn descriptor(&self) -> io::Result<u32> {
        Err(io::Error::other("selected open proof unavailable"))
    }
    pub fn finished(&self) -> bool {
        true
    }
    /// Join the proof responder.
    /// # Errors
    /// Always returns unsupported without Linux process custody.
    pub fn result(&mut self) -> io::Result<()> {
        Err(io::Error::other("selected open proof unavailable"))
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    #[test]
    fn owned_proof_answers_a_real_kernel_credential_challenge() {
        use std::io::Write as _;
        let mut proof = Proof::start().unwrap();
        let mut host = proof.remote.try_clone().unwrap();
        super::super::arm(&host).unwrap();
        host.set_read_timeout(Some(tilo_test_env::hang_bound()))
            .unwrap();
        let nonce = [53; 32];
        host.write_all(&super::super::MAGIC).unwrap();
        host.write_all(&nonce).unwrap();
        let peer =
            super::super::receive(&host, &nonce, rustix::process::geteuid().as_raw()).unwrap();
        assert_eq!(peer.pid(), std::process::id());
        assert!(peer.current());
        host.write_all(&[1]).unwrap();
        proof.result().unwrap();
        // A valid process response still cannot be replayed for a new challenge.
        host.set_read_timeout(Some(std::time::Duration::from_millis(20)))
            .unwrap();
        assert!(
            super::super::receive(&host, &[54; 32], rustix::process::geteuid().as_raw()).is_err()
        );
    }

    #[test]
    fn stalled_actual_proof_has_total_deadline() {
        let mut proof = Proof::start_with_timeout(std::time::Duration::from_millis(50))
            .expect("owned socketpair");
        tilo_test_env::wait_until("the handshake to stop without a challenge", || {
            proof.finished()
        });
        assert!(
            proof.result().is_err(),
            "stalled peer never supplied kernel proof"
        );
    }
    #[test]
    fn cancellation_stops_actual_proof_before_descriptor_custody_ends() {
        // A deadline no test outlives: only the drop can end the handshake.
        let proof = Proof::start_with_timeout(std::time::Duration::from_secs(24 * 60 * 60))
            .expect("owned socketpair");
        // Drop shuts down the exact owned endpoint and joins the responder;
        // no discovery, unrelated process signal or childwait approximation.
        let (dropped, done) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            drop(proof);
            let _ = dropped.send(());
        });
        tilo_test_env::recv("dropping the proof to stop its responder", &done);
    }
}
