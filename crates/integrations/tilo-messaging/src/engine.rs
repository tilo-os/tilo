//! What Messages asks its device for: the person's messaging accounts, as the service the launcher
//! connected the app to answers them, through `engine.call` and `engine.start`.
//!
//! Every request and answer is JSON. A refusal is one of a few words the app turns into its own
//! (`unavailable`, `revoked`, `invalid`, `reserved`, `uncertain`, `not-own-device`), so what a
//! person reads stays in the app's catalogue. The engine keeps nothing of its own: the service
//! checks the person, the account, the route and the device again on every request, so a request
//! the app made from a stale view is refused there.
//!
//! What a job answers is the job's record on this device (its detail), never a workspace
//! resource: a history page, a group's messages and a private linking code stay on the device
//! that holds the account.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll, Wake, Waker};

use serde_json::{Value, json};
use tilo_contact_directory::routing::Route;

use crate::{
    AccountState, Delivery, Direction, EnrollmentClient, EnrollmentPhase, EnrollmentReview,
    GroupChange, GroupChangeRequest, GroupClient, GroupHistory, GroupMembership, GroupRole,
    GroupSendRequest, HistoryCursor, HistoryError, HistoryPage, Message, MessagingClient, Pairing,
    SendError, SendRequest, ServiceAccount,
};

/// The ops [`Engine::run`] answers, each as a job: they wait on the service.
pub const JOBS: &[&str] = &[
    "history",
    "send",
    "groups",
    "group",
    "group_history",
    "group_send",
    "group_change",
    "link_begin",
    "link_finish",
    "first_prepare",
    "first_begin",
    "first_sims",
    "first_select_sim",
    "first_finish",
    "first_review",
];

/// The person's messaging, as this device reaches it: the clients the service lent the app, or
/// none where no service runs.
#[derive(Clone, Default)]
pub struct Engine {
    messaging: Option<MessagingClient>,
    enrollment: Option<EnrollmentClient>,
    stopped: Arc<Mutex<BTreeMap<String, Arc<AtomicBool>>>>,
}

impl Engine {
    #[must_use]
    pub fn new(messaging: Option<MessagingClient>, enrollment: Option<EnrollmentClient>) -> Self {
        Self {
            messaging,
            enrollment,
            stopped: Arc::default(),
        }
    }

    /// The clients of the connection the launcher handed this process, when it handed one.
    #[cfg(feature = "service-client")]
    #[must_use]
    pub fn inherited() -> Self {
        match crate::service::inherited() {
            Some((enrollment, messaging)) => Self::new(Some(messaging), Some(enrollment)),
            None => Self::default(),
        }
    }

    /// One request answered now.
    ///
    /// # Errors
    ///
    /// The refusal's word, or why the request reads as none.
    pub fn call(&self, op: &str, input: &str) -> Result<String, String> {
        let ask = parse(input)?;
        let answer = match op {
            "state" => self.state(&ask)?,
            "setup_cancel" => {
                if let Some(setup) = self.messaging()?.setup(&account(&ask)?) {
                    setup.cancel();
                }
                json!({})
            }
            "first_cancel" => {
                self.enrollment()?.cancel(&account(&ask)?);
                json!({})
            }
            "sims_cancel" => {
                self.enrollment()?.cancel_sim_selection();
                json!({})
            }
            other => return Err(format!("Messages asks no `{other}` of its device")),
        };
        Ok(answer.to_string())
    }

