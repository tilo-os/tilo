use std::{cell::RefCell, rc::Rc};

use automerge::transaction::Transactable;
use ui::application::{
    AppComponent, ComponentRunnerInputs, HostContext, HostContextInputs, LaunchTarget, RestoreInput,
};
use ui::application_view::InstanceViewStore;
use ui::headless::FrameHarness;
use ui::permissions::{Actor, PermissionContext, Role, UserId};
use ui::portable_storage::{PendingWrite, PortableStorage};
use ui::record::{RecordCheckout, RecordId, RecordKind, RecordOwners, RecordStore};

const PROFILE: &str = "calculator-component-test";
const DEVICE: &str = "calculator-device";
const WORKSPACE: &str = "calculator-room";

struct Fixture {
    cache: PortableStorage,
    records: RecordStore,
    views: Rc<InstanceViewStore>,
    authority: Rc<RefCell<PermissionContext>>,
    author: UserId,
}

impl Fixture {
    fn new() -> Self {
        let cache = PortableStorage::new(PROFILE, DEVICE).unwrap();
        Self {
            records: RecordOwners::portable(cache.clone())
                .open(crate::definition().app_id, WORKSPACE)
                .unwrap(),
            views: Rc::new(InstanceViewStore::portable(cache.clone(), WORKSPACE).unwrap()),
            authority: Rc::new(RefCell::new(PermissionContext::from_capabilities(
                Actor::person(Some(Role::Owner), "calculator-author"),
                vec![],
            ))),
            author: UserId("calculator-author".into()),
            cache,
        }
    }

    fn component(&self, instance: u64, target: LaunchTarget) -> AppComponent {
        self.try_component(instance, target).unwrap()
    }

    fn try_component(
        &self,
        instance: u64,
        target: LaunchTarget,
    ) -> Result<AppComponent, ui::launch::LaunchError> {
        let context = HostContext::prepare(
            crate::definition(),
            HostContextInputs {
                records: self.records.clone(),
                views: self.views.clone(),
                device_id: DEVICE.into(),
                instance_id: instance,
                permissions: self.authority.clone(),
                author: self.author.clone(),
                action_context: tilo_actions::ActionContext::default(),
            },
            &target,
        )?;
        crate::definition().instantiate(context, target, RestoreInput::None)
    }

    fn driver(&self, instance: u64, target: LaunchTarget) -> FrameHarness {
        FrameHarness::from_component(
            self.component(instance, target),
            (900, 640),
            ComponentRunnerInputs {
                theme: ui::Theme::dark(),
                text_renderer: ui::TextRenderer::new(),
            },
        )
        .unwrap()
    }

    fn acknowledge_all(&self) {
        for write in self.cache.pending_writes() {
            self.cache.acknowledge(&write, Ok(())).unwrap();
        }
    }
}

fn existing(checkout: &RecordCheckout) -> LaunchTarget {
    LaunchTarget::Record {
        checkout: checkout.clone(),
        reveal: None,
    }
}

fn activate(driver: &mut FrameHarness, name: &str) {
    let node = driver
        .a11y_node_named(name)
        .unwrap_or_else(|| panic!("missing Calculator control {name:?}"));
    driver
        .perform_a11y_action(node, &ui::a11y::Action::Activate)
        .unwrap();
}

fn expression_value(driver: &FrameHarness) -> String {
    fn find(node: &ui::headless::InspectionNode) -> Option<String> {
        if let Some(semantics) = &node.semantics
            && semantics.name.as_deref() == Some("Calculator")
        {
            return semantics.value.clone();
        }
        node.children.iter().find_map(find)
    }
    find(&driver.inspection().root).expect("Calculator semantic root")
}

fn assert_existing_refused_unchanged(fixture: &Fixture, instance: u64, checkout: &RecordCheckout) {
    let before = fixture.records.read(&checkout.id, |doc| {
        (
            doc.base_heads(),
            doc.pending_ops(),
            serde_json::to_value(automerge::AutoSerde::from(doc)).unwrap(),
        )
    });
    assert!(matches!(
        fixture.try_component(instance, existing(checkout)),
        Err(ui::launch::LaunchError::Operation(_) | ui::launch::LaunchError::ComponentSetup { .. })
    ));
    assert_eq!(
        fixture.records.read(&checkout.id, |doc| {
            (
                doc.base_heads(),
                doc.pending_ops(),
                serde_json::to_value(automerge::AutoSerde::from(doc)).unwrap(),
            )
        }),
        before
    );
}

