//! Private, acknowledged edits. Neither replies nor transcripts are app events.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tilo_clock::time::{Duration, Instant};
use tilo_lock::MutexRecover as _;

#[derive(Clone, PartialEq, Eq)]
pub struct Lease(String, bool);

impl Lease {
    /// The lease a reply names: 32 lowercase hex digits, single-line unless its multiline
    /// flag says otherwise.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        (value.len() == 32
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
        .then(|| Self(value.into(), false))
    }

    pub(crate) fn stamp(&self, key: &[u8; 32]) -> [u8; 32] {
        crate::edit_fingerprint(&self.0, usize::from(self.1), 0, key)
    }
}

pub enum Request {
    History {
        lease: Lease,
        request: crate::UndoRequest,
    },
    Prepare,
    Start(Lease),
    Check(Lease),
    Cancel(Lease),
    Commit {
        lease: Lease,
        text: zeroize::Zeroizing<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    Busy,
    Retired,
    Rejected,
    Unknown,
}

/// A local receipt shared with the original caller, independent of widget lifetime.
#[derive(Clone)]
pub struct Receipt {
    result: Arc<Mutex<Option<Result<Option<Lease>, Failure>>>>,
    canceled: Arc<AtomicBool>,
    history: Arc<Mutex<Option<(crate::UndoStatus, Instant)>>>,
    deadline: Instant,
}

/// The original host's one-shot completion end. Completing it grants no input authority;
/// the host must first bind delivery to its original field owner.
pub struct Resolver(Receipt);
impl Resolver {
    pub fn complete(self, result: Result<Option<Lease>, Failure>) {
        self.0.complete(result);
    }
}

impl Receipt {
    /// A bounded private host receipt and its one-shot completion end.
    pub fn channel() -> (Self, Resolver) {
        let receipt = Self::pending();
        (receipt.clone(), Resolver(receipt))
    }

    /// Observe cancellation of this exact scope, including after start ACK.
    pub fn was_canceled(&self) -> bool {
        self.is_canceled()
    }
    /// A receipt nothing has completed yet.
    #[must_use]
    pub fn pending() -> Self {
        Self {
            result: Arc::new(Mutex::new(None)),
            canceled: Arc::new(AtomicBool::new(false)),
            history: Arc::new(Mutex::new(None)),
            deadline: tilo_clock::now() + Duration::from_secs(10),
        }
    }

    fn history_status(&self) -> Option<crate::UndoStatus> {
        let history = self.history.lock().ok()?;
        let (status, expires) = history.as_ref()?;
        (tilo_clock::now() < *expires && !self.is_canceled()).then(|| status.clone())
    }

    pub fn failed(failure: Failure) -> Self {
        let receipt = Self::pending();
        receipt.complete(Err(failure));
        receipt
    }

    pub fn poll(&self) -> Option<Result<Option<Lease>, Failure>> {
        let mut result = self.result.lock_recover();
        if result.is_none() && tilo_clock::now() >= self.deadline {
            *result = Some(Err(Failure::Unknown));
        }
        result.clone()
    }

    /// Stop a final that has not been admitted. After admission its outcome is uncertain
    /// until the original native reply; never synthesize an undo.
    pub fn cancel(&self) {
        self.canceled.store(true, Ordering::Release);
        self.complete(Err(Failure::Unknown));
    }

    fn is_canceled(&self) -> bool {
        self.canceled.load(Ordering::Acquire)
    }

