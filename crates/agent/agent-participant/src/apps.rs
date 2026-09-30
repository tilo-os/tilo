//! Apps as an agent uses them: the real component, mounted without a window,
//! reached only through the resource/action door.
//!
//! [`HostedApp`] mounts any [`ApplicationDefinition`] — the same component a
//! desktop window or the browser runtime mounts — over a private profile
//! whose one record is the room's durable record. What the agent may do is
//! exactly what [`FrameHarness::resources`] publishes: the app's declared
//! resources and actions, plus the semantic projection of every operable
//! control (buttons, fields, tabs, toggles, sliders) that
//! `tilo_ui::a11y_tree` adds for every app. A person clicking a control and
//! the agent invoking `ui:<control>#activate` run the same dispatch, so the
//! agent reaches everything a person can without any app-specific code.
//!
//! Durable state moves as Automerge changes: [`HostedApp::take_mutation`]
//! returns this participant's own changes since the last call (never ones it
//! received), and [`HostedApp::apply`] merges another participant's.

use std::collections::HashSet;
use std::rc::Rc;

use automerge::ChangeHash;
use collab_types::DocumentMutation;
use tilo_actions::door::{ActionAddress, ActionDescriptor, DoorError};
use tilo_automerge::ApplyIncoming as _;
use ui::application::{ApplicationDefinition, ApplicationHost, ComponentRunnerInputs};
use ui::headless::{Control, ControlInput, FrameHarness};
use ui::launch::LaunchError;
use ui::permissions::{Actor, PermissionContext, Principal, Role, UserId};
use ui::record::{RecordCheckout, RecordId, RecordStore};
use ui::widget::ResourceOperationStart;

/// Resource id prefix of a control the semantic tree projects.
pub const SEMANTIC_RESOURCE_PREFIX: &str = "ui:";
/// Resource kind of a projected control.
pub const SEMANTIC_RESOURCE_KIND: &str = "tilo.ui.control";

const INSTANCE: u64 = 1;
const VIEWPORT: (u32, u32) = (1280, 800);

/// Builds the presentation inputs (theme, fonts) each mounted app gets.
pub type RunnerInputs = Rc<dyn Fn() -> ComponentRunnerInputs>;

/// The apps an agent can open, by app id.
#[derive(Clone)]
pub struct AppCatalogue {
    apps: Vec<&'static ApplicationDefinition>,
    inputs: RunnerInputs,
}

/// Every app the browser desktop registers (from the app manifests): what a
/// browser session can mount, and so what an agent in it can open.
#[must_use]
pub fn component_apps() -> Vec<&'static ApplicationDefinition> {
    crate::web_apps::web_apps()
}

impl AppCatalogue {
    /// `apps`, presented with `inputs`.
    #[must_use]
    pub fn new(apps: Vec<&'static ApplicationDefinition>, inputs: RunnerInputs) -> Self {
        Self { apps, inputs }
    }

    /// [`component_apps`] with this machine's own theme and fonts.
    #[must_use]
    pub fn native() -> Self {
        Self::new(
            component_apps(),
            Rc::new(ComponentRunnerInputs::from_native_process),
        )
    }

    /// The definition of `app_id`.
    #[must_use]
    pub fn get(&self, app_id: &str) -> Option<&'static ApplicationDefinition> {
        self.apps
            .iter()
            .copied()
            .find(|definition| definition.app_id == app_id)
    }

    /// Every app, in registration order.
    pub fn iter(&self) -> impl Iterator<Item = &'static ApplicationDefinition> + '_ {
        self.apps.iter().copied()
    }

    fn inputs(&self) -> ComponentRunnerInputs {
        (self.inputs)()
    }
}

/// Which record a [`HostedApp`] mounts.
pub enum Seed {
    /// A new record the app initializes itself.
    New,
    /// An existing record, from the durable changes the host replayed.
    Existing {
        /// The record's id.
        record: RecordId,
        /// Its Automerge changes (a snapshot, then increments), in order.
        changes: Vec<Vec<u8>>,
    },
}

