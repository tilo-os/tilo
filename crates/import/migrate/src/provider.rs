//! The migration engine as a device's actions: `sources`, `connect`, `review`, `choose`, `start`,
//! `stop`, `status` and `undo` of provider `so.tilo.migrate` on the resource of the device that
//! holds the data (`devices/<device>/migrate`). People, agents, the CLI and a migration's view call
//! the same actions: the view by the package the resource is published as ([`PACKAGE`]).
//!
//! A run is one invocation of `start`: what it brings is said as that invocation's progress, and
//! cancelling it stops the run after the item it is on, which a later `start` resumes. `stop`
//! names the run by its source and cancels that invocation, for a caller that does not hold its
//! id. Each step runs on a thread of its own, as the device's provider answers at once and is
//! followed.
//!
//! A run lands in the device's workspace, or in another the step names by its `workspace` that
//! the device is a member of, kept apart there: its review, choice and runs are that
//! workspace's. A workspace the device is not in is refused, naming the command that joins it.
//!
//! A source's key or password is an argument of `connect` typed as a secret, so it is redacted
//! wherever the invocation is shown or kept, and goes into the device's vault; the person's yes
//! to a step that is theirs alone is `start`'s `yes`.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use anyhow::{Result, anyhow};
use serde_json::{Map, Value};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, Completer,
    InvocationLog, InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest, Pending,
    Reservation,
};
use tilo_actions::{
    ActionFailure, ActionKind, ActionOutput, ActionParam, ActionSpec, EncodedAction, ExecutionMode,
    InvocationId, InvocationUpdate, OperationKind, ParamType, ResourceSpec,
};
use tilo_lock::MutexRecover as _;
use zeroize::Zeroizing;

use crate::engine::{self, Stopped, state::Place, writer::Workspace};

pub mod caller;
mod records;

use records::Records;

/// The provider a device's migrations are published under.
pub const PROVIDER: &str = "so.tilo.migrate";

/// The package the migrations resource is published as, which code calls its actions by
/// (`use so.tilo.migrate.engine as engine`): apart from the Migrations app, whose records the
/// engine writes and whose view calls these actions.
pub const PACKAGE: &str = "so.tilo.migrate.engine";

/// What a step fails with when it asks for a key or password its caller did not give: its
/// failure's details say what it asks for (`{"asks": "Linear API key (it is not shown): "}`), so
/// the caller can ask its person and invoke the step again with the `secret`.
pub const SECRET_NEEDED: &str = "secret_needed";

/// A key or password a step asks for and was not given, by the words it asks with.
#[derive(Debug)]
struct SecretNeeded(String);

impl std::fmt::Display for SecretNeeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "connecting asks for {}: give it as the step's `secret`",
            self.0.trim().trim_end_matches(':')
        )
    }
}

impl std::error::Error for SecretNeeded {}

/// The longest a follower of a step waits before looking at it again, when nothing it can be
/// told of moved.
const LOOK_AGAIN: Duration = Duration::from_secs(1);

/// The resource a device's migrations are, for the device whose key is `device`.
#[must_use]
pub fn resource(device: &[u8; 32]) -> String {
    format!("devices/{}/migrate", hex::encode(device))
}

/// What the device that runs migrations gives them.
pub trait Host: Send + Sync {
    /// The device's own workspace: where a step that names none lands.
    fn workspace(&self) -> String;
    /// Whether the device is a member of `workspace`, so a migration may land there.
    fn member(&self, workspace: &str) -> bool;
    /// Where the migrations into `workspace` are kept.
    fn place(&self, workspace: &str) -> Place;
    /// The home this device's folders, browsers and Thunderbird are read from.
    fn home(&self) -> PathBuf;
    /// The workspace folders this device keeps, by workspace id.
    fn folders(&self) -> BTreeMap<String, PathBuf>;
    /// A folder a run into `workspace` may stage what it fetches in.
    fn scratch(&self, workspace: &str) -> PathBuf;
    /// `workspace`, reached as the migration acting for `caller`: what a run makes is theirs,
    /// one History entry each run.
    ///
    /// # Errors
    /// It could not be reached.
    fn open(&self, caller: &Caller, workspace: &str) -> Result<Box<dyn Workspace>>;
}

