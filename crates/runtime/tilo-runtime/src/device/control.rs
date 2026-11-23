//! Device control: this device's own steps for `tilo-vm`, as the actions of provider
//! `so.tilo.devices` on resource `devices/<device>/vm`.
//!
//! Nothing here decides who may call: the device action service admitted the caller against
//! the person's grants before a request reaches this provider. A step that needs root runs as
//! its own oneshot unit, `tilo-device-<action>@<invocation>.service`, which the daemon (user
//! `tilo`) may start and stop and nothing else: the arguments are staged in the invocation's
//! directory, what the step writes to stderr is followed as the invocation's progress, and its
//! `result.json` settles it. Which steps there are is the steps' own: the daemon publishes what
//! `tilo-device-step catalogue` declares and starts each step's unit by name.

mod catalogue;
mod content;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, ContentPuts,
    InvocationLog, InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest, Pending,
    Reservation,
};
use tilo_actions::{ActionFailure, ActionOutput, InvocationId, InvocationUpdate, ResourceSpec};
use tilo_lock::MutexRecover as _;

pub use catalogue::Declared;
pub use content::{CONTENT_DIR, DeviceContent};

/// The provider the device's steps are published under.
pub const PROVIDER: &str = "so.tilo.devices";

/// Where each invocation's directory is staged; the steps' units read it as root.
pub const STEPS_DIR: &str = "/run/tilo/device-steps";

/// The program each step's unit runs, which declares the steps there are.
const STEP_PROGRAM: &str = "/usr/libexec/tilo/tilo-device-step";

/// How much of a step's progress one update carries: the end of what it wrote.
const PROGRESS_TAIL: usize = 64 * 1024;

/// How often a running step's progress is read.
const FOLLOW: Duration = Duration::from_millis(200);

/// Runs step `action` for invocation `id` to its end, as its unit; `Err` says why it could not.
pub type Start = Arc<dyn Fn(&str, &str) -> Result<(), String> + Send + Sync>;
/// Stops the unit of step `action` for invocation `id`.
pub type Stop = Arc<dyn Fn(&str, &str) + Send + Sync>;

/// The device's steps as an [`ActionProvider`].
pub struct DeviceControl {
    resource: String,
    steps_offered: Vec<Arc<str>>,
    resources: Vec<ResourceSpec>,
    steps: PathBuf,
    content: Arc<DeviceContent>,
    start: Start,
    stop: Stop,
    log: Mutex<InvocationLog<Arc<Run>>>,
}

/// One invocation in flight or settled.
struct Run {
    action: Arc<str>,
    state: Mutex<InvocationState>,
    cancelled: AtomicBool,
}

