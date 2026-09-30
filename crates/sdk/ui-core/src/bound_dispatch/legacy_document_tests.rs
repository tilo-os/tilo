#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "controlled regression assertions"
)]
use super::legacy_document::*;
use super::*;
use crate::widget::{AppModel, ModelPath};
use crate::widget::{Column, TileContent, TileHost};
use automerge::ReadDoc;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use tilo_actions::{
    Action, ActionAddress, ActionError, ActionSpec, EncodedAction, ExecutionLocus, LocalTransport,
    Resource,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SetValue {
    pub(crate) value: i64,
    pub(crate) fail: bool,
    pub(crate) remote: bool,
}
impl Action for SetValue {
    fn apply(&self, doc: &mut AutoCommit) -> Result<(), ActionError> {
        if doc.get(ROOT, "value")?.and_then(|(v, _)| v.as_i64()) != Some(self.value) {
            doc.put(ROOT, "value", self.value)?;
        }
        if self.fail {
            return Err(ActionError::Refused("controlled failure".into()));
        }
        Ok(())
    }
    fn locus(&self) -> ExecutionLocus {
        if self.remote {
            ExecutionLocus::ServerPreferred
        } else {
            ExecutionLocus::ClientLocal
        }
    }
}
#[derive(Clone)]
pub(crate) struct Probe {
    pub(crate) children: Rc<RefCell<Vec<Box<dyn Widget>>>>,
    commit: Rc<Cell<bool>>,
    model_passes: Rc<Cell<(usize, usize)>>,
    pub(crate) eager: Rc<Cell<bool>>,
    pub(crate) declarations: Rc<Cell<usize>>,
    pub(crate) starts: Rc<Cell<usize>>,
    pub(crate) polls: Rc<Cell<usize>>,
    pub(crate) ready: Rc<Cell<bool>>,
    pub(crate) invocation: Rc<Cell<Option<tilo_actions::InvocationId>>>,
    pub(crate) expected_owner: Rc<Cell<u64>>,
    pub(crate) actions: Rc<RefCell<crate::action_host::ActionHost>>,
    pub(crate) spec: Rc<RefCell<ActionSpec>>,
    pub(crate) queue: Rc<RefCell<VecDeque<tilo_actions::ControlInvocation>>>,
    pub(crate) acks: Rc<Cell<usize>>,
    pub(crate) ack_heads: Rc<RefCell<Vec<ChangeHash>>>,
    pub(crate) value: Rc<Cell<i64>>,
}
impl Probe {
    pub(crate) fn new() -> Self {
        Self {
            children: Rc::default(),
            commit: Rc::default(),
            model_passes: Rc::default(),
            eager: Rc::new(Cell::new(true)),
            declarations: Rc::new(Cell::new(1)),
            starts: Rc::default(),
            polls: Rc::default(),
            ready: Rc::default(),
            invocation: Rc::default(),
            expected_owner: Rc::default(),
            actions: Rc::new(RefCell::new(crate::action_host::ActionHost::new())),
            spec: Rc::new(RefCell::new(spec(2, false, false))),
            queue: Rc::default(),
            acks: Rc::default(),
            ack_heads: Rc::default(),
            value: Rc::default(),
        }
    }
    pub(crate) fn offer(&self) {
        self.queue
            .borrow_mut()
            .push_back(tilo_actions::ControlInvocation {
                address: address(),
                arguments: Default::default(),
            });
    }
}
fn spec(value: i64, fail: bool, remote: bool) -> ActionSpec {
    ActionSpec::immediate_encoded(
        "set",
        "Set owned value",
        ExecutionLocus::ClientLocal,
        EncodedAction::encode(
            "controlled.legacy",
            &SetValue {
                value,
                fail,
                remote,
            },
        )
        .unwrap(),
    )
}
pub(crate) fn address() -> ActionAddress {
    ActionAddress::new("controlled.value", "set")
}
impl Widget for Probe {
    crate::provides!(
        dyn crate::undo::CommittedActions,
        dyn crate::history_mode::capability::DocumentHistory,
        dyn crate::widget::controls::ActionControls,
        dyn crate::resources::PublishesResources,
        dyn crate::undo::UndoPoints
    );

    crate::testing::inert_widget_methods!(
        leaf_layout,
        apply_layout,
        draw,
        get_rect,
        name = "Controlled owner"
    );
    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        for child in self.children.borrow().iter() {
            visitor(child.as_ref());
        }
    }
    fn visit_resource_owners_mut(
        &mut self,
        request: &mut ResourceRequest<'_>,
    ) -> Result<(), tilo_actions::DoorError> {
        for child in self.children.borrow_mut().iter_mut() {
            request.visit(child.as_mut())?;
        }
        Ok(())
    }
    fn sync(&mut self, _: &mut AppModel, _: &ModelPath) {
        let (u, s) = self.model_passes.get();
        self.model_passes.set((u, s + 1));
    }
    fn update(&mut self, _: &mut AppModel, _: &ModelPath) -> bool {
        let (u, s) = self.model_passes.get();
        self.model_passes.set((u + 1, s));
        false
    }
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &["controlled.effect"]
    }
    fn start_operation(
        &mut self,
        op: &crate::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        _: &ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        let Some(crate::widget::OperationRequest {
            invocation,
            kind,
            type_id,
            ..
        }) = op.unscoped()
        else {
            return Ok(None);
        };
        if type_id != "controlled.effect" {
            return Ok(None);
        }
        if doc.get(ROOT, "owner").unwrap().unwrap().0.as_u64() != Some(self.expected_owner.get()) {
            return Err("wrong owner at start".into());
        }
        self.starts.set(self.starts.get() + 1);
        self.invocation.set(Some(invocation));
        Ok(Some(if kind == tilo_actions::OperationKind::View {
            crate::widget::ResourceOperationStart::Completed(tilo_actions::ActionOutput::default())
        } else {
            crate::widget::ResourceOperationStart::Accepted
        }))
    }
    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        cancel: bool,
        doc: &AutoCommit,
        _: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        if self.invocation.get() != Some(invocation) {
            return None;
        }
        assert_eq!(
            doc.get(ROOT, "owner").unwrap().unwrap().0.as_u64(),
            Some(self.expected_owner.get())
        );
        self.polls.set(self.polls.get() + 1);
        if cancel {
            self.invocation.set(None);
            return Some(tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: None,
            });
        }
        if !self.ready.replace(false) {
            return None;
        }
        self.invocation.set(None);
        Some(tilo_actions::InvocationUpdate::Succeeded {
            invocation,
            output: tilo_actions::ActionOutput::default(),
        })
    }
}