/// A device's migrations as an [`ActionProvider`].
pub struct Migrations {
    resource: String,
    resources: Vec<ResourceSpec>,
    host: Arc<dyn Host>,
    log: Mutex<InvocationLog<Arc<Step>>>,
    /// The sources a run is going for, each with the invocation of `start` that runs it.
    going: Going,
    /// Who waits for a step to move ([`ActionProvider::next_change`]).
    moved: Arc<Moved>,
}

/// The sources a run is going for, by the workspace it lands in and the source, each with the
/// invocation of `start` that runs it.
type Going = Arc<Mutex<HashMap<(String, String), (InvocationId, Arc<Step>)>>>;

/// One invocation in flight or settled.
struct Step {
    state: Mutex<InvocationState>,
    cancelled: AtomicBool,
    moved: Arc<Moved>,
}

impl Step {
    /// The step stands at `state` now, and whoever waits for it to move is told.
    fn set(&self, state: InvocationState) {
        *self.state.lock_recover() = state;
        self.moved.wake();
    }

    /// Stop the step after the item it is on: what cancelling its invocation does.
    fn stop(&self) -> Result<(), ActionCallError> {
        if self.state.lock_recover().is_terminal() {
            return Err(ActionCallError::NotCancellable {
                reason: "the step already finished".into(),
            });
        }
        // The run reads it before its next item.
        self.cancelled.store(true, Ordering::SeqCst);
        Ok(())
    }
}

/// The waits for any step to move: each is answered when one does, or at the latest after
/// [`LOOK_AGAIN`], so a follower never waits on a move it was not told of.
#[derive(Default)]
struct Moved(Mutex<Vec<Completer<()>>>);

impl Moved {
    fn wait(&self) -> Pending<()> {
        let (completer, answer) = tilo_actions::remote::pending();
        self.0.lock_recover().push(completer);
        answer
    }

    fn wake(&self) {
        let waiting = std::mem::take(&mut *self.0.lock_recover());
        for waiting in waiting {
            waiting.complete(());
        }
    }

    /// Answer every wait at least every [`LOOK_AGAIN`], for as long as anything holds `moved`.
    fn keep_answering(moved: Weak<Self>) {
        std::thread::spawn(move || {
            while let Some(moved) = moved.upgrade() {
                moved.wake();
                drop(moved);
                std::thread::park_timeout(LOOK_AGAIN);
            }
        });
    }
}

/// What each action is, takes and does.
struct Declared {
    id: &'static str,
    label: &'static str,
    description: &'static str,
    params: Vec<ActionParam>,
    execution: ExecutionMode,
}

