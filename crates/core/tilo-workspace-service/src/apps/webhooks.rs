//! Account webhooks: a workspace's events posted to a URL, as a shortcut of the workspace.
//!
//! A webhook is a record like any other: a saved shortcut whose one trigger is
//! [`TriggerSpec::Webhook`], whose run is [`WEBHOOK_POST`] on [`WEBHOOK_PROVIDER`], and whose
//! owner is the key that registered it. The trigger runner arms it where the shortcut is saved,
//! and only the device that keeps the workspace posts: [`Poster`] refuses a call anywhere else.
//! Every delivery is an invocation of that action, recorded with the workspace's invocations
//! like any other, so what was posted, what failed and how often it was tried are read there.
//!
//! A delivery is `POST <url>` with the event as its body and
//!
//! - `Tilo-Webhook-Id`, the webhook;
//! - `Tilo-Delivery`, the event's identity, the same on every retry;
//! - `Tilo-Signature: t=<unix seconds>,v1=<hex Ed25519 of "<t>.<body>">`;
//! - `Tilo-Key`, the public key the signature verifies under.
//!
//! The device that keeps the workspace signs with its own key: a webhook's receiver is told that
//! key when the webhook is made, and no secret of the person's is written into a shared
//! workspace to sign with. A `2xx` within
//! [`ANSWER_WITHIN`](crate::workspaces::webhooks::ANSWER_WITHIN) delivers it; anything else is tried
//! again by the shortcut's step policy, and a URL that never takes it leaves a failed
//! invocation record.
//!
//! Whoever holds a key names the URL, so a delivery goes only where this machine's own network
//! is not: over `https` to the public internet, a name judged in the connection's own look-up,
//! no redirect followed and no proxy of its environment taken; and over plain `http` to this
//! computer's own loopback, for a program on the person's computer.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use collab_types::signer::DeviceSigner;
use serde_json::{Value, json};
use tilo_actions::remote::invocation_record::Invocations;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueProvider, CatalogueQuery,
    CatalogueRow, InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest, Pending,
    ReceiptStatus,
};
use tilo_actions::{
    ActionAddress, ActionContract, ActionDescriptor, ActionFailure, ActionKind, ActionOutput,
    ActionParam, ExecutionLocus, InputSpec, InvocationId, InvocationUpdate, OperationKind,
    ParamType,
};
use tilo_automation::{WEBHOOK_POST, WEBHOOK_PROVIDER, WEBHOOK_RESOURCE};
use tilo_server_protocol::WorkspaceId;

use crate::workspaces::Workspaces;
use crate::workspaces::webhooks::{ANSWER_WITHIN, ATTEMPTS, refusal};

/// This computer's own name.
const LOCALHOST: &str = "localhost";

/// A look-up that answers this computer's own loopback for its own name, and no other name.
fn this_computer() -> tilo_net::address::Lookup {
    Arc::new(|name: String| {
        Box::pin(async move {
            if name.eq_ignore_ascii_case(LOCALHOST) {
                Ok(vec![
                    IpAddr::V4(Ipv4Addr::LOCALHOST),
                    IpAddr::V6(Ipv6Addr::LOCALHOST),
                ])
            } else {
                Err(std::io::Error::other(format!(
                    "{name} is not this computer"
                )))
            }
        })
    })
}

/// The clients a delivery is posted with: the public internet, dialled directly, no redirect
/// followed, connected only where the one judge passed every address the name answered with;
/// and this computer's own loopback over plain `http`.
#[derive(Clone)]
pub struct Clients {
    public: tilo_net::Client,
    here: tilo_net::Client,
}

impl Clients {
    /// # Errors
    /// A client could not be built.
    pub fn new() -> Result<Self, tilo_net::NetError> {
        let client = || {
            tilo_net::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .redirects(tilo_net::Redirects::None)
                .direct()
        };
        Ok(Self {
            public: client()
                .lookup(tilo_net::address::public_only(
                    tilo_net::address::system_lookup(),
                ))
                .build()?,
            here: client().lookup(this_computer()).build()?,
        })
    }

    /// The client `url` is posted with, or why it is posted to by none.
    fn client(&self, url: &str) -> Result<&tilo_net::Client, &'static str> {
        if let Some(why) = refusal(url) {
            return Err(why);
        }
        let plain = tilo_net::Url::parse(url).is_ok_and(|url| url.scheme() == "http");
        Ok(if plain { &self.here } else { &self.public })
    }
}

/// One delivery's post, without the provider's state: what the task that sends it needs.
#[derive(Clone)]
struct Posting {
    clients: Clients,
    signer: Arc<dyn DeviceSigner>,
    key: [u8; 32],
}

