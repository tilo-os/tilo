//! Local, frontier-bound Activity organization through the shell action door.
//! This queue is a host bridge, not remote resource admission.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::search::action_api::{ActionCatalog, AllVisible, ResourceRef};
    use crate::shell::search::providers::actions::ActionsProvider;
    use crate::shell::{chrome_state::ChromeState, state::WorkspaceTree};

    fn fixture() -> (
        crate::shell::overlay_host::ShellOverlayHost,
        serde_json::Value,
    ) {
        let mut tree = WorkspaceTree::bootstrap_demo();
        tree.active_workspace_mut()
            .unwrap()
            .set_owner("shell-fixture");
        for name in ["Revision one", "Revision two", "Revision three"] {
            tree.active_workspace_mut().unwrap().name = name.into();
            tree.seed_doc_from_cache();
            tree.commit_room_change();
        }
        let room = tree.active_workspace().unwrap().room.to_string();
        let chrome = ChromeState::new_with_fixture_handle(
            crate::shell::state::WorkspaceTreeHandle::new(tree),
            crate::shell::hosted_inputs::HostedTileInputs::default(),
        );
        let mut host = crate::shell::overlay_host::ShellOverlayHost::new(Box::new(
            ui::widget::Spacer::fixed(0.0),
        ))
        .with_chrome(chrome.clone());
        host.room_activity
            .set_view(tilo_desktop::activity::ActivityView::Explore);
        host.room_activity.refresh(&mut *chrome.borrow().tree_mut());
        let entries = host.room_activity.entries();
        assert!(entries.len() >= 2);
        let heads = chrome.borrow().tree_mut().doc.get_heads();
        let args = serde_json::json!({"room":room,"origin":"desktop_tree","frontier":heads.iter().map(ToString::to_string).collect::<Vec<_>>(),"operation":{"kind":"name","start":encode_entry(entries[0].id),"end":encode_entry(entries[1].id),"label":"Semantic review"}});
        (host, args)
    }
    fn invoke(
        host: &crate::shell::overlay_host::ShellOverlayHost,
        args: &serde_json::Value,
    ) -> crate::shell::search::PendingInvocation {
        invoke_action(host, args, EDIT)
    }
    fn invoke_action(
        host: &crate::shell::overlay_host::ShellOverlayHost,
        args: &serde_json::Value,
        id: &str,
    ) -> crate::shell::search::PendingInvocation {
        let chrome = host.chrome.as_ref().unwrap();
        let mut registry = crate::shell::search::SearchRegistry::new();
        registry.register(Rc::new(ActionsProvider::new()));
        let mut document = automerge::AutoCommit::new();
        let resource = ResourceRef::new("actions", id);
        let catalogue = ActionCatalog::build_exact(
            &registry,
            &resource,
            crate::shell::chrome_state::SearchScope::All,
            None,
            &chrome.borrow(),
            &document,
            &automerge::ROOT,
            &AllVisible,
        );
        let action = catalogue
            .select(&resource)
            .expect("shell registry discovers exact editorial action");
        match action
            .invoke_json_disposition(
                &mut ui::widget::Edits::lend(&mut document),
                &automerge::ROOT,
                chrome,
                args.as_object().unwrap(),
            )
            .unwrap()
        {
            InvocationDisposition::Pending(ticket) => ticket,
            InvocationDisposition::Accepted(_) => {
                panic!("must not report success before host execution")
            }
        }
    }
    #[test]
    fn native_semantic_editorial_writes_only_private_groups_and_rejects_stale() {
        let (mut host, args) = fixture();
        let chrome = host.chrome.clone().unwrap();
        let before = chrome
            .borrow()
            .tree_mut()
            .room_docs
            .iter_mut()
            .map(|(id, doc)| (*id, doc.save()))
            .collect::<std::collections::HashMap<_, _>>();
        let ticket = invoke(&host, &args);
        assert!(ticket().unwrap().is_none());
        host.apply_pending_history_editorial();
        assert!(ticket().unwrap().is_some());
        assert!(
            host.room_activity
                .timeline()
                .named_ranges()
                .iter()
                .any(|r| r.label == "Semantic review")
        );
        let state = chrome.borrow();
        let mut tree = state.tree_mut();
        for (id, bytes) in before {
            assert_eq!(tree.room_docs.get_mut(&id).unwrap().save(), bytes);
        }
        let bytes = tree.doc.save();
        tree.doc = automerge::AutoCommit::load(&bytes).unwrap();
        let mut restored = crate::shell::room_activity::RoomActivityPane::new();
        restored.set_view(tilo_desktop::activity::ActivityView::Explore);
        restored.refresh(&mut *tree);
        assert!(
            restored
                .timeline()
                .named_ranges()
                .iter()
                .any(|r| r.label == "Semantic review")
        );
        drop(restored);
        drop(tree);
        drop(state);
        let ticket = invoke(&host, &args);
        host.apply_pending_history_editorial();
        assert_eq!(ticket().unwrap_err().code(), "invalid_arguments");
        assert_eq!(chrome.borrow().tree_mut().doc.save(), bytes);
    }
    #[test]
    fn inspection_and_revocation_use_the_current_local_pane() {
        let (mut host, mut args) = fixture();
        let chrome = host.chrome.clone().unwrap();
        let bytes = chrome.borrow().tree_mut().doc.save();
        args.as_object_mut().unwrap().remove("operation");
        args.as_object_mut().unwrap().remove("frontier");
        let ticket = invoke_action(&host, &args, INSPECT);
        host.apply_pending_history_editorial();
        let receipt = ticket().unwrap().unwrap();
        let tilo_actions::InvocationUpdate::Succeeded { output, .. } = receipt.update else {
            panic!("inspection must complete");
        };
        let value = output.value.unwrap();
        assert!(!value["frontier"].as_array().unwrap().is_empty());
        assert!(value["entries"].as_array().unwrap().len() >= 2);
        assert!(value.get("document").is_none());
        assert_eq!(chrome.borrow().tree_mut().doc.save(), bytes);
        args["frontier"] = value["frontier"].clone();
        args["operation"] = serde_json::json!({"kind":"name","start":value["entries"][0],"end":value["entries"][1],"label":"Revoked review"});
        let ticket = invoke(&host, &args);
        chrome
            .borrow()
            .tree_mut()
            .active_workspace_mut()
            .unwrap()
            .set_owner("another-owner");
        assert!(
            !crate::shell::search::providers::actions::current_shell_action_authorized(
                &chrome.borrow(),
                EDIT
            )
        );
        host.apply_pending_history_editorial();
        assert!(ticket().is_err());
        assert_eq!(chrome.borrow().tree_mut().doc.save(), bytes);
    }
    #[test]
    fn inspection_never_discloses_other_workspace_names_after_edit() {
        let (mut host, mut args) = fixture();
        let chrome = host.chrome.clone().unwrap();
        let private_anchors = {
            let state = chrome.borrow();
            let mut tree = state.tree_mut();
            let original = tree.desktop.active;
            tree.append_empty_workspace("Private room");
            tree.desktop.active = tree.desktop.workspaces.last().unwrap().id;
            for name in ["Private revision one", "Private revision two"] {
                tree.active_workspace_mut().unwrap().name = name.into();
                tree.seed_doc_from_cache();
                tree.commit_room_change();
            }
            host.room_activity.refresh(&mut *tree);
            let entries = host.room_activity.entries();
            let start = entries[0].id;
            let end = entries.last().unwrap().id;
            ui::history_groups::name_range(
                &mut tree.doc,
                &start,
                Some(&end),
                "Confidential other room",
            );
            tree.commit_room_change();
            tree.desktop.active = original;
            host.room_activity.refresh(&mut *tree);
            drop(tree);
            [encode_entry(start), encode_entry(end)]
        };
        host.refresh_room_activity();
        args.as_object_mut().unwrap().remove("operation");
        args.as_object_mut().unwrap().remove("frontier");
        for after_mutation in [false, true] {
            let ticket = invoke_action(&host, &args, INSPECT);
            host.apply_pending_history_editorial();
            let receipt = ticket().unwrap().unwrap();
            let tilo_actions::InvocationUpdate::Succeeded { output, .. } = receipt.update else {
                panic!("inspection must succeed");
            };
            let value = output.value.unwrap();
            let serialized = value.to_string();
            assert!(!serialized.contains("Confidential other room"));
            for anchor in &private_anchors {
                assert!(!serialized.contains(anchor));
            }
            if after_mutation {
                assert!(serialized.contains("Visible review"));
            } else {
                let mut edit = args.clone();
                edit["frontier"] = value["frontier"].clone();
                edit["operation"] = serde_json::json!({"kind":"name","start":value["entries"][0],"end":value["entries"][1],"label":"Visible review"});
                let ticket = invoke(&host, &edit);
                host.apply_pending_history_editorial();
                assert!(ticket().unwrap().is_some());
            }
        }
    }

    #[test]
    fn wrong_room_and_unknown_kind_never_write() {
        let (mut host, mut args) = fixture();
        let chrome = host.chrome.clone().unwrap();
        let bytes = chrome.borrow().tree_mut().doc.save();
        args["room"] = serde_json::json!("another-room");
        let ticket = invoke(&host, &args);
        host.apply_pending_history_editorial();
        assert!(ticket().is_err());
        assert_eq!(chrome.borrow().tree_mut().doc.save(), bytes);
        args["operation"]["kind"] = serde_json::json!("blanket_clear");
        assert!(ActionArgs::bind_json(&params(false), args.as_object().unwrap()).is_err());
    }
}

