//! Genuine registered History refresh checks; constructors and full refreshes stay outside probes.
use super::*;
use crate::application::{AppComponent, LaunchTarget, RestoreInput};
use crate::application_test_support::{
    APP, ProbeHost, runner_permissions, runner_probe_definition,
};
use crate::bound_dispatch::CallbackBinding;
use crate::history_mode::owner::{HistoryOwner, LegacyHistoryLifetime, OWNED_DOCUMENT_CLONES};
use crate::history_mode::{HistoryDefinition, HistoryResource, HistoryScopeDefinition};
use crate::record::{RecordCheckout, RecordKind, RecordStore};
use automerge::{ROOT, ReadDoc, transaction::Transactable};

static DEFINITION: HistoryDefinition = HistoryDefinition {
    app_id: APP,
    scopes: &[HistoryScopeDefinition {
        key: "document",
        title: "Refresh probe",
        presentation: HistoryPresentation::Domain {
            create: crate::history_mode::host_tests::preview,
        },
        content_schema: None,
    }],
};
static UNVERSIONED_DEFINITION: HistoryDefinition = HistoryDefinition {
    app_id: APP,
    scopes: &[HistoryScopeDefinition {
        key: "document",
        title: "Unavailable history",
        presentation: HistoryPresentation::External {
            reason: "unversioned",
        },
        content_schema: None,
    }],
};

struct BoundFixture {
    component: AppComponent,
    binding: CallbackBinding,
    _host: ProbeHost,
}
impl BoundFixture {
    fn new() -> Self {
        let host = ProbeHost::new();
        let definition = runner_probe_definition();
        let context = host
            .prepare_with_permissions(definition, 940, &LaunchTarget::New, runner_permissions())
            .unwrap();
        let component = definition
            .instantiate(context, LaunchTarget::New, RestoreInput::None)
            .unwrap();
        let binding = component.callback_binding();
        binding
            .records
            .with_history(&binding.checkout.id, |document, _| {
                document.put(ROOT, "body", "committed owned body").unwrap();
                document.commit();
            })
            .unwrap();
        Self {
            component,
            binding,
            _host: host,
        }
    }
    fn history(&self, outer: &AutoCommit) -> HistoryHost {
        let source = HistorySourceRegistration::owned(
            "document",
            APP,
            HistoryOwner::component(self.binding.clone()),
        )
        .unwrap();
        HistoryHost::new(
            HistoryRegistration::new(&DEFINITION, vec![source]).unwrap(),
            outer,
        )
        .unwrap()
    }
    fn close(&mut self) {
        let report = self.component.close();
        assert!(report.record_error.is_none(), "{:?}", report.record_error);
        assert!(report.view_error.is_none(), "{:?}", report.view_error);
        assert!(
            report.teardown_error.is_none(),
            "{:?}",
            report.teardown_error
        );
    }
}
fn outer() -> AutoCommit {
    let mut document = AutoCommit::new();
    document.put(ROOT, "body", "unrelated outer body").unwrap();
    document.commit();
    document
}
fn clones() -> usize {
    OWNED_DOCUMENT_CLONES.get()
}
fn reset_clones() {
    OWNED_DOCUMENT_CLONES.set(0);
}
fn state(document: &AutoCommit) -> (Vec<automerge::ChangeHash>, usize, Option<String>) {
    (
        document.base_heads(),
        document.pending_ops(),
        document
            .get(ROOT, "body")
            .unwrap()
            .and_then(|(value, _)| value.as_str().map(str::to_owned)),
    )
}

#[test]
fn refresh_heads_current_and_changed_real_component_frontier_without_owned_clone() {
    let mut fixture = BoundFixture::new();
    let outer = outer();
    let mut history = fixture.history(&outer);
    assert!(history.request_open());
    history.refresh(&outer);
    let owner = HistoryOwner::component(fixture.binding.clone());
    let expected = owner.document(&outer).unwrap().base_heads();
    reset_clones();
    let current = history.source().committed_heads(&outer).unwrap();
    let unchanged = history.needs_refresh(&outer, 900.0);
    fixture
        .binding
        .records
        .with_history(&fixture.binding.checkout.id, |document, _| {
            document
                .put(ROOT, "body", "next committed owned body")
                .unwrap();
            document.commit();
        })
        .unwrap();
    let changed = history.needs_refresh(&outer, 900.0);
    let next = history.source().committed_heads(&outer).unwrap();
    let clone_count = clones();
    fixture.close();
    assert_eq!(current, expected);
    assert_ne!(
        current,
        outer.base_heads(),
        "the owner must supply the frontier"
    );
    assert!(!unchanged);
    assert!(changed);
    assert_ne!(next, current);
    assert_eq!(
        clone_count, 0,
        "the refresh predicate must not clone the owned graph"
    );
}