#[expect(
    clippy::too_many_lines,
    reason = "one table of every step, read as the actions a caller sees"
)]
fn declared() -> Vec<Declared> {
    let workspace = || {
        ActionParam::optional(
            "workspace",
            ParamType::Text,
            "The workspace the migration lands in, by its id: one this device is a member of. \
             The device's own when not given.",
        )
    };
    let source = || {
        ActionParam::required(
            "source",
            ParamType::Text,
            "A source, as `sources` names it: home, browsers, thunderbird, mail, gdrive, \
             onedrive, dropbox, icloud, nextcloud, webdav, outline, linear or forgejo.",
        )
    };
    // Item keys are text, so code calls `choose` with a `List<Text>` (the window's checkbox).
    let texts = || serde_json::json!({"type": "array", "items": {"type": "string"}});
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
                    "The Thunderbird profile: one of this device's by its name, when it has \
                     several, or any profile by the path of its folder or its prefs.js, as the \
                     person handed it over.",
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
                workspace(),
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
                )
                .with_schema(texts()),
                ActionParam::optional(
                    "account",
                    ParamType::Text,
                    "A cloud drive: the connected account to read, by its id.",
                ),
                ActionParam::optional(
                    "items",
                    ParamType::Array,
                    "Items by the key the review lists; for home, also a folder by its path.",
                )
                .with_schema(texts()),
                ActionParam::optional("leave_out", ParamType::Boolean, "Leave these out instead."),
                ActionParam::optional(
                    "only",
                    ParamType::Boolean,
                    "Choose only these: everything else is left out.",
                ),
                workspace(),
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
                workspace(),
            ],
            execution: ExecutionMode::Background { cancellable: true },
        },
        Declared {
            id: "stop",
            label: "Stop",
            description: "Stop the source's run after the item it is on, as cancelling its \
                          `start` does; a later `start` resumes it.",
            params: vec![source(), workspace()],
            execution: ExecutionMode::Immediate,
        },
        Declared {
            id: "status",
            label: "Status",
            description: "How each source's migration stands.",
            params: vec![
                ActionParam::optional(
                    "source",
                    ParamType::Text,
                    "One source; all of them when not given.",
                ),
                workspace(),
            ],
            execution: ExecutionMode::Immediate,
        },
        Declared {
            id: "undo",
            label: "Undo",
            description: "Take a run back: what it brought leaves the workspace, as undoing its \
                          History entry does.",
            params: vec![
                ActionParam::required(
                    "run",
                    ParamType::Text,
                    "The run, by the id `status` names, or a source for its last run.",
                ),
                workspace(),
            ],
            execution: ExecutionMode::Background { cancellable: false },
        },
    ]
}

/// The resource `resource` with an action for each step.
fn resources(resource: &str) -> Vec<ResourceSpec> {
    let mut spec = ResourceSpec::new(resource, "devices.migrate", "Migrations");
    spec.app_id = Some(PACKAGE.to_owned());
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
        let moved = Arc::new(Moved::default());
        Moved::keep_answering(Arc::downgrade(&moved));
        Self {
            resources: resources(&resource),
            resource,
            host,
            log: Mutex::default(),
            going: Arc::default(),
            moved,
        }
    }

    /// The action `request` names, its arguments admitted against its parameters, and the
    /// workspace it lands in: the one it names, when the device is a member of it, or the
    /// device's own.
    fn admit(
        &self,
        request: &InvokeRequest,
    ) -> Result<(String, Map<String, Value>, String), ActionCallError> {
        let address = &request.target.address;
        if request.target.provider != PROVIDER || address.resource != self.resource {
            return Err(ActionCallError::UnknownAction);
        }
        let action = address.action.clone();
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
        let workspace = match arguments.get("workspace").and_then(Value::as_str) {
            None => self.host.workspace(),
            Some(named) if named == self.host.workspace() || self.host.member(named) => {
                named.to_owned()
            }
            Some(named) => {
                return Err(ActionCallError::Refused {
                    reason: format!("{}", caller::not_in(named)),
                });
            }
        };
        Ok((action, arguments, workspace))
    }
}

impl ActionProvider for Migrations {
    fn serves(&self, provider: &str) -> bool {
        provider == PROVIDER
    }