use crate::shell::search::action_api::{ActionArgs, ActionError, ActionParam, ArgError, ParamType};
use crate::shell::search::{ActionReceipt, InvocationDisposition, ResourceKind, SearchResult};
use automerge::transaction::Transactable;
use std::{cell::RefCell, rc::Rc};
use tilo_desktop::activity::{
    EditorialSession, HistoryEditorialOperation as Op, HistoryEditorialRequest, HistoryOrigin,
};

pub const EDIT: &str = "history.groups.edit";
pub const INSPECT: &str = "history.groups.inspect";
type Reply = Rc<RefCell<Option<Result<ActionReceipt, ActionError>>>>;
#[derive(Debug)]
pub struct Pending {
    pub request: HistoryEditorialRequest,
    pub inspect: bool,
    reply: Reply,
}
impl Pending {
    pub fn finish(&self, result: Result<ActionReceipt, ActionError>) {
        *self.reply.borrow_mut() = Some(result);
        crate::shell::search::invocation_wake::waker().wake_by_ref();
    }
}
fn invalid(reason: impl Into<String>) -> ActionError {
    ActionError::Arguments(ArgError::Constraint {
        name: "request".into(),
        reason: reason.into(),
    })
}
fn hex<const N: usize>(value: &str) -> Result<[u8; N], ActionError> {
    if value.len() != N.saturating_mul(2) || !value.is_ascii() {
        return Err(invalid("invalid exact history identifier"));
    }
    let mut bytes = [0; N];
    for (pair, byte) in value.as_bytes().chunks_exact(2).zip(bytes.iter_mut()) {
        let pair =
            std::str::from_utf8(pair).map_err(|_| invalid("invalid exact history identifier"))?;
        *byte = u8::from_str_radix(pair, 16)
            .map_err(|_| invalid("invalid exact history identifier"))?;
    }
    Ok(bytes)
}
fn encode_entry(id: ui::HistoryEntryId) -> String {
    use std::fmt::Write as _;
    id.0.iter().fold(String::with_capacity(32), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}
fn entry(value: &str) -> Result<ui::HistoryEntryId, ActionError> {
    Ok(ui::HistoryEntryId(hex(value)?))
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Session {
    record: Option<String>,
    start: String,
    end: String,
    label: String,
}
impl Session {
    fn convert(self) -> Result<EditorialSession, ActionError> {
        Ok(EditorialSession {
            record: self.record,
            start: entry(&self.start)?,
            end: entry(&self.end)?,
            label: self.label,
        })
    }
}
#[derive(serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    Name {
        record: Option<String>,
        start: String,
        end: String,
        label: String,
    },
    Clear {
        record: String,
        start: String,
        end: String,
    },
    Split {
        record: String,
        left_end: String,
        right_start: String,
        label: String,
        right_label: String,
    },
    Merge {
        records: Vec<String>,
        start: String,
        end: String,
        label: String,
    },
    MoveBoundary {
        left: Session,
        right: Session,
        left_end: String,
        right_start: String,
    },
}
impl Operation {
    fn convert(self) -> Result<Op, ActionError> {
        Ok(match self {
            Self::Name {
                record,
                start,
                end,
                label,
            } => {
                if label.trim().is_empty() {
                    return Err(invalid("use clear with an exact record to remove a name"));
                }
                Op::Name(ui::GroupNameRequest {
                    record_id: record,
                    start: entry(&start)?,
                    end: entry(&end)?,
                    label,
                })
            }
            Self::Clear { record, start, end } => Op::Name(ui::GroupNameRequest {
                record_id: Some(record),
                start: entry(&start)?,
                end: entry(&end)?,
                label: String::new(),
            }),
            Self::Split {
                record,
                left_end,
                right_start,
                label,
                right_label,
            } => Op::Split {
                record,
                left_end: entry(&left_end)?,
                right_start: entry(&right_start)?,
                label,
                right_label,
            },
            Self::Merge {
                records,
                start,
                end,
                label,
            } => Op::Merge {
                records,
                start: entry(&start)?,
                end: entry(&end)?,
                label,
            },
            Self::MoveBoundary {
                left,
                right,
                left_end,
                right_start,
            } => Op::MoveBoundary {
                left: left.convert()?,
                right: right.convert()?,
                left_end: entry(&left_end)?,
                right_start: entry(&right_start)?,
            },
        })
    }
}
fn params(inspect: bool) -> Vec<ActionParam> {
    use serde_json::json;
    let id = json!({"type":"string","pattern":"^[0-9a-fA-F]{32}$","minLength":32,"maxLength":32});
    let record = json!({"type":"string","minLength":1,"maxLength":128});
    let label = json!({"type":"string","minLength":1,"maxLength":ui::history_groups::MAX_LABEL_LEN,"pattern":"^[^\\u0000-\\u001f\\u007f]*$"});
    let session = json!({"type":"object","additionalProperties":false,"required":["start","end","label"],"properties":{"record":record,"start":id,"end":id,"label":label}});
    let mut result = vec![
        ActionParam::required("room", ParamType::Text, "Exact active room ID.").with_schema(json!({"minLength":1,"maxLength":128})),
        ActionParam::required("origin", ParamType::Text, "Exact owner: desktop_tree only; remote document owners are not admitted here.").with_choices(["desktop_tree"]),
        (if inspect { ActionParam::optional("frontier", ParamType::Array, "Optional exact reviewed heads; omit to inspect the current pane.") } else { ActionParam::required("frontier", ParamType::Array, "Exact current desktop heads, in order, returned by inspection.") }).with_schema(json!({"minItems":1,"maxItems":64,"uniqueItems":true,"items":{"type":"string","pattern":"^[0-9a-fA-F]{64}$","minLength":64,"maxLength":64}})),
    ];
    if !inspect {
        let variant =
            |kind: &str,
             required: Vec<&str>,
             mut properties: serde_json::Map<String, serde_json::Value>| {
                properties.insert("kind".into(), json!({"const":kind,"type":"string"}));
                let mut required = required;
                required.push("kind");
                json!({"type":"object","additionalProperties":false,"required":required,"properties":properties})
            };
        let props = |v: serde_json::Value| v.as_object().cloned().unwrap_or_default();
        result.push(ActionParam::required("operation", ParamType::Object, "Exact name, clear, split, merge or move_boundary request; inspect again after every edit.").with_schema(json!({"oneOf":[
            variant("name",vec!["start","end","label"],props(json!({"record":record,"start":id,"end":id,"label":label}))),
            variant("clear",vec!["record","start","end"],props(json!({"record":record,"start":id,"end":id}))),
            variant("split",vec!["record","left_end","right_start","label","right_label"],props(json!({"record":record,"left_end":id,"right_start":id,"label":label,"right_label":label}))),
            variant("merge",vec!["records","start","end","label"],props(json!({"records":{"type":"array","minItems":2,"maxItems":64,"uniqueItems":true,"items":record},"start":id,"end":id,"label":label}))),
            variant("move_boundary",vec!["left","right","left_end","right_start"],props(json!({"left":session,"right":session,"left_end":id,"right_start":id})))
        ]})));
    }
    result
}
fn begin(
    chrome: &crate::shell::chrome_state::Chrome,
    args: &ActionArgs,
    inspect: bool,
) -> Result<InvocationDisposition, ActionError> {
    let id = if inspect { INSPECT } else { EDIT };
    if !crate::shell::search::providers::actions::current_shell_action_authorized(
        &chrome.borrow(),
        id,
    ) {
        return Err(ActionError::Refused(
            "current workspace authority does not permit history organization".into(),
        ));
    }
    let values = args.to_json();
    let frontier = if let Some(values) = args.array("frontier") {
        values
            .iter()
            .map(|v| {
                Ok(automerge::ChangeHash(hex(v
                    .as_str()
                    .ok_or_else(|| invalid("invalid frontier"))?)?))
            })
            .collect::<Result<Vec<_>, ActionError>>()?
    } else if inspect {
        let state = chrome.borrow();
        let mut tree = state.tree_mut();
        if tree.doc.pending_ops() != 0 {
            return Err(invalid("uncommitted history is unavailable"));
        }
        tree.doc.get_heads()
    } else {
        return Err(invalid("missing exact frontier"));
    };
    if frontier.is_empty() || frontier.len() > 64 {
        return Err(invalid("supply 1 to 64 exact current heads"));
    }
    if args.text("origin") != Some("desktop_tree") {
        return Err(invalid("only desktop_tree is locally supported"));
    }
    let operation = if inspect {
        Op::Merge {
            records: vec![],
            start: ui::HistoryEntryId([0; 16]),
            end: ui::HistoryEntryId([0; 16]),
            label: String::new(),
        }
    } else {
        serde_json::from_value::<Operation>(
            values
                .get("operation")
                .cloned()
                .ok_or_else(|| invalid("missing operation"))?,
        )
        .map_err(|e| invalid(e.to_string()))?
        .convert()?
    };
    let reply = Rc::new(RefCell::new(None));
    let mut state = chrome.borrow_mut();
    if state.history_editorial_request.is_some() {
        return Err(ActionError::Refused(
            "another history request is pending".into(),
        ));
    }
    state.history_editorial_request = Some(Pending {
        request: HistoryEditorialRequest {
            room: args
                .text("room")
                .ok_or_else(|| invalid("missing room"))?
                .into(),
            origin: HistoryOrigin::DesktopTree,
            frontier,
            operation,
        },
        inspect,
        reply: reply.clone(),
    });
    crate::shell::search::invocation_wake::waker().wake_by_ref();
    Ok(InvocationDisposition::Pending(Rc::new(move || {
        reply.borrow().clone().transpose()
    })))
}