impl crate::undo::UndoPoints for Probe {
    fn needs_commit(&self) -> bool {
        self.commit.get()
    }
    fn on_commit(&mut self) {
        self.commit.set(false);
    }
}

impl crate::resources::PublishesResources for Probe {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        self.value.set(
            doc.get(ROOT, "value")
                .unwrap()
                .and_then(|(v, _)| v.as_i64())
                .unwrap_or(-1),
        );
        if !self.eager.get() {
            return vec![];
        }
        (0..self.declarations.get())
            .map(|_| {
                Resource::new("controlled.value", "controlled", "Owned value")
                    .with_action(self.spec.borrow().clone())
            })
            .collect()
    }
    fn resource(&self, _: &AutoCommit, id: &str) -> Option<Resource> {
        (id == "controlled.value" && !self.eager.get() && self.declarations.get() > 0).then(|| {
            Resource::new("controlled.value", "controlled", "Owned value")
                .with_action(self.spec.borrow().clone())
        })
    }
}

impl crate::widget::controls::ActionControls for Probe {
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.queue.borrow_mut().pop_front()
    }
}

impl crate::history_mode::capability::DocumentHistory for Probe {
    fn document_action_applied(
        &mut self,
        _: &EncodedAction,
        before: &[ChangeHash],
        _: &AutoCommit,
        _: &ObjId,
    ) {
        self.acks.set(self.acks.get() + 1);
        *self.ack_heads.borrow_mut() = before.to_vec();
    }
}

impl crate::undo::CommittedActions for Probe {
    fn take_committed_action(&mut self) -> Option<Vec<ChangeHash>> {
        self.actions.borrow_mut().take_committed()
    }
}
crate::testing::test_app_widget!(
    Probe,
    "Controlled owner",
    icon = crate::widget::IconDescriptor::Named("document".into())
);
pub(crate) struct Fixture {
    _temp: tempfile::TempDir,
    _owners: crate::record::RecordOwners,
    pub(crate) store: RecordStore,
    pub(crate) checkout: RecordCheckout,
    pub(crate) target: LegacyResourceTarget,
    pub(crate) tile: TileHost,
    pub(crate) probe: Probe,
}
impl Fixture {
    pub(crate) fn new(id: u64) -> Self {
        Self::in_room(id, "legacy")
    }
    pub(crate) fn in_room(id: u64, room: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let mut owners = crate::record::RecordOwners::new(temp.path().join("profile"));
        let store = owners.open("so.tilo.controlled", room).unwrap();
        let checkout = RecordCheckout::new(
            crate::RecordId::for_tab(id),
            crate::record::RecordKind::new("controlled"),
        );
        store
            .reserve_record(&checkout)
            .unwrap()
            .initialize(|doc| {
                doc.put(ROOT, "value", 1).map_err(|e| e.to_string())?;
                doc.put(ROOT, "owner", id).map_err(|e| e.to_string())?;
                Ok(())
            })
            .unwrap();
        let probe = Probe::new();
        probe.expected_owner.set(id);
        let mut tile = TileHost::new(TileContent::App(Box::new(probe.clone())));
        tile.bind_record(store.clone(), checkout.clone());
        let target = LegacyResourceTarget {
            owner: tile.history_owner().unwrap(),
            address: address(),
        };
        Self {
            _temp: temp,
            _owners: owners,
            store,
            checkout,
            target,
            tile,
            probe,
        }
    }
    pub(crate) fn _temp_path_for_test(&self) -> std::path::PathBuf {
        self._temp.path().join("profile")
    }
    pub(crate) fn into_closed_profile(self) -> (tempfile::TempDir, RecordCheckout) {
        (self._temp, self.checkout)
    }
    pub(crate) fn take_tile(&mut self) -> TileHost {
        std::mem::replace(
            &mut self.tile,
            TileHost::new(TileContent::Notice {
                title: "vacated".into(),
                detail: "fixture moved".into(),
            }),
        )
    }
    pub(crate) fn snapshot(&self) -> (Vec<u8>, u64, Option<u64>) {
        self.store
            .with_action_state(&self.checkout.id, |doc, history, state| {
                (doc.save(), history.revision(), state.generation)
            })
            .unwrap()
    }
}
pub(crate) fn registry() -> tilo_actions::ActionRegistry {
    let mut registry = tilo_actions::ActionRegistry::new();
    registry.register::<SetValue>("controlled.legacy");
    registry
}
fn invoke(
    root: &mut dyn Widget,
    target: &LegacyResourceTarget,
    registry: &tilo_actions::ActionRegistry,
    transport: &mut dyn tilo_actions::ActionTransport,
    permissions: &PermissionContext,
) -> Result<LegacyDocumentOutcome, tilo_actions::DoorError> {
    invoke_legacy_document(
        root,
        target,
        LegacyDocumentServices {
            registry,
            transport,
            qualified_local_transport: true,
            authority: permissions,
            author: &UserId("fixture-user".into()),
        },
        LegacyInvocation {
            argument: None,
            arguments: None,
            actor: "fixture-user",
            broker_admitted: false,
            ambient: None,
        },
    )
}

