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

use tilo_resource::{Invoke, Resource};
use ui_core::application::{ApplicationHost, ComponentRunnerInputs};
use ui_core::bound_dispatch::BoundActionAddress;
use ui_core::headless::FrameHarness;
use ui_core::launch::LaunchError;
use ui_core::permissions::{Actor, PermissionContext, Role, UserId};
use ui_core::record::{RecordCheckout, RecordStore};

use super::app::{DATA_DIR_INSTANCE, ResourceApp, data_dir_host, default_record, definition};

const INSTANCE: u64 = 7;

enum Profile {
    /// A private temporary profile, removed with the app.
    Temporary(Rc<tempfile::TempDir>),
    /// An explicit `--data-dir` profile the app itself would open.
    DataDir(PathBuf),
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
        let author = UserId("test-author".into());
        let (mut host, instance) = match &profile {
            Profile::Temporary(directory) => (
                ApplicationHost::new_with_profile_lifetime(
                    directory.path().join("state"),
                    "test-device".into(),
                    "test-workspace",
                    author.clone(),
                    Rc::new(RefCell::new(PermissionContext::from_capabilities(
                        Actor::person(Some(role), &author.0),
                        vec![],
                    ))),
                    tilo_actions::ActionContext::default(),
                    directory.clone(),
                )?,
                INSTANCE,
            ),
            Profile::DataDir(directory) => {
                (data_dir_host(directory, &author, role)?, DATA_DIR_INSTANCE)
            }
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
            marker: std::marker::PhantomData,
        })
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

    /// The record's version history.
    pub fn versions(&self) -> Vec<tilo_resource::history::Version> {
        self.records
            .read(&self.checkout.id, |doc| {
                tilo_resource::history::versions(&mut doc.clone())
            })
            .unwrap_or_default()
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

impl<A: ResourceApp> Drop for HeadlessApp<A> {
    fn drop(&mut self) {
        let _ = self.close();
    }
}