    /// Complete a receipt once; a later completion is ignored.
    pub fn complete(&self, value: Result<Option<Lease>, Failure>) {
        let mut result = self.result.lock_recover();
        if result.is_none() {
            *result = Some(value);
        }
    }
}

#[derive(Default)]
pub struct Transactions {
    next: u32,
    pending: std::collections::HashMap<u32, (Receipt, bool)>,
}

impl Transactions {
    pub fn admit(&mut self, request: Request) -> Option<(Receipt, serde_json::Value)> {
        self.pending
            .retain(|_, (receipt, _)| receipt.poll().is_none());
        if self.pending.len() >= 4 {
            return None;
        }
        let id = self
            .next
            .checked_add(1)
            .filter(|id| i32::try_from(*id).is_ok())?;
        let (action, lease, text, returns_lease) = match request {
            Request::History { lease, request } => (
                match request {
                    crate::UndoRequest::Undo => "undo_text_input",
                    crate::UndoRequest::Redo => "redo_text_input",
                },
                Some(lease),
                None,
                false,
            ),
            Request::Prepare => ("prepare_text_input", None, None, true),
            Request::Start(lease) => ("start_text_input", Some(lease), None, true),
            Request::Check(lease) => ("check_text_input", Some(lease), None, true),
            Request::Cancel(lease) => ("cancel_text_input", Some(lease), None, false),
            Request::Commit { lease, text } => {
                if text.is_empty() || text.len() > 4000 {
                    return None;
                }
                ("commit_text_input", Some(lease), Some(text), false)
            }
        };
        let expires = tilo_clock::wall()
            .duration_since(tilo_clock::time::UNIX_EPOCH)
            .ok()?
            .as_millis()
            .checked_add(5000)
            .and_then(|ms| u64::try_from(ms).ok())?;
        let mut value = serde_json::json!({"op":"text_input","request_id":id,"action":action,"expires_ms":expires});
        if matches!(action, "undo_text_input" | "redo_text_input") {
            value["history_scope"] = serde_json::json!("page");
        }
        if let Some(lease) = lease {
            value["lease"] = serde_json::json!(lease.0);
        }
        if let Some(text) = text {
            value["text"] = serde_json::json!(&*text);
        }
        let receipt = Receipt::pending();
        self.pending.insert(id, (receipt.clone(), returns_lease));
        self.next = id;
        Some((receipt, value))
    }

    pub fn reply(&mut self, value: &serde_json::Value) {
        let Some(id) = value["request_id"]
            .as_u64()
            .and_then(|id| u32::try_from(id).ok())
        else {
            return;
        };
        let Some((receipt, returns_lease)) = self.pending.remove(&id) else {
            return;
        };
        let mut result = if value["ok"] == true {
            if returns_lease {
                value["data"]["lease"]
                    .as_str()
                    .and_then(Lease::parse)
                    .and_then(|mut lease| {
                        lease.1 = match value["data"].get("multiline") {
                            None => false,
                            Some(policy) => policy.as_bool()?,
                        };
                        Some(lease)
                    })
                    .map(Some)
                    .ok_or(Failure::Unknown)
            } else {
                Ok(None)
            }
        } else if value["code"] == "busy" {
            Err(Failure::Busy)
        } else if value["code"] == "text_input_retired" {
            Err(Failure::Retired)
        } else {
            let code = value["code"].as_str().unwrap_or("");
            Err(
                if matches!(
                    code,
                    "busy"
                        | "text_input_changed"
                        | "text_input_composition"
                        | "text_input_retired"
                        | "invalid_request"
                        | "secret_field"
                        | "not_editable"
                        | "text_input_large"
                        | "text_input_expired"
                        | "rejected"
                        | "hidden_view"
                ) {
                    Failure::Rejected
                } else {
                    Failure::Unknown
                },
            )
        };
        if returns_lease
            && matches!(result, Ok(Some(_)))
            && let Some(history) = value.get("history")
        {
            if let (Some(can_undo), Some(can_redo)) =
                (history["can_undo"].as_bool(), history["can_redo"].as_bool())
                && history.as_object().is_some_and(|fields| fields.len() == 2)
            {
                *receipt.history.lock_recover() = Some((
                    crate::UndoStatus {
                        can_undo,
                        can_redo,
                        ..Default::default()
                    },
                    tilo_clock::now() + Duration::from_millis(500),
                ));
            } else {
                result = Err(Failure::Unknown);
            }
        }
        receipt.complete(result);
    }

