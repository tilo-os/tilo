//! The person's account with the host this session runs on, published to the apps
//! attached here as `host-account`: the plan it is on, the payment method on file,
//! what its Linux machines used, the invoices the provider issued and the page where
//! the person manages all of it.
//!
//! Billing is the provider's, never a form Tilo draws: this reading says what the host
//! answered ([`tilo_actions::remote::server_account`]) and the pane opens the
//! provider's own page for a card, a plan or an invoice. The reading is the person's,
//! so only Settings reads it.
//!
//! The host is asked off the session's loop: a read waits on the host's socket, and
//! the loop draws windows while it does. Until the first answer lands the reading says
//! it is reading, and a host that cannot be asked is a state of its own
//! ([`State::Unreachable`]) rather than an account without usage.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use tilo_actions::remote::server_account::{InvoiceStatus, ServerAccount};
use tilo_actions::{ActionAddress, ApiError};
use tilo_lock::MutexRecover as _;
use tilo_service::{Service, Started};

use crate::server_account::Source;

/// The resource this session publishes the account as.
pub(crate) const RESOURCE: &str = "host-account";

/// The app the account is read for: billing is the person's own, and Settings shows
/// it. An agent learns what it needs from the refusal of the action it took.
const SETTINGS_APP: &str = tilo_app_id::apps::SETTINGS;

/// How long a host that could not be answered waits before it is asked again, so a page
/// left open on an unreachable host comes back by itself.
const RETRY: Duration = Duration::from_secs(30);

/// The host account resource, as the session's catalogue holds it.
#[derive(tilo_resource::Row)]
#[row(kind = "host-account", label = "Hosting account")]
pub struct HostAccountRoot;

/// Where the account stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// The host has not answered yet.
    Reading,
    /// The host answered: the account is on a plan, a card or invoices.
    Available,
    /// The host sells plans and this person has no account with it yet.
    SignedOut,
    /// There is no host to pay: this computer, or a server that sells nothing.
    NoAccount,
    /// The host could not be asked.
    Unreachable,
}

impl State {
    const fn word(self) -> &'static str {
        match self {
            Self::Reading => "reading",
            Self::Available => "available",
            Self::SignedOut => "signed-out",
            Self::NoAccount => "no-account",
            Self::Unreachable => "unreachable",
        }
    }
}

/// The host account as a view reads it.
pub(crate) struct Served {
    source: Source,
    /// The reading as it stands, once the host answered.
    held: Mutex<Option<Value>>,
    /// The reading that landed and the loop has not taken yet.
    landed: Arc<Mutex<Option<Value>>>,
    /// The host's answer is wanted again.
    stale: Arc<AtomicBool>,
    /// A read is on its way.
    in_flight: Arc<AtomicBool>,
    /// When the host was last asked.
    asked: Mutex<Instant>,
    /// The plans file, followed for as long as the session runs: a card or a plan
    /// written there is the account changing.
    _watch: Option<tilo_files::native::ChangeWatch>,
}

impl Served {
    /// The account of this session's host, watched where it keeps its plans.
    pub(crate) fn new() -> Self {
        Self::of(Source::session())
    }

    /// The account `source` answers.
    pub(crate) fn of(source: Source) -> Self {
        let stale = Arc::new(AtomicBool::new(true));
        let watch = match &source {
            Source::Hosted(hosted) => {
                let stale = Arc::clone(&stale);
                match hosted.watch(move || stale.store(true, Ordering::Relaxed)) {
                    Ok(watch) => Some(watch),
                    Err(error) => {
                        tracing::warn!(%error, "host account: the plans file is not followed");
                        None
                    }
                }
            }
            Source::Device(_) => None,
        };
        Self {
            source,
            held: Mutex::default(),
            landed: Arc::default(),
            stale,
            in_flight: Arc::default(),
            asked: Mutex::new(Instant::now()),
            _watch: watch,
        }
    }

    /// Ask the host now, on this thread, and keep its answer until the loop takes it:
    /// what a test does where the loop runs no reader of its own.
    #[cfg(test)]
    pub(crate) fn refresh(&self) {
        *self.landed.lock_recover() = Some(reading_of(&self.source));
        self.in_flight.store(false, Ordering::Relaxed);
    }

    /// The reading as it stands, or the one it would have before the host answered.
    fn reading(&self) -> Value {
        if let Some(held) = self.held.lock_recover().clone() {
            return held;
        }
        shell(State::Reading, host_name(&self.source), "")
    }

    /// Whether the host could not be asked last time, so it is worth asking again.
    fn unreachable(&self) -> bool {
        self.held
            .lock_recover()
            .as_ref()
            .and_then(|reading| reading.get("state"))
            .and_then(Value::as_str)
            == Some(State::Unreachable.word())
    }
}

impl Service for Served {
    fn resources(&self) -> Vec<tilo_actions::ResourceSpec> {
        vec![tilo_resource::typed_resource::<HostAccountRoot>(
            RESOURCE,
            crate::i18n::t("session-host-account"),
        )]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        (resource == RESOURCE).then(|| self.reading())
    }

    /// The reading for the app `app` reading it: the person's own account is
    /// Settings', and another program reads none of it.
    fn read_for(&self, app: &str, _workspace: &str, resource: &str) -> Option<Value> {
        (resource == RESOURCE && app == SETTINGS_APP).then(|| self.reading())
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        _arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        Err(ApiError::Unknown(ActionAddress::new(resource, action)))
    }

