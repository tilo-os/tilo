use super::*;
use crate::widget::{AppModel, ModelPath};
use automerge::{ReadDoc, transaction::Transactable};
use serde::{Deserialize, Serialize};
use tilo_actions::{Action, ActionError, ActionTransport, ExecutionLocus};

#[derive(Debug, Serialize, Deserialize)]
struct Write {
    value: String,
    server: bool,
    refuse: bool,
}
impl Action for Write {
    fn apply(&self, d: &mut AutoCommit) -> Result<(), ActionError> {
        if self.refuse {
            return Err(ActionError::Refused("fixture refusal".into()));
        }
        if d.get(ROOT, "body")?
            .and_then(|(v, _)| v.as_str().map(str::to_owned))
            .as_deref()
            != Some(&self.value)
        {
            d.put(ROOT, "body", self.value.as_str())?;
        }
        Ok(())
    }
    fn locus(&self) -> ExecutionLocus {
        if self.server {
            ExecutionLocus::ServerPreferred
        } else {
            ExecutionLocus::ClientLocal
        }
    }
}
#[derive(Default)]
struct Owner {
    receipts: Vec<(tilo_actions::EncodedAction, Vec<ChangeHash>, String)>,
    effect_receipts: Vec<(tilo_actions::InvocationId, Vec<ChangeHash>, String)>,
    effect_replies: Vec<tilo_actions::InvocationId>,
    retained_source: Option<tilo_actions::InvocationId>,
    sibling: Option<Box<Owner>>,
}
impl Widget for Owner {
    crate::testing::inert_widget_methods!(
        apply_layout,
        draw,
        get_rect,
        name = "Acknowledgment owner"
    );
    crate::provides!(
        dyn crate::history_mode::capability::DocumentHistory,
        dyn crate::widget::document_effects::DocumentEffects
    );

    fn start_operation(
        &mut self,
        op: &crate::widget::ResourceOperation<'_>,
        doc: &mut crate::widget::Edits<'_>,
        path: &ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        if let Some(started) = crate::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        Ok(op.unscoped().map(|_| {
            crate::widget::ResourceOperationStart::Completed(tilo_actions::ActionOutput::default())
        }))
    }
    fn sync(&mut self, _: &AppModel, _: &ModelPath) {}
    fn build_layout(
        &mut self,
        t: &mut taffy::TaffyTree<()>,
        _: &crate::Theme,
        _: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        t.new_leaf(taffy::Style::default()).unwrap()
    }
    fn children_mut(&mut self) -> Vec<&mut dyn Widget> {
        self.sibling
            .as_mut()
            .map_or_else(Vec::new, |s| vec![s.as_mut() as &mut dyn Widget])
    }
}

impl crate::widget::document_effects::DocumentEffects for Owner {
    fn start_document_effects(
        &mut self,
        operation: &crate::widget::ResourceOperation<'_>,
        applied: &crate::widget::document_effects::AppliedDocument<'_>,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        operation.check()?;
        assert_eq!(operation.kind(), tilo_actions::OperationKind::Document);
        self.effect_replies.push(applied.invocation());
        self.effect_receipts.push((
            operation.invocation(),
            applied.before().to_vec(),
            applied
                .document()
                .get(ROOT, "body")
                .unwrap()
                .unwrap()
                .0
                .as_str()
                .unwrap()
                .into(),
        ));
        Ok(Some(crate::widget::ResourceOperationStart::Accepted))
    }
}

impl crate::history_mode::capability::DocumentHistory for Owner {
    fn document_action_applied(
        &mut self,
        action: &tilo_actions::EncodedAction,
        before: &[ChangeHash],
        doc: &AutoCommit,
        _: &ObjId,
    ) {
        self.receipts.push((
            action.clone(),
            before.to_vec(),
            doc.get(ROOT, "body")
                .unwrap()
                .unwrap()
                .0
                .as_str()
                .unwrap()
                .into(),
        ));
    }
}
struct Host;
impl ActionTransport for Host {
    fn replicate(&mut self, _: &tilo_actions::ActionEnvelope) {}
    fn submit_to_host(&mut self, _: &tilo_actions::ActionEnvelope) -> tilo_actions::HostDecision {
        tilo_actions::HostDecision::AcceptedByHost
    }
}
fn execute(
    owner: &mut Owner,
    doc: &mut AutoCommit,
    action: &Write,
    broker: bool,
) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
    execute_mode(owner, doc, action, broker, false)
}

fn execute_mode(
    owner: &mut Owner,
    doc: &mut AutoCommit,
    action: &Write,
    broker: bool,
    background: bool,
) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
    let mut registry = tilo_actions::ActionRegistry::new();
    registry.register::<Write>("ack.write");
    let resource = tilo_actions::Resource::new("ack.owner", "ack.owner", "Owner");
    let mut spec =
        tilo_actions::ActionSpec::immediate("write", "Write", "ack.write", action).unwrap();
    if background {
        spec.contract.execution = tilo_actions::ExecutionMode::Background { cancellable: true };
        spec.contract.result = tilo_actions::ResultSpec::Job;
    }
    let invocation = tilo_actions::InvocationId::new();
    let source_invocation = owner
        .retained_source
        .or_else(|| broker.then_some(invocation));
    execute_resource_operation(
        owner,
        doc,
        &ROOT,
        &registry,
        &mut Host,
        ResourceOperation {
            target_route: None,
            address: &tilo_actions::ActionAddress::new("ack.owner", "write"),
            resource: &resource,
            spec: &spec,
            encoded: tilo_actions::EncodedAction::encode("ack.write", action).unwrap(),
            actor: "local",
            local_author: "local",
            broker_admitted: broker,
            invocation,
            source_invocation,
            ambient: None,
            scope: None,
        },
    )
}

