//! Private, acknowledged edits. Neither replies nor transcripts are app events.
use std::sync::{Arc, Mutex};

use crate::time::{Duration, Instant};

#[derive(Clone, PartialEq, Eq)]
pub struct Lease(String);

impl Lease {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        (value.len() == 32
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
        .then(|| Self(value.into()))
    }

    pub(crate) fn stamp(&self, key: &[u8; 32]) -> [u8; 32] {
        crate::ime::edit_fingerprint(&self.0, 0, 0, key)
    }
}

pub enum Request {
    Prepare,
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
    /// No acknowledgement: do not infer success or automatically retry.
    Unknown,
}

/// A local receipt shared with the original caller, independent of widget lifetime.
#[derive(Clone)]
pub struct Receipt {
    result: Arc<Mutex<Option<Result<Option<Lease>, Failure>>>>,
    deadline: Instant,
}

/// The original host's one-shot completion end. Completing it grants no input
/// authority; the host must first bind delivery to its original field owner.
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
    pub(crate) fn pending() -> Self {
        Self {
            result: Arc::new(Mutex::new(None)),
            deadline: crate::time::now() + Duration::from_secs(10),
        }
    }

    pub fn failed(failure: Failure) -> Self {
        let receipt = Self::pending();
        receipt.complete(Err(failure));
        receipt
    }

    pub fn poll(&self) -> Option<Result<Option<Lease>, Failure>> {
        let mut result = self
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if result.is_none() && crate::time::now() >= self.deadline {
            *result = Some(Err(Failure::Unknown));
        }
        result.clone()
    }

    /// Stop a final that has not been admitted. After admission its outcome is
    /// uncertain until the original native reply; never synthesize an undo.
    pub fn cancel(&self) {
        self.complete(Err(Failure::Unknown));
    }

    pub(crate) fn complete(&self, value: Result<Option<Lease>, Failure>) {
        let mut result = self
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if result.is_none() {
            *result = Some(value);
        }
    }
}