#[test]
fn retained_policy_time_document_door_denies_delayed_grant_without_mutation() {
    use crate::permissions::{Actor, CapOverride, Scope, Subject, Verb};
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };
    let expiry = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600;
    let time = Arc::new(AtomicU64::new(expiry - 1));
    let clock = time.clone();
    let authority = PermissionContext::from_capabilities(
        Actor::person(None, "fixture-user"),
        vec![
            CapOverride::grant(
                Subject::person("fixture-user"),
                Scope::App("so.tilo.controlled".into()),
                Verb::Write,
            )
            .with_expiry(expiry),
        ],
    )
    .with_policy_time_sampler(Arc::new(move || Ok(clock.load(Ordering::Acquire))));
    let mut fixture = Fixture::new(811);
    assert!(
        authorize_legacy_owner(
            &fixture.target.owner,
            &authority,
            &UserId("fixture-user".into())
        )
        .is_ok()
    );
    let before = fixture.snapshot();
    time.store(expiry, Ordering::Release);
    assert!(
        invoke(
            &mut fixture.tile,
            &fixture.target,
            &registry(),
            &mut LocalTransport::new(),
            &authority
        )
        .is_err()
    );
    assert_eq!(
        fixture.snapshot(),
        before,
        "denial must precede document and history effects"
    );
}