impl Posting {
    /// Post `body` to `url`, signed as `key`, as delivery `delivery` of `webhook`.
    async fn post(
        &self,
        url: &str,
        webhook: &str,
        delivery: &str,
        body: &[u8],
    ) -> Result<u16, String> {
        let client = self.clients.client(url).map_err(str::to_owned)?;
        let time = tilo_clock::unix_now_ms() / 1000;
        let mut signed = format!("{time}.").into_bytes();
        signed.extend_from_slice(body);
        let signature = collab_types::signer::sign_now(self.signer.as_ref(), signed)
            .map_err(|error| error.to_string())?;
        let answered = client
            .post(url)
            .header("content-type", "application/json")
            .header("tilo-webhook-id", webhook)
            .header("tilo-delivery", delivery)
            .header(
                "tilo-signature",
                format!("t={time},v1={}", hex::encode(signature)),
            )
            .header("tilo-key", hex::encode(self.key))
            .body(body.to_vec())
            .timeout(ANSWER_WITHIN)
            .send()
            .await
            .map_err(|error| error.to_string())?;
        Ok(answered.status().as_u16())
    }
}

/// The action that posts one webhook delivery, as the device that keeps the workspace runs it.
pub struct Poster {
    workspaces: Workspaces,
    posting: Posting,
    invocations: Arc<Invocations>,
    states: Arc<Mutex<HashMap<InvocationId, InvocationState>>>,
    attempts: Arc<Mutex<HashMap<InvocationId, u32>>>,
}

impl Poster {
    /// A poster over `workspaces`, recording its deliveries in `invocations`, signing with the
    /// key the device that keeps the workspace signs its own writes with.
    ///
    /// # Errors
    /// A client could not be built.
    pub fn new(
        workspaces: Workspaces,
        invocations: Arc<Invocations>,
    ) -> Result<Self, tilo_net::NetError> {
        let signing = workspaces.signing_device();
        Ok(Self {
            posting: Posting {
                clients: Clients::new()?,
                signer: Arc::clone(&signing.signer),
                key: signing.cert.device_public_key,
            },
            workspaces,
            invocations,
            states: Arc::default(),
            attempts: Arc::default(),
        })
    }
}

/// The catalogue row a shortcut's save reads for the delivery action.
fn row() -> CatalogueRow {
    CatalogueRow::for_action(
        WEBHOOK_PROVIDER,
        WEBHOOK_PROVIDER,
        ActionDescriptor {
            did: None,
            state: tilo_actions::ActionState::default(),
            semantic_identity: None,
            address: ActionAddress::new(WEBHOOK_RESOURCE, WEBHOOK_POST),
            resource_kind: WEBHOOK_RESOURCE.to_owned(),
            resource_label: "Webhooks".to_owned(),
            action_label: "Post a webhook delivery".to_owned(),
            keywords: Vec::new(),
            icon: None,
            description: Some(
                "Post one event of this workspace to a webhook's URL, signed by the device that \
                 keeps the workspace. A webhook's own shortcut runs it; a person runs it to \
                 test a receiver."
                    .to_owned(),
            ),
            locus: ExecutionLocus::ServerPreferred,
            kind: OperationKind::Effect,
            contract: ActionContract {
                kind: ActionKind::Outside,
                ..ActionContract::default()
            },
            input: InputSpec::None,
            params: vec![
                ActionParam::required("url", ParamType::Text, "The URL to post to."),
                ActionParam::required("webhook", ParamType::Text, "The webhook's id."),
                ActionParam::required(
                    "event",
                    ParamType::Object,
                    "The event, as the workspace's event stream gives it.",
                ),
                ActionParam::optional("path", ParamType::Text, "The record the webhook watches."),
                ActionParam::optional(
                    "attempts",
                    ParamType::Integer,
                    "How often the step tries before the delivery is failed.",
                ),
            ],
            view: None,
        },
    )
}

