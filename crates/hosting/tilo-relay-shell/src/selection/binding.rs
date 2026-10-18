//! The desktop's shell retention authority, bound to a workspace store.
//!
//! [`NeighbourReplay`] is the adapter a host injects (through
//! [`crate::ShellPayloads::replay`]); it reads a [`RetentionAuthority`],
//! checks it against the store, persists it as `shell-authority-v1.json` and
//! installs a [`DesktopBinding`]. The store then asks that binding for the
//! [`Selection`], [`SelectedRead`] and [`tail_cache::FastTail`] it steps.
//!
//! The persisted file is a conflict fence, never trust: a reopened store is
//! unbound until its owner binds the same authority again.
use super::tail_cache;
use super::{SelectedRead, Selection};
use crate::retained_proof::RetentionAuthority;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use tilo_relay_store::StoreError;
use tilo_relay_store::bounded::{Step, WorkLimit, WorkStats};
use tilo_relay_store::shell::{
    BatchSelectionOutcome, Publication, SelectedReplay, SelectionOutcome, ShellBinding,
    ShellReplay, ShellSelection, ShellStore, ShellTail,
};

/// The file a store's bound authority is fenced with.
const AUTHORITY_FILE: &str = "shell-authority-v1.json";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedAuthority {
    version: u32,
    authority: RetentionAuthority,
}

/// A [`RetentionAuthority`] bound to one workspace store.
pub struct DesktopBinding {
    authority: RetentionAuthority,
}
impl DesktopBinding {
    /// The bound authority.
    #[must_use]
    pub fn authority(&self) -> &RetentionAuthority {
        &self.authority
    }
}

impl ShellBinding for DesktopBinding {
    fn governor(&self) -> &tilo_user_identity::DeviceCert {
        self.authority.governor()
    }
    fn verify_room_retirement(
        &self,
        store: &dyn ShellStore,
        publisher: [u8; 32],
        body: &[u8],
    ) -> Result<(), StoreError> {
        if publisher != self.authority.governor().device_public_key {
            return Err(super::invalid(
                "retirement publisher is not the pinned governor",
            ));
        }
        let mut selected = SelectedRead::new(store)?
            .ok_or_else(|| super::invalid("room retirement requires selected predecessor"))?;
        while selected.step()? == Step::More {}
        let prior = selected
            .basis
            .room
            .as_ref()
            .ok_or_else(|| super::invalid("room retirement requires selected predecessor"))?;
        crate::retained_proof::verify_room_replacement(
            prior,
            publisher,
            *blake3::hash(b"shell-tree").as_bytes(),
            body,
        )
        .map_err(|_| super::invalid("room retirement predecessor mismatch"))?;
        Ok(())
    }
    fn selection(
        &self,
        store: &dyn ShellStore,
        scratch: PathBuf,
        incoming: Option<[u8; 32]>,
    ) -> Result<Box<dyn ShellSelection>, StoreError> {
        Ok(Box::new(Selection::new(store, scratch, incoming)?))
    }
    fn projected_selection(
        &self,
        store: &dyn ShellStore,
        scratch: PathBuf,
        incoming: Vec<[u8; 32]>,
        lineage_overrides: BTreeMap<OsString, PathBuf>,
    ) -> Result<Box<dyn ShellSelection>, StoreError> {
        Ok(Box::new(Selection::new_projected(
            store,
            scratch,
            incoming,
            lineage_overrides,
        )?))
    }
    fn selected_read(
        &self,
        store: &dyn ShellStore,
    ) -> Result<Option<Box<dyn SelectedReplay>>, StoreError> {
        Ok(SelectedRead::new(store)?.map(|read| Box::new(read) as Box<dyn SelectedReplay>))
    }
    fn checkout_tail(
        &self,
        store: &dyn ShellStore,
        bytes: &[u8],
    ) -> Result<Option<Box<dyn ShellTail>>, StoreError> {
        Ok(tail_cache::checkout(store, bytes)?.map(|tail| Box::new(tail) as Box<dyn ShellTail>))
    }
    fn incoming_route(&self, bytes: &[u8], workspace: u64) -> Option<String> {
        tail_cache::incoming_route(bytes, workspace)
    }
}