    pub fn unavailable(&mut self) {
        for (_, (receipt, _)) in self.pending.drain() {
            receipt.complete(Err(Failure::Unknown));
        }
    }
}

#[derive(Clone)]
enum Operation {
    Prepare,
    Start(Lease),
    Check(Lease),
    Cancel(Lease),
    Commit(Lease),
}
struct Pending {
    receipt: Receipt,
    operation: Operation,
    final_receipt: Option<Receipt>,
}
// Retirement acknowledges internal lease cleanup. The original caller sees a known
// refusal regardless of whether a heartbeat or the final observed it first.
fn public_failure(failure: Failure) -> Failure {
    if failure == Failure::Retired {
        Failure::Rejected
    } else {
        failure
    }
}

struct Activation {
    lease: Lease,
    receipt: Receipt,
}

enum FinalValue {
    Text(zeroize::Zeroizing<String>),
    History(crate::UndoRequest),
}

struct Final {
    lease: Lease,
    text: FinalValue,
    receipt: Receipt,
    needs_check: bool,
}

/// An original-field transaction.
pub struct Guard {
    lease: Option<Lease>,
    history: Option<Receipt>,
    pending: Option<Pending>,
    retiring: Option<Lease>,
    final_text: Option<Final>,
    activation: Option<Activation>,
    active_receipt: Option<Receipt>,
    activated: bool,
    poll_at: Instant,
    next: Instant,
    revoked: bool,
}
impl Default for Guard {
    fn default() -> Self {
        Self {
            lease: None,
            history: None,
            pending: None,
            retiring: None,
            final_text: None,
            activation: None,
            active_receipt: None,
            activated: false,
            poll_at: tilo_clock::now(),
            next: tilo_clock::now(),
            revoked: false,
        }
    }
}
impl Guard {
    pub fn invalidate(&mut self, _engine: &super::SharedEngine) {
        self.history = None;
        if let Some(receipt) = self.active_receipt.take() {
            receipt.cancel();
        }
        if let Some(lease) = self.lease.take() {
            self.retiring = Some(lease);
        }
        if let Some(final_text) = self.final_text.take() {
            final_text.receipt.complete(Err(Failure::Rejected));
            self.retiring = Some(final_text.lease);
        }
        if let Some(activation) = self.activation.take() {
            activation.receipt.complete(Err(Failure::Rejected));
        }
        self.revoked = true;
        self.next = tilo_clock::now();
    }

    fn completed(
        &mut self,
        pending: Pending,
        result: Result<Option<Lease>, Failure>,
        focused: bool,
    ) {
        if matches!(result, Ok(Some(_))) {
            self.history = Some(pending.receipt.clone());
        }
        match pending.operation {
            Operation::Prepare => match result {
                Ok(Some(lease)) if focused && !self.revoked => {
                    self.lease = Some(lease);
                    self.active_receipt = None;
                    self.activated = false;
                }
                Ok(Some(lease)) => self.retiring = Some(lease),
                _ => self.lease = None,
            },
            Operation::Start(lease) => {
                let unchanged = matches!(&result, Ok(Some(current)) if *current == lease);
                let admitted = pending
                    .final_receipt
                    .as_ref()
                    .is_some_and(|receipt| receipt.poll().is_none());
                if unchanged && focused && !self.revoked && admitted {
                    self.active_receipt.clone_from(&pending.final_receipt);
                    self.next = tilo_clock::now() + Duration::from_millis(500);
                    if let Some(receipt) = pending.final_receipt {
                        receipt.complete(Ok(None));
                    }
                } else {
                    self.lease = None;
                    self.retiring = Some(lease);
                    if let Some(receipt) = pending.final_receipt {
                        receipt.complete(Err(public_failure(
                            result.err().unwrap_or(Failure::Rejected),
                        )));
                    }
                }
            }
            Operation::Check(lease) => {
                let unchanged = matches!(&result, Ok(Some(current)) if *current == lease);
                if unchanged {
                    if let Some(final_text) = self.final_text.as_mut() {
                        final_text.needs_check = false;
                    }
                }
                if !unchanged && !matches!(result, Err(Failure::Busy)) {
                    if let Some(receipt) = self.active_receipt.take() {
                        receipt.cancel();
                    }
                    self.lease = None;
                    if !matches!(result, Err(Failure::Retired)) {
                        self.retiring = Some(lease);
                    }
                    if let Some(final_text) = self.final_text.take() {
                        final_text.receipt.complete(Err(public_failure(
                            result.err().unwrap_or(Failure::Unknown),
                        )));
                    }
                } else if self.revoked || !focused {
                    self.lease = None;
                    self.retiring = Some(lease);
                }
            }
            Operation::Cancel(lease) => {
                if matches!(result, Ok(None) | Err(Failure::Retired)) {
                    self.retiring = None;
                } else {
                    self.retiring = Some(lease);
                }
            }
            Operation::Commit(lease) => {
                self.active_receipt = None;
                if !matches!(result, Ok(None) | Err(Failure::Retired)) {
                    self.retiring = Some(lease);
                }
                if let Some(receipt) = pending.final_receipt {
                    receipt.complete(result.map_err(public_failure));
                }
            }
        }
    }

