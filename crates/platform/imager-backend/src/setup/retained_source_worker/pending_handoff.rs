//! Root-owned NEW unread stream to the same actual source provider.
use super::{RetainedSourceWorker, refused};
use crate::retained_source::{
    records::{self, HandoffRole},
    workspace::ControlStream as UnixStream,
};
use launcher::owned_service::RetainedWorkerWitness;
use runtime_fs::directory_backend::WorkspacePendingReplacementBackend;
use runtime_portal::retained_role::NativeHistoryPendingHandoff;
use std::{
    fs::File,
    io::{self, Write},
    net::Shutdown,
    sync::Arc,
    time::Duration,
};
/// Retained by the root coordinator through daemon transfer/commit or observed
/// source retirement. Its descriptors are descriptive evidence and transport,
/// never a constructor for source execution or daemon history authority.
pub struct PendingHandoff {
    stream: UnixStream,
    sealed: File,
    program: File,
    witness: RetainedWorkerWitness,
    seed: Arc<super::super::retained_selected_edit::RootSelectedReplacementSeed>,
    pending: Arc<dyn WorkspacePendingReplacementBackend>,
    record: NativeHistoryPendingHandoff,
}
impl PendingHandoff {
    pub fn check(&self) -> io::Result<()> {
        self.seed.check()?;
        self.witness.check()?;
        self.pending.check().map_err(io::Error::other)?;
        let observed = self.pending.observe().map_err(io::Error::other)?;
        if self.record.enrollment_context_digest != self.seed.history_context_digest()?
            || self.record.source_owner != observed.source_owner
            || self.record.operation_id != uuid::Uuid::from_bytes(observed.operation).to_string()
            || self.record.intent_digest != observed.intent_digest
        {
            return Err(refused());
        }
        self.witness.check()?;
        self.seed.check()
    }
    /// Borrow for existing enrolled daemon SCM_RIGHTS handoff. No duplication of
    /// the root's active Capnp connection and no QPE frame is consumed here.
    pub fn descriptors(&self) -> io::Result<(&File, &UnixStream, &File)> {
        self.check()?;
        if records::file_identity(&self.program)? != self.record.worker_program {
            return Err(refused());
        }
        Ok((&self.sealed, &self.stream, &self.program))
    }
    /// Observe release of ONLY this accepted daemon RPC server and its callbacks.
    /// Caller must first admit actual successor ACK + fresh Reader continuity.
    /// No namespace stop is issued; the shared source witness must stay live.
    pub fn retire_rpc(&self) -> io::Result<()> {
        self.witness.check_retirement_custody()?;
        self.pending
            .retire_daemon_handoff(self.record.ticket)
            .map_err(io::Error::other)?;
        self.witness.check_retirement_custody()
    }
    pub fn pending(&self) -> &Arc<dyn WorkspacePendingReplacementBackend> {
        &self.pending
    }
}
impl Drop for PendingHandoff {
    fn drop(&mut self) {
        // Exact fresh stream only. This cannot cancel or close the original Files
        // connection; the editor source owner retains unknown native effects.
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}
fn seal(bytes: &[u8]) -> io::Result<File> {
    records::seal(bytes, HandoffRole::Pending)
}
impl RetainedSourceWorker {
    /// Call only after the parent's exclusive LiveLineage Pending admission and
    /// callback drain, before allowing the daemon to submit EXCHANGE.
    pub fn connect_pending_handoff(
        &self,
        pending: Arc<dyn WorkspacePendingReplacementBackend>,
    ) -> io::Result<PendingHandoff> {
        let before = self.check_current_custody()?;
        let seed = self.replacement_seed.as_ref().ok_or_else(refused)?.clone();
        seed.check()?;
        pending.check().map_err(io::Error::other)?;
        let observed = pending.observe().map_err(io::Error::other)?;
        observed.validate().map_err(io::Error::other)?;
        if observed.source_owner
            != self
                .authority
                .as_ref()
                .ok_or_else(refused)?
                .native_owner_sha256()?
        {
            return Err(refused());
        }
        let mut challenge = [0; 32];
        challenge[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        challenge[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        let ticket = pending
            .arm_daemon_handoff(challenge)
            .map_err(io::Error::other)?;
        let (mut stream, witness) = self
            .endpoint
            .as_ref()
            .ok_or_else(refused)?
            .connect_pending_worker()?;
        let program = witness.held_program()?;
        let record = NativeHistoryPendingHandoff {
            schema: 1,
            enrollment_context_digest: seed.history_context_digest()?,
            source_owner: observed.source_owner,
            operation_id: uuid::Uuid::from_bytes(observed.operation).to_string(),
            intent_digest: observed.intent_digest,
            worker_pid: witness.pid(),
            worker_start: witness.start_time(),
            worker_uid: witness.uid(),
            worker_gid: witness.gid(),
            worker_program: witness.program_identity(),
            ticket,
            challenge,
        };
        let sealed = seal(&record.encode()?)?;
        if self.check_current_custody()? != before {
            return Err(refused());
        }
        witness.check()?;
        seed.check()?;
        if pending.observe().map_err(io::Error::other)? != observed {
            return Err(refused());
        }
        // Only a fixed 52-byte prelude. QPE remains unread for the independently
        // authenticated daemon; the source supervisor's total deadline is live.
        stream.set_write_timeout(Some(Duration::from_secs(1)))?;
        let mut prelude = [0; 52];
        prelude[..4].copy_from_slice(b"RPH1");
        prelude[4..20].copy_from_slice(&ticket);
        prelude[20..].copy_from_slice(&challenge);
        if stream.write(&prelude)? != prelude.len() {
            return Err(io::Error::other(
                "partial source handoff prelude; no replay",
            ));
        }
        stream.set_write_timeout(None)?;
        let handoff = PendingHandoff {
            stream,
            sealed,
            program,
            witness,
            seed,
            pending,
            record,
        };
        handoff.check()?;
        if self.check_current_custody()? != before {
            return Err(refused());
        }
        Ok(handoff)
    }
}