/// Why an app could not be mounted or its record not merged.
#[derive(Debug)]
pub enum AppError {
    /// No such app in the catalogue.
    UnknownApp(String),
    /// Launching the component failed.
    Launch(String),
    /// The record's durable changes could not be merged.
    Merge(String),
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownApp(app) => write!(f, "no app {app} can be opened here"),
            Self::Launch(reason) => write!(f, "the app did not launch: {reason}"),
            Self::Merge(reason) => write!(f, "the record did not merge: {reason}"),
        }
    }
}

impl From<LaunchError> for AppError {
    fn from(error: LaunchError) -> Self {
        Self::Launch(error.to_string())
    }
}

impl std::error::Error for AppError {}

/// One app mounted headless on one record.
pub struct HostedApp {
    app_id: &'static str,
    record: RecordId,
    records: RecordStore,
    harness: FrameHarness,
    /// Heads up to which this participant's changes were taken.
    taken: Vec<ChangeHash>,
    /// Changes that arrived from other participants; never republished.
    received: HashSet<ChangeHash>,
    /// Keeps the private profile alive while the app runs.
    _profile: Rc<tempfile::TempDir>,
}

impl HostedApp {
    /// Mount `definition` for `participant` (its author id) in `room`.
    ///
    /// `agent` makes the participant an agent principal holding the editor
    /// role; otherwise it is a person holding the owner role, as a browser
    /// tab of the record's owner does.
    ///
    /// # Errors
    ///
    /// Returns why the record could not be seeded or the app not launched.
    pub fn open(
        catalogue: &AppCatalogue,
        app_id: &str,
        room: &str,
        participant: &str,
        agent: bool,
        seed: Seed,
    ) -> Result<Self, AppError> {
        let definition = catalogue
            .get(app_id)
            .ok_or_else(|| AppError::UnknownApp(app_id.to_owned()))?;
        let profile =
            Rc::new(tempfile::tempdir().map_err(|error| AppError::Launch(error.to_string()))?);
        let actor = if agent {
            Actor {
                role: Some(Role::Editor),
                key: None,
                principals: vec![Principal::Agent(participant.to_owned())],
            }
        } else {
            Actor::person(Some(Role::Owner), participant)
        };
        let permissions = Rc::new(std::cell::RefCell::new(
            PermissionContext::from_capabilities(actor, Vec::new()),
        ));
        let lifetime: Rc<dyn std::any::Any> = profile.clone();
        let mut host = ApplicationHost::new_with_profile_lifetime(
            profile.path().join("state"),
            participant.to_owned(),
            room,
            UserId(participant.to_owned()),
            permissions,
            tilo_actions::ActionContext::default(),
            lifetime,
        )?;
        let (checkout, changes) = match seed {
            Seed::New if definition.keeps_room_record() => (
                RecordCheckout::package_primary(definition.record_kind.clone()),
                Vec::new(),
            ),
            Seed::New => (
                RecordCheckout::new(RecordId::minted(), definition.record_kind.clone()),
                Vec::new(),
            ),
            Seed::Existing { record, changes } => (
                RecordCheckout::new(record, definition.record_kind.clone()),
                changes,
            ),
        };
        let mut received = HashSet::new();
        if !changes.is_empty() && definition.keeps_room_record() {
            return Err(AppError::Merge(
                "the room's own record arrives through the room, not as a copy".into(),
            ));
        }
        if !changes.is_empty() {
            // Records begin with one genesis every member derives alike, so a
            // record that arrives is installed as it is, not re-created here.
            let mut doc = automerge::AutoCommit::new();
            for change in &changes {
                doc.load_incoming(change)
                    .map_err(|error| AppError::Merge(error.to_string()))?;
            }
            received.extend(hashes(&mut doc));
            let store: Rc<dyn durable_store::DurableStore> =
                Rc::new(app_state::FsStore::new(profile.path().join("state")));
            ui::record::transfer::install_as(
                &store,
                definition.app_id,
                room,
                checkout.clone(),
                &mut doc,
            )
            .map_err(|error| AppError::Merge(error.to_string()))?;
        }
        let records = host.records_for(definition)?;
        let prepared = host.prepare_default(
            definition,
            INSTANCE,
            checkout.clone(),
            None,
            catalogue.inputs(),
        )?;
        let component =
            definition.instantiate(prepared.context, prepared.target, prepared.restore)?;
        let mut harness =
            FrameHarness::from_component(component, VIEWPORT, prepared.runner_inputs)?;
        harness.relayout();
        Ok(Self {
            app_id: definition.app_id,
            record: checkout.id,
            records,
            harness,
            taken: Vec::new(),
            received,
            _profile: profile,
        })
    }

