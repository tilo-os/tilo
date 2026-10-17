//! Root replay of the ordinary launch selected by the live certified Shell.
//! Launcher declarations are evidence to compare, never source/person authority.
use super::installed_retained_actor::InstalledRetainedLaunchInput;
use launcher::retained_view::ConsumerLaunchBinding;
use runtime_portal::process_binding::ExecutingPeer;
use sha2::{Digest, Sha256};
use std::{
    io,
    sync::{Arc, atomic::AtomicBool},
};

fn refused() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}

/// Only the root-installed actor constructs this after comparing the genuine
/// certified selection, signed target package and actual executing launcher.
/// It remains live custody through every view handoff and namespace lifetime.
pub(super) struct ReviewedConsumerSelection {
    input: Arc<InstalledRetainedLaunchInput>,
    launcher: Arc<ExecutingPeer>,
    binding: ConsumerLaunchBinding,
    signed_context: Vec<u8>,
}
impl ReviewedConsumerSelection {
    pub(super) fn review(
        input: Arc<InstalledRetainedLaunchInput>,
        launcher: Arc<ExecutingPeer>,
        binding: ConsumerLaunchBinding,
        signed_context: Vec<u8>,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        let value = Self {
            input,
            launcher,
            binding,
            signed_context,
        };
        value.check(cancel)?;
        Ok(value)
    }
    pub(super) fn check(&self, cancel: &AtomicBool) -> io::Result<()> {
        if rustix::process::getuid().as_raw() != 0 || rustix::process::geteuid().as_raw() != 0 {
            return Err(refused());
        }
        self.input.revalidate(cancel)?;
        self.binding.matches_peer(&self.launcher)?;
        self.binding.verify_peer_program(&self.launcher)?;
        let (uid, gid) = self.input.target_credentials(cancel)?;
        let selected = self.input.binding();
        if !self.launcher.current()
            || (self.launcher.uid(), self.launcher.gid()) != (uid, gid)
            || self.binding.operation_id != selected.operation_id
            || self.binding.app_id != selected.app_id
            || self.binding.package_sha256 != selected.package_sha256
            || self.binding.room != selected.room
            || self.binding.resource_record != "retained-home"
            || selected.resource_record != "retained-home"
            || self.binding.shell_context_sha256 != selected.context_sha256
            || self.signed_context.is_empty()
            || self.signed_context.len() > 64 * 1024
            || self.binding.signed_shell_context_bytes()? != self.signed_context
            || hex::encode(Sha256::digest(&self.signed_context)) != selected.context_sha256
            || self.binding.room
                != runtime_collab::retained_person::personal_room(&self.input.person_public_key())
        {
            return Err(refused());
        }
        // This replays signature/root/payload integrity independently of the
        // launcher's report, without introducing a root publisher allowlist.
        // Ordinary app trust remains the authenticated target Shell's choice.
        self.binding.verify_signed_gui_package()?;
        self.input.revalidate(cancel)?;
        self.binding.matches_peer(&self.launcher)
    }
    pub(super) fn uses_input(&self, input: &Arc<InstalledRetainedLaunchInput>) -> bool {
        Arc::ptr_eq(&self.input, input)
    }
    pub(super) fn binding(&self) -> &ConsumerLaunchBinding {
        &self.binding
    }
    pub(super) fn retained_launcher(&self) -> Arc<ExecutingPeer> {
        Arc::clone(&self.launcher)
    }
    pub(super) fn launcher(&self) -> &ExecutingPeer {
        &self.launcher
    }
}
