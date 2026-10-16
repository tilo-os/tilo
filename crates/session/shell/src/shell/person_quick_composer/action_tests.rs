use super::PersonDraftOwner;
use super::actions::*;
use std::sync::{Arc, Mutex};
use tilo_actions::remote::*;
use tilo_actions::{
    ActionAddress, ActionContract, ActionDescriptor, ActionOutput, ActionParam, ExecutionLocus,
    InputSpec, InvocationId, OperationKind, ParamType,
};

struct ChatHost {
    caller: Caller,
    owner: PersonDraftOwner,
    calls: Mutex<Vec<InvokeRequest>>,
    result: serde_json::Value,
    wrong_workspace: bool,
}
impl ActionHost for ChatHost {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        assert_eq!(
            caller, &self.caller,
            "retain certified Person, device and execution"
        );
        let page = if let Some(route) = query.canonical_route {
            assert_eq!(route.room, self.owner.workspace);
            assert_eq!(route.app_id, "so.tilo.chat");
            assert_eq!(route.record_id, "");
            CataloguePage {
                exhausted: true,
                providers: vec![CatalogueProvider {
                    provider: "opaque-native-chat".into(),
                    app_id: "so.tilo.chat".into(),
                    room: if self.wrong_workspace {
                        "other".into()
                    } else {
                        route.room
                    },
                    record_id: String::new(),
                    browsable: true,
                }],
                ..CataloguePage::default()
            }
        } else {
            assert_eq!(query.provider.as_deref(), Some("opaque-native-chat"));
            let root = query.target.is_none();
            let address = if let Some(address) = query.target {
                assert_eq!(
                    address.resource,
                    format!("chat.person:{}", self.owner.person_key)
                );
                address
            } else {
                assert!(matches!(
                    query.needle.as_str(),
                    "open-direct-conversation" | "send-direct-message"
                ));
                ActionAddress::new("workspace:opaque-record", &query.needle)
            };
            CataloguePage {
                exhausted: true,
                revision: "contact-r1".into(),
                rows: vec![CatalogueRow::for_action(
                    "opaque-native-chat",
                    "so.tilo.chat",
                    ActionDescriptor {
                        address,
                        semantic_identity: None,
                        resource_kind: "chat.person".into(),
                        resource_label: "Untrusted display alias".into(),
                        action_label: "Action".into(),
                        description: None,
                        locus: ExecutionLocus::ClientLocal,
                        kind: OperationKind::View,
                        contract: ActionContract::default(),
                        input: InputSpec::None,
                        params: if root {
                            vec![
                                ActionParam::required(
                                    "public_key",
                                    ParamType::Text,
                                    "Certified person",
                                ),
                                ActionParam::required("text", ParamType::Text, "Private body"),
                            ]
                        } else {
                            Vec::new()
                        },
                        view: None,
                    },
                )],
                ..CataloguePage::default()
            }
        };
        Pending::ready(Ok(page))
    }
    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        assert_eq!(caller, &self.caller);
        let id = request.invocation;
        assert_eq!(request.revision.as_deref(), Some("contact-r1"));
        assert_eq!(
            request.on_behalf_of, None,
            "this is the certified person's own invocation"
        );
        self.calls.lock().unwrap().push(request);
        Pending::ready(Ok(InvocationReceipt {
            invocation: id,
            status: ReceiptStatus::Completed {
                output: ActionOutput {
                    value: Some(self.result.clone()),
                    resources: Vec::new(),
                },
            },
        }))
    }
    fn observe(&self, _: &Caller, _: &InvocationId) -> Option<InvocationState> {
        None
    }
    fn cancel(&self, _: &Caller, _: &InvocationId) -> Result<(), ActionHostError> {
        Err(ActionHostError::NotPermitted)
    }
}