    /// The app's id.
    #[must_use]
    pub const fn app_id(&self) -> &'static str {
        self.app_id
    }

    /// The record's id, as its durable route names it.
    #[must_use]
    pub fn record(&self) -> &str {
        self.record.as_str()
    }

    /// The record's id.
    #[must_use]
    pub const fn record_id(&self) -> &RecordId {
        &self.record
    }

    /// Every action the app publishes now, declared and semantic, as inert
    /// descriptors.
    pub fn survey(&mut self) -> Vec<ActionDescriptor> {
        self.harness.relayout();
        tilo_actions::door::survey(&self.harness.resources())
    }

    /// Invoke one published action through the app's own door: availability,
    /// confirmation and strict argument binding exactly as for a person.
    ///
    /// # Errors
    ///
    /// Returns the door's refusal or the action's failure.
    pub fn invoke(
        &mut self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirm: bool,
    ) -> Result<ResourceOperationStart, DoorError> {
        let result = self
            .harness
            .invoke_action(address, arguments, confirm, None);
        self.harness.relayout();
        result
    }

    /// Every on-screen control, whether or not the app declared an action
    /// for it: what a person sees and can operate.
    pub fn controls(&mut self) -> Vec<Control> {
        self.harness.relayout();
        self.harness.controls()
    }

    /// Operate the control `id` the way a person's input does: press,
    /// focus, set its value, type into it, scroll it.
    ///
    /// # Errors
    ///
    /// Returns why the control is not there, does not take this input, or
    /// the action it triggered was refused.
    pub fn operate(&mut self, id: &str, input: &ControlInput) -> Result<(), String> {
        let result = self.harness.operate_control(id, input);
        self.harness.relayout();
        result
    }

    /// Where a person would point to do what `address` does: the centre of
    /// the control a projected action drives, in the app's surface
    /// coordinates.
    #[must_use]
    pub fn pointer_for(&self, address: &ActionAddress) -> Option<[f32; 2]> {
        self.harness
            .projected_control_bounds(&address.resource)
            .map(centre)
    }

    /// Where a person would point to do what `descriptor` does, for any
    /// action: the control a projected action drives; else the on-screen
    /// control whose name shares the most words with the action's label
    /// (then its resource's label), the smallest on a tie; else the
    /// record's own surface (its largest document, table or text area);
    /// else the middle of the app. So a declared action moves the cursor
    /// to the button a person would press, or at least onto the record.
    pub fn pointer_for_action(&mut self, descriptor: &ActionDescriptor) -> [f32; 2] {
        if let Some(pointer) = self.pointer_for(&descriptor.address) {
            return pointer;
        }
        let controls: Vec<Control> = self
            .controls()
            .into_iter()
            .filter(|control| control.bounds.is_some())
            .collect();
        let area = |control: &Control| control.bounds.map_or(f64::MAX, |b| b.area());
        for label in [&descriptor.action_label, &descriptor.resource_label] {
            let wanted = label_words(label);
            let best = controls
                .iter()
                .map(|control| {
                    let own = label_words(control.name.as_deref().unwrap_or_default());
                    (
                        wanted.iter().filter(|word| own.contains(word)).count(),
                        control,
                    )
                })
                .filter(|(shared, _)| *shared > 0)
                .max_by(|(a, left), (b, right)| a.cmp(b).then(area(right).total_cmp(&area(left))));
            if let Some((_, control)) = best
                && let Some(bounds) = control.bounds
            {
                return centre(bounds);
            }
        }
        controls
            .iter()
            .filter(|control| {
                matches!(
                    control.role,
                    ui::accesskit::Role::Document
                        | ui::accesskit::Role::Table
                        | ui::accesskit::Role::Grid
                        | ui::accesskit::Role::MultilineTextInput
                        | ui::accesskit::Role::TextInput
                )
            })
            .filter(|control| {
                control.bounds.is_some_and(|b| {
                    b.width() < f64::from(VIEWPORT.0) || b.height() < f64::from(VIEWPORT.1)
                })
            })
            .max_by(|left, right| area(left).total_cmp(&area(right)))
            .and_then(|control| control.bounds)
            .map_or_else(
                || {
                    centre(ui::kurbo::Rect::new(
                        0.0,
                        0.0,
                        f64::from(VIEWPORT.0),
                        f64::from(VIEWPORT.1),
                    ))
                },
                centre,
            )
    }

    /// The centre of the control `id`, in the app's surface coordinates.
    #[must_use]
    pub fn pointer_for_control(&self, id: &str) -> Option<[f32; 2]> {
        self.harness.control(id)?.bounds.map(centre)
    }

    /// Read the record's document.
    pub fn read<R>(&self, read: impl FnOnce(&automerge::AutoCommit) -> R) -> Option<R> {
        self.records.read(&self.record, read)
    }

    /// This participant's changes since the last call, or `None`. The first
    /// call on a new record returns the whole document as a snapshot.
    pub fn take_mutation(&mut self) -> Option<DocumentMutation> {
        self.harness.relayout();
        let taken = &mut self.taken;
        let received = &self.received;
        self.records
            .with(&self.record, |doc| {
                let heads = doc.get_heads();
                if heads == *taken {
                    return None;
                }
                let mutation = if taken.is_empty() && received.is_empty() {
                    DocumentMutation::Snapshot(doc.save())
                } else {
                    let own: Vec<u8> = doc
                        .get_changes(taken)
                        .iter()
                        .filter(|change| !received.contains(&change.hash()))
                        .flat_map(|change| change.raw_bytes().to_vec())
                        .collect();
                    *taken = heads;
                    return (!own.is_empty()).then_some(DocumentMutation::Incremental(own));
                };
                *taken = heads;
                Some(mutation)
            })
            .flatten()
    }

    /// Merge another participant's durable change.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Merge`] when the bytes are not Automerge changes.
    pub fn apply(&mut self, mutation: &DocumentMutation) -> Result<(), AppError> {
        let bytes = match mutation {
            DocumentMutation::Snapshot(bytes) | DocumentMutation::Incremental(bytes) => bytes,
        };
        let received = &mut self.received;
        let merged = self.records.with(&self.record, |doc| {
            let before = doc.get_heads();
            doc.load_incoming(bytes)
                .map_err(|error| AppError::Merge(error.to_string()))?;
            received.extend(doc.get_changes(&before).iter().map(automerge::Change::hash));
            Ok(())
        });
        self.harness.relayout();
        merged.unwrap_or_else(|| Err(AppError::Merge("the record is not open".into())))
    }

    /// Unmount, flushing the record.
    pub fn close(mut self) {
        let _ = self.harness.flush_component_records();
        self.harness.close_components();
    }
}

