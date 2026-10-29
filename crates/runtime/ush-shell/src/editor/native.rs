//! The line at the prompt served to the app as a field ([`crate::field`]):
//! what was published, the lease on it, and the one replacement that can be
//! undone and made again.

use super::Edit;
use super::line::Line;
use crate::field::{Field, LEASE_NS, Lease, Reply, Request, Status, TEXT_BYTES};

/// Where the last replacement stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    None,
    Applied,
    Undone,
}

/// The field's state across the session's prompts.
pub(crate) struct Native {
    generation: u64,
    revision: u64,
    /// The revision the lease was published for, and when it ends.
    lease_revision: u64,
    lease_expiry: u64,
    phase: Phase,
    /// The replacement that can be undone, and the revision it left.
    checkpoint: u64,
    checkpoint_revision: u64,
    before: Line,
    after: Line,
    /// The last request that changed the line, and its receipt.
    last: Option<(u64, Request, Reply)>,
}

impl Default for Native {
    fn default() -> Self {
        Self {
            generation: 1,
            revision: 1,
            lease_revision: 0,
            lease_expiry: 0,
            phase: Phase::None,
            checkpoint: 0,
            checkpoint_revision: 0,
            before: Line::default(),
            after: Line::default(),
            last: None,
        }
    }
}

fn bump(value: &mut u64) {
    *value = value.saturating_add(1);
}

/// The cursor, in bytes of the line.
fn cursor(edit: &Edit) -> u32 {
    u32::try_from(edit.line.before().len()).unwrap_or(u32::MAX)
}

/// Whether the line can be offered: the editor is at its prompt with
/// nothing else under way, and the line is text the app can carry.
fn offerable(edit: &Edit) -> bool {
    let text = edit.line.text();
    edit.search.is_none()
        && edit.waiting.is_none()
        && !edit.accept_when_answered
        && text.len() <= TEXT_BYTES
        && !text.chars().any(char::is_control)
}

const fn lease_of(request: &Request) -> Option<(&Lease, u64)> {
    match request {
        Request::Replace {
            lease, transaction, ..
        }
        | Request::Undo { lease, transaction }
        | Request::Redo { lease, transaction }
        | Request::Query { lease, transaction } => Some((lease, *transaction)),
        Request::Inspect | Request::Revoke => None,
    }
}

fn plain(request: u64, status: Status, transaction: u64) -> Reply {
    Reply {
        request,
        status,
        lease: None,
        transaction,
        can_undo: false,
        can_redo: false,
        text: String::new(),
    }
}

impl Native {
    /// A new prompt: leases on the last one end.
    pub(super) fn prompt(&mut self) {
        self.phase = Phase::None;
        bump(&mut self.generation);
        bump(&mut self.revision);
    }

    /// A key was read: the line is the person's again.
    pub(super) fn key(&mut self) {
        self.phase = Phase::None;
        bump(&mut self.revision);
    }

    /// The prompt is left: the line is no longer offered.
    pub(super) fn leave(&mut self, field: &dyn Field) {
        self.phase = Phase::None;
        bump(&mut self.revision);
        self.publish(field, 0, None);
    }

    /// Say what the field is now, answering `request` (zero: unasked).
    pub(super) fn publish(&mut self, field: &dyn Field, request: u64, edit: Option<&Edit>) {
        let now = field.now_ns();
        let Some(edit) = edit.filter(|edit| offerable(edit) && now > 0) else {
            field.reply(&plain(request, Status::Unavailable, 0));
            return;
        };
        if self.lease_revision == self.revision && now >= self.lease_expiry {
            bump(&mut self.revision);
        }
        if self.lease_revision != self.revision {
            self.lease_revision = self.revision;
            self.lease_expiry = now.saturating_add(LEASE_NS);
        }
        let at = cursor(edit);
        let current = self.checkpoint_revision == self.revision;
        field.reply(&Reply {
            request,
            status: Status::Ready,
            lease: Some(Lease {
                incarnation: field.incarnation(),
                generation: self.generation,
                revision: self.revision,
                expires_ns: self.lease_expiry,
                anchor: at,
                focus: at,
            }),
            transaction: 0,
            can_undo: self.phase == Phase::Applied && current,
            can_redo: self.phase == Phase::Undone && current,
            text: edit.line.text(),
        });
    }