    /// Ask the host when its answer is wanted again, off this thread: the loop draws
    /// windows while the host answers. Whether a new reading landed.
    fn poll(&mut self) -> bool {
        if let Some(landed) = self.landed.lock_recover().take() {
            *self.held.lock_recover() = Some(landed);
            *self.asked.lock_recover() = Instant::now();
            return true;
        }
        if self.unreachable() && self.asked.lock_recover().elapsed() >= RETRY {
            self.stale.store(true, Ordering::Relaxed);
        }
        if self.stale.swap(false, Ordering::Relaxed)
            && !self.in_flight.swap(true, Ordering::Relaxed)
        {
            let source = self.source.clone();
            let landed = Arc::clone(&self.landed);
            let in_flight = Arc::clone(&self.in_flight);
            // The host's own socket is asked with a patience of its own: never here.
            if let Err(error) = std::thread::Builder::new()
                .name("host-account".to_owned())
                .spawn(move || {
                    *landed.lock_recover() = Some(reading_of(&source));
                    in_flight.store(false, Ordering::Relaxed);
                })
            {
                tracing::warn!(%error, "host account: the host is not asked");
                self.in_flight.store(false, Ordering::Relaxed);
            }
        }
        false
    }

    /// When an unreachable host is asked again: the loop wakes then and [`Self::poll`]
    /// asks.
    fn wakes_in(&self) -> Option<Duration> {
        self.unreachable()
            .then(|| RETRY.saturating_sub(self.asked.lock_recover().elapsed()))
    }
}

/// The account as `source` answers it, in the words a view reads: [`shell`] with the
/// state, and the plan, card, usage, page and invoices the host gave.
pub(crate) fn reading_of(source: &Source) -> Value {
    match source.try_read(tilo_clock::unix_now_ms()) {
        Err(why) => shell(
            State::Unreachable,
            host_name(source),
            &format!("Your host did not answer: {why}"),
        ),
        Ok(account) if !account.sells() => shell(
            State::NoAccount,
            &account.server,
            "Your workspaces run on this server itself, so there is nothing to pay for.",
        ),
        Ok(account) => account_reading(&account),
    }
}

/// The reading of a host that sells: what the account is on, and the invoices it was
/// sent.
fn account_reading(account: &ServerAccount) -> Value {
    let subscription = account.subscription.as_ref();
    let on_plan = subscription.is_some_and(|subscription| {
        subscription.plan.is_some()
            || subscription.payment_method.is_some()
            || !subscription.invoices.is_empty()
    });
    let (state, words) = if on_plan {
        (State::Available, "")
    } else {
        (
            State::SignedOut,
            "You are not signed in to this host yet: sign in on its page to see the plan and invoices.",
        )
    };
    let mut reading = shell(state, &account.server, words);
    let plan = account.plan();
    reading["plan_name"] = json!(plan.map(|plan| plan.name.clone()).unwrap_or_default());
    reading["plan_price"] = json!(plan.and_then(|plan| plan.price.clone()).unwrap_or_default());
    reading["manage"] = json!(account.manage_billing.as_deref().map_or("", page));
    if let Some(subscription) = subscription {
        if let Some(card) = &subscription.payment_method {
            reading["has_card"] = json!(true);
            reading["card_brand"] = json!(card.brand.clone().unwrap_or_default());
            reading["card_last4"] = json!(card.last4.clone().unwrap_or_default());
        }
        reading["usage_seconds"] = json!(subscription.usage.vm_seconds());
        if let Some(included) = subscription
            .limits
            .as_ref()
            .and_then(|limits| limits.included_vm_seconds)
        {
            reading["included_seconds"] = json!(included);
        }
        reading["credit"] = json!(subscription.credit.clone().unwrap_or_default());
        reading["invoices"] = Value::Array(
            subscription
                .invoices
                .iter()
                .map(|invoice| {
                    json!({
                        "id": invoice.id,
                        "amount": invoice.amount,
                        "status": status_word(invoice.status),
                        "issued": issued(invoice.issued_ms),
                        "page": page(&invoice.url),
                    })
                })
                .collect(),
        );
    }
    reading
}

/// A reading with `state`, `server` and `words`, and nothing else the host has said:
/// every other field reads empty.
fn shell(state: State, server: &str, words: &str) -> Value {
    json!({
        "state": state.word(),
        "server": server,
        "words": words,
        "plan_name": "",
        "plan_price": "",
        "has_card": false,
        "card_brand": "",
        "card_last4": "",
        "usage_seconds": 0,
        "included_seconds": Value::Null,
        "credit": "",
        "manage": "",
        "invoices": [],
    })
}

/// The host's name, as the account would say it, for a host that did not answer.
fn host_name(source: &Source) -> &str {
    match source {
        Source::Hosted(hosted) => &hosted.name,
        Source::Device(name) => name,
    }
}

/// Where an invoice stands, as its row says.
const fn status_word(status: InvoiceStatus) -> &'static str {
    match status {
        InvoiceStatus::Paid => "paid",
        InvoiceStatus::Open => "open",
        InvoiceStatus::PastDue => "past-due",
        InvoiceStatus::Void => "void",
    }
}

/// When an invoice was issued, as ISO-8601 text; none for one the provider gave no time
/// for.
fn issued(ms: u64) -> Value {
    i64::try_from(ms)
        .ok()
        .filter(|ms| *ms > 0)
        .and_then(|ms| jiff::Timestamp::from_millisecond(ms).ok())
        .map_or(Value::Null, |at| json!(at.to_string()))
}

/// The page `url` names, when it is a web page the provider serves over https: never
/// anything else an answer might name.
fn page(url: &str) -> &str {
    if url.starts_with("https://") { url } else { "" }
}

#[cfg(test)]
mod tests;