impl ActionProvider for Poster {
    fn serves(&self, provider: &str) -> bool {
        provider == WEBHOOK_PROVIDER
    }

    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        let Some(workspace) = caller.workspace() else {
            return Pending::ready(Err(ActionCallError::NotPermitted));
        };
        let mut page = CataloguePage::from_rows("tilo.webhooks:1".into(), vec![row()], &query);
        page.providers.push(CatalogueProvider {
            provider: WEBHOOK_PROVIDER.to_owned(),
            app_id: WEBHOOK_PROVIDER.to_owned(),
            workspace: workspace.to_owned(),
            browsable: false,
            ..CatalogueProvider::default()
        });
        Pending::ready(Ok(page))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one delivery in order: what was asked, recorded once, posted, and how it ended"
    )]
    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let (completer, answer) = tilo_actions::remote::pending();
        let workspaces = self.workspaces.clone();
        let posting = self.posting.clone();
        let invocations = Arc::clone(&self.invocations);
        let states = Arc::clone(&self.states);
        let attempts = Arc::clone(&self.attempts);
        let caller = caller.clone();
        tokio::spawn(async move {
            let workspace = match governs(&workspaces, &caller).await {
                Ok(workspace) => workspace,
                Err(error) => {
                    completer.complete(Err(error));
                    return;
                }
            };
            let args = match &request.argument {
                InvokeArgument::Json(args) => Value::Object(args.clone()),
                _ => json!({}),
            };
            let text = |name: &str| args.get(name).and_then(Value::as_str);
            let (Some(url), Some(webhook), Some(event)) =
                (text("url"), text("webhook"), args.get("event"))
            else {
                completer.complete(Err(ActionCallError::invalid_request(
                    "`url`, `webhook` and `event` are required",
                )));
                return;
            };
            let body = match serde_json::to_vec(event) {
                Ok(body) => body,
                Err(error) => {
                    completer.complete(Err(ActionCallError::invalid_request(format!(
                        "the event does not encode: {error}"
                    ))));
                    return;
                }
            };
            let delivery = event
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or(webhook)
                .to_owned();
            let budget = args
                .get("attempts")
                .and_then(Value::as_u64)
                .and_then(|attempts| u32::try_from(attempts).ok())
                .unwrap_or(ATTEMPTS)
                .max(1);
            let id = request.invocation;
            // What this device was asked for, recorded once: a retried step sends the same id.
            let attempt = {
                let mut attempts = attempts.lock().unwrap_or_else(|poison| poison.into_inner());
                let made = attempts.entry(id).or_insert(0);
                *made = made.saturating_add(1);
                if *made == 1 {
                    let route: Arc<dyn ActionProvider + Send + Sync> = Arc::new(Recorder {
                        states: Arc::clone(&states),
                    });
                    invocations.record(
                        &caller,
                        &caller,
                        &request,
                        Some(&workspace),
                        (route, caller.clone()),
                    );
                }
                *made
            };
            let answered = posting.post(url, webhook, &delivery, &body).await;
            let (code, update) = match answered {
                Ok(code) if (200..300).contains(&code) => {
                    let output = ActionOutput {
                        value: Some(json!({"status": code})),
                        ..ActionOutput::default()
                    };
                    (
                        Ok(InvocationReceipt {
                            invocation: id,
                            status: ReceiptStatus::Completed {
                                output: output.clone(),
                            },
                        }),
                        InvocationUpdate::Succeeded {
                            invocation: id,
                            output,
                        },
                    )
                }
                Ok(code) => {
                    let error = failure(format!("the webhook answered {code}"), attempt, budget);
                    (Err(error.clone()), failed_update(id, &error))
                }
                Err(why) => {
                    let error = failure(why, attempt, budget);
                    (Err(error.clone()), failed_update(id, &error))
                }
            };
            states
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .insert(id, InvocationState::new(id, update));
            completer.complete(code);
        });
        answer
    }

    fn observe(&self, _caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.states
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(id)
            .cloned()
    }

    fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(ActionCallError::NotCancellable {
            reason: "a delivery that is posting is not stopped; the webhook is".into(),
        }))
    }
}

/// The route a recorded delivery answers through: what it came to, kept by the poster.
struct Recorder {
    states: Arc<Mutex<HashMap<InvocationId, InvocationState>>>,
}

impl ActionProvider for Recorder {
    fn query(
        &self,
        _caller: &Caller,
        _query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownAction))
    }

    fn invoke(
        &self,
        _caller: &Caller,
        _request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownAction))
    }

    fn observe(&self, _caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.states
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(id)
            .cloned()
    }

    fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(ActionCallError::NotCancellable {
            reason: "a delivery that is posting is not stopped; the webhook is".into(),
        }))
    }
}

/// The failed invocation record a delivery leaves: its code and words, and whether trying again
/// could go differently.
fn failed_update(id: InvocationId, error: &ActionCallError) -> InvocationUpdate {
    InvocationUpdate::Failed {
        invocation: id,
        error: ActionFailure {
            code: error.code().to_owned(),
            message: error.to_string(),
            details: None,
            retryable: matches!(error, ActionCallError::Unavailable { .. }),
        },
    }
}

/// How a failed attempt is answered: unavailable while the step has tries left, and refused on
/// its last, so the run ends with a failed invocation record instead of retrying for ever.
fn failure(why: String, attempt: u32, attempts: u32) -> ActionCallError {
    if attempt >= attempts {
        ActionCallError::Refused { reason: why }
    } else {
        ActionCallError::Unavailable { reason: why }
    }
}

/// Why a delivery may not be posted from `workspaces`: only the device that keeps the workspace
/// posts its webhooks.
async fn governs(workspaces: &Workspaces, caller: &Caller) -> Result<String, ActionCallError> {
    let workspace = caller.workspace().ok_or(ActionCallError::NotPermitted)?;
    let id = WorkspaceId::parse(workspace).ok_or(ActionCallError::NotPermitted)?;
    if workspaces.governed_copy(id).await.is_none() {
        return Err(ActionCallError::Refused {
            reason: "a webhook is posted by the device that keeps its workspace".into(),
        });
    }
    // Its deliveries are the workspace's person's: a member reaches no URL through it.
    let person = workspaces.device_cert().user_public_key;
    let own = caller.user_public_key() == Some(&person)
        || caller.acting().person_id() == Some(hex::encode(person).as_str());
    if !own {
        return Err(ActionCallError::NotPermitted);
    }
    Ok(workspace.to_owned())
}