    pub fn pump(&mut self, engine: &super::SharedEngine, focused: bool) {
        self.poll_at = tilo_clock::now() + Duration::from_millis(50);
        if self
            .active_receipt
            .as_ref()
            .is_some_and(Receipt::is_canceled)
        {
            self.invalidate(engine);
        }
        if let Some(pending) = self.pending.as_ref() {
            let Some(result) = pending.receipt.poll() else {
                return;
            };
            let Some(pending) = self.pending.take() else {
                return;
            };
            self.completed(pending, result, focused);
        }
        if !focused && !self.revoked {
            self.invalidate(engine);
        }
        if let Some(activation) = self.activation.take() {
            if activation.receipt.poll().is_some()
                || self.revoked
                || !focused
                || self.lease.as_ref() != Some(&activation.lease)
            {
                activation.receipt.complete(Err(Failure::Rejected));
                self.invalidate(engine);
                self.retiring = Some(activation.lease);
            } else {
                self.pending = engine
                    .text_input(Request::Start(activation.lease.clone()))
                    .map(|receipt| Pending {
                        receipt,
                        operation: Operation::Start(activation.lease.clone()),
                        final_receipt: Some(activation.receipt.clone()),
                    });
                if self.pending.is_none() {
                    activation.receipt.complete(Err(Failure::Rejected));
                    self.invalidate(engine);
                    self.retiring = Some(activation.lease);
                }
                return;
            }
        }
        if let Some(final_text) = self.final_text.take() {
            if final_text.receipt.poll().is_some() || self.revoked || !focused {
                final_text.receipt.complete(Err(Failure::Rejected));
                self.retiring = Some(final_text.lease);
            } else if final_text.needs_check {
                if tilo_clock::now() >= self.next {
                    self.pending = engine
                        .text_input(Request::Check(final_text.lease.clone()))
                        .map(|receipt| Pending {
                            receipt,
                            operation: Operation::Check(final_text.lease.clone()),
                            final_receipt: None,
                        });
                    self.next = tilo_clock::now() + Duration::from_millis(500);
                }
                self.final_text = Some(final_text);
                return;
            } else {
                let lease = final_text.lease;
                let request = match final_text.text {
                    FinalValue::Text(text) => Request::Commit {
                        lease: lease.clone(),
                        text,
                    },
                    FinalValue::History(request) => Request::History {
                        lease: lease.clone(),
                        request,
                    },
                };
                let receipt = engine.text_input(request);
                if let Some(receipt) = receipt {
                    self.pending = Some(Pending {
                        receipt,
                        operation: Operation::Commit(lease),
                        final_receipt: Some(final_text.receipt),
                    });
                } else {
                    final_text.receipt.complete(Err(Failure::Rejected));
                    self.retiring = Some(lease);
                }
                return;
            }
        }
        if tilo_clock::now() < self.next {
            return;
        }
        self.next = tilo_clock::now() + Duration::from_millis(500);
        let (request, operation) = if let Some(lease) = self.retiring.clone() {
            (Request::Cancel(lease.clone()), Operation::Cancel(lease))
        } else if focused {
            self.revoked = false;
            self.lease
                .clone()
                .map_or((Request::Prepare, Operation::Prepare), |lease| {
                    (Request::Check(lease.clone()), Operation::Check(lease))
                })
        } else {
            return;
        };
        self.pending = engine.text_input(request).map(|receipt| Pending {
            receipt,
            operation,
            final_receipt: None,
        });
    }
    pub fn history_status(&self) -> Option<crate::UndoStatus> {
        if !self.ready() || self.revoked || self.activated {
            return None;
        }
        let receipt = self.history.as_ref()?;
        let Ok(Some(lease)) = receipt.poll()? else {
            return None;
        };
        (self.lease.as_ref() == Some(&lease))
            .then(|| receipt.history_status())
            .flatten()
    }