// Catches seeded screenshot/demo state leaking into checked New construction.
#[test]
fn checked_new_is_an_empty_real_calculator_document_with_a_unique_record() {
    let fixture = Fixture::new();
    let mut first = fixture.component(1, LaunchTarget::New);
    let mut second = fixture.component(2, LaunchTarget::New);
    assert_eq!(
        crate::definition().record_kind,
        RecordKind::new("calculator.document")
    );
    assert_ne!(first.record().id, second.record().id);
    for checkout in [first.record(), second.record()] {
        let model = fixture
            .records
            .read(&checkout.id, crate::doc::load)
            .flatten()
            .expect("checked New publishes the Calculator schema");
        assert!(model.history.is_empty());
        assert!(model.funcs.is_empty());
        assert!(model.surfaces.is_empty());
        assert!(model.expr.is_empty());
    }
    fixture.acknowledge_all();
    assert!(first.close().record_error.is_none());
    assert!(second.close().record_error.is_none());
}

// Catches bypasses of package, record-kind, or host namespace admission.
#[test]
fn checked_admission_refuses_wrong_package_kind_and_device_namespace() {
    let fixture = Fixture::new();
    let foreign_records = RecordOwners::portable(fixture.cache.clone())
        .open("so.tilo.foreign", WORKSPACE)
        .unwrap();
    let Err(package_error) = HostContext::prepare(
        crate::definition(),
        HostContextInputs {
            records: foreign_records,
            views: fixture.views.clone(),
            device_id: DEVICE.into(),
            instance_id: 10,
            permissions: fixture.authority.clone(),
            author: fixture.author.clone(),
            action_context: tilo_actions::ActionContext::default(),
        },
        &LaunchTarget::New,
    ) else {
        panic!("foreign package namespace must be refused");
    };
    assert!(matches!(
        package_error,
        ui::launch::LaunchError::BindingMismatch
    ));

    let wrong = RecordCheckout::new(RecordId::minted(), RecordKind::new("document"));
    fixture
        .records
        .reserve_record(&wrong)
        .unwrap()
        .initialize(|_| Ok(()))
        .unwrap();
    assert!(matches!(
        HostContext::prepare(
            crate::definition(),
            HostContextInputs {
                records: fixture.records.clone(),
                views: fixture.views.clone(),
                device_id: DEVICE.into(),
                instance_id: 11,
                permissions: fixture.authority.clone(),
                author: fixture.author.clone(),
                action_context: tilo_actions::ActionContext::default()
            },
            &existing(&wrong),
        ),
        Err(ui::launch::LaunchError::BindingMismatch)
    ));

    let foreign_views = Rc::new(
        InstanceViewStore::portable(
            PortableStorage::new(PROFILE, "other-device").unwrap(),
            WORKSPACE,
        )
        .unwrap(),
    );
    assert!(matches!(
        HostContext::prepare(
            crate::definition(),
            HostContextInputs {
                records: fixture.records.clone(),
                views: foreign_views,
                device_id: DEVICE.into(),
                instance_id: 12,
                permissions: fixture.authority,
                author: fixture.author,
                action_context: tilo_actions::ActionContext::default()
            },
            &LaunchTarget::New,
        ),
        Err(ui::launch::LaunchError::BindingMismatch)
    ));
}

// Catches a model-only adapter: these are the actual custom-painted keypad
// semantics, component binding, action executor, and persistent Automerge record.
#[test]
fn real_keypad_calculation_reopens_and_undoes_through_document_history() {
    let fixture = Fixture::new();
    let component = fixture.component(21, LaunchTarget::New);
    let checkout = component.record().clone();
    let mut driver = FrameHarness::from_component(
        component,
        (900, 640),
        ComponentRunnerInputs {
            theme: ui::Theme::dark(),
            text_renderer: ui::TextRenderer::new(),
        },
    )
    .unwrap();
    activate(&mut driver, "Calculator key 2");
    activate(&mut driver, "Add");
    activate(&mut driver, "Calculator key 3");
    activate(&mut driver, "Evaluate expression");
    let history = fixture
        .records
        .read(&checkout.id, |doc| crate::doc::load(doc).unwrap().history)
        .unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].expr, "2+3");
    assert_eq!(history[0].result, "5");
    assert!(driver.perform_undo());
    assert!(
        fixture
            .records
            .read(&checkout.id, |doc| crate::doc::load(doc)
                .unwrap()
                .history
                .is_empty())
            .unwrap()
    );
    assert!(driver.perform_redo());
    assert_eq!(
        fixture
            .records
            .read(&checkout.id, |doc| crate::doc::load(doc).unwrap().history
                [0]
            .result
            .clone())
            .unwrap(),
        "5"
    );
    fixture.acknowledge_all();
    driver.close_components();

    let mut reopened = fixture.driver(22, existing(&checkout));
    assert_eq!(
        fixture
            .records
            .read(&checkout.id, |doc| crate::doc::load(doc).unwrap().history
                [0]
            .expr
            .clone())
            .unwrap(),
        "2+3"
    );
    reopened.close_components();
}

