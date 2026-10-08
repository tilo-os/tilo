//! Selection and dismissal share the palette's existing semantic action door.
use crate::shell::i18n::t;
use crate::shell::portal_dialog::PortalDialogQueue;
use crate::shell::search::action_api::ActionError;
use crate::shell::search::{
    ResourceKind, SearchBudget, SearchContext, SearchCursor, SearchPage, SearchProvider,
    SearchResult,
};

pub(crate) const PROVIDER_ID: &str = "resource-selections";

pub(crate) struct ResourceSelectionsProvider {
    pub(crate) queue: PortalDialogQueue,
    pub(crate) ranker: ui::search::ListIndex,
}

impl ResourceSelectionsProvider {
    fn rank(&self, needle: &str, rows: Vec<SearchResult>) -> Vec<SearchResult> {
        let fields: Vec<_> = rows
            .iter()
            .map(|row| {
                vec![
                    (row.title.clone(), ui::search::FieldWeight::TITLE),
                    (
                        row.subtitle.clone().unwrap_or_default(),
                        ui::search::FieldWeight::SUBTITLE,
                    ),
                ]
            })
            .collect();
        crate::shell::search::rank_rows(&self.ranker, needle, rows, &fields)
    }
}

impl SearchProvider for ResourceSelectionsProvider {
    fn id(&self) -> &str {
        PROVIDER_ID
    }
    fn name(&self) -> &'static str {
        "Resource selections"
    }
    fn display_name(&self) -> String {
        t("resource-selection-title")
    }
    fn resource_kinds(&self) -> &[ResourceKind] {
        &[ResourceKind::Action]
    }
    fn query(&self, context: SearchContext<'_>) -> Vec<SearchResult> {
        let Some(room) = context
            .chrome
            .tree()
            .active_workspace()
            .map(|workspace| workspace.room.to_string())
        else {
            return Vec::new();
        };
        let Some(view) = self.queue.resource_selection(&room) else {
            return Vec::new();
        };
        let mut rows: Vec<_> = view
            .request
            .candidates
            .iter()
            .map(|candidate| {
                let token = candidate.token;
                let queue = self.queue.clone();
                let handle = view.handle.clone();
                let room = room.clone();
                let title = candidate.selected.row.resource.label.clone();
                let detail = candidate
                    .selected
                    .row
                    .action
                    .as_ref()
                    .map_or_else(String::new, |action| action.action_label.clone());
                let score = crate::shell::search::remote_score(context.needle, &title, &detail);
                let mut row = SearchResult::nullary_fallible(
                    ResourceKind::Action,
                    PROVIDER_ID,
                    format!("select:{}:{token}", view.handle),
                    title,
                    Some(detail),
                    score,
                    move |_, _, chrome| {
                        let current_room = chrome
                            .borrow()
                            .tree()
                            .active_workspace()
                            .map(|workspace| workspace.room.to_string());
                        if current_room.as_deref() != Some(room.as_str()) {
                            return Err(ActionError::Refused(t("resource-selection-retired")));
                        }
                        queue
                            .answer_resource_selection(&room, &handle, Some(token))
                            .map_err(ActionError::Refused)
                    },
                );
                row.operation_kind = tilo_actions::OperationKind::Effect;
                row.badge = view
                    .request
                    .truncated
                    .then(|| t("resource-selection-truncated-short"));
                row
            })
            .collect();
        let queue = self.queue.clone();
        let handle = view.handle;
        let dismiss_room = room;
        let mut dismiss = SearchResult::nullary_fallible(
            ResourceKind::Action,
            PROVIDER_ID,
            format!("dismiss:{handle}"),
            t("resource-selection-dismiss"),
            view.request
                .truncated
                .then(|| t("resource-selection-truncated")),
            0,
            move |_, _, chrome| {
                if chrome
                    .borrow()
                    .tree()
                    .active_workspace()
                    .is_none_or(|workspace| workspace.room.to_string() != dismiss_room)
                {
                    return Err(ActionError::Refused(t("resource-selection-retired")));
                }
                queue
                    .answer_resource_selection(&dismiss_room, &handle, None)
                    .map_err(ActionError::Refused)
            },
        );
        dismiss.operation_kind = tilo_actions::OperationKind::Effect;
        rows.push(dismiss);
        // Exact semantic discovery supplies the resource id as its needle.
        // Match an existing row verbatim before human label ranking.
        if let Some(index) = rows.iter().position(|row| row.id == context.needle) {
            return vec![rows.swap_remove(index)];
        }
        self.rank(context.needle, rows)
    }
    fn search_revision(&self, context: &SearchContext<'_>) -> String {
        context
            .chrome
            .tree()
            .active_workspace()
            .and_then(|workspace| self.queue.resource_selection(&workspace.room.to_string()))
            .map_or_else(|| "none".into(), |view| view.handle)
    }
    fn query_page(
        &self,
        context: SearchContext<'_>,
        cursor: Option<&SearchCursor>,
        budget: SearchBudget,
    ) -> SearchPage {
        let revision = self.search_revision(&context);
        if cursor.is_some_and(|cursor| cursor.revision != revision) {
            return SearchPage::stale(revision);
        }
        let Some(start) = cursor.map_or(Some(0), |cursor| cursor.token.parse::<usize>().ok())
        else {
            return SearchPage::stale(revision);
        };
        let rows = self.query(context);
        if self.search_revision(&context) != revision || start > rows.len() {
            return SearchPage::stale(revision);
        }
        let mut results = Vec::new();
        let mut bytes = 0usize;
        let mut offset = start;
        for row in rows.iter().skip(start).take(budget.max_work) {
            let row_bytes = row
                .title
                .len()
                .saturating_add(row.id.len())
                .saturating_add(row.subtitle.as_ref().map_or(0, String::len));
            if results.len() >= budget.max_results
                || bytes.saturating_add(row_bytes) > budget.max_bytes
            {
                break;
            }
            bytes = bytes.saturating_add(row_bytes);
            offset = offset.saturating_add(1);
            results.push(row.clone());
        }
        let exhausted = offset == rows.len();
        SearchPage {
            revision: revision.clone(),
            results,
            next_cursor: (!exhausted).then(|| SearchCursor {
                revision,
                token: offset.to_string(),
            }),
            exhausted,
            stale_cursor: false,
            budget_exhausted: !exhausted,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::shell::chrome_state::{Chrome, ChromeState};
    use crate::shell::search::action_api::{ActionCatalog, AllVisible, ResourceRef};
    use crate::shell::search::{SearchRegistry, SearchScope};
    use crate::shell::state::WorkspaceTree;
    use automerge::{AutoCommit, ROOT};
    use std::rc::Rc;
    use tilo_actions::remote::{
        CatalogueProvider, CatalogueResource, CatalogueRow, ResourceSelectionCandidate,
        ResourceSelectionPromptRequest, SelectedResource,
    };

    fn ready_answer(
        answer: &mut tilo_actions::remote::Pending<Option<uuid::Uuid>>,
    ) -> Option<uuid::Uuid> {
        match answer.poll_ready() {
            tilo_actions::remote::PendingPoll::Ready(answer) => answer,
            _ => panic!("actual deferred host reply must be ready"),
        }
    }

    fn assert_waiting(answer: &mut tilo_actions::remote::Pending<Option<uuid::Uuid>>) {
        assert!(matches!(
            answer.poll_ready(),
            tilo_actions::remote::PendingPoll::Waiting
        ));
    }

    pub(crate) fn prompt(room: &str) -> ResourceSelectionPromptRequest {
        let provider = CatalogueProvider {
            provider: "opaque-host-issued-test-token".into(),
            app_id: "so.tilo.code-editor".into(),
            room: room.into(),
            record_id: "original-file-record".into(),
            browsable: true,
        };
        let resource = CatalogueResource {
            id: "file:original".into(),
            kind: "code_editor.file".into(),
            label: "accelerator.rs".into(),
            presentation: None,
        };
        let action = tilo_actions::ActionDescriptor {
            address: tilo_actions::ActionAddress::new(&resource.id, "read-text-snapshot"),
            semantic_identity: tilo_actions::ResourceSemanticIdentity::new(
                "code_editor.file",
                "original-file-record",
            ),
            resource_kind: resource.kind.clone(),
            resource_label: resource.label.clone(),
            action_label: "Read text snapshot".into(),
            description: None,
            locus: tilo_actions::ExecutionLocus::ClientLocal,
            kind: tilo_actions::OperationKind::Effect,
            contract: tilo_actions::ActionContract::default(),
            input: tilo_actions::InputSpec::None,
            params: Vec::new(),
            view: None,
        };
        ResourceSelectionPromptRequest {
            room: room.into(),
            source_app_id: "so.tilo.chip-design".into(),
            source_record_id: "chip-project".into(),
            truncated: false,
            candidates: vec![ResourceSelectionCandidate {
                token: uuid::Uuid::new_v4(),
                selected: SelectedResource {
                    provider: provider.clone(),
                    row: CatalogueRow {
                        provider: provider.provider,
                        app_id: provider.app_id,
                        parent_resource: String::new(),
                        resource,
                        action: Some(action),
                        has_children: false,
                        visibility: None,
                    },
                    revision: "original-publication-revision".into(),
                },
            }],
        }
    }

    fn actions(queue: &PortalDialogQueue, chrome: &Chrome, id: &str) -> ActionCatalog {
        let mut registry = SearchRegistry::new();
        registry.register(Rc::new(ResourceSelectionsProvider {
            queue: queue.clone(),
            ranker: ui::search::ListIndex::default(),
        }));
        ActionCatalog::build_exact(
            &registry,
            &ResourceRef::new(PROVIDER_ID, id),
            SearchScope::All,
            None,
            &chrome.borrow(),
            &AutoCommit::new(),
            &ROOT,
            &AllVisible,
        )
    }

    #[test]
    fn resource_selection_semantic_select_and_dismiss_answer_actual_waiter_once() {
        for choose in [true, false] {
            let chrome = ChromeState::new(WorkspaceTree::bootstrap_default());
            let room = chrome
                .borrow()
                .tree()
                .active_workspace()
                .expect("workspace")
                .room
                .to_string();
            let queue = PortalDialogQueue::default();
            let request = prompt(&room);
            let token = request
                .candidates
                .first()
                .expect("original candidate")
                .token;
            let mut receive = queue
                .enqueue_resource_selection([7; 32], 9, request)
                .expect("pending request");
            let view = queue.resource_selection(&room).expect("same host request");
            let id = if choose {
                format!("select:{}:{token}", view.handle)
            } else {
                format!("dismiss:{}", view.handle)
            };
            let catalog = actions(&queue, &chrome, &id);
            let selected = catalog
                .select(&ResourceRef::new(PROVIDER_ID, &id))
                .expect("semantic action");
            let mut doc = AutoCommit::new();
            selected
                .invoke_nullary(&mut ui::widget::Edits::lend(&mut doc), &ROOT, &chrome)
                .expect("same palette operation");
            assert_eq!(ready_answer(&mut receive), choose.then_some(token));
            assert!(queue.resource_selection(&room).is_none());
            assert!(
                selected
                    .invoke_nullary(&mut ui::widget::Edits::lend(&mut doc), &ROOT, &chrome)
                    .is_err()
            );
        }
    }

    #[test]
    fn resource_selection_foreign_device_scope_and_replacement_cannot_answer_retained_request() {
        let queue = PortalDialogQueue::default();
        let room = uuid::Uuid::new_v4().to_string();
        let request = prompt(&room);
        let token = request
            .candidates
            .first()
            .expect("original candidate")
            .token;
        let mut receive = queue
            .enqueue_resource_selection([7; 32], 9, request.clone())
            .expect("original request");
        let original = queue.resource_selection(&room).expect("original view");
        assert!(
            queue
                .answer_resource_selection("another-room", &original.handle, Some(token))
                .is_err()
        );
        assert!(!queue.withdraw_resource_selection(&room, [8; 32], 9));
        assert!(!queue.withdraw_resource_selection("another-room", [7; 32], 9));
        assert_waiting(&mut receive);
        queue
            .answer_resource_selection(&room, &original.handle, None)
            .expect("dismiss original");
        assert_eq!(ready_answer(&mut receive), None);
        let mut replacement = queue
            .enqueue_resource_selection([7; 32], 9, request)
            .expect("replacement request");
        assert!(
            queue
                .answer_resource_selection(&room, &original.handle, None)
                .is_err()
        );
        assert_waiting(&mut replacement);
        let current = queue.resource_selection(&room).expect("replacement view");
        assert_ne!(current.handle, original.handle);
        assert!(
            queue
                .answer_resource_selection(&room, &current.handle, Some(uuid::Uuid::new_v4()))
                .is_err()
        );
        queue
            .answer_resource_selection(&room, &current.handle, Some(token))
            .expect("replacement's own choice");
        assert_eq!(ready_answer(&mut replacement), Some(token));
    }

    #[test]
    fn resource_selection_requester_disconnect_retires_semantic_rows() {
        let chrome = ChromeState::new(WorkspaceTree::bootstrap_default());
        let room = chrome
            .borrow()
            .tree()
            .active_workspace()
            .expect("workspace")
            .room
            .to_string();
        let queue = PortalDialogQueue::default();
        let request = prompt(&room);
        let token = request
            .candidates
            .first()
            .expect("original candidate")
            .token;
        let receive = queue
            .enqueue_resource_selection([7; 32], 9, request)
            .expect("request");
        let original = queue.resource_selection(&room).expect("original view");
        let id = format!("select:{}:{token}", original.handle);
        assert!(
            actions(&queue, &chrome, &id)
                .select(&ResourceRef::new(PROVIDER_ID, &id))
                .is_some()
        );
        drop(receive);
        assert!(
            actions(&queue, &chrome, &id)
                .select(&ResourceRef::new(PROVIDER_ID, &id))
                .is_none()
        );
        assert!(queue.resource_selection(&room).is_none());
    }
}