#[test]
fn refresh_heads_leave_real_pending_operations_and_body_unmodified() {
    let mut fixture = BoundFixture::new();
    let outer = outer();
    let history = fixture.history(&outer);
    fixture
        .binding
        .records
        .with_history(&fixture.binding.checkout.id, |document, _| {
            document.put(ROOT, "body", "unfinished owned edit").unwrap();
        })
        .unwrap();
    let before = fixture
        .binding
        .records
        .read_checked(&fixture.binding.checkout.id, state)
        .unwrap();
    assert!(
        before.1 > 0,
        "the probe must have a genuinely unfinished transaction"
    );
    reset_clones();
    let heads = history.source().committed_heads(&outer).unwrap();
    let refresh = history.needs_refresh(&outer, 900.0);
    let after = fixture
        .binding
        .records
        .read_checked(&fixture.binding.checkout.id, state)
        .unwrap();
    let clone_count = clones();
    fixture.close();
    assert_eq!(heads, before.0);
    assert_eq!(after, before);
    assert!(!refresh, "uncommitted operations do not advance base_heads");
    assert_eq!(clone_count, 0);
}

#[test]
fn refresh_heads_busy_read_refuses_even_without_controller_and_recovers_after_loan() {
    let mut fixture = BoundFixture::new();
    let outer = outer();
    let mut history = fixture.history(&outer);
    for no_controller in [false, true] {
        if no_controller {
            history.controller = None;
        }
        let (heads, refresh) = fixture
            .binding
            .records
            .read_checked(&fixture.binding.checkout.id, |_| {
                (
                    history.source().committed_heads(&outer),
                    history.needs_refresh(&outer, 900.0),
                )
            })
            .unwrap();
        assert_eq!(heads, Err(HistoryRegistrationError::UnsupportedOwner));
        assert!(
            refresh,
            "failed admission requires refresh, controllerNone={no_controller}"
        );
        assert!(history.source().committed_heads(&outer).is_ok());
        assert!(!history.needs_refresh(&outer, 900.0));
    }
    fixture.close();
}

#[test]
fn refresh_heads_closed_real_component_refuses_even_without_controller() {
    let mut fixture = BoundFixture::new();
    let outer = outer();
    let mut history = fixture.history(&outer);
    fixture.close();
    for no_controller in [false, true] {
        if no_controller {
            history.controller = None;
        }
        assert_eq!(
            history.source().committed_heads(&outer),
            Err(HistoryRegistrationError::UnsupportedOwner)
        );
        assert!(
            history.needs_refresh(&outer, 900.0),
            "closed custody is not a current frontier"
        );
    }
}

#[test]
fn refresh_heads_foreign_registration_and_stale_record_identity_remain_refused() {
    let mut fixture = BoundFixture::new();
    let owner = HistoryOwner::component(fixture.binding.clone());
    assert!(matches!(
        HistorySourceRegistration::owned("document", "foreign.application", owner),
        Err(HistoryRegistrationError::ForeignSource)
    ));
    fixture.close();

    let store = RecordStore::in_memory();
    let live = RecordCheckout::new(crate::RecordId::for_tab(941), RecordKind::new("note"));
    store.open_record(&live);
    let lifetime = LegacyHistoryLifetime::new();
    let live_owner = HistoryOwner::legacy(store.clone(), live.clone(), &lifetime);
    let resource = live_owner.resource(APP).unwrap();
    let stale = RecordCheckout::new(live.id.clone(), RecordKind::new("foreign-kind"));
    let owner = HistoryOwner::legacy(store, stale, &lifetime);
    assert!(matches!(
        HistorySourceRegistration::owned("document", APP, owner.clone()),
        Err(HistoryRegistrationError::UnsupportedOwner)
    ));
    // The retained descriptor alone cannot authorize a now-invalid checkout.
    let source = HistorySourceRegistration::OwnedDocument {
        scope: "document",
        owner,
        resource,
    };
    assert_eq!(
        source.document(&outer()).err(),
        Some(HistoryRegistrationError::UnsupportedOwner)
    );
    assert_eq!(
        source.committed_heads(&outer()),
        Err(HistoryRegistrationError::UnsupportedOwner)
    );
    lifetime.active.set(false);
}

#[test]
fn refresh_heads_host_and_unversioned_borrow_outer_committed_frontier() {
    let mut document = outer();
    let host_source = HistorySourceRegistration::HostDocument {
        scope: "document",
        resource: HistoryResource::new("host", "room", APP, crate::RecordId::for_tab(942)),
    };
    let unversioned = HistorySourceRegistration::Unversioned {
        scope: "document",
        reason: "unversioned",
    };
    let mut host = HistoryHost::new(
        HistoryRegistration::new(&DEFINITION, vec![host_source]).unwrap(),
        &document,
    )
    .unwrap();
    let unversioned_host = HistoryHost::new(
        HistoryRegistration::new(&UNVERSIONED_DEFINITION, vec![unversioned]).unwrap(),
        &document,
    )
    .unwrap();
    assert!(host.request_open());
    host.refresh(&document);
    assert!(!host.needs_refresh(&document, 900.0));
    assert!(unversioned_host.controller.is_none());
    document.put(ROOT, "body", "pending outer edit").unwrap();
    let before = state(&document);
    assert!(before.1 > 0);
    assert_eq!(host.source().committed_heads(&document).unwrap(), before.0);
    assert_eq!(
        unversioned_host
            .source()
            .committed_heads(&document)
            .unwrap(),
        before.0
    );
    assert!(!host.needs_refresh(&document, 900.0));
    assert!(!unversioned_host.needs_refresh(&document, 900.0));
    assert_eq!(state(&document), before);
    document.commit();
    assert!(host.needs_refresh(&document, 900.0));
    assert!(!unversioned_host.needs_refresh(&document, 900.0));
    assert_eq!(
        unversioned_host
            .source()
            .committed_heads(&document)
            .unwrap(),
        document.base_heads()
    );
}

