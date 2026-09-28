//! File-owned records consume native history without opening another writer.
use super::{AutoCommit, RecordCheckout, RecordSet, validate_identity};

/// One original SDK capture awaiting consumption by the file record owner.
/// This value carries data and the capture acknowledgement, not authority.
pub struct InitialFileHistory {
    document: AutoCommit,
    capture: crate::collaboration::CommittedDocument,
}
impl InitialFileHistory {
    /// Install this exact record history before any file import/defaults.
    /// The caller must settle the outgoing record through its normal save or
    /// discard flow first. No cache writer or new graph is created here.
    pub fn install_into(self, document: &mut AutoCommit) -> Result<(), String> {
        self.capture.with_mutation(|_| ())?;
        *document = self.document;
        self.capture.applied();
        Ok(())
    }
}
impl RecordSet {
    /// Retain the selected native read mode while keeping the file as the only
    /// local durable writer. Missing native construction never selects Local.
    pub fn for_local_files(manifest_id: impl Into<String>) -> Result<Self, String> {
        let bridge = crate::collaboration::native_initial::file_bridge()?;
        let mut set = Self::open(manifest_id);
        set.persistent = false;
        set.native_initial = bridge;
        set.file_mode_selected = true;
        Ok(set)
    }
    /// Retain this exact selected file scope for later opens after construction.
    pub fn file_history_reader(&self) -> Result<FileHistoryReader, String> {
        if !self.file_mode_selected
            || self.persistent
            || self.backend.is_some()
            || self.portable.is_some()
        {
            return Err("file history requires the file-owned record set".into());
        }
        Ok(FileHistoryReader {
            package_id: self.package_id.clone(),
            home: self.home.clone(),
            bridge: self.native_initial.clone(),
        })
    }
    pub fn initial_file_history(
        &self,
        want: &RecordCheckout,
    ) -> Result<Option<InitialFileHistory>, String> {
        self.file_history_reader()?.read(want)
    }
}

/// Read capability plumbing retained from the actual construction worker.
/// Clones share its weak transport and exact package/room scope; no writer or
/// independent authority is created.
#[derive(Clone)]
pub struct FileHistoryReader {
    package_id: String,
    home: String,
    bridge: Option<crate::collaboration::native_initial::InitialReadBridge>,
}
impl FileHistoryReader {
    /// Only an explicitly selected local file mode can construct a new local
    /// document. Required native creation needs its separate governor door.
    pub fn require_local_creation(&self) -> Result<(), String> {
        if self.bridge.is_some() {
            return Err("native document creation requires governor admission".into());
        }
        Ok(())
    }
    /// None is the retained, positively selected local mode. A required read
    /// that is missing, replaced or closed always returns an error.
    pub fn read(&self, want: &RecordCheckout) -> Result<Option<InitialFileHistory>, String> {
        if !want.is_valid_local_storage_request() {
            return Err("file record identity is invalid".into());
        }
        let Some(bridge) = &self.bridge else {
            return Ok(None);
        };
        let capture = bridge.record(&self.package_id, &self.home, want)?;
        let document = capture.with_mutation(|mutation| {
            let collab_types::DocumentMutation::Snapshot(bytes) = mutation else {
                return Err("initial file history is not a snapshot".to_string());
            };
            let document = AutoCommit::load(bytes).map_err(|error| error.to_string())?;
            validate_identity(&document, want, &self.home).map_err(|error| error.to_string())?;
            Ok(document)
        })??;
        Ok(Some(InitialFileHistory { document, capture }))
    }
}
