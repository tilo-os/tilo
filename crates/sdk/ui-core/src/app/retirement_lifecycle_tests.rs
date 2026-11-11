//! Real SDK construction/lifecycle tests; the marker is resource-only.
use super::*;
use crate::application::{AppComponent, HostContext, LaunchTarget, RestoreInput};
use crate::application_test_support::{ProbeHost, definition, runner_inputs, runner_permissions};
use crate::semantic_document_read::retirement::{
    MountRetirement, RetirementDomain, RetirementError, RetirementHandle, retirement_test_probe,
};
use crate::widget::{Container, IconDescriptor};
use automerge::{ROOT, transaction::Transactable};
use std::{
    cell::RefCell,
    panic::{AssertUnwindSafe, catch_unwind},
    rc::Rc,
};

struct Observations {
    handle: RetirementHandle,
    armed: Cell<bool>,
    panic_capture: Cell<bool>,
    panic_install: Cell<Option<usize>>,
    installs: Cell<usize>,
    capture_pause: RefCell<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>,
    events: RefCell<Vec<(&'static str, bool)>>,
}
impl Observations {
    fn retired(&self) -> bool {
        matches!(
            retirement_test_probe(&self.handle),
            Err(RetirementError::Retired)
        )
    }
    fn record(&self, event: &'static str) {
        self.events.borrow_mut().push((event, self.retired()));
    }
}
thread_local! {
    static OBSERVED: RefCell<Option<Rc<Observations>>> = const { RefCell::new(None) };
    static PANIC_STAGE: Cell<&'static str> = const { Cell::new("") };
    static STAGES: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
}
fn stage(name: &'static str) {
    STAGES.with_borrow_mut(|events| events.push(name));
    assert!(PANIC_STAGE.get() != name, "constructor probe: {name}");
}
fn mounted() -> (RetirementDomain, MountRetirement, Rc<Observations>) {
    let domain = RetirementDomain::new().unwrap();
    let owner = domain.marker().unwrap();
    let observed = Rc::new(Observations {
        handle: owner.handle(),
        armed: Cell::new(false),
        panic_capture: Cell::new(false),
        panic_install: Cell::new(None),
        installs: Cell::new(0),
        capture_pause: RefCell::new(None),
        events: RefCell::new(vec![]),
    });
    let mount = MountRetirement::new(owner);
    OBSERVED.with_borrow_mut(|slot| *slot = Some(observed.clone()));
    (domain, mount, observed)
}
struct Root {
    inner: Container,
    observed: Rc<Observations>,
}
impl Widget for Root {
    crate::provides!(dyn crate::widget::controls::ActionControls);

    crate::testing::delegate_widget_methods!(
        inner: sync,
        build_layout,
        apply_layout,
        draw,
        handle_event,
        get_rect
    );

