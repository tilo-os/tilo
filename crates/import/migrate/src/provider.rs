//! The migration engine as a device's actions: `sources`, `connect`, `review`, `choose`, `start`,
//! `status` and `undo` of provider `so.tilo.migrate` on the resource of the device that holds the
//! data (`devices/<device>/migrate`). People, agents, the CLI and a migration's view call the same
//! actions.
//!
//! A run is one invocation of `start`: what it brings is said as that invocation's progress, and
//! cancelling it stops the run after the item it is on, which a later `start` resumes. Each step
//! runs on a thread of its own, as the device's provider answers at once and is followed.
//!
//! A source's key or password is an argument of `connect` typed as a secret, so it is redacted
//! wherever the invocation is shown or kept, and goes into the device's vault; the person's yes
//! to a step that is theirs alone is `start`'s `yes`.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use serde_json::{Map, Value};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationLog,
    InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest, Pending, Reservation,
};
use tilo_actions::{
    ActionFailure, ActionKind, ActionOutput, ActionParam, ActionSpec, EncodedAction, ExecutionMode,
    InvocationId, InvocationUpdate, OperationKind, ParamType, ResourceSpec,
};
use tilo_lock::MutexRecover as _;
use zeroize::Zeroizing;

use crate::engine::{self, Stopped, state::Place, writer::Workspace};

/// The provider a device's migrations are published under.
pub const PROVIDER: &str = "so.tilo.migrate";

/// The resource a device's migrations are, for the device whose key is `device`.
#[must_use]
pub fn resource(device: &[u8; 32]) -> String {
    format!("devices/{}/migrate", hex::encode(device))
}

/// What the device that runs migrations gives them.
pub trait Host: Send + Sync {
    /// Where the migrations into the device's workspace are kept.
    fn place(&self) -> Place;
    /// The workspace the migrations land in.
    fn workspace(&self) -> String;
    /// The home this device's folders, browsers and Thunderbird are read from.
    fn home(&self) -> PathBuf;
    /// The workspace folders this device keeps, by workspace id.
    fn folders(&self) -> BTreeMap<String, PathBuf>;
    /// A folder a run may stage what it fetches in.
    fn scratch(&self) -> PathBuf;
    /// The workspace, reached as the migration acting for `caller`: what a run makes is theirs,
    /// one History entry each run.
    ///
    /// # Errors
    /// It could not be reached.
    fn open(&self, caller: &Caller) -> Result<Box<dyn Workspace>>;
}

/// A device's migrations as an [`ActionProvider`].
pub struct Migrations {
    resource: String,
    resources: Vec<ResourceSpec>,
    host: Arc<dyn Host>,
    log: Mutex<InvocationLog<Arc<Step>>>,
    /// The sources a run is going for, by the invocation of `start` that runs it.
    going: Arc<Mutex<HashMap<String, InvocationId>>>,
}

/// One invocation in flight or settled.
struct Step {
    state: Mutex<InvocationState>,
    cancelled: AtomicBool,
}

/// What each action is, takes and does.
struct Declared {
    id: &'static str,
    label: &'static str,
    description: &'static str,
    params: Vec<ActionParam>,
    execution: ExecutionMode,
}