    fn published(&self) -> Vec<ResourceSpec> {
        self.resources.clone()
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
        let (action, arguments, workspace) = match self.admit(&request) {
            Ok(admitted) => admitted,
            Err(error) => return Pending::ready(Err(error)),
        };
        let id = request.invocation;
        let reserved = self.log.lock_recover().reserve(caller, &request);
        match reserved {
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
            moved: Arc::clone(&self.moved),
        });
        let running = Running {
            id,
            step: Arc::clone(&step),
            host: Arc::clone(&self.host),
            going: Arc::clone(&self.going),
            caller: caller.clone(),
            workspace,
            device: self
                .resource
                .trim_start_matches("devices/")
                .trim_end_matches("/migrate")
                .to_owned(),
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

    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        let step = self.log.lock_recover().get(caller, id).cloned()?;
        // Asked for before looking, so a move between the two still answers it.
        let moved = self.moved.wait();
        if step.state.lock_recover().is_terminal() {
            self.moved.wake();
        }
        Some(moved)
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        let step = self.log.lock_recover().get(caller, id).cloned();
        Pending::ready(step.map_or(Err(ActionCallError::UnknownInvocation), |step| step.stop()))
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
    going: Going,
    caller: Caller,
    /// The workspace the step lands in.
    workspace: String,
    /// The key of the device that holds the data, hex, as the migration's record names it.
    device: String,
}

impl Running {
    /// The workspace the step lands in, reached as the migration acting for its caller.
    fn open(&self) -> Result<Box<dyn Workspace>> {
        self.host.open(&self.caller, &self.workspace)
    }

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
            Err(error) => {
                let asks = error
                    .chain()
                    .find_map(|cause| cause.downcast_ref::<SecretNeeded>())
                    .map(|needed| needed.0.clone());
                InvocationUpdate::Failed {
                    invocation: self.id,
                    error: ActionFailure {
                        code: if asks.is_some() {
                            SECRET_NEEDED
                        } else {
                            "migration_failed"
                        }
                        .into(),
                        message: format!("{error:#}"),
                        details: asks.map(|asks| serde_json::json!({ "asks": asks })),
                        retryable: false,
                    },
                }
            }
        };
        self.step.set(InvocationState::new(self.id, update));
    }

    /// What the step `action` answers.
    fn answer(&self, action: &str, arguments: &Map<String, Value>) -> Result<Value> {
        let answered = self.stepped(action, arguments);
        // A run tells the migration's record as it goes ([`Self::start`]).
        if !matches!(action, "review" | "choose" | "undo") {
            return answered;
        }
        let named = |name: &str| {
            arguments
                .get(name)
                .and_then(Value::as_str)
                .and_then(|source| engine::registry::named(source).ok())
                .map(|row| row.key.to_owned())
        };
        let source = match (action, &answered) {
            ("undo", Ok(value)) => value
                .get("source")
                .and_then(Value::as_str)
                .map(str::to_owned),
            ("undo", Err(_)) => named("run"),
            _ => named("source"),
        };
        let Some(source) = source else {
            return answered;
        };
        let place = self.host.place(&self.workspace);
        let open = || self.open();
        let mut records = Records::of(&open, &place, &self.device, &source);
        let told = match &answered {
            Ok(value) if action == "undo" => records.forgot(
                value
                    .get("run_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            ),
            Ok(_) => match place.review(&source) {
                Ok(Some(review)) if action == "review" => records.reviewed(&review).map(drop),
                Ok(Some(review)) => records.chose(&review),
                Ok(None) => Ok(()),
                Err(error) => Err(error),
            },
            Err(error) => records.refused(&format!("{error:#}")),
        };
        match (answered, told) {
            (answered, Ok(())) => answered,
            (Ok(_), Err(error)) => Err(error.context("the migration's record was not told")),
            (Err(error), Err(also)) => {
                Err(error.context(format!("and the migration's record was not told: {also:#}")))
            }
        }
    }

    /// What the step `action` itself answers.
    fn stepped(&self, action: &str, arguments: &Map<String, Value>) -> Result<Value> {
        let text = |name: &str| arguments.get(name).and_then(Value::as_str);
        let flag = |name: &str| arguments.get(name).and_then(Value::as_bool) == Some(true);
        let place = self.host.place(&self.workspace);
        let (home, folders, scratch) = (
            self.host.home(),
            self.host.folders(),
            self.host.scratch(&self.workspace),
        );
        let open = || self.open();
        let given = text("secret").map(|secret| Zeroizing::new(secret.to_owned()));
        let secret = |prompt: &str, _: Option<&std::path::Path>| {
            given
                .clone()
                .ok_or_else(|| anyhow::Error::new(SecretNeeded(prompt.to_owned())))
        };
        // A run going for a source is another invocation's: this one's own is not in its way.
        let (going, own, into) = (Arc::clone(&self.going), self.id, self.workspace.clone());
        let running = move |source: &str| {
            going
                .lock_recover()
                .get(&(into.clone(), source.to_owned()))
                .is_some_and(|(running, _)| *running != own)
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
            "stop" => self.stop(source()?),
            "status" => engine::status(&cx, text("source")),
            "undo" => {
                let run = text("run").ok_or_else(|| anyhow!("undo takes a run"))?;
                engine::undo(&cx, run, &mut *self.open()?)
            }
            other => Err(anyhow!("there is no step {other}")),
        }
    }

    /// `start`: one run of `source`, said as this invocation's progress, while no other run of
    /// it is going.
    fn start(&self, cx: &engine::Cx<'_>, source: &str, yes: bool) -> Result<Value> {
        let key = engine::registry::named(source)?.key;
        let going_key = (self.workspace.clone(), key.to_owned());
        {
            let mut going = self.going.lock_recover();
            if going.contains_key(&going_key) {
                return Err(anyhow!(
                    "{key} is already being brought over; its status says how far"
                ));
            }
            going.insert(going_key.clone(), (self.id, Arc::clone(&self.step)));
        }
        let sequence = AtomicU64::new(0);
        let mut progress = |line: &str| {
            let sequence = sequence.fetch_add(1, Ordering::Relaxed).saturating_add(1);
            self.step.set(InvocationState::new(
                self.id,
                InvocationUpdate::Progress {
                    invocation: self.id,
                    sequence,
                    phase: "start".into(),
                    completed: None,
                    total: None,
                    message: Some(line.to_owned()),
                },
            ));
        };
        let mut confirm = |_: &str| -> Result<bool> { Ok(yes) };
        let stopping = || self.step.cancelled.load(Ordering::SeqCst);
        let workspace = self.workspace.clone();
        // The migration's record follows the run, through a session of its own: the run's own
        // session lands what it brings.
        let open = || self.open();
        let mut records = Records::of(&open, cx.place, &self.device, key);
        let mut run = None;
        let mut told = |told: engine::Told<'_>| match told {
            engine::Told::Begun(begun) => {
                run = Some(begun.id.clone());
                records.begun(begun)
            }
            engine::Told::Landed {
                keys,
                kind,
                count,
                already,
            } => records.landed(
                run.as_deref().unwrap_or_default(),
                keys,
                kind,
                count,
                already,
            ),
        };
        let ran = engine::start(
            cx,
            source,
            engine::Asking {
                progress: &mut progress,
                told: &mut told,
                stopping: &stopping,
                confirm: &mut confirm,
                workspace: &workspace,
                hosted: false,
            },
            &|| self.open(),
        );
        self.going.lock_recover().remove(&going_key);
        // A run that began says how it ended; one refused before it began says why.
        let ended = if run.is_some() {
            cx.place
                .journal(key)
                .and_then(|journal| records.ended(&journal))
        } else if let Err(error) = &ran {
            records.refused(&format!("{error:#}"))
        } else {
            Ok(())
        };
        let answer = match (ran, ended) {
            (ran, Ok(())) => ran,
            (Ok(_), Err(error)) => Err(error.context("the migration's record was not told")),
            (Err(error), Err(also)) => {
                Err(error.context(format!("and the migration's record was not told: {also:#}")))
            }
        }?;
        if self.step.cancelled.load(Ordering::SeqCst)
            && answer.get("state").and_then(Value::as_str) == Some("stopped")
        {
            return Err(Stopped.into());
        }
        Ok(answer)
    }

    /// `stop`: the run going for `source` stops after the item it is on, as cancelling its
    /// invocation of `start` stops it.
    fn stop(&self, source: &str) -> Result<Value> {
        let key = engine::registry::named(source)?.key;
        let going = self
            .going
            .lock_recover()
            .get(&(self.workspace.clone(), key.to_owned()))
            .cloned();
        let Some((run, step)) = going else {
            return Err(anyhow!("nothing of {key} is being brought over"));
        };
        step.stop()
            .map_err(|error| anyhow!("{key}'s run cannot stop: {}", error.said()))?;
        Ok(serde_json::json!({ "source": key, "run": run.0.to_string() }))
    }
}

#[cfg(test)]
mod tests;
