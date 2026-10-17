//! Explicit, password-authenticated continuation of owned prepared connections.
//! The backend owns checkpoint storage; the journey retains the current draft lease.
use super::PreparedAccounts;
pub(in crate::setup::journey) use imager_backend::account_checkpoint::Receipt;
use imager_backend::prepared_identity::DraftLock;
use std::{io, path::Path};
use zeroize::Zeroizing;

fn invalid() -> io::Error {
    io::Error::other("Prepared connections could not be authenticated")
}
pub(in crate::setup::journey) fn save(
    handle: &PreparedAccounts,
    root: &Path,
    binding: &str,
    group: [u8; 32],
    password: &[u8],
) -> io::Result<Receipt> {
    imager_backend::account_checkpoint::save(handle, root, binding, group, password)
}
pub(in crate::setup::journey) fn restore(
    root: &Path,
    receipt: &Receipt,
    password: &[u8],
) -> io::Result<PreparedAccounts> {
    imager_backend::account_checkpoint::restore(root, receipt, password)
}
pub(super) enum Completion {
    Saved(Receipt, PreparedAccounts),
    Restored(PreparedAccounts, Option<DraftLock>),
}
pub(super) struct Worker {
    restoring: bool,
    result: std::sync::mpsc::Receiver<io::Result<Completion>>,
}
impl Worker {
    pub(super) fn save(
        handle: Option<PreparedAccounts>,
        guard: crate::setup::account_transfer::DestinationGuard,
        root: std::path::PathBuf,
        binding: String,
        group: [u8; 32],
        password: Zeroizing<Vec<u8>>,
    ) -> Self {
        Self::spawn(false, move || {
            let handle = match handle {
                Some(handle) => handle,
                None => imager_backend::account_checkpoint::empty(guard)?,
            };
            let receipt = save(&handle, &root, &binding, group, &password)?;
            Ok(Completion::Saved(receipt, handle))
        })
    }
    #[cfg(test)]
    pub(super) fn restore(
        root: std::path::PathBuf,
        receipt: Receipt,
        password: Zeroizing<Vec<u8>>,
    ) -> Self {
        Self::spawn(true, move || {
            restore(&root, &receipt, &password).map(|handle| Completion::Restored(handle, None))
        })
    }
    pub(super) fn restore_ready(
        ready: crate::setup::journey::draft::ReadyDraft,
        password: Zeroizing<Vec<u8>>,
    ) -> Self {
        Self::spawn(true, move || {
            let guard = ready.acquire_current()?;
            if !ready.choices().includes_accounts() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Saved connections were left out of this preparation",
                ));
            }
            let receipt = ready.account_checkpoint().ok_or_else(invalid)?;
            let handle = restore(
                &ready.operation_directory().join("accounts"),
                receipt,
                &password,
            )?;
            Ok(Completion::Restored(handle, Some(guard)))
        })
    }
    fn spawn(
        restoring: bool,
        work: impl FnOnce() -> io::Result<Completion> + Send + 'static,
    ) -> Self {
        let (send, result) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = send.send(work());
        });
        Self { restoring, result }
    }
    pub(super) fn restoring(&self) -> bool {
        self.restoring
    }
    pub(super) fn poll(&self) -> Option<io::Result<Completion>> {
        match self.result.try_recv() {
            Ok(result) => Some(result),
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(Err(invalid())),
        }
    }
}