    /// One job's work, waited for on the caller's thread: its answer, or the refusal's word.
    ///
    /// # Errors
    ///
    /// The refusal's word, or why the request reads as none.
    pub fn run(&self, op: &str, input: &str) -> Result<Value, String> {
        let ask = parse(input)?;
        match op {
            "history" => self.history(&ask),
            "send" => self.send(&ask),
            "groups" => {
                let list = wait(self.groups(&ask)?.list()).map_err(refusal)?;
                Ok(json!({
                    "service": list.account.service,
                    "account": list.account.account,
                    "groups": list.groups.iter().map(|g| json!({"key": g.id, "title": g.title})).collect::<Vec<_>>(),
                }))
            }
            "group" => {
                let membership =
                    wait(self.groups(&ask)?.read(text(&ask, "group")?)).map_err(refusal)?;
                Ok(membership_json(&membership))
            }
            "group_history" => {
                let history =
                    wait(self.groups(&ask)?.history(text(&ask, "group")?)).map_err(refusal)?;
                Ok(group_history_json(&history))
            }
            "group_send" => self.group_send(&ask),
            "group_change" => self.group_change(&ask),
            "link_begin" => {
                let setup = self.setup(&ask)?;
                let pairing = wait(setup.begin()).map_err(refusal)?;
                linking_json(&pairing)
            }
            "link_finish" => {
                let setup = self.setup(&ask)?;
                let linked = wait(setup.finish(text(&ask, "attempt")?)).map_err(refusal)?;
                Ok(json!({
                    "service": linked.account.service,
                    "account": linked.account.account,
                    "scope": linked.scope,
                }))
            }
            "first_prepare" => {
                let review =
                    wait(self.enrollment()?.prepare(text(&ask, "service")?)).map_err(refusal)?;
                Ok(review_json(&review))
            }
            "first_begin" => {
                let pairing =
                    wait(self.enrollment()?.begin(text(&ask, "service")?)).map_err(refusal)?;
                linking_json(&pairing)
            }
            "first_sims" => {
                let enrollment = self.enrollment()?;
                if !enrollment.supports_sim_selection() {
                    return Err(refusal(SendError::Unavailable));
                }
                let choices = wait(enrollment.sim_choices()).map_err(refusal)?;
                Ok(json!({
                    "choices": choices.iter().map(|c| json!({"selection": c.selection, "label": c.label})).collect::<Vec<_>>(),
                }))
            }
            "first_select_sim" => {
                let review = wait(self.enrollment()?.select_sim(text(&ask, "selection")?))
                    .map_err(refusal)?;
                Ok(review_json(&review))
            }
            "first_finish" => {
                let review = wait(
                    self.enrollment()?
                        .finish(account(&ask)?, text(&ask, "attempt")?),
                )
                .map_err(refusal)?;
                Ok(review_json(&review))
            }
            "first_review" => {
                let enrollment = self.enrollment()?;
                let account = account(&ask)?;
                wait(enrollment.review(account.clone(), text(&ask, "display_name")?))
                    .map_err(refusal)?;
                Ok(json!({"phase": phase(enrollment.phase(&account))}))
            }
            other => Err(format!("Messages starts no `{other}` on its device")),
        }
    }