#[derive(Default)]
pub(crate) struct Transactions {
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
            Request::Prepare => ("prepare_text_input", None, None, true),
            Request::Check(lease) => ("check_text_input", Some(lease), None, true),
            Request::Cancel(lease) => ("cancel_text_input", Some(lease), None, false),
            Request::Commit { lease, text } => {
                if text.is_empty() || text.len() > 4000 {
                    return None;
                }
                ("commit_text_input", Some(lease), Some(text), false)
            }
        };
        let expires = crate::time::wall()
            .duration_since(crate::time::UNIX_EPOCH)
            .ok()?
            .as_millis()
            .checked_add(5000)
            .and_then(|ms| u64::try_from(ms).ok())?;
        let mut value = serde_json::json!({"op":"text_input","request_id":id,"action":action,"expires_ms":expires});
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
        let result = if value["ok"] == true {
            if returns_lease {
                value["data"]["lease"]
                    .as_str()
                    .and_then(Lease::parse)
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
    Check(Lease),
    Cancel(Lease),
    Commit(Lease),
}
struct Pending {
    receipt: Receipt,
    operation: Operation,
    final_receipt: Option<Receipt>,
}
// Retirement acknowledges internal lease cleanup. The original caller sees a
// known refusal regardless of whether a heartbeat or the final observed it first.
fn public_failure(failure: Failure) -> Failure {
    if failure == Failure::Retired {
        Failure::Rejected
    } else {
        failure
    }
}

struct Final {
    lease: Lease,
    text: zeroize::Zeroizing<String>,
    receipt: Receipt,
    needs_check: bool,
}

/// An original-field transaction. Never replace a queued final's target while
/// an earlier check is pending; retirement is acknowledged before re-preparing.
pub(crate) struct Guard {
    lease: Option<Lease>,
    pending: Option<Pending>,
    retiring: Option<Lease>,
    final_text: Option<Final>,
    next: Instant,
    revoked: bool,
}
impl Default for Guard {
    fn default() -> Self {
        Self {
            lease: None,
            pending: None,
            retiring: None,
            final_text: None,
            next: crate::time::now(),
            revoked: false,
        }
    }
}
impl Guard {
    pub fn invalidate(&mut self, _engine: &super::SharedEngine) {
        if let Some(lease) = self.lease.take() {
            self.retiring = Some(lease);
        }
        if let Some(final_text) = self.final_text.take() {
            final_text.receipt.complete(Err(Failure::Rejected));
            self.retiring = Some(final_text.lease);
        }
        self.revoked = true;
        self.next = crate::time::now();
    }

    fn completed(
        &mut self,
        pending: Pending,
        result: Result<Option<Lease>, Failure>,
        focused: bool,
    ) {
        match pending.operation {
            Operation::Prepare => match result {
                Ok(Some(lease)) if focused && !self.revoked => self.lease = Some(lease),
                Ok(Some(lease)) => self.retiring = Some(lease),
                _ => self.lease = None,
            },
            Operation::Check(lease) => {
                let unchanged = matches!(&result, Ok(Some(current)) if *current == lease);
                if unchanged {
                    if let Some(final_text) = self.final_text.as_mut() {
                        final_text.needs_check = false;
                    }
                }
                if !unchanged && !matches!(result, Err(Failure::Busy)) {
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
        if let Some(final_text) = self.final_text.take() {
            if final_text.receipt.poll().is_some() || self.revoked || !focused {
                final_text.receipt.complete(Err(Failure::Rejected));
                self.retiring = Some(final_text.lease);
            } else if final_text.needs_check {
                if crate::time::now() >= self.next {
                    self.pending = engine
                        .text_input(Request::Check(final_text.lease.clone()))
                        .map(|receipt| Pending {
                            receipt,
                            operation: Operation::Check(final_text.lease.clone()),
                            final_receipt: None,
                        });
                    self.next = crate::time::now() + Duration::from_millis(500);
                }
                self.final_text = Some(final_text);
                return;
            } else {
                let lease = final_text.lease;
                let receipt = engine.text_input(Request::Commit {
                    lease: lease.clone(),
                    text: final_text.text,
                });
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
        if crate::time::now() < self.next {
            return;
        }
        self.next = crate::time::now() + Duration::from_millis(500);
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
    pub fn stamp(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        Some(self.lease.as_ref()?.stamp(key))
    }
    pub fn ready(&self) -> bool {
        self.lease.is_some() && self.final_text.is_none()
    }
    pub fn has_work(&self) -> bool {
        self.pending.is_some() || self.final_text.is_some() || self.retiring.is_some()
    }
    pub fn deadline(&self) -> Instant {
        if self.pending.is_some() || self.final_text.is_some() {
            crate::time::now() + Duration::from_millis(50)
        } else {
            self.next
        }
    }
    pub fn commit(&mut self, _engine: &super::SharedEngine, text: &str) -> Option<Receipt> {
        if self.final_text.is_some()
            || self.retiring.is_some()
            || text.is_empty()
            || text.len() > 4000
        {
            return None;
        }
        let lease = self.lease.take()?;
        let receipt = Receipt::pending();
        self.final_text = Some(Final {
            lease,
            text: zeroize::Zeroizing::new(text.into()),
            receipt: receipt.clone(),
            needs_check: self
                .pending
                .as_ref()
                .is_some_and(|pending| matches!(pending.operation, Operation::Check(_))),
        });
        self.next = crate::time::now();
        Some(receipt)
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::webview::{FrameSlot, SharedEngine, WebViewCommand, WebViewEngine, WebViewEvent};

    #[derive(Default)]
    struct Controlled {
        requests: Mutex<Vec<(Request, Receipt)>>,
    }
    impl WebViewEngine for Controlled {
        fn send(&self, _: WebViewCommand) {}
        fn frames(&self) -> FrameSlot {
            Arc::new(Mutex::new(None))
        }
        fn drain_events(&self) -> Vec<WebViewEvent> {
            Vec::new()
        }
        fn text_input(&self, request: Request) -> Option<Receipt> {
            let receipt = Receipt::pending();
            self.requests
                .lock()
                .unwrap()
                .push((request, receipt.clone()));
            Some(receipt)
        }
    }
    fn lease() -> Lease {
        Lease::parse("0123456789abcdef0123456789abcdef").unwrap()
    }
    fn leased_guard() -> Guard {
        Guard {
            lease: Some(lease()),
            pending: None,
            retiring: None,
            final_text: None,
            next: crate::time::now(),
            revoked: false,
        }
    }
    fn checked_guard() -> (Guard, Arc<Controlled>, SharedEngine, Receipt) {
        let controlled = Arc::new(Controlled::default());
        let engine: SharedEngine = controlled.clone();
        let mut guard = leased_guard();
        guard.pump(&engine, true);
        let check = controlled.requests.lock().unwrap()[0].1.clone();
        (guard, controlled, engine, check)
    }
    #[test]
    fn busy_original_check_must_finish_before_one_final_is_admitted() {
        let (mut guard, controlled, engine, check) = checked_guard();
        let final_receipt = guard.commit(&engine, "private original").unwrap();
        guard.pump(&engine, true);
        assert_eq!(controlled.requests.lock().unwrap().len(), 1);
        check.complete(Err(Failure::Busy));
        guard.pump(&engine, true);
        assert!(final_receipt.poll().is_none());
        let requests = controlled.requests.lock().unwrap();
        assert!(matches!(requests[1].0, Request::Check(_)));
        let retry = requests[1].1.clone();
        drop(requests);
        retry.complete(Ok(Some(lease())));
        guard.pump(&engine, true);
        let requests = controlled.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        match &requests[2].0 {
            Request::Commit {
                lease: original,
                text,
            } => {
                assert!(*original == lease());
                assert_eq!(&**text, "private original");
            }
            _ => panic!("a matching check must admit exactly one original final"),
        }
        requests[2].1.complete(Ok(None));
        drop(requests);
        guard.pump(&engine, true);
        assert!(matches!(final_receipt.poll(), Some(Ok(None))));
    }
    #[test]
    fn cancellation_while_check_is_pending_prevents_final_admission() {
        let (mut guard, controlled, engine, check) = checked_guard();
        let final_receipt = guard.commit(&engine, "discarded original").unwrap();
        final_receipt.cancel();
        check.complete(Ok(Some(lease())));
        guard.pump(&engine, true);
        assert!(matches!(final_receipt.poll(), Some(Err(Failure::Unknown))));
        assert!(
            controlled
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|(request, _)| !matches!(request, Request::Commit { .. }))
        );
    }
    #[test]
    fn blurred_busy_retirement_keeps_cleanup_work_without_repreparing() {
        let controlled = Arc::new(Controlled::default());
        let engine: SharedEngine = controlled.clone();
        let mut guard = leased_guard();
        guard.pump(&engine, false);
        controlled.requests.lock().unwrap()[0]
            .1
            .complete(Err(Failure::Busy));
        guard.pump(&engine, false);
        assert!(guard.has_work());
        assert_eq!(
            controlled.requests.lock().unwrap().len(),
            1,
            "busy cleanup obeys its bounded retry cadence"
        );
        guard.next = crate::time::now();
        guard.pump(&engine, false);
        assert!(
            controlled
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|(request, _)| matches!(request, Request::Cancel(_)))
        );
    }
    #[test]
    fn original_owner_drop_reports_unknown_without_admitting_queued_final() {
        let (mut guard, controlled, engine, _) = checked_guard();
        let final_receipt = guard.commit(&engine, "original transcript").unwrap();
        drop(guard);
        assert!(matches!(final_receipt.poll(), Some(Err(Failure::Unknown))));
        assert_eq!(controlled.requests.lock().unwrap().len(), 1);
    }
}