fn declared() -> Vec<Declared> {
    let source = || {
        ActionParam::required(
            "source",
            ParamType::Text,
            "A source, as `sources` names it: home, browsers, thunderbird, mail, gdrive, \
             onedrive, dropbox, icloud, nextcloud, webdav, outline, linear or forgejo.",
        )
    };
    vec![
        Declared {
            id: "sources",
            label: "Sources",
            description: "Every source, what it holds on this device, and how to connect the \
                          ones that need it.",
            params: Vec::new(),
            execution: ExecutionMode::Immediate,
        },
        Declared {
            id: "connect",
            label: "Connect",
            description: "Connect a source that needs signing in. Its key or password goes into \
                          this device's vault.",
            params: vec![
                source(),
                ActionParam::optional(
                    "url",
                    ParamType::Text,
                    "The address: a Nextcloud or WebDAV server, Outline's site, the Forgejo \
                     repository or owner whose issues come over.",
                ),
                ActionParam::optional(
                    "login",
                    ParamType::Text,
                    "The account: a mail address, an Apple ID, a Nextcloud or WebDAV user name.",
                ),
                ActionParam::optional("secret", ParamType::Secret, "The key or password."),
            ],
            execution: ExecutionMode::Background { cancellable: false },
        },
        Declared {
            id: "review",
            label: "Review",
            description: "List what a source holds, with counts, keeping what was chosen. A \
                          cloud drive is listed a folder and a page at a time.",
            params: vec![
                source(),
                ActionParam::optional(
                    "profile",
                    ParamType::Text,
                    "The Thunderbird profile, when this device has several.",
                ),
                ActionParam::optional(
                    "account",
                    ParamType::Text,
                    "A cloud drive: the connected account to read, by its id.",
                ),
                ActionParam::optional(
                    "within",
                    ParamType::Text,
                    "A cloud drive: the folder to list, by the key a review listed it under; \
                     its top when not given.",
                ),
                ActionParam::optional(
                    "cursor",
                    ParamType::Text,
                    "A cloud drive: where the page starts, the `next` the page before answered.",
                ),
                ActionParam::optional(
                    "limit",
                    ParamType::Integer,
                    "A cloud drive: how many items the page lists at most.",
                ),
            ],
            execution: ExecutionMode::Background { cancellable: false },
        },
        Declared {
            id: "choose",
            label: "Choose",
            description: "Choose what of a reviewed source comes over, or what stays.",
            params: vec![
                source(),
                ActionParam::optional(
                    "keys",
                    ParamType::Array,
                    "Items by the key a review lists them under, of any depth for a cloud \
                     drive: a folder brings everything under it, and `path:Work/Essay.docx` \
                     names an item by its path in the drive.",
                ),
                ActionParam::optional(
                    "account",
                    ParamType::Text,
                    "A cloud drive: the connected account to read, by its id.",
                ),
                ActionParam::optional(
                    "items",
                    ParamType::Array,
                    "Items by the key the review lists; for home, also a folder by its path.",
                ),
                ActionParam::optional("leave_out", ParamType::Boolean, "Leave these out instead."),
                ActionParam::optional(
                    "only",
                    ParamType::Boolean,
                    "Choose only these: everything else is left out.",
                ),
            ],
            execution: ExecutionMode::Background { cancellable: false },
        },
        Declared {
            id: "start",
            label: "Start",
            description: "Bring the chosen things over, as one run in the workspace; resumes a \
                          stopped run. Cancelling it stops it after the item it is on.",
            params: vec![
                source(),
                ActionParam::optional(
                    "yes",
                    ParamType::Boolean,
                    "The person said yes to connecting the chosen mail accounts.",
                ),
                ActionParam::optional(
                    "account",
                    ParamType::Text,
                    "A cloud drive: the connected account to read, by its id.",
                ),
                ActionParam::optional(
                    "destination",
                    ParamType::Text,
                    "A cloud drive: the workspace folder its files go into, by its path in the \
                     workspace's files (Documents/Trip); a folder named after the drive when \
                     not given.",
                ),
            ],
            execution: ExecutionMode::Background { cancellable: true },
        },
        Declared {
            id: "status",
            label: "Status",
            description: "How each source's migration stands.",
            params: vec![ActionParam::optional(
                "source",
                ParamType::Text,
                "One source; all of them when not given.",
            )],
            execution: ExecutionMode::Immediate,
        },
        Declared {
            id: "undo",
            label: "Undo",
            description: "Take a run back: what it brought leaves the workspace, as undoing its \
                          History entry does.",
            params: vec![ActionParam::required(
                "run",
                ParamType::Text,
                "The run, by the id `status` names, or a source for its last run.",
            )],
            execution: ExecutionMode::Background { cancellable: false },
        },
    ]
}

/// The resource `resource` with an action for each step.
fn resources(resource: &str) -> Vec<ResourceSpec> {
    let mut spec = ResourceSpec::new(resource, "devices.migrate", "Migrations");
    for step in declared() {
        let type_id = format!("{PROVIDER}/{}", step.id);
        let encoded = type_id.clone();
        let mut action = ActionSpec::typed_operation(
            step.id,
            step.label,
            "",
            OperationKind::Effect,
            type_id,
            step.params,
            move |_| EncodedAction {
                type_id: encoded.clone(),
                payload: Vec::new(),
            },
        )
        .with_description(step.description);
        let contract = &mut action.type_mut().contract;
        contract.kind = ActionKind::Outside;
        contract.execution = step.execution;
        spec.actions.push(action);
    }
    vec![spec]
}

impl Migrations {
    /// The migrations `host` runs, published on `resource`.
    #[must_use]
    pub fn new(resource: String, host: Arc<dyn Host>) -> Self {
        Self {
            resources: resources(&resource),
            resource,
            host,
            log: Mutex::default(),
            going: Arc::default(),
        }
    }