fn centre(rect: ui::kurbo::Rect) -> [f32; 2] {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "surface coordinates are well inside f32"
    )]
    [
        f64::midpoint(rect.x0, rect.x1) as f32,
        f64::midpoint(rect.y0, rect.y1) as f32,
    ]
}

/// Lowercase words of a label, without a plural `s`.
fn label_words(label: &str) -> Vec<String> {
    label
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| word.len() > 2)
        .map(|word| {
            word.strip_suffix('s')
                .filter(|stem| stem.len() > 2)
                .unwrap_or(word)
                .to_owned()
        })
        .collect()
}

fn hashes(doc: &mut automerge::AutoCommit) -> HashSet<ChangeHash> {
    doc.get_changes(&[])
        .iter()
        .map(automerge::Change::hash)
        .collect()
}

/// The semantic node a `ui:` resource id names.
#[must_use]
pub fn semantic_node(resource: &str) -> Option<String> {
    let encoded = resource.strip_prefix(SEMANTIC_RESOURCE_PREFIX)?;
    percent_encoding::percent_decode_str(encoded)
        .decode_utf8()
        .ok()
        .map(std::borrow::Cow::into_owned)
}

/// Whether a descriptor is a projected control rather than an action the
/// app declared.
#[must_use]
pub fn is_semantic(descriptor: &ActionDescriptor) -> bool {
    descriptor
        .address
        .resource
        .starts_with(SEMANTIC_RESOURCE_PREFIX)
}