#[test]
fn document_effects_distinguish_retained_source_custody_from_local_reply_identity() {
    let source = tilo_actions::InvocationId::new();
    let mut owner = Owner {
        retained_source: Some(source),
        ..Owner::default()
    };
    let mut doc = AutoCommit::new();
    let action = Write {
        value: "source-owned".into(),
        server: false,
        refuse: false,
    };
    assert_eq!(
        execute_mode(&mut owner, &mut doc, &action, true, true).unwrap(),
        crate::widget::ResourceOperationStart::Accepted
    );
    assert_eq!(owner.effect_receipts[0].0, source);
    assert_ne!(owner.effect_replies[0], source);
    assert_eq!(owner.effect_receipts[0].2, "source-owned");
}

#[test]
fn document_effects_defer_only_the_actual_locally_applied_background_action() {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "body", "before").unwrap();
    let before = doc.get_heads();
    let mut owner = Owner {
        sibling: Some(Box::default()),
        ..Owner::default()
    };
    let local = Write {
        value: "after".into(),
        server: false,
        refuse: false,
    };
    assert_eq!(
        execute_mode(&mut owner, &mut doc, &local, false, true).unwrap(),
        crate::widget::ResourceOperationStart::Accepted
    );
    assert_eq!(owner.effect_receipts.len(), 1);
    assert_eq!(owner.effect_receipts[0].1, before);
    assert_eq!(owner.effect_receipts[0].2, "after");
    assert!(owner.sibling.as_ref().unwrap().effect_receipts.is_empty());
    assert!(matches!(
        execute(&mut owner, &mut doc, &local, false).unwrap(),
        crate::widget::ResourceOperationStart::Completed(_)
    ));
    assert_eq!(owner.effect_receipts.len(), 1);
    let submitted = Write {
        value: "host".into(),
        server: true,
        refuse: false,
    };
    assert!(matches!(
        execute_mode(&mut owner, &mut doc, &submitted, false, true).unwrap(),
        crate::widget::ResourceOperationStart::Completed(_)
    ));
    assert_eq!(owner.effect_receipts.len(), 1);
    let refused = Write {
        value: "refused".into(),
        server: false,
        refuse: true,
    };
    assert!(execute_mode(&mut owner, &mut doc, &refused, false, true).is_err());
    assert_eq!(owner.effect_receipts.len(), 1);
}
#[test]
fn document_actions_acknowledge_only_locally_applied_exact_owner_frontiers() {
    // Case `locally_applied_document_acknowledges_exact_owner_frontier_and_noop_only`.
    {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "body", "before").unwrap();
        let before = doc.get_heads();
        let mut owner = Owner {
            sibling: Some(Box::default()),
            ..Default::default()
        };
        let local = Write {
            value: "after".into(),
            server: false,
            refuse: false,
        };
        execute(&mut owner, &mut doc, &local, false).unwrap();
        assert_eq!(owner.receipts.len(), 1);
        assert_eq!(owner.receipts[0].1, before);
        assert_eq!(owner.receipts[0].2, "after");
        assert_eq!(
            owner.receipts[0].0,
            tilo_actions::EncodedAction::encode("ack.write", &local).unwrap()
        );
        let unchanged = doc.base_heads();
        execute(&mut owner, &mut doc, &local, false).unwrap();
        assert_eq!(owner.receipts.len(), 2);
        assert_eq!(doc.base_heads(), unchanged);
        let remote = Write {
            value: "host".into(),
            server: true,
            refuse: false,
        };
        execute(&mut owner, &mut doc, &remote, false).unwrap();
        assert_eq!(owner.receipts.len(), 2);
        assert_eq!(doc.base_heads(), unchanged);
        let failure = Write {
            value: "refused".into(),
            server: false,
            refuse: true,
        };
        assert!(execute(&mut owner, &mut doc, &failure, false).is_err());
        assert_eq!(owner.receipts.len(), 2);
        execute(&mut owner, &mut doc, &remote, true).unwrap();
        assert_eq!(owner.receipts.len(), 3);
        assert_eq!(owner.receipts[2].2, "host");
        assert!(owner.sibling.as_ref().unwrap().receipts.is_empty());
    }

    // Case `view_and_effect_operations_do_not_acknowledge_document_actions`.
    {
        let mut owner = Owner::default();
        let mut doc = AutoCommit::new();
        let action = Write {
            value: "must not write".into(),
            server: false,
            refuse: false,
        };
        let registry = tilo_actions::ActionRegistry::new();
        let resource = tilo_actions::Resource::new("owner", "owner", "Owner");
        for kind in [
            tilo_actions::OperationKind::View,
            tilo_actions::OperationKind::Effect,
        ] {
            let spec =
                tilo_actions::ActionSpec::operation("run", "Run", kind, "operation", &action)
                    .unwrap();
            execute_resource_operation(
                &mut owner,
                &mut doc,
                &ROOT,
                &registry,
                &mut Host,
                ResourceOperation {
                    target_route: None,
                    address: &tilo_actions::ActionAddress::new("owner", "run"),
                    resource: &resource,
                    spec: &spec,
                    encoded: tilo_actions::EncodedAction::encode_operation("operation", &action)
                        .unwrap(),
                    actor: "local",
                    local_author: "local",
                    broker_admitted: false,
                    invocation: tilo_actions::InvocationId::new(),
                    source_invocation: None,
                    ambient: None,
                    scope: None,
                },
            )
            .unwrap();
        }
        assert!(owner.receipts.is_empty());
        assert!(doc.get(ROOT, "body").unwrap().is_none());
        assert_eq!(doc.pending_ops(), 0);
    }
}