    /// The action `request` names and its arguments, admitted against its parameters.
    fn admit(
        &self,
        request: &InvokeRequest,
    ) -> Result<(String, Map<String, Value>), ActionCallError> {
        let address = &request.target.address;
        if request.target.provider != PROVIDER || address.resource != self.resource {
            return Err(ActionCallError::UnknownAction);
        }
        let action = address.action.to_string();
        let arguments = match &request.argument {
            InvokeArgument::Json(arguments) => arguments.clone(),
            InvokeArgument::None | InvokeArgument::Text(_) => Map::new(),
        };
        // A refusal's details can quote the arguments, a password among them: only its code
        // goes back.
        tilo_actions::api::admit(
            &self.resources,
            address,
            &arguments,
            request.confirmation.as_ref(),
        )
        .map_err(|error| match ActionCallError::from_api(&error) {
            ActionCallError::InvalidRequest { .. } => ActionCallError::InvalidRequest {
                reason: format!("{action} was refused; its parameters say what it takes"),
            },
            other => other,
        })?;
        Ok((action, arguments))
    }
}

impl ActionProvider for Migrations {
    fn serves(&self, provider: &str) -> bool {
        provider == PROVIDER
    }

    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Ok(CataloguePage::published(
            PROVIDER,
            &self.resources,
            &query,
        )))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let (action, arguments) = match self.admit(&request) {
            Ok(admitted) => admitted,
            Err(error) => return Pending::ready(Err(error)),
        };
        let id = request.invocation;
        match self.log.lock_recover().reserve(caller, &request) {
            Err(error) => return Pending::ready(Err(error)),
            Ok(Reservation::Admitted) => {
                let step = self.log.lock_recover().get(caller, &id).cloned();
                return Pending::ready(
                    step.map(|step| step.state.lock_recover().receipt())
                        .ok_or(ActionCallError::UnknownInvocation),
                );
            }
            Ok(Reservation::Reserved) => {}
        }
        let step = Arc::new(Step {
            state: Mutex::new(InvocationState::new(
                id,
                InvocationUpdate::Accepted { invocation: id },
            )),
            cancelled: AtomicBool::new(false),
        });
        let running = Running {
            id,
            step: Arc::clone(&step),
            host: Arc::clone(&self.host),
            going: Arc::clone(&self.going),
            caller: caller.clone(),
        };
        let spawned = std::thread::Builder::new()
            .name(format!("migrate-{action}"))
            .spawn(move || running.run(&action, &arguments));
        if let Err(error) = spawned {
            self.log.lock_recover().abandon(&id);
            return Pending::ready(Err(ActionCallError::Unavailable {
                reason: format!("the device could not run the step: {error}"),
            }));
        }
        let receipt = step.state.lock_recover().receipt();
        self.log.lock_recover().complete(&id, step);
        Pending::ready(Ok(receipt))
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.log
            .lock_recover()
            .get(caller, id)
            .map(|step| step.state.lock_recover().clone())
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        let step = self.log.lock_recover().get(caller, id).cloned();
        Pending::ready(match step {
            None => Err(ActionCallError::UnknownInvocation),
            Some(step) if step.state.lock_recover().is_terminal() => {
                Err(ActionCallError::NotCancellable {
                    reason: "the step already finished".into(),
                })
            }
            Some(step) => {
                // The run reads it before its next item.
                step.cancelled.store(true, Ordering::SeqCst);
                Ok(())
            }
        })
    }
}

/// The cloud drive `source` names, when it names one: its review, choice and destination take a
/// drive's own shapes.
fn cloud(source: &str) -> Result<Option<&'static engine::registry::Row>> {
    let row = engine::registry::named(source)?;
    Ok((row.engine == engine::registry::Engine::Cloud).then_some(row))
}

/// One step, running on its own thread.
struct Running {
    id: InvocationId,
    step: Arc<Step>,
    host: Arc<dyn Host>,
    going: Arc<Mutex<HashMap<String, InvocationId>>>,
    caller: Caller,
}

impl Running {
    fn run(self, action: &str, arguments: &Map<String, Value>) {
        let update = match self.answer(action, arguments) {
            Ok(value) => InvocationUpdate::Succeeded {
                invocation: self.id,
                output: ActionOutput {
                    value: Some(value),
                    resources: Vec::new(),
                },
            },
            Err(error) if error.is::<Stopped>() => InvocationUpdate::Cancelled {
                invocation: self.id,
                message: Some("stopped after the item it was on; start resumes it".into()),
            },
            Err(error) => InvocationUpdate::Failed {
                invocation: self.id,
                error: ActionFailure {
                    code: "migration_failed".into(),
                    message: format!("{error:#}"),
                    details: None,
                    retryable: false,
                },
            },
        };
        *self.step.state.lock_recover() = InvocationState::new(self.id, update);
    }

