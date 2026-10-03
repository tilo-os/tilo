//! Testing a framework app the way it runs: the real component runtime,
//! storage, permissions, undo and history, without a window.
//!
//! Prefer [`tilo_resource::Replica`] for the resource's own behaviour (it
//! needs no UI). Use [`TestApp`] to prove what the host adds: that controls
//! invoke actions, that undo reverts them, that a viewer is refused, that
//! state survives a reopen.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use tilo_resource::{Effect, Invoke, Resource};
use ui_core::application::{ApplicationHost, ComponentRunnerInputs, PreparedComponentInputs};
use ui_core::bound_dispatch::BoundActionAddress;
use ui_core::headless::FrameHarness;
use ui_core::launch::LaunchError;
use ui_core::permissions::{Actor, PermissionContext, Role, UserId};

const AUTHOR: &str = "test-author";
use ui_core::record::{RecordCheckout, RecordStore};

use super::app::{
    DATA_DIR_INSTANCE, ResourceApp, data_dir_host, default_record, definition, role_permissions,
};

const INSTANCE: u64 = 7;

enum Profile {
    /// A private temporary profile, removed with the app.
    Temporary(Rc<tempfile::TempDir>),
    /// An explicit `--data-dir` profile the app itself would open.
    DataDir(PathBuf),
}

/// A component prepared on a private or explicit profile keeps its settings
/// in that profile too, never in the person's session document.
pub(crate) fn with_settings_file(
    mut prepared: PreparedComponentInputs,
    file: std::path::PathBuf,
) -> PreparedComponentInputs {
    prepared
        .context
        .provide_settings(ui_core::storage::SettingsDocument::File(file));
    prepared
}

/// The settings document of a `--data-dir` profile.
pub(crate) fn data_dir_settings(directory: &std::path::Path) -> std::path::PathBuf {
    std::path::absolute(directory)
        .unwrap_or_else(|_| directory.to_path_buf())
        .join("settings.json")
}

impl Profile {
    /// The profile's settings document, which the app's typed settings use
    /// instead of the person's.
    fn settings(&self) -> PathBuf {
        match self {
            Self::Temporary(directory) => directory.path().join("settings.json"),
            Self::DataDir(directory) => data_dir_settings(directory),
        }
    }
}

/// One framework app mounted headlessly: a test, a fixture seeder or an
/// inspection tool drives it through the real runtime.
pub struct HeadlessApp<A: ResourceApp> {
    harness: Option<FrameHarness>,
    records: RecordStore,
    checkout: RecordCheckout,
    instance: u64,
    profile: Profile,
    role: Role,
    /// The last effect [`Self::effect`] invoked.
    last_effect: Option<tilo_actions::InvocationId>,
    /// Shared with the host: changing it is a role change the running app
    /// sees, as when the record's owner changes someone's access.
    permissions: Rc<RefCell<PermissionContext>>,
    marker: std::marker::PhantomData<A>,
}

/// The name tests use for [`HeadlessApp`].
pub type TestApp<A> = HeadlessApp<A>;

fn operation(error: impl std::fmt::Display) -> LaunchError {
    LaunchError::Operation(error.to_string())
}

impl<A: ResourceApp> HeadlessApp<A> {
    /// The app as its owner opens it on a fresh private profile.
    ///
    /// # Errors
    /// Returns any launch, storage or construction failure.
    pub fn new() -> Result<Self, LaunchError> {
        Self::with_role(Role::Owner)
    }

    /// The app as a participant with `role` opens it on a fresh profile.
    ///
    /// # Errors
    /// Returns any launch, storage or construction failure.
    pub fn with_role(role: Role) -> Result<Self, LaunchError> {
        let profile = Rc::new(tempfile::tempdir().map_err(operation)?);
        Self::open(Profile::Temporary(profile), role)
    }

    /// The app on the profile `--data-dir directory` opens, as its owner.
    /// Only one writer may hold a profile: this fails while the app (or
    /// another `HeadlessApp`) has it open.
    ///
    /// # Errors
    /// Returns any launch, storage or construction failure.
    pub fn open_data_dir(directory: &std::path::Path) -> Result<Self, LaunchError> {
        Self::open(Profile::DataDir(directory.to_path_buf()), Role::Owner)
    }