    /// Whether `lease` is the one published for the line as it is.
    fn holds(&self, field: &dyn Field, lease: &Lease, edit: &Edit) -> bool {
        let now = field.now_ns();
        offerable(edit)
            && now > 0
            && now < lease.expires_ns
            && lease.expires_ns.saturating_sub(now) <= LEASE_NS
            && self.lease_revision == self.revision
            && lease.incarnation == field.incarnation()
            && (lease.generation, lease.revision, lease.expires_ns)
                == (self.generation, self.revision, self.lease_expiry)
            && (lease.anchor, lease.focus) == (cursor(edit), cursor(edit))
    }

    /// Serve request `id`. Answers whether the line changed: the caller
    /// draws it again.
    pub(super) fn serve(
        &mut self,
        field: &dyn Field,
        id: u64,
        request: Request,
        edit: &mut Edit,
    ) -> bool {
        let Some((lease, transaction)) = lease_of(&request) else {
            if request == Request::Revoke {
                self.phase = Phase::None;
                bump(&mut self.revision);
                field.reply(&plain(id, Status::Revoked, 0));
                self.publish(field, 0, Some(edit));
            } else {
                self.publish(field, id, Some(edit));
            }
            return false;
        };
        let lease = *lease;
        if transaction == 0 {
            field.reply(&plain(id, Status::Invalid, transaction));
            return false;
        }
        // A query reads the receipt of the edit it names; it never makes it
        // again.
        if matches!(request, Request::Query { .. }) {
            let receipt = self.last.as_ref().filter(|(last, asked, _)| {
                *last == transaction
                    && lease_of(asked).is_some_and(|(l, _)| l.incarnation == lease.incarnation)
            });
            match receipt {
                Some((_, _, reply)) => field.reply(&Reply {
                    request: id,
                    ..reply.clone()
                }),
                None => field.reply(&plain(id, Status::Unknown, transaction)),
            }
            return false;
        }
        // The same request again gets the same receipt.
        if let Some((last, asked, reply)) = &self.last
            && *last == id
        {
            if *asked == request {
                field.reply(reply);
            } else {
                field.reply(&plain(id, Status::Invalid, transaction));
            }
            return false;
        }
        if !self.holds(field, &lease, edit) {
            field.reply(&plain(id, Status::Stale, transaction));
            return false;
        }
        let status = match &request {
            Request::Replace { text, .. } => {
                let (start, end) = (lease.anchor.min(lease.focus), lease.anchor.max(lease.focus));
                let line = edit.line.text();
                let (Ok(start), Ok(end)) = (usize::try_from(start), usize::try_from(end)) else {
                    field.reply(&plain(id, Status::Invalid, transaction));
                    return false;
                };
                let (Some(head), Some(tail)) = (line.get(..start), line.get(end..)) else {
                    field.reply(&plain(id, Status::Invalid, transaction));
                    return false;
                };
                let replaced = format!("{head}{text}{tail}");
                if text.chars().any(char::is_control) || replaced.len() > TEXT_BYTES {
                    field.reply(&plain(id, Status::Invalid, transaction));
                    return false;
                }
                edit.checkpoint(false);
                self.before = edit.line.clone();
                let at = head.chars().count().saturating_add(text.chars().count());
                edit.line = Line::with_cursor(&replaced, at);
                self.after = edit.line.clone();
                self.checkpoint = transaction;
                self.phase = Phase::Applied;
                Status::Applied
            }
            Request::Undo { .. } if self.phase == Phase::Applied && self.current(transaction) => {
                edit.line = self.before.clone();
                self.phase = Phase::Undone;
                Status::Undone
            }
            Request::Redo { .. } if self.phase == Phase::Undone && self.current(transaction) => {
                edit.line = self.after.clone();
                self.phase = Phase::Applied;
                Status::Redone
            }
            _ => {
                field.reply(&plain(id, Status::Stale, transaction));
                return false;
            }
        };
        bump(&mut self.revision);
        self.checkpoint_revision = self.revision;
        let reply = plain(id, status, transaction);
        field.reply(&reply);
        self.last = Some((id, request, reply));
        self.publish(field, 0, Some(edit));
        true
    }

    /// Whether `transaction` is the replacement the line last took, with
    /// nothing changed since.
    const fn current(&self, transaction: u64) -> bool {
        transaction == self.checkpoint && self.checkpoint_revision == self.revision
    }
}