#[test]
fn exact_chat_pair_owner_result_is_required_and_other_workspace_is_refused() {
    for wrong_workspace in [false, true] {
        for accepted in [false, true] {
            let owner = PersonDraftOwner {
                workspace: uuid::Uuid::from_u128(45).to_string(),
                person_key: "ab".repeat(32),
                caller: "cd".repeat(32),
                identity_revision: 1,
            };
            let caller = Caller::authenticated([0xcd; 32], [0xef; 32])
                .in_room(&owner.workspace)
                .in_new_execution();
            let host = Arc::new(ChatHost {
                caller: caller.clone(),
                owner: owner.clone(),
                calls: Mutex::new(Vec::new()),
                result: serde_json::json!({"peer": owner.person_key, "accepted": accepted}),
                wrong_workspace,
            });
            let mut call = Call::new(
                crate::shell::certified_action_session::DesktopActionSession::Mock {
                    host: host.clone(),
                    caller,
                },
                owner,
                Some("private body".into()),
            );
            assert_eq!(call.pump(), Some(accepted && !wrong_workspace));
            drop(call);
            let calls = host.calls.lock().unwrap();
            assert_eq!(calls.len(), usize::from(!wrong_workspace));
            if let Some(request) = calls.first() {
                assert_eq!(request.target.address.action, "send-direct-message");
                assert_eq!(
                    request.argument,
                    InvokeArgument::Json(serde_json::Map::from_iter([
                        ("public_key".into(), "ab".repeat(32).into()),
                        ("text".into(), "private body".into()),
                    ]))
                );
            }
        }
    }
}

#[test]
fn expansion_uses_real_private_pair_open_not_profile() {
    let owner = PersonDraftOwner {
        workspace: uuid::Uuid::from_u128(46).to_string(),
        person_key: "ab".repeat(32),
        caller: "cd".repeat(32),
        identity_revision: 1,
    };
    let caller = Caller::authenticated([0xcd; 32], [0xef; 32])
        .in_room(&owner.workspace)
        .in_new_execution();
    let host = Arc::new(ChatHost {
        caller: caller.clone(),
        owner: owner.clone(),
        calls: Mutex::new(Vec::new()),
        result: serde_json::Value::Null,
        wrong_workspace: false,
    });
    let mut call = Call::new(
        crate::shell::certified_action_session::DesktopActionSession::Mock {
            host: host.clone(),
            caller,
        },
        owner,
        None,
    );
    assert_eq!(call.pump(), Some(true));
    drop(call);
    let calls = host.calls.lock().unwrap();
    let [request] = calls.as_slice() else {
        panic!("opening the private pair must invoke exactly one action");
    };
    assert_eq!(request.target.address.action, "open-direct-conversation");
    assert_eq!(
        request.argument,
        InvokeArgument::Json(serde_json::Map::from_iter([(
            "public_key".into(),
            "ab".repeat(32).into()
        ),]))
    );
    drop(calls);
}

fn transfer_fixture() -> (
    super::PersonQuickComposer,
    crate::shell::certified_action_session::DesktopActionSession,
) {
    use ui::widget::Widget;

    fn prepare(widget: &mut dyn Widget) -> bool {
        if widget.test_id() == Some("person-quick-editor") {
            let input = widget
                .as_any_mut()
                .unwrap()
                .downcast_mut::<ui::widget::TextInput>()
                .unwrap();
            input.set_caret_position(ui::text_selection::TextPosition::new(3));
            input.restore_private_preedit("compose", Some((1, 4)));
            return true;
        }
        widget.children_mut().into_iter().any(prepare)
    }

    let owner = PersonDraftOwner {
        workspace: uuid::Uuid::from_u128(47).to_string(),
        person_key: "ab".repeat(32),
        caller: "cd".repeat(32),
        identity_revision: 1,
    };
    let caller = Caller::authenticated([0xcd; 32], [0xef; 32]).in_room(&owner.workspace);
    let host = Arc::new(ChatHost {
        caller: caller.clone(),
        owner: owner.clone(),
        calls: Mutex::new(Vec::new()),
        result: serde_json::Value::Null,
        wrong_workspace: false,
    });
    let session =
        crate::shell::certified_action_session::DesktopActionSession::Mock { host, caller };
    let mut composer = super::PersonQuickComposer::default();
    composer.open(
        owner,
        "Alice",
        vello::kurbo::Rect::new(10.0, 10.0, 42.0, 42.0),
        vello::kurbo::Rect::new(0.0, 0.0, 800.0, 600.0),
    );
    composer.draft.set("private draft".into());
    Widget::sync(
        &mut composer,
        &automerge::AutoCommit::new(),
        &automerge::ROOT,
    );
    assert!(prepare(&mut composer));
    (composer, session)
}