    fn open(profile: Profile, role: Role) -> Result<Self, LaunchError> {
        let author = UserId(AUTHOR.into());
        let permissions = role_permissions(AUTHOR, role);
        let (mut host, instance) = match &profile {
            Profile::Temporary(directory) => (
                ApplicationHost::new_with_profile_lifetime(
                    directory.path().join("state"),
                    "test-device".into(),
                    "test-workspace",
                    author,
                    Rc::clone(&permissions),
                    tilo_actions::ActionContext::default(),
                    directory.clone(),
                )?,
                INSTANCE,
            ),
            Profile::DataDir(directory) => (
                data_dir_host(directory, &author, Rc::clone(&permissions))?,
                DATA_DIR_INSTANCE,
            ),
        };
        let definition = definition::<A>();
        let records = host.records_for(definition)?;
        let checkout = default_record::<A>();
        let prepared = host.prepare_default(
            definition,
            instance,
            checkout.clone(),
            None,
            ComponentRunnerInputs::from_native_process(),
        )?;
        let prepared = with_settings_file(prepared, profile.settings());
        let component =
            definition.instantiate(prepared.context, prepared.target, prepared.restore)?;
        let harness =
            FrameHarness::from_component(component, A::SHOT_VIEWPORT, prepared.runner_inputs)?;
        Ok(Self {
            harness: Some(harness),
            records,
            checkout,
            instance,
            profile,
            role,
            last_effect: None,
            permissions,
            marker: std::marker::PhantomData,
        })
    }

    /// The app's typed settings in this profile's settings document, which
    /// the running app follows: a write here is what another window, the
    /// Settings app or another of the person's devices writing them does.
    pub fn settings<T: ui_core::storage::SettingsSchema>(&self) -> ui_core::storage::Settings<T> {
        ui_core::storage::Settings::for_app(A::ID).in_file(self.profile.settings())
    }

    /// Change the participant's role while the app runs, as the record's
    /// owner changing their access does. Takes effect on the next frame,
    /// which this runs.
    pub fn set_role(&mut self, role: Role) {
        *self.permissions.borrow_mut() =
            PermissionContext::from_capabilities(Actor::person(Some(role), AUTHOR), vec![]);
        self.role = role;
        self.harness().relayout();
    }

    /// Close the app (flushing its record) and open it again on the same
    /// profile, as a relaunch would.
    ///
    /// # Errors
    /// Returns the flush or relaunch failure.
    pub fn reopen(mut self) -> Result<Self, LaunchError> {
        self.close()?;
        let profile = match &self.profile {
            Profile::Temporary(directory) => Profile::Temporary(Rc::clone(directory)),
            Profile::DataDir(directory) => Profile::DataDir(directory.clone()),
        };
        let role = self.role;
        // Release this instance's record writer before the next one opens it.
        drop(self);
        Self::open(profile, role)
    }

    /// Make every edit so far durable, as closing the app does.
    ///
    /// # Errors
    /// Returns the record writer's storage failure.
    pub fn flush(&mut self) -> Result<(), LaunchError> {
        self.harness().flush_component_records().map_err(operation)
    }

    fn close(&mut self) -> Result<(), LaunchError> {
        if let Some(mut harness) = self.harness.take() {
            harness.flush_component_records().map_err(operation)?;
            harness.close_components();
        }
        Ok(())
    }

    /// The headless frame harness, for input-level tests (`click_named`,
    /// `type_text`, `targets`).
    ///
    /// # Panics
    /// Panics after the app was closed, which the API does not allow.
    #[allow(clippy::expect_used, reason = "closing consumes the TestApp")]
    pub fn harness(&mut self) -> &mut FrameHarness {
        self.harness.as_mut().expect("app is open")
    }

    /// Invoke a declared action through the host door, exactly as a control,
    /// the palette, an agent or a script does.
    ///
    /// # Errors
    /// Returns the host's refusal: unknown action, bad arguments, denied
    /// permission or the action's own refusal.
    pub fn invoke(&mut self, invoke: Invoke<A::Resource>) -> Result<(), tilo_actions::DoorError> {
        self.invoke_as(invoke, false)
    }

    /// [`Self::invoke`] after the caller confirmed it, as the host's
    /// confirmation prompt does for a destructive action.
    ///
    /// # Errors
    /// As [`Self::invoke`], except that no confirmation is required.
    pub fn invoke_confirmed(
        &mut self,
        invoke: Invoke<A::Resource>,
    ) -> Result<(), tilo_actions::DoorError> {
        self.invoke_as(invoke, true)
    }