#[test]
fn retained_policy_time_document_door_rejects_clock_failure_before_effects() {
    let authority =
        PermissionContext::permissive().with_policy_time_sampler(std::sync::Arc::new(|| {
            Err("retired owner clock origin".into())
        }));
    let mut fixture = Fixture::new(812);
    let before = fixture.snapshot();
    assert!(
        authorize_legacy_owner(
            &fixture.target.owner,
            &authority,
            &UserId("fixture-user".into())
        )
        .is_err()
    );
    assert!(
        invoke(
            &mut fixture.tile,
            &fixture.target,
            &registry(),
            &mut LocalTransport::new(),
            &authority
        )
        .is_err()
    );
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn legacy_route_two_owners_exact_envelope_noop_undo_and_no_outer_fallback() {
    let mut a = Fixture::new(451);
    let b = Fixture::new(452);
    let before = a.snapshot();
    let sibling = b.snapshot();
    let a_probe = a.probe.clone();
    let b_probe = b.probe.clone();
    a_probe.offer();
    let mut root = Column::new()
        .push(Box::new(std::mem::replace(
            &mut a.tile,
            TileHost::new(TileContent::Notice {
                title: "unused".into(),
                detail: "unused".into(),
            }),
        )))
        .push(Box::new(b.tile));
    let mut outer = AutoCommit::new();
    outer.put(ROOT, "outer", 42).unwrap();
    outer.commit();
    let outer_bytes = outer.save();
    assert!(select_local_target(&root, &outer, &address()).is_err());
    assert!(crate::widget::controls::take_control_invocation(&mut root).is_none());
    let controls = drain_legacy_controls(&mut root).controls;
    assert_eq!(controls.len(), 1);
    assert!(controls[0].0.owner.same_owner(&a.target.owner));
    let mut transport = LocalTransport::new();
    let outcome = invoke(
        &mut root,
        &controls[0].0,
        &registry(),
        &mut transport,
        &PermissionContext::permissive(),
    )
    .unwrap();
    assert!(outcome.changed);
    // The author's handle, then this device's part (`device_actor`).
    assert!(
        outcome
            .envelope
            .actor
            .as_deref()
            .is_some_and(|actor| actor.starts_with(b"fixture-user\x1f")),
        "{:?}",
        outcome.envelope.actor
    );
    assert_eq!(
        outcome.envelope.base_change_hashes(),
        *a_probe.ack_heads.borrow()
    );
    assert_eq!(
        outcome
            .envelope
            .context
            .as_ref()
            .unwrap()
            .app
            .as_ref()
            .unwrap()
            .id,
        "so.tilo.controlled"
    );
    assert_eq!(a_probe.acks.get(), 1);
    assert_eq!(b_probe.acks.get(), 0);
    assert_eq!(outer.save(), outer_bytes);
    assert_eq!(
        b.store
            .with_action_state(&b.checkout.id, |doc, h, s| (
                doc.save(),
                h.revision(),
                s.generation
            ))
            .unwrap(),
        sibling
    );
    let after = a.snapshot();
    assert_eq!(after.1, before.1 + 1);
    assert_eq!(after.2, Some(before.2.unwrap() + 1));
    let noop = invoke(
        &mut root,
        &a.target,
        &registry(),
        &mut transport,
        &PermissionContext::permissive(),
    )
    .unwrap();
    assert!(!noop.changed);
    assert_eq!(a.snapshot(), after);
    assert_eq!(a_probe.acks.get(), 2);
    assert!(drain_legacy_controls(&mut root).controls.is_empty());
    legacy_history_request(
        &a.target.owner,
        crate::undo::UndoRequest::Undo,
        &PermissionContext::permissive(),
        &UserId("fixture-user".into()),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        a.store
            .read_checked(&a.checkout.id, |d| d
                .get(ROOT, "value")
                .unwrap()
                .unwrap()
                .0
                .as_i64())
            .unwrap(),
        Some(1)
    );
    legacy_history_request(
        &a.target.owner,
        crate::undo::UndoRequest::Redo,
        &PermissionContext::permissive(),
        &UserId("fixture-user".into()),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        a.store
            .read_checked(&a.checkout.id, |d| d
                .get(ROOT, "value")
                .unwrap()
                .unwrap()
                .0
                .as_i64())
            .unwrap(),
        Some(2)
    );
}
#[test]
fn legacy_route_fresh_decoder_contract_rollback_and_pending_refusals() {
    let mut f = Fixture::new(460);
    let before = f.snapshot();
    let registry = registry();
    let mut transport = LocalTransport::new();
    let refused = |f: &mut Fixture, reg: &tilo_actions::ActionRegistry, t: &mut LocalTransport| {
        assert!(
            invoke(
                &mut f.tile,
                &f.target,
                reg,
                t,
                &PermissionContext::permissive()
            )
            .is_err()
        );
        assert_eq!(f.snapshot(), before);
        assert_eq!(f.probe.acks.get(), 0);
    };
    refused(&mut f, &tilo_actions::ActionRegistry::new(), &mut transport);
    *f.probe.spec.borrow_mut() = spec(2, true, false);
    refused(&mut f, &registry, &mut transport);
    *f.probe.spec.borrow_mut() = spec(2, false, true);
    refused(&mut f, &registry, &mut transport);
    *f.probe.spec.borrow_mut() = spec(2, false, false);
    f.probe.spec.borrow_mut().locus = ExecutionLocus::ServerPreferred;
    refused(&mut f, &registry, &mut transport);
    *f.probe.spec.borrow_mut() = spec(2, false, false);
    f.probe.spec.borrow_mut().contract.execution =
        tilo_actions::ExecutionMode::Background { cancellable: true };
    refused(&mut f, &registry, &mut transport);
    *f.probe.spec.borrow_mut() = spec(2, false, false);
    f.probe.spec.borrow_mut().contract.confirmation = tilo_actions::ConfirmationPolicy::Confirm;
    refused(&mut f, &registry, &mut transport);
    *f.probe.spec.borrow_mut() = spec(2, false, false);
    f.store
        .with_checked(&f.checkout.id, |doc| {
            doc.put(ROOT, "unfinished", true).unwrap()
        })
        .unwrap();
    let pending = f
        .store
        .read_checked(&f.checkout.id, |doc| (doc.base_heads(), doc.pending_ops()))
        .unwrap();
    assert!(
        invoke(
            &mut f.tile,
            &f.target,
            &registry,
            &mut transport,
            &PermissionContext::permissive()
        )
        .is_err()
    );
    assert_eq!(
        f.store
            .read_checked(&f.checkout.id, |doc| (doc.base_heads(), doc.pending_ops()))
            .unwrap(),
        pending
    );
    assert_eq!(f.probe.acks.get(), 0);
    f.store
        .with_checked(&f.checkout.id, |doc| doc.rollback())
        .unwrap();
    assert!(
        invoke(
            &mut f.tile,
            &f.target,
            &registry,
            &mut transport,
            &PermissionContext::permissive()
        )
        .unwrap()
        .changed
    );
}
#[test]
fn legacy_route_captured_target_ignores_busy_sibling_but_bare_address_refuses() {
    let a = Fixture::new(470);
    let b = Fixture::new(471);
    let store = b.store.clone();
    let checkout = b.checkout.clone();
    let target = a.target.clone();
    let root = Column::new().push(Box::new(a.tile)).push(Box::new(b.tile));
    store
        .with_checked(&checkout.id, |_| {
            assert!(select_local_target(&root, &AutoCommit::new(), &address()).is_err());
            assert!(
                resolve_legacy_invocation(&root, &target).is_ok(),
                "captured custody must not depend on an unrelated busy sibling"
            );
        })
        .unwrap();
}

#[test]
fn legacy_route_fresh_authority_availability_result_arguments_and_transport_refuse_without_effects()
{
    use crate::permissions::*;
    let mut f = Fixture::new(480);
    let before = f.snapshot();
    let mut transport = LocalTransport::new();
    let registry = registry();
    let denied = PermissionContext::from_capabilities(Actor::person(None, "fixture-user"), vec![]);
    assert!(invoke(&mut f.tile, &f.target, &registry, &mut transport, &denied).is_err());
    let grant = |actor: &str, expiry: Expiry, verb: Verb| {
        let mut cap = CapOverride::new(
            Subject::Principal(Principal::Person(actor.into())),
            Scope::App("so.tilo.controlled".into()),
            Effect::Grant,
            verb,
        );
        cap.expiry = expiry;
        PermissionContext::from_capabilities(Actor::person(None, actor), vec![cap]).at_unix_time(1)
    };
    for authority in [
        grant("fixture-user", Expiry::AtUnix(2), Verb::Write),
        grant("other-user", Expiry::Never, Verb::Write),
        grant("fixture-user", Expiry::Never, Verb::Read),
    ] {
        assert!(
            invoke(
                &mut f.tile,
                &f.target,
                &registry,
                &mut transport,
                &authority
            )
            .is_err()
        );
    }
    // A weak operation permission never weakens the mandatory Document write.
    f.probe.spec.borrow_mut().contract.permission = tilo_actions::PermissionRequirement::None;
    assert!(invoke(&mut f.tile, &f.target, &registry, &mut transport, &denied).is_err());
    *f.probe.spec.borrow_mut() = spec(2, false, false);
    f.probe.spec.borrow_mut().contract.permission = tilo_actions::PermissionRequirement::Admin;
    assert!(
        invoke(
            &mut f.tile,
            &f.target,
            &registry,
            &mut transport,
            &grant("fixture-user", Expiry::Never, Verb::Write)
        )
        .is_err()
    );
    *f.probe.spec.borrow_mut() = spec(2, false, false);
    f.probe.spec.borrow_mut().contract.availability = tilo_actions::Availability::Unavailable {
        code: "later".into(),
        reason: "controlled current refusal".into(),
    };
    assert!(
        invoke(
            &mut f.tile,
            &f.target,
            &registry,
            &mut transport,
            &PermissionContext::permissive()
        )
        .is_err()
    );
    *f.probe.spec.borrow_mut() = spec(2, false, false);
    f.probe.spec.borrow_mut().contract.result = tilo_actions::ResultSpec::Json {
        schema: serde_json::json!({"type":"boolean"}),
    };
    assert!(
        invoke(
            &mut f.tile,
            &f.target,
            &registry,
            &mut transport,
            &PermissionContext::permissive()
        )
        .is_err()
    );
    *f.probe.spec.borrow_mut() = spec(2, false, false);
    for (actor, broker, qualified) in [
        ("foreign", false, true),
        ("fixture-user", true, true),
        ("fixture-user", false, false),
    ] {
        assert!(
            invoke_legacy_document(
                &mut f.tile,
                &f.target,
                LegacyDocumentServices {
                    registry: &registry,
                    transport: &mut transport,
                    qualified_local_transport: qualified,
                    authority: &PermissionContext::permissive(),
                    author: &UserId("fixture-user".into())
                },
                LegacyInvocation {
                    argument: None,
                    arguments: None,
                    actor,
                    broker_admitted: broker,
                    ambient: None
                }
            )
            .is_err()
        );
    }
    let arguments = serde_json::Map::from_iter([("unexpected".into(), true.into())]);
    assert!(
        invoke_legacy_document(
            &mut f.tile,
            &f.target,
            LegacyDocumentServices {
                registry: &registry,
                transport: &mut transport,
                qualified_local_transport: true,
                authority: &PermissionContext::permissive(),
                author: &UserId("fixture-user".into())
            },
            LegacyInvocation {
                argument: None,
                arguments: Some(&arguments),
                actor: "fixture-user",
                broker_admitted: false,
                ambient: None
            }
        )
        .is_err()
    );
    assert_eq!(f.snapshot(), before);
    assert_eq!(f.probe.acks.get(), 0);
    assert!(transport.replicated.is_empty());
    assert!(
        invoke(
            &mut f.tile,
            &f.target,
            &registry,
            &mut transport,
            &PermissionContext::permissive()
        )
        .unwrap()
        .changed
    );
    assert_eq!(transport.replicated.len(), 1);
}
#[test]
fn legacy_route_closed_rebound_busy_and_exhausted_owners_never_fall_back() {
    let mut f = Fixture::new(490);
    let before = f.snapshot();
    let mut transport = LocalTransport::new();
    let registry = registry();
    let store = f.store.clone();
    let id = f.checkout.id.clone();
    store
        .with_checked(&id, |_| {
            assert!(
                invoke(
                    &mut f.tile,
                    &f.target,
                    &registry,
                    &mut transport,
                    &PermissionContext::permissive()
                )
                .is_err()
            );
        })
        .unwrap();
    store
        .with_action_state(&id, |_, _, state| state.generation = None)
        .unwrap();
    assert!(
        invoke(
            &mut f.tile,
            &f.target,
            &registry,
            &mut transport,
            &PermissionContext::permissive()
        )
        .is_err()
    );
    store
        .with_action_state(&id, |_, _, state| state.generation = before.2)
        .unwrap();
    crate::component_lifecycle::close_components(&mut f.tile);
    assert!(
        invoke(
            &mut f.tile,
            &f.target,
            &registry,
            &mut transport,
            &PermissionContext::permissive()
        )
        .is_err()
    );
    let replacement = Probe::new();
    let mut tile = TileHost::new(TileContent::App(Box::new(replacement.clone())));
    tile.bind_record(f.store.clone(), f.checkout.clone());
    assert!(
        invoke(
            &mut tile,
            &f.target,
            &registry,
            &mut transport,
            &PermissionContext::permissive()
        )
        .is_err()
    );
    assert_eq!(f.snapshot(), before);
    assert_eq!(replacement.acks.get(), 0);
    assert!(transport.replicated.is_empty());
}

#[test]
fn legacy_route_nested_owners_and_outer_component_duplicate_claims_keep_captured_destinations() {
    use crate::application::{AppComponent, LaunchTarget};
    use crate::application_test_support::{ProbeHost, probe_definition};
    let host = ProbeHost::new();
    let mut context = host
        .prepare(probe_definition(), 581, &LaunchTarget::New)
        .unwrap();
    context
        .initialize_new_record(|doc| {
            doc.put(ROOT, "value", 1).unwrap();
            Ok(())
        })
        .unwrap();
    let component = AppComponent::new(context, Box::new(Probe::new()), registry());
    let mut a = Fixture::new(580);
    let target = a.target.clone();
    let outer_probe = Probe::new();
    let mut root = Column::new()
        .push(Box::new(a.take_tile()))
        .push(Box::new(TileHost::from_component(component)))
        .push(Box::new(outer_probe.clone()));
    assert!(
        matches!(select_local_target(&root, &AutoCommit::new(), &address()), Err(tilo_actions::DoorError::Unavailable { code, .. }) if code == "ambiguous_action_target")
    );
    let mut transport = LocalTransport::new();
    assert!(
        invoke(
            &mut root,
            &target,
            &registry(),
            &mut transport,
            &PermissionContext::permissive()
        )
        .unwrap()
        .changed
    );
    assert_eq!(a.probe.acks.get(), 1);
    assert_eq!(outer_probe.acks.get(), 0);

    let mut parent = Fixture::new(582);
    let mut child = Fixture::new(583);
    parent.probe.declarations.set(0);
    parent
        .probe
        .children
        .borrow_mut()
        .push(Box::new(child.take_tile()));
    let before = parent.snapshot();
    child.probe.offer();
    let controls = drain_legacy_controls(&mut parent.tile).controls;
    assert_eq!(controls.len(), 1);
    assert!(controls[0].0.owner.same_owner(&child.target.owner));
    assert!(
        invoke(
            &mut parent.tile,
            &controls[0].0,
            &registry(),
            &mut transport,
            &PermissionContext::permissive()
        )
        .unwrap()
        .changed
    );
    assert_eq!(parent.snapshot(), before);
    assert_eq!(child.probe.acks.get(), 1);
    assert_eq!(parent.probe.acks.get(), 0);
}

#[test]
fn legacy_route_duplicate_lazy_declarations_refuse_and_unique_lazy_action_executes() {
    let mut f = Fixture::new(590);
    f.probe.declarations.set(0);
    let first = Probe::new();
    first.eager.set(false);
    let second = Probe::new();
    second.eager.set(false);
    f.probe.children.borrow_mut().push(Box::new(first));
    f.probe.children.borrow_mut().push(Box::new(second));
    let before = f.snapshot();
    let mut transport = LocalTransport::new();
    assert!(
        invoke(
            &mut f.tile,
            &f.target,
            &registry(),
            &mut transport,
            &PermissionContext::permissive()
        )
        .is_err(),
        "two lazy declarations in the same owner must refuse"
    );
    assert_eq!(f.snapshot(), before);
    assert_eq!(f.probe.acks.get(), 0);
    assert!(transport.replicated.is_empty());
    f.probe.children.borrow_mut().pop();
    assert!(
        invoke(
            &mut f.tile,
            &f.target,
            &registry(),
            &mut transport,
            &PermissionContext::permissive()
        )
        .unwrap()
        .changed
    );
    assert_eq!(f.probe.acks.get(), 1);
}

#[test]
fn legacy_status_preserves_real_pending_owned_action_until_ordinary_owner_completion() {
    use crate::application::{LaunchTarget, RestoreInput};
    use crate::application_test_support::{ProbeHost, instantiate_probe};
    let host = ProbeHost::new();
    let component = instantiate_probe(&host, 601, LaunchTarget::New, RestoreInput::None).unwrap();
    let binding = component.callback_binding();
    binding
        .with_document(&mut AutoCommit::new(), |doc, _| {
            doc.put(ROOT, "body", "pending").unwrap();
        })
        .unwrap();
    let mut tile = TileHost::new(TileContent::App(Box::new(Probe::new())));
    tile.bind_record(binding.records.clone(), binding.checkout.clone());
    let mut tree = TaffyTree::new();
    let mut pointers = HashMap::new();
    let root = tile.build_layout(&mut tree, &crate::Theme::default(), &mut pointers);
    let mut map = BoundNodeMap::new();
    // SAFETY: this test owns the unchanged live tile and its just-built layout.
    unsafe {
        map.rebuild(&tile, &tree, root, &pointers);
    }
    let node = pointers
        .keys()
        .copied()
        .find(|id| map.legacy_for(*id).is_some())
        .unwrap();
    let snapshot = || {
        binding
            .records
            .with_action_state(&binding.checkout.id, |doc, history, state| {
                (
                    doc.clone().save(),
                    doc.base_heads(),
                    doc.pending_ops(),
                    history.revision(),
                    state.generation,
                    state
                        .pending
                        .as_ref()
                        .map(|p| (p.instance_id, p.before.clone())),
                )
            })
            .unwrap()
    };
    let before = snapshot();
    assert!(before.5.is_some());
    for _ in 0..3 {
        assert!(map.legacy_status(node, &binding.author).is_some());
    }
    assert_eq!(snapshot(), before);
    let mut committer = Probe::new();
    committer.commit.set(true);
    binding
        .with_record_frame(&mut AutoCommit::new(), false, |doc, _| {
            binding.drain_widget_actions(&mut committer, doc)
        })
        .unwrap()
        .unwrap();
    let after = snapshot();
    assert_eq!(
        after.5.as_ref().unwrap().1,
        after.1,
        "ordinary completion retains only a clean continuation"
    );
    assert_eq!(after.3, before.3 + 1);
}

#[test]
fn legacy_route_actual_file_write_revocation_never_falls_back_to_app_grant() {
    use crate::permissions::*;
    let mut f = Fixture::new(610);
    let path = f._temp.path().join("controlled-owned-file.txt");
    let checkout = RecordCheckout::new(
        crate::RecordId::for_file(&path),
        crate::record::RecordKind::new("controlled"),
    );
    f.store
        .reserve_record(&checkout)
        .unwrap()
        .initialize(|doc| {
            doc.put(ROOT, "value", 1).unwrap();
            Ok(())
        })
        .unwrap();
    f.tile.bind_record(f.store.clone(), checkout.clone());
    f.checkout = checkout;
    f.target.owner = f.tile.history_owner().unwrap();
    let principal = Subject::Principal(Principal::Person("fixture-user".into()));
    let app = CapOverride::new(
        principal.clone(),
        Scope::App("so.tilo.controlled".into()),
        Effect::Grant,
        Verb::Write,
    );
    let file = CapOverride::new(
        principal,
        Scope::FsPath(path.to_string_lossy().into_owned()),
        Effect::Grant,
        Verb::Write,
    );
    let authority =
        |grants| PermissionContext::from_capabilities(Actor::person(None, "fixture-user"), grants);
    let before = f.snapshot();
    let mut transport = LocalTransport::new();
    assert!(
        invoke(
            &mut f.tile,
            &f.target,
            &registry(),
            &mut transport,
            &authority(vec![app.clone()])
        )
        .is_err()
    );
    assert_eq!(f.snapshot(), before);
    assert!(transport.replicated.is_empty());
    assert!(
        invoke(
            &mut f.tile,
            &f.target,
            &registry(),
            &mut transport,
            &authority(vec![app.clone(), file])
        )
        .unwrap()
        .changed
    );
    let after = f.snapshot();
    *f.probe.spec.borrow_mut() = spec(3, false, false);
    assert!(
        invoke(
            &mut f.tile,
            &f.target,
            &registry(),
            &mut transport,
            &authority(vec![app])
        )
        .is_err()
    );
    assert_eq!(f.snapshot(), after);
    assert_eq!(f.probe.acks.get(), 1);
    assert_eq!(transport.replicated.len(), 1);
}

#[test]
fn legacy_route_headless_two_real_owners_capture_and_model_ticks_do_not_duplicate_action() {
    let mut a = Fixture::new(620);
    let mut b = Fixture::new(621);
    let root = Column::new()
        .push(Box::new(a.take_tile()))
        .push(Box::new(b.take_tile()));
    let mut state = crate::State::new();
    state.doc.put(ROOT, "outer-only", 17).unwrap();
    state.commit_action(None, None);
    let mut driver = crate::headless::Driver::new(state, Box::new(root), (700, 400));
    driver
        .action_registry_mut()
        .register::<SetValue>("controlled.legacy");
    driver.key("Shift"); // Complete ordinary initial layout settling before measuring equal event passes.
    let outer = driver.state_mut().doc.save();
    let a0 = a.snapshot();
    let b0 = b.snapshot();
    let a_pass0 = a.probe.model_passes.replace((0, 0));
    let b_pass0 = b.probe.model_passes.replace((0, 0));
    assert_eq!(a_pass0, b_pass0);
    driver.key("Shift");
    let a1 = a.snapshot();
    let b1 = b.snapshot();
    let model_passes = a.probe.model_passes.replace((0, 0));
    assert_eq!(b.probe.model_passes.replace((0, 0)), model_passes);
    assert!(model_passes.0 > 0 && model_passes.1 > 0);
    let model_a = a1.2.unwrap() - a0.2.unwrap();
    let model_b = b1.2.unwrap() - b0.2.unwrap();
    a.probe.offer();
    driver.key("Shift");
    assert_eq!(a.probe.model_passes.replace((0, 0)), model_passes);
    assert_eq!(b.probe.model_passes.replace((0, 0)), model_passes);
    let a2 = a.snapshot();
    let b2 = b.snapshot();
    assert_eq!(a.probe.acks.get(), 1);
    assert_eq!(b.probe.acks.get(), 0);
    assert_eq!(a2.1, a1.1 + 1);
    assert_eq!(b2.1, b1.1);
    assert_eq!(b2.0, b1.0);
    assert_eq!(a2.2, Some(a1.2.unwrap() + model_a + 1));
    assert_eq!(b2.2, Some(b1.2.unwrap() + model_b));
    assert_eq!(driver.state_mut().doc.save(), outer);
    driver.key("Shift");
    assert_eq!(a.probe.model_passes.replace((0, 0)), model_passes);
    assert_eq!(b.probe.model_passes.replace((0, 0)), model_passes);
    let a3 = a.snapshot();
    let b3 = b.snapshot();
    assert_eq!(a3.1, a2.1);
    assert_eq!(a3.0, a2.0);
    assert_eq!(a3.2, Some(a2.2.unwrap() + model_a));
    assert_eq!(b3.1, b2.1);
    assert_eq!(b3.0, b2.0);
    assert_eq!(b3.2, Some(b2.2.unwrap() + model_b));
    assert_eq!(a.probe.acks.get(), 1);
    a.probe.offer();
    driver.key("Shift");
    assert_eq!(a.probe.model_passes.get(), model_passes);
    assert_eq!(b.probe.model_passes.get(), model_passes);
    let a4 = a.snapshot();
    let b4 = b.snapshot();
    assert_eq!(a4.0, a3.0);
    assert_eq!(a4.1, a3.1);
    assert_eq!(a4.2, Some(a3.2.unwrap() + model_a));
    assert_eq!(b4.0, b3.0);
    assert_eq!(b4.1, b3.1);
    assert_eq!(b4.2, Some(b3.2.unwrap() + model_b));
    assert_eq!(a.probe.acks.get(), 2);
    assert_eq!(b.probe.acks.get(), 0);
    assert_eq!(driver.state_mut().doc.save(), outer);
}

pub(crate) fn independent_batch_fixture(
    case: &str,
    failed_first: bool,
    view: bool,
) -> (Fixture, Fixture, Column) {
    let mut a = Fixture::new(710);
    let mut b = Fixture::new(711);
    if view {
        *a.probe.spec.borrow_mut() = ActionSpec::operation(
            "set",
            "Disposable view",
            tilo_actions::OperationKind::View,
            "controlled.effect",
            &(),
        )
        .unwrap();
        *b.probe.spec.borrow_mut() = a.probe.spec.borrow().clone();
    }
    match case {
        "closed" => {
            crate::component_lifecycle::close_components(&mut b.tile);
            assert!(b.tile.history_owner().is_none());
        }
        "unavailable" => {
            let missing = RecordCheckout::new(
                crate::RecordId::for_tab(712),
                crate::record::RecordKind::new("controlled"),
            );
            b.tile.bind_record(b.store.clone(), missing);
            assert!(b.tile.history_owner().is_none());
        }
        "busy" => {}
        _ => panic!("unknown fixture case"),
    }
    assert!(b.tile.has_separate_document_owner());
    let first = if failed_first {
        b.take_tile()
    } else {
        a.take_tile()
    };
    let second = if failed_first {
        a.take_tile()
    } else {
        b.take_tile()
    };
    (
        a,
        b,
        Column::new().push(Box::new(first)).push(Box::new(second)),
    )
}
