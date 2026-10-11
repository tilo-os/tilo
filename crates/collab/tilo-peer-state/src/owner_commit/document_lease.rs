//! Additional host-local restrictions, never a wire grant or workspace authority.
//! The platform supplies exact records and an explicit completion deadline from
//! its open/action request. Apps cannot obtain a lease through this module.
use super::{CommitOperation, OwnerCommitRequest};
use std::{
    collections::BTreeMap,
    sync::{Mutex, MutexGuard},
    time::{Instant, SystemTime},
};

/// Minimum access requested by the platform operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentAccess {
    Read,
    Write,
}

/// Explicit selection for a host's consent workflow. Constructing this value
/// grants nothing; the host must separately authenticate the request and both
/// owners, validate existing records and qualify confinement/exec enforcement.
pub struct DocumentIntent {
    app: String,
    room: String,
    records: BTreeMap<String, DocumentAccess>,
    deadline: SystemTime,
}

impl DocumentIntent {
    /// No inferred records, access or duration. Conflicting duplicate records
    /// are refused rather than widened to Write.
    /// # Errors
    /// Returns the missing or ambiguous platform selection.
    pub fn new(
        app: String,
        room: String,
        records: Vec<(String, DocumentAccess)>,
        deadline: SystemTime,
    ) -> Result<Self, String> {
        if app.trim().is_empty() || app == "*" {
            return Err("select one application for the development operation".into());
        }
        if uuid::Uuid::parse_str(&room).is_err() {
            return Err("select one canonical workspace for the development operation".into());
        }
        if deadline <= SystemTime::now() {
            return Err("select a future completion deadline for the development operation".into());
        }
        let mut selected = BTreeMap::new();
        for (record, access) in records {
            // The empty record id is the SDK's existing application root.
            if record == "*" {
                return Err(
                    "select the exact existing records for the development operation".into(),
                );
            }
            if selected
                .insert(record, access)
                .is_some_and(|old| old != access)
            {
                return Err("one record has conflicting requested access".into());
            }
        }
        if selected.is_empty() {
            return Err("select the exact existing records for the development operation".into());
        }
        Ok(Self {
            app,
            room,
            records: selected,
            deadline,
        })
    }
}

struct Status {
    revoked: bool,
    write: bool,
}

/// An additional revocable restriction held only by the host. This does not
/// replace certificate, membership, signature or source-transition checks.
/// In particular, a pidfd/source-stamp callback alone cannot qualify exec
/// enforcement. No launch admission or broker wiring constructs this lease.
pub struct DocumentCommitLease {
    intent: DocumentIntent,
    expires: Instant,
    status: Mutex<Status>,
    execution_current: Box<dyn Fn() -> Result<(), String> + Send + Sync>,
}

impl DocumentCommitLease {
    /// Attach the host's liveness/qualified-supervisor check to its explicit
    /// selection. This is a restriction, not evidence of consent or authority.
    pub fn new(
        intent: DocumentIntent,
        execution_current: Box<dyn Fn() -> Result<(), String> + Send + Sync>,
    ) -> Self {
        let expires = Instant::now()
            .checked_add(
                intent
                    .deadline
                    .duration_since(SystemTime::now())
                    .unwrap_or_default(),
            )
            .unwrap_or_else(Instant::now);
        Self {
            intent,
            expires,
            status: Mutex::new(Status {
                revoked: false,
                write: true,
            }),
            execution_current,
        }
    }

    /// Synchronously fence subsequent turns; an already serialized turn ends
    /// before revocation returns. Poisoning already makes every turn fail.
    pub fn revoke(&self) {
        if let Ok(mut status) = self.status.lock() {
            status.revoked = true;
        }
    }

    /// Withdraw Write without widening the selected Read surface.
    pub fn downgrade_to_read(&self) {
        if let Ok(mut status) = self.status.lock() {
            status.write = false;
        }
    }

    /// Enter under the canonical owner's mutation lock and retain this turn
    /// through durable publication. The owner must refuse absent records.
    /// # Errors
    /// Refuses non-document requests and mismatched, read-only or stale scope.
    pub fn enter(&self, request: &OwnerCommitRequest) -> Result<DocumentCommitTurn<'_>, String> {
        let CommitOperation::Document { app, record, .. } = &request.operation else {
            return Err("document lease cannot authorize another operation family".into());
        };
        if request.room != self.intent.room
            || app != &self.intent.app
            || self.intent.records.get(record) != Some(&DocumentAccess::Write)
        {
            return Err(
                "document write is outside the selected workspace, application or Write records"
                    .into(),
            );
        }
        let turn = DocumentCommitTurn {
            lease: self,
            status: self
                .status
                .lock()
                .map_err(|_| "document lease lock poisoned")?,
        };
        turn.check()?;
        Ok(turn)
    }
}

/// Keeps revocation/downgrade ordered with the owner's actual side effect.
pub struct DocumentCommitTurn<'a> {
    lease: &'a DocumentCommitLease,
    status: MutexGuard<'a, Status>,
}

impl DocumentCommitTurn<'_> {
    /// Recheck immediately before durable publication, not only queue admission.
    /// # Errors
    /// Refuses revocation, downgrade, expiry or an ended/fenced execution.
    pub fn check(&self) -> Result<(), String> {
        if self.status.revoked {
            return Err("document execution lease revoked".into());
        }
        if !self.status.write {
            return Err("document execution lease is now read-only".into());
        }
        if SystemTime::now() >= self.lease.intent.deadline || Instant::now() >= self.lease.expires {
            return Err("document execution lease expired".into());
        }
        (self.lease.execution_current)()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intent_requires_exact_selection_without_inventing_access_or_duration() {
        let room = uuid::Uuid::new_v4().to_string();
        let deadline = SystemTime::now() + std::time::Duration::from_secs(60);
        assert!(DocumentIntent::new("app".into(), room.clone(), Vec::new(), deadline).is_err());
        assert!(
            DocumentIntent::new(
                "app".into(),
                room.clone(),
                vec![("*".into(), DocumentAccess::Read)],
                deadline
            )
            .is_err()
        );
        assert!(
            DocumentIntent::new(
                "app".into(),
                room.clone(),
                vec![(String::new(), DocumentAccess::Read)],
                SystemTime::UNIX_EPOCH
            )
            .is_err()
        );
        assert!(
            DocumentIntent::new(
                "app".into(),
                room.clone(),
                vec![
                    (String::new(), DocumentAccess::Read),
                    (String::new(), DocumentAccess::Write)
                ],
                deadline
            )
            .is_err()
        );
        let selection = DocumentIntent::new(
            "app".into(),
            room,
            vec![(String::new(), DocumentAccess::Read)],
            deadline,
        )
        .unwrap();
        assert_eq!(selection.records.len(), 1);
        assert_eq!(selection.records.get(""), Some(&DocumentAccess::Read));
        assert_eq!(selection.deadline, deadline);
    }
}