    /// Run `op` on a thread of its own as the job `job`, telling `moved` its state's word and its
    /// detail: `running`, then `done` with the answer, `failed` with `{"why": word}`, or
    /// `cancelled` once [`Engine::cancel`] stopped it.
    ///
    /// # Errors
    ///
    /// The op is no job of this engine's, or no thread could run it.
    pub fn start(
        &self,
        job: &str,
        op: &str,
        input: &str,
        moved: impl Fn(&str, &str) + Send + 'static,
    ) -> Result<(), String> {
        if !JOBS.contains(&op) {
            return Err(format!("Messages starts no `{op}` on its device"));
        }
        let stop = Arc::new(AtomicBool::new(false));
        self.stopped
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(job.to_owned(), stop.clone());
        let engine = self.clone();
        let (job, op, input) = (job.to_owned(), op.to_owned(), input.to_owned());
        std::thread::Builder::new()
            .name("messages-job".into())
            .spawn(move || {
                moved("running", "{}");
                let answer = engine.run(&op, &input);
                engine
                    .stopped
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&job);
                match answer {
                    _ if stop.load(Ordering::Relaxed) => moved("cancelled", "{}"),
                    Ok(answer) => moved("done", &answer.to_string()),
                    Err(why) => moved("failed", &json!({ "why": why }).to_string()),
                }
            })
            .map(drop)
            .map_err(|error| format!("no thread runs the job: {error}"))
    }

    /// Stop the job `job`: what it answers is no longer anyone's. False for a job that ended.
    #[must_use]
    pub fn cancel(&self, job: &str) -> bool {
        self.stopped
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(job)
            .is_some_and(|stop| !stop.swap(true, Ordering::Relaxed))
    }

    fn messaging(&self) -> Result<&MessagingClient, String> {
        self.messaging
            .as_ref()
            .ok_or_else(|| refusal(SendError::Unavailable))
    }

    fn enrollment(&self) -> Result<&EnrollmentClient, String> {
        self.enrollment
            .as_ref()
            .ok_or_else(|| refusal(SendError::Unavailable))
    }

    fn groups(&self, ask: &Value) -> Result<GroupClient, String> {
        self.messaging()?
            .groups(&account(ask)?)
            .ok_or_else(|| refusal(SendError::Unavailable))
    }

    fn setup(&self, ask: &Value) -> Result<crate::SetupClient, String> {
        self.messaging()?
            .setup(&account(ask)?)
            .ok_or_else(|| refusal(SendError::Unavailable))
    }

    /// Where each account the app names stands now, from the service's last word: its state,
    /// its first enrollment's phase, whether it can be linked or its groups read here, and how
    /// soon its linking code wants drawing again.
    fn state(&self, ask: &Value) -> Result<Value, String> {
        let accounts: Vec<ServiceAccount> = ask
            .get("accounts")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(account)
            .collect::<Result<_, _>>()?;
        let routes: Vec<Route> = ask
            .get("routes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|route| serde_json::from_value(route.clone()).map_err(|e| e.to_string()))
            .collect::<Result<_, _>>()?;
        let messaging = self.messaging.as_ref();
        let enrollment = self.enrollment.as_ref();
        Ok(json!({
            "available": messaging.is_some(),
            "first": enrollment.is_some(),
            "sims": enrollment.is_some_and(EnrollmentClient::supports_sim_selection),
            "actor": messaging.map_or("", MessagingClient::actor),
            "accounts": accounts.iter().map(|account| {
                let setup = messaging.and_then(|m| m.setup(account));
                json!({
                    "service": account.service,
                    "account": account.account,
                    "state": messaging.map_or(AccountState::Unavailable, |m| m.account_state(account)).as_str(),
                    "phase": enrollment.map_or("empty", |e| phase(e.phase(account))),
                    "linked": enrollment.is_some_and(|e| e.linked(account).is_some()),
                    "linkable": setup.is_some(),
                    "wake_ms": setup.and_then(|s| s.wake_after()).map_or(0, |after| u64::try_from(after.as_millis()).unwrap_or(u64::MAX)),
                    "groups": messaging.is_some_and(|m| m.groups(account).is_some()),
                })
            }).collect::<Vec<_>>(),
            "routes": routes.iter().map(|route| messaging.is_some_and(|m| m.supports(route))).collect::<Vec<_>>(),
        }))
    }

    fn history(&self, ask: &Value) -> Result<Value, String> {
        let route = route(ask)?;
        let before = ask
            .get("before")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty());
        let cursor = before.map(|id| HistoryCursor::before(&marker(id)));
        let page = wait(self.messaging()?.history().read(route.clone(), cursor))
            .map_err(history_refusal)?;
        if page.route != route {
            return Err(history_refusal(HistoryError::Revoked));
        }
        Ok(page_json(&page))
    }

    fn send(&self, ask: &Value) -> Result<Value, String> {
        let messaging = self.messaging()?;
        let request = SendRequest {
            route: route(ask)?,
            operation: text(ask, "operation")?,
            text: text(ask, "text")?.into(),
        };
        let receipt = wait(messaging.send(request)).map_err(refusal)?;
        let page = wait(messaging.history().read(receipt.route.clone(), None)).ok();
        Ok(json!({
            "operation": receipt.operation,
            "status": match receipt.delivery {
                Delivery::Submitted => "submitted",
                Delivery::ServerAcknowledged => "server-acknowledged",
                _ => "uncertain",
            },
            "page": page.as_ref().map(page_json),
        }))
    }

    fn group_send(&self, ask: &Value) -> Result<Value, String> {
        let groups = self.groups(ask)?;
        let group = text(ask, "group")?;
        wait(groups.send(GroupSendRequest {
            group: group.clone(),
            review: text(ask, "review")?,
            operation: text(ask, "operation")?,
            text: text(ask, "text")?.into(),
        }))
        .map_err(refusal)?;
        let history = wait(groups.history(group.clone())).ok();
        let membership = wait(groups.read(group)).ok();
        Ok(json!({
            "history": history.as_ref().map(group_history_json),
            "membership": membership.as_ref().map(membership_json),
        }))
    }

    fn group_change(&self, ask: &Value) -> Result<Value, String> {
        let groups = self.groups(ask)?;
        let group = text(ask, "group")?;
        let change = match text(ask, "change")?.as_str() {
            "add" => GroupChange::Add(route(ask)?),
            "remove" => GroupChange::Remove(text(ask, "member")?),
            "leave" => GroupChange::Leave,
            other => return Err(format!("no group change is called `{other}`")),
        };
        let left = matches!(change, GroupChange::Leave);
        wait(groups.change(GroupChangeRequest {
            group: group.clone(),
            review: text(ask, "review")?,
            operation: text(ask, "operation")?,
            change,
        }))
        .map_err(refusal)?;
        let membership = if left {
            None
        } else {
            wait(groups.read(group)).ok()
        };
        Ok(json!({"membership": membership.as_ref().map(membership_json)}))
    }
}