/// The device's steps for the device whose key is `device`, as its units run them: the ones
/// this device's step program declares.
///
/// # Errors
/// Why the step program declared none: it is not installed, or it did not answer.
pub fn device_control(device: [u8; 32]) -> Result<Arc<DeviceControl>, String> {
    let output = std::process::Command::new(STEP_PROGRAM)
        .arg("catalogue")
        .output()
        .map_err(|error| format!("{STEP_PROGRAM} could not run: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "{STEP_PROGRAM} catalogue failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let declared: Vec<Declared> = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("{STEP_PROGRAM} catalogue: {error}"))?;
    Ok(Arc::new(DeviceControl::new(
        device,
        &declared,
        Path::new(STEPS_DIR),
        Path::new(CONTENT_DIR),
        Arc::new(|action, id| systemctl("start", action, id)),
        Arc::new(|action, id| {
            let _ = systemctl("stop", action, id);
        }),
    )))
}

fn unit(action: &str, id: &str) -> String {
    format!("tilo-device-{action}@{id}.service")
}

fn systemctl(verb: &str, action: &str, id: &str) -> Result<(), String> {
    let unit = unit(action, id);
    let output = std::process::Command::new("systemctl")
        .args([verb, "--quiet", &unit])
        .output()
        .map_err(|error| format!("could not run systemctl: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{unit} did not run: {}; journalctl -u {unit} says why",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// How a step that failed settles: what it said, its own error whole as the details.
fn failure(message: String, details: Option<serde_json::Value>) -> InvocationUpdate {
    InvocationUpdate::Failed {
        invocation: InvocationId::default(),
        error: ActionFailure {
            code: "step_failed".into(),
            message,
            details,
            retryable: false,
        },
    }
}

/// The update `result.json` settles an invocation with.
fn settled(dir: &Path) -> Result<InvocationUpdate, String> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Outcome {
        Ok(serde_json::Value),
        Err(serde_json::Value),
    }
    let bytes = std::fs::read(dir.join("result.json"))
        .map_err(|error| format!("the step left no result: {error}"))?;
    match serde_json::from_slice(&bytes).map_err(|error| format!("the step's result: {error}"))? {
        Outcome::Ok(value) => Ok(InvocationUpdate::Succeeded {
            invocation: InvocationId::default(),
            output: ActionOutput {
                value: Some(value),
                resources: Vec::new(),
            },
        }),
        Outcome::Err(error) => {
            let message = error
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("the step failed")
                .to_owned();
            Ok(failure(message, Some(error)))
        }
    }
}

/// The progress update for what `dir/progress` holds, and how many bytes that is: `completed`
/// counts every byte the step wrote and `message` is the end of it, from a line's start, so a
/// caller that printed up to some byte prints only what follows.
fn progress(dir: &Path, action: &str) -> Option<(u64, InvocationUpdate)> {
    let text = std::fs::read(dir.join("progress")).ok()?;
    let written = u64::try_from(text.len())
        .ok()
        .filter(|&written| written > 0)?;
    let tail = match text.len().checked_sub(PROGRESS_TAIL) {
        None => text.as_slice(),
        Some(cut) => text
            .get(cut..)
            .and_then(|rest| {
                let line = rest.iter().position(|&byte| byte == b'\n')?;
                rest.get(line.saturating_add(1)..)
            })
            .unwrap_or_default(),
    };
    Some((
        written,
        InvocationUpdate::Progress {
            invocation: InvocationId::default(),
            sequence: written,
            phase: action.to_owned(),
            completed: Some(written),
            total: None,
            message: Some(String::from_utf8_lossy(tail).into_owned()),
        },
    ))
}

impl DeviceControl {
    /// The `declared` steps of the device whose key is `device`, staged under `steps`, reading
    /// content from `content`, run by `start` and stopped by `stop`.
    #[must_use]
    pub fn new(
        device: [u8; 32],
        declared: &[Declared],
        steps: &Path,
        content: &Path,
        start: Start,
        stop: Stop,
    ) -> Self {
        let resource = format!("devices/{}/vm", hex::encode(device));
        Self {
            steps_offered: declared
                .iter()
                .map(|step| Arc::from(step.id.as_str()))
                .collect(),
            resources: catalogue::resources(PROVIDER, &resource, declared),
            resource,
            steps: steps.to_owned(),
            content: Arc::new(DeviceContent::at(content)),
            start,
            stop,
            log: Mutex::default(),
        }
    }

    /// The step `request` names, its arguments admitted against its parameters.
    fn admit(
        &self,
        request: &InvokeRequest,
    ) -> Result<(Arc<str>, serde_json::Map<String, serde_json::Value>), ActionCallError> {
        let address = &request.target.address;
        let action = self
            .steps_offered
            .iter()
            .find(|id| ***id == *address.action)
            .filter(|_| request.target.provider == PROVIDER && address.resource == self.resource)
            .ok_or(ActionCallError::UnknownAction)?;
        let arguments = match &request.argument {
            InvokeArgument::Json(arguments) => arguments.clone(),
            InvokeArgument::None | InvokeArgument::Text(_) => serde_json::Map::new(),
        };
        // A refusal's details can quote the arguments, a passphrase among them: only its code
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
        Ok((Arc::clone(action), arguments))
    }

    /// Stage `arguments` and run `action` in the background, following it into `run`.
    fn begin(
        &self,
        id: InvocationId,
        run: &Arc<Run>,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), ActionCallError> {
        let name = id.0.simple().to_string();
        let dir = self.steps.join(&name);
        let staged = std::fs::create_dir_all(&dir).and_then(|()| {
            std::fs::write(
                dir.join("args.json"),
                serde_json::to_vec(arguments).map_err(std::io::Error::other)?,
            )
        });
        if let Err(error) = staged {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(ActionCallError::Unavailable {
                reason: format!("the device could not stage the step: {error}"),
            });
        }
        let (run, start, action) = (
            Arc::clone(run),
            Arc::clone(&self.start),
            Arc::clone(&run.action),
        );
        let stop = Arc::clone(&self.stop);
        std::thread::Builder::new()
            .name(format!("device-{action}"))
            .spawn(move || {
                let update = follow(&run, &dir, (&start, &stop), &name);
                let _ = std::fs::remove_dir_all(&dir);
                *run.state.lock_recover() = InvocationState::new(id, update);
            })
            .map(drop)
            .map_err(|error| ActionCallError::Unavailable {
                reason: format!("the device could not follow the step: {error}"),
            })
    }
}

/// Run the unit of `run`'s step for invocation `name` with its directory `dir`, publishing its
/// progress as it comes, and answer how it settled. Once the caller cancels, the unit is stopped
/// until it ends: a stop that came before its start finished would otherwise be lost.
fn follow(
    run: &Arc<Run>,
    dir: &Path,
    (start, stop): (&Start, &Stop),
    name: &str,
) -> InvocationUpdate {
    let (start, action, unit_name) = (Arc::clone(start), Arc::clone(&run.action), name.to_owned());
    let started = std::thread::spawn(move || start(&action, &unit_name));
    let action = &*run.action;
    let mut shown = 0;
    loop {
        let finished = started.is_finished();
        if let Some((written, update)) =
            progress(dir, action).filter(|(written, _)| *written != shown)
        {
            shown = written;
            let mut state = run.state.lock_recover();
            *state = InvocationState::new(state.invocation(), update);
        }
        if finished {
            break;
        }
        if run.cancelled.load(Ordering::SeqCst) {
            stop(action, name);
        }
        std::thread::sleep(FOLLOW);
    }
    let ran = started
        .join()
        .unwrap_or_else(|_| Err("the step's unit could not be followed".into()));
    match settled(dir) {
        Ok(update) => update,
        Err(_) if run.cancelled.load(Ordering::SeqCst) => InvocationUpdate::Cancelled {
            invocation: InvocationId::default(),
            message: Some("stopped at the caller's request".into()),
        },
        Err(missing) => failure(ran.err().unwrap_or(missing), None),
    }
}

impl ActionProvider for DeviceControl {
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
        let reserved = self.log.lock_recover().reserve(caller, &request);
        match reserved {
            Err(error) => return Pending::ready(Err(error)),
            Ok(Reservation::Admitted) => {
                let run = self.log.lock_recover().get(caller, &id).cloned();
                return Pending::ready(
                    run.map(|run| run.state.lock_recover().receipt())
                        .ok_or(ActionCallError::UnknownInvocation),
                );
            }
            Ok(Reservation::Reserved) => {}
        }
        let run = Arc::new(Run {
            action,
            state: Mutex::new(InvocationState::new(
                id,
                InvocationUpdate::Accepted { invocation: id },
            )),
            cancelled: AtomicBool::new(false),
        });
        if let Err(error) = self.begin(id, &run, &arguments) {
            self.log.lock_recover().abandon(&id);
            return Pending::ready(Err(error));
        }
        let receipt = run.state.lock_recover().receipt();
        self.log.lock_recover().complete(&id, run);
        Pending::ready(Ok(receipt))
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.log
            .lock_recover()
            .get(caller, id)
            .map(|run| run.state.lock_recover().clone())
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        let run = self.log.lock_recover().get(caller, id).cloned();
        Pending::ready(match run {
            None => Err(ActionCallError::UnknownInvocation),
            Some(run) if run.state.lock_recover().is_terminal() => {
                Err(ActionCallError::NotCancellable {
                    reason: "the step already finished".into(),
                })
            }
            Some(run) => {
                // The step's follower stops its unit.
                run.cancelled.store(true, Ordering::SeqCst);
                Ok(())
            }
        })
    }

    fn content_puts(&self) -> Option<Arc<dyn ContentPuts>> {
        Some(Arc::clone(&self.content) as Arc<dyn ContentPuts>)
    }
}

#[cfg(test)]
mod tests;