    /// What an agent sees of the running app: every published action with
    /// its address, label, description and typed parameters, in a stable
    /// order. Snapshot it so a change shows how it altered the vocabulary an
    /// agent, a script or a screen reader works with, and what it costs in
    /// context.
    pub fn agent_surface(&mut self) -> serde_json::Value {
        let resources = self.harness().resources();
        let mut rows: Vec<serde_json::Value> = tilo_actions::door::survey(&resources)
            .into_iter()
            .map(|descriptor| {
                serde_json::json!({
                    "resource": descriptor.address.resource.as_str(),
                    "action": descriptor.address.action,
                    "resource_kind": descriptor.resource_kind,
                    "label": descriptor.action_label,
                    "description": descriptor.description,
                    "operation": descriptor.kind,
                    "params": descriptor.params,
                })
            })
            .collect();
        rows.sort_by_key(|row| (row["resource"].to_string(), row["action"].to_string()));
        serde_json::Value::Array(rows)
    }

    /// Run a view or effect operation (`look`, `export`) through the host
    /// door, as an agent or script does, and answer with what it produced;
    /// `None` while it is still running.
    ///
    /// # Errors
    /// As [`Self::invoke`].
    pub fn operate(
        &mut self,
        invoke: Invoke<A::Resource>,
    ) -> Result<Option<tilo_actions::ActionOutput>, tilo_actions::DoorError> {
        let Invoke { id, arguments, .. } = invoke;
        let address = BoundActionAddress {
            instance_id: self.instance,
            address: tilo_actions::ActionAddress::new(<A::Resource as Resource>::KIND, id),
        };
        let harness = self.harness();
        let started = harness.invoke_bound_action(&address, &arguments)?;
        harness.relayout();
        Ok(started.completed())
    }

    /// Invoke one of the app's effects through the host door, as a button,
    /// the palette or an agent does, and wait for its answer: what it
    /// reported at once, or the result of the job it started. Progress the
    /// job reported is in [`FrameHarness::job_updates`].
    ///
    /// # Errors
    /// Returns the host's refusal or the effect's failure.
    pub fn effect<E: Effect>(
        &mut self,
        effect: &E,
    ) -> Result<tilo_actions::ActionOutput, tilo_actions::DoorError> {
        self.effect_as(effect, false)
    }

    /// [`Self::effect`] after the caller confirmed it, as the host's
    /// confirmation prompt does for a destructive effect.
    ///
    /// # Errors
    /// As [`Self::effect`].
    pub fn effect_confirmed<E: Effect>(
        &mut self,
        effect: &E,
    ) -> Result<tilo_actions::ActionOutput, tilo_actions::DoorError> {
        self.effect_as(effect, true)
    }

    /// What the job of the last [`Self::effect`] reported, in order: its
    /// progress, then its result.
    pub fn effect_progress(&mut self) -> Vec<tilo_actions::InvocationUpdate> {
        let Some(invocation) = self.last_effect else {
            return Vec::new();
        };
        self.harness().job_updates(invocation).to_vec()
    }

    fn effect_as<E: Effect>(
        &mut self,
        effect: &E,
        confirmed: bool,
    ) -> Result<tilo_actions::ActionOutput, tilo_actions::DoorError> {
        let address = BoundActionAddress {
            instance_id: self.instance,
            address: tilo_actions::ActionAddress::new(<A::Resource as Resource>::KIND, effect.id()),
        };
        let (invocation, started) =
            self.harness()
                .start_bound_job(&address, &effect.arguments(), confirmed)?;
        self.last_effect = Some(invocation);
        let harness = self.harness();
        if let Some(output) = started.completed() {
            harness.relayout();
            return Ok(output);
        }
        let finished = |harness: &FrameHarness| {
            harness
                .job_updates(invocation)
                .iter()
                .find(|update| update.is_terminal())
                .cloned()
        };
        harness.wait_until(ui_core::time::Duration::from_secs(30), |harness| {
            finished(harness).is_some()
        });
        match finished(harness) {
            Some(tilo_actions::InvocationUpdate::Succeeded { output, .. }) => Ok(output),
            Some(tilo_actions::InvocationUpdate::Failed { error, .. }) => {
                Err(tilo_actions::DoorError::Failed(error.message))
            }
            Some(_) => Err(tilo_actions::DoorError::Failed("cancelled".into())),
            None => Err(tilo_actions::DoorError::Failed(format!(
                "`{}` did not finish",
                effect.id()
            ))),
        }
    }