// Catches accidental replication/restoration of expression or mode state.
#[test]
fn two_instances_keep_expression_views_independent_on_one_shared_record() {
    let fixture = Fixture::new();
    let first = fixture.component(31, LaunchTarget::New);
    let checkout = first.record().clone();
    let mut first = FrameHarness::from_component(
        first,
        (900, 640),
        ComponentRunnerInputs {
            theme: ui::Theme::dark(),
            text_renderer: ui::TextRenderer::new(),
        },
    )
    .unwrap();
    let mut second = fixture.driver(32, existing(&checkout));
    first
        .perform_semantic_named(
            "calculator_app",
            Some("calculator.expression"),
            &ui::a11y::Action::SetValue("111".into()),
        )
        .unwrap();
    second
        .perform_semantic_named(
            "calculator_app",
            Some("calculator.expression"),
            &ui::a11y::Action::SetValue("222".into()),
        )
        .unwrap();
    assert_eq!(expression_value(&first), "111");
    assert_eq!(expression_value(&second), "222");
    assert!(
        fixture
            .records
            .read(&checkout.id, |doc| crate::doc::load(doc)
                .unwrap()
                .history
                .is_empty())
            .unwrap()
    );
    first.close_components();
    second.close_components();
}

// Catches Existing silently initializing a missing or malformed Calculator schema.
#[test]
fn existing_malformed_record_is_refused_without_reseeding_or_replacement() {
    let fixture = Fixture::new();
    let checkout = RecordCheckout::new(RecordId::minted(), crate::definition().record_kind.clone());
    fixture
        .records
        .reserve_record(&checkout)
        .unwrap()
        .initialize(|doc| {
            doc.put(automerge::ROOT, "calculator_resources_v2", true)
                .map_err(|error| error.to_string())?;
            let history = doc
                .put_object(
                    automerge::ROOT,
                    crate::doc::HISTORY,
                    automerge::ObjType::Map,
                )
                .map_err(|error| error.to_string())?;
            let row = doc
                .put_object(&history, "malformed-row", automerge::ObjType::Map)
                .map_err(|error| error.to_string())?;
            doc.put(&row, "expr", 7)
                .map_err(|error| error.to_string())?;
            doc.put(&row, "result", "must not default expr")
                .map_err(|error| error.to_string())?;
            doc.put(&row, "rank", 0)
                .map_err(|error| error.to_string())?;
            doc.put(&row, "deleted", false)
                .map_err(|error| error.to_string())?;
            Ok(())
        })
        .unwrap();
    assert_existing_refused_unchanged(&fixture, 41, &checkout);
}

#[test]
fn existing_false_current_schema_marker_is_refused_without_mutation() {
    let fixture = Fixture::new();
    let checkout = RecordCheckout::new(RecordId::minted(), crate::definition().record_kind.clone());
    fixture
        .records
        .reserve_record(&checkout)
        .unwrap()
        .initialize(|doc| {
            doc.put(automerge::ROOT, "calculator_resources_v2", false)
                .map_err(|error| error.to_string())?;
            Ok(())
        })
        .unwrap();
    assert_existing_refused_unchanged(&fixture, 42, &checkout);
}