fn populate(document: &mut AutoCommit, size: usize) {
    let value = "x".repeat(256);
    for index in 0..size {
        document
            .put(ROOT, format!("item-{index:04}"), value.as_str())
            .unwrap();
    }
    document.commit();
    assert_eq!(document.base_heads().len(), 1);
}
fn populated_document(size: usize) -> AutoCommit {
    let mut document = AutoCommit::new();
    populate(&mut document, size);
    document
}

#[test]
fn refresh_heads_allocation_growth_independent_of_real_graph_size_in_all_document_routes() {
    use crate::semantic_document_read::retirement::measure_test_allocations;
    let mut failures = Vec::new();
    for route in ["owned", "host", "unversioned"] {
        let mut observations = Vec::new();
        for size in [16, 2048] {
            let document = populated_document(size);
            let store = RecordStore::in_memory();
            let checkout =
                RecordCheckout::new(crate::RecordId::for_tab(943), RecordKind::new("note"));
            store.open_record(&checkout);
            let lifetime = LegacyHistoryLifetime::new();
            let source = match route {
                "owned" => {
                    store
                        .with_history(&checkout.id, |owned, _| populate(owned, size))
                        .unwrap();
                    // The owner validates its real parked graph; no opaque stand-in.
                    HistorySourceRegistration::owned(
                        "document",
                        APP,
                        HistoryOwner::legacy(store.clone(), checkout.clone(), &lifetime),
                    )
                    .unwrap()
                }
                "host" => HistorySourceRegistration::HostDocument {
                    scope: "document",
                    resource: HistoryResource::new("host", "room", APP, checkout.id.clone()),
                },
                _ => HistorySourceRegistration::Unversioned {
                    scope: "document",
                    reason: "unversioned",
                },
            };
            let expected = source.document(&document).unwrap().base_heads();
            assert_eq!(
                expected.len(),
                1,
                "each graph keeps exactly one frontier head"
            );
            assert_eq!(source.committed_heads(&document).unwrap(), expected); // warm up outside probe
            reset_clones();
            let (observed, stats) = measure_test_allocations(|| {
                let mut matching = true;
                for _ in 0..8 {
                    matching &= source.committed_heads(&document).unwrap() == expected;
                }
                matching
            });
            let clone_count = clones();
            assert!(observed);
            observations.push((size, stats.requested, stats.allocations, clone_count));
            lifetime.active.set(false);
        }
        let small = observations[0];
        let large = observations[1];
        // Same route, owner ID/metadata, and one-head frontier: document content
        // must contribute zero additional allocation requests, not a loose timing threshold.
        if large.1 != small.1 || large.2 != small.2 || large.3 != 0 || small.3 != 0 {
            failures.push(format!("{route}: small={small:?}, large={large:?}"));
        }
    }
    assert!(
        failures.is_empty(),
        "content-dependent refresh allocation/owned clones: {failures:#?}"
    );
}

#[test]
fn closed_history_defers_frontier_refresh_until_opening() {
    let mut fixture = BoundFixture::new();
    let outer = outer();
    let mut history = fixture.history(&outer);
    let before = history
        .controller
        .as_ref()
        .unwrap()
        .observed_live_heads
        .clone();
    fixture
        .binding
        .records
        .with_history(&fixture.binding.checkout.id, |doc, _| {
            doc.put(ROOT, "body", "edited with history closed").unwrap();
            doc.commit();
        })
        .unwrap();
    let next = super::super::sorted_heads(history.source().committed_heads(&outer).unwrap());
    assert_ne!(before, next);
    assert!(!history.needs_refresh(&outer, 900.0));
    history.refresh(&outer);
    assert!(
        !history.needs_refresh(&outer, 900.0),
        "closed refresh must not request itself again"
    );
    assert_eq!(
        history.controller.as_ref().unwrap().observed_live_heads,
        before
    );
    assert!(history.request_open());
    assert!(history.needs_refresh(&outer, 900.0));
    history.refresh(&outer);
    assert_eq!(
        history.controller.as_ref().unwrap().observed_live_heads,
        next
    );
    assert!(!history.needs_refresh(&outer, 900.0));
    fixture.close();
}