    /// Run the frame the app runs after an event: edits the views and
    /// effects submitted are applied, and what they change is drawn.
    pub fn settle(&mut self) {
        self.harness()
            .wait_until(ui_core::time::Duration::ZERO, |_| true);
    }

    /// Invoke any published action of the resource by id with JSON
    /// arguments, as an agent's `tilo_invoke` does — the framework's own
    /// (`undo`, `history`, `render`) as well as the app's — and answer with
    /// what it reported.
    ///
    /// # Errors
    /// As [`Self::invoke`].
    pub fn call(
        &mut self,
        id: &str,
        arguments: serde_json::Value,
    ) -> Result<Option<serde_json::Value>, tilo_actions::DoorError> {
        self.call_within(None, id, arguments)
    }

    /// [`Self::call`] as one agent run: every call naming the same `run`
    /// belongs to it, the way an agent's connection groups its invocations.
    ///
    /// # Errors
    /// As [`Self::invoke`].
    pub fn call_in_run(
        &mut self,
        run: &str,
        id: &str,
        arguments: serde_json::Value,
    ) -> Result<Option<serde_json::Value>, tilo_actions::DoorError> {
        let ambient = tilo_actions::ActionContext::default().with_group(
            run,
            run,
            ui_core::history_source::steps::AGENT_RUN,
        );
        self.call_within(Some(&ambient), id, arguments)
    }

    fn call_within(
        &mut self,
        ambient: Option<&tilo_actions::ActionContext>,
        id: &str,
        arguments: serde_json::Value,
    ) -> Result<Option<serde_json::Value>, tilo_actions::DoorError> {
        let serde_json::Value::Object(arguments) = arguments else {
            return Err(tilo_actions::DoorError::InvalidArguments(
                "arguments are an object".into(),
            ));
        };
        let address = BoundActionAddress {
            instance_id: self.instance,
            address: tilo_actions::ActionAddress::new(<A::Resource as Resource>::KIND, id),
        };
        let harness = self.harness();
        let started = match ambient {
            Some(ambient) => harness.invoke_bound_action_within(&address, &arguments, ambient)?,
            None => harness.invoke_bound_action(&address, &arguments)?,
        };
        harness.relayout();
        Ok(started.completed().and_then(|output| output.value))
    }

    fn invoke_as(
        &mut self,
        invoke: Invoke<A::Resource>,
        confirmed: bool,
    ) -> Result<(), tilo_actions::DoorError> {
        let Invoke { id, arguments, .. } = invoke;
        let address = BoundActionAddress {
            instance_id: self.instance,
            address: tilo_actions::ActionAddress::new(<A::Resource as Resource>::KIND, id),
        };
        let harness = self.harness();
        if confirmed {
            harness.invoke_bound_action_confirmed(&address, &arguments)?;
        } else {
            harness.invoke_bound_action(&address, &arguments)?;
        }
        harness.relayout();
        Ok(())
    }