pub fn rows() -> Vec<SearchResult> {
    [false, true]
        .into_iter()
        .map(|inspect| {
            let id = if inspect { INSPECT } else { EDIT };
            let key = if inspect {
                "search-action-history-groups-inspect"
            } else {
                "search-action-history-groups-edit"
            };
            SearchResult {
                provider_id: "actions".into(),
                id: id.into(),
                kind: ResourceKind::Action,
                operation_kind: if inspect {
                    tilo_actions::OperationKind::View
                } else {
                    tilo_actions::OperationKind::Effect
                },
                title: crate::shell::i18n::t(key),
                subtitle: None,
                badge: None,
                score: 0,
                params: params(inspect),
                contract: Some(crate::shell::search::providers::actions::shell_action_contract(id)),
                on_open: Rc::new(|_, _, _, _| {
                    Err(invalid(
                        "supply exact room, origin, frontier and operation arguments",
                    ))
                }),
                on_invoke: None,
                on_begin_invoke: Some(Rc::new(move |_, _, chrome, args: &ActionArgs| {
                    begin(chrome, args, inspect)
                })),
            }
        })
        .collect()
}

impl crate::shell::overlay_host::ShellOverlayHost {
    pub(super) fn apply_pending_history_editorial(&mut self) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let Some(pending) = chrome.borrow_mut().history_editorial_request.take() else {
            return;
        };
        let id = if pending.inspect { INSPECT } else { EDIT };
        let state = chrome.borrow();
        if !crate::shell::search::providers::actions::current_shell_action_authorized(&state, id) {
            pending.finish(Err(ActionError::Refused(
                "history authority was revoked".into(),
            )));
            return;
        }
        let mut tree = state.tree_mut();
        let room_matches = tree
            .desktop
            .active_workspace()
            .is_some_and(|w| w.room.to_string() == pending.request.room);
        if !room_matches
            || tree.doc.pending_ops() != 0
            || tree.doc.get_heads() != pending.request.frontier
            || !self
                .room_activity
                .history_editorial_matches(&*tree, &pending.request)
        {
            pending.finish(Err(invalid(
                "room or frontier changed; inspect the current Activity pane before retrying",
            )));
            return;
        }
        // Do not open, rebind or refresh the pane here: that would silently
        // replace the context the caller reviewed with a new capability.
        if pending.inspect {
            let records = state.records().clone();
            let local = self.room_records.borrow();
            self.room_activity
                .refresh_records_with_local(&mut *tree, &records, &*local);
            let timeline = self.room_activity.timeline();
            // Document-wide marker storage is private, even when bounded.
            // Only expose names admitted into a currently displayed band.
            let mut visible = Vec::new();
            for group in timeline.groups() {
                group.walk(&mut visible);
            }
            visible.retain(|g| {
                timeline
                    .entries()
                    .get(g.start..g.end)
                    .unwrap_or_default()
                    .iter()
                    .any(|e| timeline.filter().matches(e))
            });
            let groups = visible.iter().filter_map(|g| {
                let record = g.record_id.as_deref().filter(|id| id.len() <= 128)?;
                Some(serde_json::json!({"record":record,"start":encode_entry(g.first_entry),"end":encode_entry(g.last_entry),"label":g.title(timeline.entries()).chars().take(ui::history_groups::MAX_LABEL_LEN).collect::<String>()}))
            }).take(64).collect::<Vec<_>>();
            let display_groups = visible.iter()
                .take(64)
                .map(|g| serde_json::json!({
                    "start": encode_entry(g.first_entry),
                    "end": encode_entry(g.last_entry),
                    "record": g.record_id.as_deref().filter(|id| id.len() <= 128),
                    "label": g.title(timeline.entries()).chars().take(ui::history_groups::MAX_LABEL_LEN).collect::<String>()
                }))
                .collect::<Vec<_>>();
            let entries = self
                .room_activity
                .entries()
                .iter()
                .take(256)
                .map(|e| encode_entry(e.id))
                .collect::<Vec<_>>();
            pending.finish(Ok(ActionReceipt::completed_with(serde_json::json!({"room":pending.request.room,"origin":"desktop_tree","frontier":pending.request.frontier.iter().map(ToString::to_string).collect::<Vec<_>>(),"groups":groups,"display_groups":display_groups,"entries":entries}))));
        } else if self
            .room_activity
            .apply_history_editorial(&mut *tree, &pending.request)
        {
            // The mutation's tree refresh is not a source-admitted room
            // projection. Restore aggregation before a cached inspection can
            // observe it, rather than waiting for the next paint refresh.
            let records = state.records().clone();
            let local = self.room_records.borrow();
            self.room_activity
                .refresh_records_with_local(&mut *tree, &records, &*local);
            drop(tree);
            pending.finish(Ok(ActionReceipt::completed()));
        } else {
            pending.finish(Err(invalid("unknown group, stale pane/frontier, invalid label or boundary, or no live local writer; inspect again before retrying")));
        }
    }
}