impl ShellSelection for Selection {
    fn step(&mut self, limit: WorkLimit) -> Result<Step<SelectionOutcome>, StoreError> {
        Self::step(self, limit)
    }
    fn step_projection(
        &mut self,
        limit: WorkLimit,
    ) -> Result<Step<BatchSelectionOutcome>, StoreError> {
        Self::step_projection(self, limit)
    }
    fn publication_on_error(&self) -> Option<Publication> {
        Self::publication_on_error(self)
    }
    fn current_switch_attempted(&self) -> bool {
        Self::current_switch_attempted(self)
    }
    fn cleanup_paths(&self) -> (PathBuf, PathBuf) {
        Self::cleanup_paths(self)
    }
    fn stats(&self) -> &WorkStats {
        Self::stats(self)
    }
    fn want_route(&mut self, key: String) {
        Self::want_route(self, key);
    }
    fn diagnostic_phase(&self) -> String {
        Self::diagnostic_phase(self)
    }
}

impl SelectedReplay for SelectedRead {
    fn step(&mut self) -> Result<Step<()>, StoreError> {
        Self::step(self)
    }
    fn obsolete(&self, bytes: &[u8]) -> Result<bool, StoreError> {
        Self::obsolete(self, bytes)
    }
}

/// Binding and reading a store's desktop shell authority.
pub trait ShellAuthority {
    /// Install the locally supplied shell pin `authority` before handing this
    /// store to the ordered persistence owner. Persisted bytes are a conflict
    /// fence, never an independent source of trust; an opened store remains
    /// unbound until this call.
    ///
    /// # Errors
    ///
    /// Returns `StoreError` when `authority` names another workspace,
    /// conflicts with the bound or persisted authority, or cannot be
    /// persisted.
    fn bind_shell_authority(&mut self, authority: RetentionAuthority) -> Result<(), StoreError>;
    /// The exact locally supplied authority, absent for generic stores.
    fn shell_authority(&self) -> Option<&RetentionAuthority>;
}

impl<S: ShellStore + ?Sized> ShellAuthority for S {
    fn bind_shell_authority(&mut self, authority: RetentionAuthority) -> Result<(), StoreError> {
        let invalid = |message| StoreError::Io(io::Error::new(io::ErrorKind::InvalidData, message));
        if !tilo_relay_protocol::room_workspace_id_matches(authority.room(), self.workspace_id()) {
            return Err(invalid("shell authority room/workspace mismatch"));
        }
        if self
            .shell_authority()
            .is_some_and(|prior| prior != &authority)
        {
            return Err(invalid("conflicting local shell authority"));
        }
        let path = self.root().join(AUTHORITY_FILE);
        match std::fs::read(&path) {
            Ok(bytes) => {
                let prior: PersistedAuthority =
                    serde_json::from_slice(&bytes).map_err(|e| StoreError::Json(e.to_string()))?;
                if prior.version != 1 || prior.authority != authority {
                    return Err(invalid("conflicting persisted shell authority"));
                }
                // Re-establish the durability barrier even after an interrupted
                // previous attempt left the exact file visible.
                tilo_relay_store::fsync(&std::fs::File::open(&path)?)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let bytes = serde_json::to_vec(&PersistedAuthority {
                    version: 1,
                    authority: authority.clone(),
                })
                .map_err(|e| StoreError::Json(e.to_string()))?;
                tilo_relay_store::atomic_write(&path, &bytes)?;
            }
            Err(error) => return Err(error.into()),
        }
        tilo_relay_store::fsync(&std::fs::File::open(self.root())?)?;
        self.install_shell_binding(Arc::new(DesktopBinding { authority }));
        Ok(())
    }
    fn shell_authority(&self) -> Option<&RetentionAuthority> {
        bound_authority(self.shell_binding()?)
    }
}

/// The authority `binding` holds when this crate's adapter installed it.
pub(crate) fn bound_authority(binding: &Arc<dyn ShellBinding>) -> Option<&RetentionAuthority> {
    let binding: &dyn std::any::Any = binding.as_ref();
    binding
        .downcast_ref::<DesktopBinding>()
        .map(DesktopBinding::authority)
}

/// The desktop's shell replay adapter: binds a [`RetentionAuthority`] read
/// from its serialised form.
#[derive(Clone, Copy, Debug, Default)]
pub struct NeighbourReplay;

impl ShellReplay for NeighbourReplay {
    fn bind(
        &self,
        store: &mut dyn ShellStore,
        authority: &serde_json::Value,
    ) -> Result<(), StoreError> {
        let authority = RetentionAuthority::deserialize(authority)
            .map_err(|error| StoreError::Json(error.to_string()))?;
        store.bind_shell_authority(authority)
    }
}