fn parse(input: &str) -> Result<Value, String> {
    if input.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(input).map_err(|error| format!("the request is no JSON: {error}"))
}

fn text(ask: &Value, name: &str) -> Result<String, String> {
    ask.get(name)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("the request names no `{name}`"))
}

fn account(ask: &Value) -> Result<ServiceAccount, String> {
    Ok(ServiceAccount::new(
        text(ask, "service")?,
        text(ask, "account")?,
    ))
}

fn route(ask: &Value) -> Result<Route, String> {
    let route = ask
        .get("route")
        .ok_or_else(|| "the request names no `route`".to_owned())?;
    serde_json::from_value(route.clone())
        .map_err(|error| format!("the route reads as none: {error}"))
}

/// A message standing for the one whose marker `id` is: what a cursor is made from.
fn marker(id: &str) -> Message {
    Message {
        id: id.to_owned(),
        text: String::new().into(),
        direction: Direction::Incoming,
        timestamp_ms: 0,
        delivery: None,
    }
}

/// The word a refusal of the service is answered with.
fn refusal(error: SendError) -> String {
    match error {
        SendError::Unavailable => "unavailable",
        SendError::Revoked => "revoked",
        SendError::InvalidRequest => "invalid",
        SendError::AlreadyReserved => "reserved",
        SendError::Uncertain => "uncertain",
        SendError::NotYourDevice => "not-own-device",
    }
    .to_owned()
}

fn history_refusal(error: HistoryError) -> String {
    match error {
        HistoryError::Unavailable => "unavailable",
        HistoryError::Revoked => "revoked",
        HistoryError::InvalidRoute | HistoryError::InvalidPage | HistoryError::InvalidCursor => {
            "invalid"
        }
    }
    .to_owned()
}

fn phase(phase: EnrollmentPhase) -> &'static str {
    match phase {
        EnrollmentPhase::Empty => "empty",
        EnrollmentPhase::Linking => "linking",
        EnrollmentPhase::Review => "review",
        EnrollmentPhase::Ready => "ready",
        EnrollmentPhase::Interrupted => "interrupted",
    }
}

fn delivery(delivery: Option<Delivery>) -> &'static str {
    match delivery {
        None => "",
        Some(Delivery::Submitted) => "submitted",
        Some(Delivery::ServerAcknowledged) => "acknowledged",
        Some(Delivery::RecipientDelivered) => "delivered",
        Some(Delivery::RecipientRead) => "read",
        Some(Delivery::Uncertain) => "uncertain",
    }
}