fn pending_handoff(
    composer: &super::PersonQuickComposer,
    session: crate::shell::certified_action_session::DesktopActionSession,
) -> Call {
    let mut call = Call::new(session, composer.owner.clone().unwrap(), None);
    call.handoff = Some(Handoff {
        token: uuid::Uuid::new_v4(),
        source: InputContext {
            generation: composer.generation,
            composition_generation: composer.composition_generation,
            editor: composer.editor_snapshot().unwrap(),
        },
    });
    call
}

#[test]
fn late_adoption_cannot_clear_identical_editor_reopened_for_same_person() {
    use ui::widget::Widget;
    let (mut composer, session) = transfer_fixture();
    let call = pending_handoff(&composer, session);
    let original_generation = composer.generation;
    composer.dismiss();
    assert!(composer.generation > original_generation);
    composer.open(
        call.owner.clone(),
        "Alice",
        vello::kurbo::Rect::new(10.0, 10.0, 42.0, 42.0),
        vello::kurbo::Rect::new(0.0, 0.0, 800.0, 600.0),
    );
    Widget::sync(
        &mut composer,
        &automerge::AutoCommit::new(),
        &automerge::ROOT,
    );
    let source = &call.handoff.as_ref().unwrap().source;
    super::restore_preedit(&mut composer.popover, &source.editor);
    assert!(composer.editor_snapshot().unwrap() == source.editor);
    composer.finish_call(&call, true);
    drop(call);
    assert!(composer.is_open());
    assert_eq!(composer.draft.get(), "private draft");
}

#[test]
fn failed_staging_restores_captured_composition_without_moving_caret() {
    let (mut composer, session) = transfer_fixture();
    let captured = composer.editor_snapshot().unwrap();
    assert_eq!(captured.caret, 3);
    let mut blurred = captured.clone();
    blurred.preedit.clear();
    blurred.preedit_cursor = None;
    super::restore_preedit(&mut composer.popover, &blurred);
    assert!(composer.editor_snapshot().unwrap().preedit.is_empty());
    let owner = composer.owner.clone().unwrap();
    assert!(
        composer
            .stage_expansion(session, owner, captured.clone())
            .is_err()
    );
    assert!(composer.editor_snapshot().unwrap() == captured);
    assert_eq!(composer.draft.get(), "private draft");
    assert!(composer.is_open());
}

#[test]
fn adoption_never_clears_changed_composition_or_preedit_cursor() {
    for change in 0..3 {
        let (mut composer, session) = transfer_fixture();
        let call = pending_handoff(&composer, session);
        let mut editor = composer.editor_snapshot().unwrap();
        match change {
            0 => editor.preedit_cursor = Some((2, 5)),
            1 => editor.preedit.clear(),
            _ => {
                composer.composition_generation =
                    super::next_generation(composer.composition_generation);
            }
        }
        super::restore_preedit(&mut composer.popover, &editor);
        composer.finish_call(&call, true);
        assert!(composer.is_open());
        assert_eq!(composer.draft.get(), "private draft");
        assert!(composer.editor_snapshot().unwrap() == editor);
        assert!(composer.enabled.get());
    }
}