    /// What the step `action` answers.
    fn answer(&self, action: &str, arguments: &Map<String, Value>) -> Result<Value> {
        let text = |name: &str| arguments.get(name).and_then(Value::as_str);
        let flag = |name: &str| arguments.get(name).and_then(Value::as_bool) == Some(true);
        let place = self.host.place();
        let (home, folders, scratch) = (self.host.home(), self.host.folders(), self.host.scratch());
        let open = || self.host.open(&self.caller);
        let given = text("secret").map(|secret| Zeroizing::new(secret.to_owned()));
        let secret = |prompt: &str, _: Option<&std::path::Path>| {
            given.clone().ok_or_else(|| {
                anyhow!("connecting asks for {prompt}: give it as the step's `secret`")
            })
        };
        // A run going for a source is another invocation's: this one's own is not in its way.
        let (going, own) = (Arc::clone(&self.going), self.id);
        let running = move |source: &str| {
            going
                .lock_recover()
                .get(source)
                .is_some_and(|running| *running != own)
        };
        let cx = engine::Cx {
            place: &place,
            running: &running,
            scratch: &scratch,
            home: &home,
            folders: &folders,
            open: &open,
            secret: &secret,
        };
        let source = || text("source").ok_or_else(|| anyhow!("{action} takes a source"));
        match action {
            "sources" => Ok(engine::sources(&cx)),
            "connect" => engine::connect(
                &cx,
                source()?,
                engine::Connecting {
                    url: text("url"),
                    login: text("login"),
                    secret_file: None,
                },
            ),
            "review" => match cloud(source()?)? {
                Some(row) => engine::clouds::review(
                    &cx,
                    row,
                    engine::clouds::Page {
                        account: text("account"),
                        within: text("within"),
                        cursor: text("cursor"),
                        limit: arguments
                            .get("limit")
                            .and_then(Value::as_u64)
                            .and_then(|limit| usize::try_from(limit).ok()),
                    },
                ),
                None => engine::review(&cx, source()?, text("profile")),
            },
            "choose" => {
                let items: Vec<String> = ["keys", "items"]
                    .iter()
                    .filter_map(|name| arguments.get(*name).and_then(Value::as_array))
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect();
                let (leave_out, only) = (flag("leave_out"), flag("only"));
                match cloud(source()?)? {
                    Some(row) => {
                        if let Some(account) = text("account") {
                            engine::clouds::keep_account(&place, row, account)?;
                        }
                        engine::clouds::choose(&cx, row, &items, leave_out, only)
                    }
                    None => engine::choose(&cx, source()?, &items, leave_out, only),
                }
            }
            "start" => {
                if let Some(row) = cloud(source()?)? {
                    if let Some(account) = text("account") {
                        engine::clouds::keep_account(&place, row, account)?;
                    }
                    if let Some(given) = text("destination") {
                        let destination = engine::clouds::destination(row, Some(given))?;
                        engine::clouds::keep_destination(&place, row, &destination)?;
                    }
                }
                self.start(&cx, source()?, flag("yes"))
            }
            "status" => engine::status(&cx, text("source")),
            "undo" => {
                let run = text("run").ok_or_else(|| anyhow!("undo takes a run"))?;
                engine::undo(&cx, run, &mut *self.host.open(&self.caller)?)
            }
            other => Err(anyhow!("there is no step {other}")),
        }
    }

    /// `start`: one run of `source`, said as this invocation's progress, while no other run of
    /// it is going.
    fn start(&self, cx: &engine::Cx<'_>, source: &str, yes: bool) -> Result<Value> {
        let key = engine::registry::named(source)?.key;
        {
            let mut going = self.going.lock_recover();
            if going.contains_key(key) {
                return Err(anyhow!(
                    "{key} is already being brought over; its status says how far"
                ));
            }
            going.insert(key.to_owned(), self.id);
        }
        let sequence = AtomicU64::new(0);
        let mut progress = |line: &str| {
            let sequence = sequence.fetch_add(1, Ordering::Relaxed).saturating_add(1);
            *self.step.state.lock_recover() = InvocationState::new(
                self.id,
                InvocationUpdate::Progress {
                    invocation: self.id,
                    sequence,
                    phase: "start".into(),
                    completed: None,
                    total: None,
                    message: Some(line.to_owned()),
                },
            );
        };
        let mut confirm = |_: &str| -> Result<bool> { Ok(yes) };
        let stopping = || self.step.cancelled.load(Ordering::SeqCst);
        let workspace = self.host.workspace();
        let ran = engine::start(
            cx,
            source,
            engine::Asking {
                progress: &mut progress,
                stopping: &stopping,
                confirm: &mut confirm,
                workspace: &workspace,
                hosted: false,
            },
            &|| self.host.open(&self.caller),
        );
        self.going.lock_recover().remove(key);
        let answer = ran?;
        if self.step.cancelled.load(Ordering::SeqCst)
            && answer.get("state").and_then(Value::as_str) == Some("stopped")
        {
            return Err(Stopped.into());
        }
        Ok(answer)
    }
}

#[cfg(test)]
mod tests;