fn message_json(message: &Message) -> Value {
    json!({
        "key": message.id,
        "text": message.text.as_str(),
        "incoming": message.direction == Direction::Incoming,
        "at_ms": i64::try_from(message.timestamp_ms).unwrap_or(i64::MAX),
        "at": instant(message.timestamp_ms),
        "delivery": delivery(message.delivery),
    })
}

/// `ms` since 1970 as the UTC instant a window reads a time from (`2026-11-26T09:30:00Z`).
fn instant(ms: u64) -> String {
    let seconds = ms / 1000;
    let days = i64::try_from(seconds / 86_400).unwrap_or(i64::MAX);
    let of_day = seconds % 86_400;
    // Howard Hinnant's days-to-civil: the proleptic Gregorian date of a day count.
    let z = days.wrapping_add(719_468);
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = doe
        .wrapping_sub(doe / 1460)
        .wrapping_add(doe / 36_524)
        .wrapping_sub(doe / 146_096)
        / 365;
    let doy = doe.wrapping_sub(
        365i64
            .wrapping_mul(yoe)
            .wrapping_add(yoe / 4)
            .wrapping_sub(yoe / 100),
    );
    let mp = 5i64.wrapping_mul(doy).wrapping_add(2) / 153;
    let day = doy
        .wrapping_sub(153i64.wrapping_mul(mp).wrapping_add(2) / 5)
        .wrapping_add(1);
    let month = if mp < 10 {
        mp.wrapping_add(3)
    } else {
        mp.wrapping_sub(9)
    };
    let year = yoe
        .wrapping_add(era.wrapping_mul(400))
        .wrapping_add(i64::from(month <= 2));
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        of_day / 3600,
        of_day % 3600 / 60,
        of_day % 60
    )
}

fn page_json(page: &HistoryPage) -> Value {
    json!({
        "messages": page.messages.iter().map(message_json).collect::<Vec<_>>(),
        "more": page.more,
    })
}

fn linking_json(pairing: &Pairing) -> Result<Value, String> {
    let code =
        std::str::from_utf8(pairing.qr_bytes()).map_err(|_| refusal(SendError::Unavailable))?;
    Ok(json!({
        "service": pairing.account.service,
        "account": pairing.account.account,
        "attempt": pairing.attempt,
        "generation": pairing.generation,
        "code": code,
    }))
}

fn review_json(review: &EnrollmentReview) -> Value {
    json!({
        "service": review.account.service,
        "account": review.account.account,
        "scope": review.scope,
        "number": review.number.clone().unwrap_or_default(),
    })
}

fn role(role: GroupRole) -> &'static str {
    match role {
        GroupRole::Member => "member",
        GroupRole::Admin => "admin",
        GroupRole::SuperAdmin => "super-admin",
    }
}

fn membership_json(page: &GroupMembership) -> Value {
    json!({
        "service": page.account.service,
        "account": page.account.account,
        "group": page.group.id,
        "title": page.group.title,
        "announcement": page.announcement,
        "self_member": page.self_member,
        "review": page.review,
        "members": page.members.iter().map(|m| json!({"address": m.address, "role": role(m.role)})).collect::<Vec<_>>(),
    })
}

fn group_history_json(history: &GroupHistory) -> Value {
    json!({
        "group": history.group,
        "messages": history.messages.iter().map(|row| {
            let mut message = message_json(&row.message);
            if let Some(fields) = message.as_object_mut() {
                fields.insert("author".into(), row.author.clone().into());
            }
            message
        }).collect::<Vec<_>>(),
    })
}

/// Wait for `future` on this thread: the service's answers arrive on its connection's own thread,
/// which wakes this one.
fn wait<F: Future>(future: F) -> F::Output {
    struct Thread(std::thread::Thread);
    impl Wake for Thread {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(Thread(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
            return output;
        }
        std::thread::park();
    }
}