    pub fn stamp(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        Some(self.lease.as_ref()?.stamp(key))
    }
    pub fn multiline(&self) -> bool {
        self.ready() && self.lease.as_ref().is_some_and(|lease| lease.1)
    }
    pub fn ready(&self) -> bool {
        self.lease.is_some()
            && self
                .active_receipt
                .as_ref()
                .is_none_or(|receipt| !receipt.is_canceled())
            && self.final_text.is_none()
            && self
                .activation
                .as_ref()
                .is_none_or(|activation| activation.receipt.poll().is_none())
            && self.pending.as_ref().is_none_or(|pending| {
                !matches!(pending.operation, Operation::Start(_))
                    || pending
                        .final_receipt
                        .as_ref()
                        .is_some_and(|receipt| receipt.poll().is_none())
            })
    }
    pub fn has_work(&self) -> bool {
        self.pending.is_some()
            || self.final_text.is_some()
            || self.activation.is_some()
            || self.retiring.is_some()
    }
    pub fn deadline(&self) -> Instant {
        if self.pending.is_some() || self.final_text.is_some() || self.activation.is_some() {
            self.poll_at
        } else {
            self.next
        }
    }
    /// Explicit consent starts one bounded session on the existing lease.
    pub fn start(&mut self) -> Option<Receipt> {
        if self.revoked || self.activated || self.final_text.is_some() || self.retiring.is_some() {
            return None;
        }
        let lease = self.lease.clone()?;
        let receipt = Receipt::pending();
        self.activated = true;
        self.activation = Some(Activation {
            lease,
            receipt: receipt.clone(),
        });
        self.next = tilo_clock::now();
        self.poll_at = self.next;
        Some(receipt)
    }

    pub fn history(&mut self, request: crate::UndoRequest) -> Option<Receipt> {
        if self.activated {
            return None;
        }
        self.queue_final(FinalValue::History(request))
    }
    pub fn commit(&mut self, _engine: &super::SharedEngine, text: &str) -> Option<Receipt> {
        if text.is_empty() || text.len() > 4000 {
            return None;
        }
        self.queue_final(FinalValue::Text(zeroize::Zeroizing::new(text.into())))
    }
    fn queue_final(&mut self, value: FinalValue) -> Option<Receipt> {
        if self.final_text.is_some()
            || self.activation.is_some()
            || self
                .pending
                .as_ref()
                .is_some_and(|pending| matches!(pending.operation, Operation::Start(_)))
            || self.retiring.is_some()
            || self.revoked
        {
            return None;
        }
        let lease = self.lease.take()?;
        let receipt = Receipt::pending();
        self.final_text = Some(Final {
            lease,
            text: value,
            receipt: receipt.clone(),
            needs_check: self
                .pending
                .as_ref()
                .is_some_and(|pending| matches!(pending.operation, Operation::Check(_))),
        });
        self.next = tilo_clock::now();
        self.poll_at = self.next;
        Some(receipt)
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(activation) = &self.activation {
            activation.receipt.complete(Err(Failure::Unknown));
        }
        if let Some(final_text) = &self.final_text {
            final_text.receipt.complete(Err(Failure::Unknown));
        }
        if let Some(pending) = &self.pending {
            if let Some(receipt) = &pending.final_receipt {
                receipt.complete(Err(Failure::Unknown));
            }
        }
    }
}