    /// The resource state as stored.
    ///
    /// # Panics
    /// Panics when the record is unreadable, which a test should surface.
    #[allow(
        clippy::expect_used,
        reason = "a test harness surfaces corruption loudly"
    )]
    pub fn state(&self) -> <A::Resource as Resource>::State {
        self.records
            .read(&self.checkout.id, |doc| {
                <A::Resource as Resource>::read(doc)
            })
            .expect("record is open")
            .expect("resource reads")
    }

    /// Read the stored document itself — the parts of a large resource its
    /// state does not hold until a view demands them.
    ///
    /// # Panics
    /// Panics when the record is not open, which the API does not allow.
    #[allow(
        clippy::expect_used,
        reason = "a test harness surfaces a closed record loudly"
    )]
    pub fn read_document<T>(&self, read: impl FnOnce(&tilo_resource::Doc) -> T) -> T {
        self.records
            .read(&self.checkout.id, read)
            .expect("record is open")
    }

    /// A copy of the stored document written by another participant: a
    /// collaborator's replica, for a test to edit and [`Self::merge_peer`]
    /// back.
    pub fn peer_document(&self) -> tilo_resource::Doc {
        self.read_document(|doc| doc.clone().fork())
    }

    /// Exchange edits with a collaborator's replica, as sync does both ways:
    /// the peer's edits arrive in the running app (and its views), and the
    /// app's edits arrive in `peer`.
    ///
    /// # Errors
    /// Returns why either side refused the other's document.
    pub fn merge_peer(&mut self, peer: &mut tilo_resource::Doc) -> Result<(), String> {
        use tilo_automerge::ApplyIncoming;
        let mut ours = self.read_document(Clone::clone);
        peer.merge_incoming(&mut ours)
            .map_err(|error| error.to_string())?;
        self.records
            .merge_peer_document(&self.checkout, peer.save())?;
        self.harness().relayout();
        Ok(())
    }

    /// The record's version history.
    pub fn versions(&self) -> Vec<tilo_resource::history::Version> {
        self.records
            .read(&self.checkout.id, |doc| {
                tilo_resource::history::versions(&mut doc.clone())
            })
            .unwrap_or_default()
    }

    /// Stage a screenshot scenario (`None` for a plain `--shots`) exactly as
    /// the launcher does before a capture: its actions, then its view state.
    /// The harness then holds the frame `--shots --scenario` would write.
    ///
    /// # Errors
    /// Returns the host's refusal of a scenario action.
    pub fn stage(&mut self, scenario: Option<&str>) -> Result<(), LaunchError> {
        super::stage::stage::<A>(self.harness(), scenario)
    }

    /// The version at `heads` as History mode previews it: the app's own
    /// view, read-only, over the document as it was then.
    ///
    /// # Errors
    /// Returns why that version cannot be read or drawn.
    pub fn past_version(
        &self,
        heads: &[ui_core::automerge::ChangeHash],
    ) -> Result<super::preview::PastVersion<A::Resource>, String> {
        let past = self
            .records
            .read(&self.checkout.id, |doc| doc.clone().fork_at(heads))
            .ok_or("the record is not open")?
            .map_err(|error| error.to_string())?;
        super::preview::render_past::<A>(past, A::SHOT_VIEWPORT)
    }

    /// A collaborator's edit arriving: `edit` writes on their own replica of
    /// the record as it stands now, under their own actor, and the change is
    /// merged in as sync merges a peer's. It is not the local participant's
    /// action, so it never lands on their undo stack.
    ///
    /// # Panics
    /// Panics when the record is not open or the change does not merge,
    /// which a test should surface.
    #[allow(
        clippy::expect_used,
        reason = "a test harness surfaces a failed merge loudly"
    )]
    pub fn merge_peer_edit(&mut self, edit: impl FnOnce(&mut tilo_resource::Doc)) {
        use tilo_automerge::ApplyIncoming as _;
        let mut peer = self
            .records
            .with(&self.checkout.id, tilo_resource::Doc::fork)
            .expect("record is open");
        edit(&mut peer);
        peer.commit();
        self.records
            .with(&self.checkout.id, |doc| doc.merge_incoming(&mut peer))
            .expect("record is open")
            .expect("a peer's change merges");
        self.harness().relayout();
    }

    /// Undo the local participant's last action, as Ctrl+Z does.
    pub fn undo(&mut self) -> bool {
        let harness = self.harness();
        let undone = harness.perform_undo();
        harness.relayout();
        undone
    }

    /// Redo, as Ctrl+Shift+Z does.
    pub fn redo(&mut self) -> bool {
        let harness = self.harness();
        let redone = harness.perform_redo();
        harness.relayout();
        redone
    }
}

/// Inspect the window `A`'s live launch opens: the same definition and
/// component, mounted in a native [`ui_core::App`] on a fresh private
/// profile, without running its event loop. For what the window itself
/// declares, such as capture protection.
///
/// # Errors
/// Returns any launch, storage or construction failure.
pub fn inspect_live_window<A: ResourceApp, T>(
    inspect: impl FnOnce(&ui_core::App) -> T,
) -> Result<T, LaunchError> {
    let profile = tempfile::tempdir().map_err(operation)?;
    let mut host = ApplicationHost::new(
        profile.path().join("state"),
        "test-device".into(),
        "test-workspace",
        UserId(AUTHOR.into()),
        role_permissions(AUTHOR, Role::Owner),
        tilo_actions::ActionContext::default(),
    )?;
    let definition = definition::<A>();
    let prepared = host.prepare_default(
        definition,
        INSTANCE,
        default_record::<A>(),
        None,
        ComponentRunnerInputs::from_native_process(),
    )?;
    let component = definition.instantiate(prepared.context, prepared.target, prepared.restore)?;
    let mut app = ui_core::App::from_component(component, prepared.runner_inputs)?;
    let seen = inspect(&app);
    app.close_components();
    Ok(seen)
}

impl<A: ResourceApp> Drop for HeadlessApp<A> {
    fn drop(&mut self) {
        let _ = self.close();
    }
}