    fn name(&self) -> &str {
        "retirement lifecycle probe"
    }
}

impl crate::widget::controls::ActionControls for Root {
    fn install_control_custody(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    ) {
        let index = self.observed.installs.get();
        self.observed.installs.set(index + 1);
        assert!(
            self.observed.panic_install.get() != Some(index),
            "control installation probe"
        );
        crate::widget::controls::install_control_custody(&mut self.inner, domain, attachment);
    }
}
crate::testing::test_app_widget!(
    Root,
    "Retirement lifecycle probe",
    icon = IconDescriptor::Glyph("R"),
    {
        fn capture_application_view(
            &self,
        ) -> Result<Option<crate::application_view::ApplicationLocalView>, crate::view::ThawError>
        {
            if self.observed.armed.get() {
                self.observed.record("capture");
                if let Some((started, resume)) = self.observed.capture_pause.borrow_mut().take() {
                    started.send(()).unwrap();
                    resume.recv().unwrap();
                }
                assert!(
                    !self.observed.panic_capture.replace(false),
                    "real capture probe"
                );
            }
            Ok(None)
        }
    }
);
impl Drop for Root {
    fn drop(&mut self) {
        self.observed.record("root-drop");
    }
}
fn factory(
    mut context: HostContext,
    target: LaunchTarget,
    _: RestoreInput,
) -> Result<AppComponent, crate::launch::LaunchError> {
    if matches!(target, LaunchTarget::New) {
        context
            .initialize_new_record(|doc| {
                doc.put(ROOT, "body", "real document")
                    .map_err(|error| error.to_string())
            })
            .map_err(crate::launch::LaunchError::Record)?;
    }
    let observed = OBSERVED.with_borrow(|slot| slot.as_ref().unwrap().clone());
    Ok(AppComponent::new(
        context,
        Box::new(Root {
            inner: Container::new(),
            observed,
        }),
        tilo_actions::ActionRegistry::new(),
    ))
}
fn component(host: &ProbeHost, mount: MountRetirement) -> AppComponent {
    let definition = definition(factory);
    let mut context = host
        .prepare_with_permissions(&definition, 1, &LaunchTarget::New, runner_permissions())
        .unwrap();
    context.bind_retirement(mount).unwrap();
    definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .unwrap()
}
fn app(component: AppComponent) -> App {
    App::from_component_using(component, runner_inputs(), |inputs, binding| {
        AppConstructionInputs::component_with_services(
            inputs,
            binding,
            AppConstructionInputs::controlled(),
        )
    })
    .unwrap()
}
fn assert_retired_events(observed: &Observations, expected: &[&str]) {
    let events = observed.events.borrow();
    assert_eq!(
        events.iter().map(|(event, _)| *event).collect::<Vec<_>>(),
        expected
    );
    assert!(
        events.iter().all(|(_, retired)| *retired),
        "callbacks ran before retirement: {events:?}"
    );
}
#[test]
fn retirement_lifecycle_component_close_retires_before_capture_and_drop_even_when_capture_unwinds()
{
    for capture_panics in [false, true] {
        let host = ProbeHost::new();
        let (_domain, mount, observed) = mounted();
        let mut component = component(&host, mount);
        observed.armed.set(true);
        observed.panic_capture.set(capture_panics);
        assert_eq!(
            catch_unwind(AssertUnwindSafe(|| component.close())).is_err(),
            capture_panics
        );
        assert_retired_events(&observed, &["capture", "root-drop"]);
    }
}
#[test]
fn retirement_lifecycle_checkpoint_stays_open_until_admitted_close() {
    let host = ProbeHost::new();
    let (_domain, mount, observed) = mounted();
    let mut component = component(&host, mount);
    observed.armed.set(true);
    assert!(component.checkpoint_for_close().is_empty());
    assert_eq!(*observed.events.borrow(), vec![("capture", false)]);
    assert!(!observed.retired());
    observed.events.borrow_mut().clear();
    component.finish_admitted_close();
    assert_retired_events(&observed, &["root-drop"]);
}
#[test]
fn retirement_lifecycle_outer_app_close_and_drop_preserve_capture_behavior() {
    for explicit in [true, false] {
        let host = ProbeHost::new();
        let (_domain, mount, observed) = mounted();
        let mut app = app(component(&host, mount));
        observed.armed.set(true);
        if explicit {
            app.close_components();
        }
        drop(app);
        assert_retired_events(
            &observed,
            if explicit {
                &["capture", "root-drop"]
            } else {
                &["root-drop"]
            },
        );
    }
}
#[test]
fn retirement_lifecycle_native_wake_close_retires_without_tree_traversal() {
    let host = ProbeHost::new();
    let (_domain, mount, observed) = mounted();
    let mut app = app(component(&host, mount));
    observed.armed.set(true);
    app.close_wake_scope();
    assert!(
        observed.retired(),
        "outer wake close left the marker usable"
    );
    assert!(observed.events.borrow().is_empty());
    drop(app);
    assert_retired_events(&observed, &["root-drop"]);
}
fn staged(
    inputs: crate::application::ComponentRunnerInputs,
    binding: &crate::bound_dispatch::CallbackBinding,
) -> AppConstructionInputs {
    let mut inputs = AppConstructionInputs::component_with_services(
        inputs,
        binding,
        AppConstructionInputs::controlled(),
    );
    inputs.debug_mode = || {
        stage("debug");
        false
    };
    inputs.theme_store_stamp = || {
        stage("stamp");
        None
    };
    inputs.screen_reader = || {
        stage("screen");
        None
    };
    inputs.pending_continuation_restore = || {
        stage("restore");
        None
    };
    inputs.profiler = || {
        stage("profiler");
        None
    };
    inputs.floating_surface_client = || {
        stage("floating");
        None
    };
    inputs.high_contrast = || {
        stage("contrast");
        false
    };
    inputs
}
#[test]
fn retirement_lifecycle_inner_initializer_unwind_retires_before_root_cleanup() {
    let expected = [
        "debug", "stamp", "screen", "restore", "profiler", "floating", "contrast",
    ];
    for (index, panic_stage) in expected.iter().enumerate() {
        let host = ProbeHost::new();
        let (_domain, mount, observed) = mounted();
        let component = component(&host, mount);
        STAGES.with_borrow_mut(Vec::clear);
        PANIC_STAGE.set(panic_stage);
        assert!(
            catch_unwind(AssertUnwindSafe(|| App::from_component_using(
                component,
                runner_inputs(),
                staged
            )))
            .is_err()
        );
        PANIC_STAGE.set("");
        assert_eq!(STAGES.with_borrow(Clone::clone), expected[..=index]);
        assert_retired_events(&observed, &["root-drop"]);
    }
}
#[test]
fn retirement_lifecycle_construction_service_unwind_retires_before_root_cleanup() {
    let host = ProbeHost::new();
    let (_domain, mount, observed) = mounted();
    let component = component(&host, mount);
    assert!(
        catch_unwind(AssertUnwindSafe(|| App::from_component_using(
            component,
            runner_inputs(),
            |_, _| panic!("services probe")
        )))
        .is_err()
    );
    assert_retired_events(&observed, &["root-drop"]);
}
#[test]
fn retirement_lifecycle_successful_transfer_does_not_retire_early() {
    let host = ProbeHost::new();
    let (domain, mount, observed) = mounted();
    let app = app(component(&host, mount));
    assert!(!observed.retired());
    drop(app);
    assert_retired_events(&observed, &["root-drop"]);
    OBSERVED.with_borrow_mut(|slot| *slot = None);
    drop(observed);
    drop(domain);
}

#[test]
fn retirement_lifecycle_control_installation_panic_retires_before_root_drop() {
    // Installation 0 happens in the component constructor; 1 and 2 happen
    // while the app mounts the component and its outer controls.
    for index in [0, 1, 2] {
        let host = ProbeHost::new();
        let (_domain, mount, observed) = mounted();
        observed.panic_install.set(Some(index));
        let unwound = catch_unwind(AssertUnwindSafe(|| {
            let component = component(&host, mount);
            if index > 0 {
                app(component);
            }
        }));
        assert!(unwound.is_err());
        assert_retired_events(&observed, &["root-drop"]);
    }
}