#[test]
fn existing_current_row_missing_required_fields_is_refused_without_mutation() {
    let fixture = Fixture::new();
    let checkout = RecordCheckout::new(RecordId::minted(), crate::definition().record_kind.clone());
    fixture
        .records
        .reserve_record(&checkout)
        .unwrap()
        .initialize(|doc| {
            doc.put(automerge::ROOT, "calculator_resources_v2", true)
                .map_err(|error| error.to_string())?;
            let history = doc
                .put_object(
                    automerge::ROOT,
                    crate::doc::HISTORY,
                    automerge::ObjType::Map,
                )
                .map_err(|error| error.to_string())?;
            let row = doc
                .put_object(&history, "incomplete-row", automerge::ObjType::Map)
                .map_err(|error| error.to_string())?;
            doc.put(&row, "expr", "2+2")
                .map_err(|error| error.to_string())?;
            doc.put(&row, "result", "4")
                .map_err(|error| error.to_string())?;
            Ok(())
        })
        .unwrap();
    assert_existing_refused_unchanged(&fixture, 43, &checkout);
}

// Catches stale checked bindings retaining write access after authority changes.
#[test]
fn revoked_viewer_authority_refuses_real_calculator_mutation_without_record_change() {
    let fixture = Fixture::new();
    let component = fixture.component(45, LaunchTarget::New);
    let checkout = component.record().clone();
    let mut driver = FrameHarness::from_component(
        component,
        (900, 640),
        ComponentRunnerInputs {
            theme: ui::Theme::dark(),
            text_renderer: ui::TextRenderer::new(),
        },
    )
    .unwrap();
    let address = ui::bound_dispatch::BoundActionAddress {
        instance_id: 45,
        address: tilo_actions::ActionAddress::new("calculator.document", "add-function"),
    };
    driver
        .invoke_bound_action(
            &address,
            serde_json::json!({ "value": "x" }).as_object().unwrap(),
        )
        .unwrap();
    assert_eq!(
        fixture
            .records
            .read(&checkout.id, |doc| crate::doc::load(doc).unwrap().funcs[0]
                .expr
                .clone())
            .unwrap(),
        "x"
    );
    fixture.acknowledge_all();
    *fixture.authority.borrow_mut() = PermissionContext::from_capabilities(
        Actor::person(Some(Role::Viewer), &fixture.author.0),
        vec![],
    );
    let before = fixture.records.read(&checkout.id, |doc| {
        (
            doc.base_heads(),
            doc.pending_ops(),
            serde_json::to_value(automerge::AutoSerde::from(doc)).unwrap(),
        )
    });
    assert!(
        driver
            .invoke_bound_action(
                &address,
                serde_json::json!({ "value": "denied" })
                    .as_object()
                    .unwrap(),
            )
            .is_err()
    );
    assert_eq!(
        fixture.records.read(&checkout.id, |doc| {
            (
                doc.base_heads(),
                doc.pending_ops(),
                serde_json::to_value(automerge::AutoSerde::from(doc)).unwrap(),
            )
        }),
        before
    );
    driver.close_components();
}

// Catches loss of the component-owned portable store after a failed close flush.
#[test]
fn close_persistence_failure_retains_the_record_owner_for_explicit_retry() {
    let fixture = Fixture::new();
    let component = fixture.component(51, LaunchTarget::New);
    let checkout = component.record().clone();
    let mut driver = FrameHarness::from_component(
        component,
        (900, 640),
        ComponentRunnerInputs {
            theme: ui::Theme::dark(),
            text_renderer: ui::TextRenderer::new(),
        },
    )
    .unwrap();
    fixture.acknowledge_all();
    activate(&mut driver, "Calculator key 9");
    activate(&mut driver, "Evaluate expression");
    driver.close_components();
    let mut report = driver
        .take_component_close_reports()
        .into_iter()
        .find(|report| report.record_error.is_some())
        .expect("failed close retains record recovery");
    assert!(report.record_retry.is_some());
    let record_write: PendingWrite = fixture
        .cache
        .pending_writes()
        .into_iter()
        .find(|write| matches!(write.key, ui::portable_storage::StorageKey::Record { .. }))
        .expect("close queues the real record write before reporting pending durability");
    fixture
        .cache
        .acknowledge(&record_write, Err("injected durable write failure".into()))
        .unwrap();
    assert!(
        report
            .retry_record()
            .unwrap_err()
            .contains("injected durable write failure")
    );
    fixture.cache.acknowledge(&record_write, Ok(())).unwrap();
    report.retry_record().unwrap();
    assert!(report.record_retry.is_none());
    assert_eq!(
        fixture
            .records
            .read(&checkout.id, |doc| crate::doc::load(doc).unwrap().history
                [0]
            .result
            .clone())
            .unwrap(),
        "9"
    );
}
