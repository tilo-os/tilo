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
    sibling: Option<Box<Owner>>,
}
impl Widget for Owner {
    crate::provides!(dyn crate::history_mode::capability::DocumentHistory);

    fn start_operation(
        &mut self,
        op: &crate::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        if let Some(started) = crate::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        Ok(op.unscoped().map(|_| {
            crate::widget::ResourceOperationStart::Completed(tilo_actions::ActionOutput::default())
        }))
    }
    fn sync(&mut self, _: &mut AppModel, _: &ModelPath) {}
    fn build_layout(
        &mut self,
        t: &mut taffy::TaffyTree<()>,
        _: &crate::Theme,
        _: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        t.new_leaf(taffy::Style::default()).unwrap()
    }
    fn apply_layout(&mut self, _: &taffy::TaffyTree<()>, _: taffy::NodeId, _: vello::kurbo::Point) {
    }
    fn draw(
        &mut self,
        _: &mut vello::Scene,
        _: vello::kurbo::Affine,
        _: &mut crate::TextRenderer,
        _: &crate::Theme,
        _: &crate::presence::PresenceCtx,
    ) {
    }
    fn name(&self) -> &str {
        "Acknowledgment owner"
    }
    fn get_rect(&self) -> vello::kurbo::Rect {
        vello::kurbo::Rect::ZERO
    }
    fn children_mut(&mut self) -> Vec<&mut dyn Widget> {
        self.sibling
            .as_mut()
            .map_or_else(Vec::new, |s| vec![s.as_mut() as &mut dyn Widget])
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
    let mut registry = tilo_actions::ActionRegistry::new();
    registry.register::<Write>("ack.write");
    let resource = tilo_actions::Resource::new("ack.owner", "ack.owner", "Owner");
    let spec = tilo_actions::ActionSpec::immediate("write", "Write", "ack.write", action).unwrap();
    execute_resource_operation(
        owner,
        doc,
        &ROOT,
        &registry,
        &mut Host,
        ResourceOperation {
            address: &tilo_actions::ActionAddress::new("ack.owner", "write"),
            resource: &resource,
            spec: &spec,
            encoded: tilo_actions::EncodedAction::encode("ack.write", action).unwrap(),
            actor: "local",
            local_author: "local",
            broker_admitted: broker,
            invocation: tilo_actions::InvocationId::new(),
            ambient: None,
            scope: None,
        },
    )
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
                    address: &tilo_actions::ActionAddress::new("owner", "run"),
                    resource: &resource,
                    spec: &spec,
                    encoded: tilo_actions::EncodedAction::encode_operation("operation", &action)
                        .unwrap(),
                    actor: "local",
                    local_author: "local",
                    broker_admitted: false,
                    invocation: tilo_actions::InvocationId::new(),
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
