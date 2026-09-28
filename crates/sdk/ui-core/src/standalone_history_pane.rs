//! Shared, read-only history presentation for standalone app hosts.
//!
//! The host owns document admission and history reads. This retained tree owns
//! only presentation and queues intents through the standard widget actions.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::history::{HistoryEntry, HistoryEntryId};
use crate::widget::{ActivityTimeline, Button, ModalDialog, Row, TaffyTreeExt, Text, Widget};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryPaneIntent {
    Select(HistoryEntryId),
    ClearSelection,
    Compare(Vec<HistoryEntryId>),
    Close,
    ReviewRestore,
    ConfirmRestore,
    CancelRestore,
}

enum QueuedIntent {
    Activate(HistoryEntryId),
    Close,
    ReviewRestore,
    ConfirmRestore,
    CancelRestore,
}

/// The ordinary retained widget tree used by the standalone history host.
pub struct StandaloneHistoryPane {
    header: Row,
    timeline: ActivityTimeline,
    summary: Text,
    summary_value: Rc<RefCell<String>>,
    restore: Button,
    review: Option<ModalDialog<Text>>,
    intents: Rc<RefCell<VecDeque<QueuedIntent>>>,
    observed_selection: Option<Vec<HistoryEntryId>>,
    child_nodes: Vec<NodeId>,
    rect: Rect,
}

impl Default for StandaloneHistoryPane {
    fn default() -> Self {
        Self::new()
    }
}

impl StandaloneHistoryPane {
    pub fn new() -> Self {
        let intents = Rc::new(RefCell::new(VecDeque::new()));
        let close_intents = Rc::clone(&intents);
        let activate_intents = Rc::clone(&intents);
        let restore_intents = Rc::clone(&intents);
        let summary_value = Rc::new(RefCell::new("standalone-history-select-hint".to_owned()));
        let summary_source = Rc::clone(&summary_value);
        Self {
            header: Row::new()
                .push(Box::new(
                    Text::new("standalone-history-title").with_heading_level(2),
                ))
                .push(Box::new(
                    Button::new("standalone-history-close", "standalone-history-close", None)
                        .on_view(move || close_intents.borrow_mut().push_back(QueuedIntent::Close)),
                )),
            timeline: ActivityTimeline::new().on_activate_entry(move |id| {
                activate_intents
                    .borrow_mut()
                    .push_back(QueuedIntent::Activate(*id));
            }),
            summary: Text::dynamic(move || summary_source.borrow().clone())
                .with_id("standalone-history-summary")
                .with_live(crate::a11y::Politeness::Polite),
            summary_value,
            restore: Button::new(
                "standalone-history-restore",
                "standalone-history-restore",
                None,
            )
            .on_view(move || {
                restore_intents
                    .borrow_mut()
                    .push_back(QueuedIntent::ReviewRestore);
            })
            .with_enabled(false),
            review: None,
            intents,
            observed_selection: None,
            child_nodes: Vec::new(),
            rect: Rect::ZERO,
        }
    }

    pub fn set_entries(&mut self, entries: &[HistoryEntry]) {
        self.timeline.set_entries(entries.to_vec());
    }

    /// Project a controller selection without feeding it back as a user intent.
    /// Returns false for unknown, duplicate or noncontiguous IDs: the timeline
    /// has a range selection, so these must not silently select extra changes.
    /// The host may still show an exact noncontiguous comparison in its detail.
    pub fn set_selected_entries(&mut self, entries: &[HistoryEntryId]) -> bool {
        let mut positions = Vec::with_capacity(entries.len());
        for id in entries {
            let Some(position) = self
                .timeline
                .entries()
                .iter()
                .position(|entry| entry.id == *id)
            else {
                self.set_selection_without_intent(None);
                return false;
            };
            positions.push(position);
        }
        positions.sort_unstable();
        if positions
            .windows(2)
            .any(|pair| !matches!(pair, [left, right] if *right == left.saturating_add(1)))
        {
            self.set_selection_without_intent(None);
            return false;
        }
        let range = positions.first().copied().zip(positions.last().copied());
        self.set_selection_without_intent(range);
        true
    }

    fn set_selection_without_intent(&mut self, selection: Option<(usize, usize)>) {
        self.timeline.set_selection(selection);
        self.observed_selection = self.selected_ids();
        self.intents
            .borrow_mut()
            .retain(|intent| !matches!(intent, QueuedIntent::Activate(_)));
    }

    /// A host-produced, grounded comparison or selection detail. No document
    /// handle is retained by this presentation tree.
    pub fn set_comparison_summary(&mut self, summary: Option<&str>) {
        summary
            .unwrap_or("standalone-history-select-hint")
            .clone_into(&mut self.summary_value.borrow_mut());
    }

    pub fn clear(&mut self) {
        self.timeline.set_entries(Vec::new());
        self.timeline.set_selection(None);
        self.timeline.clear_filters();
        self.timeline.clear_row_focus();
        self.intents.borrow_mut().clear();
        self.observed_selection = None;
        self.review = None;
        self.restore.set_enabled(false);
        self.set_comparison_summary(None);
    }

    /// The host decides whether the current selection can be restored; the
    /// pane never infers write authority from its own state.
    pub fn set_restore_enabled(&mut self, enabled: bool) {
        self.restore.set_enabled(enabled);
    }

    /// Ask the person to confirm returning to the version named `label`.
    pub fn open_review(&mut self, label: &str) {
        let confirm = Rc::clone(&self.intents);
        let cancel = Rc::clone(&self.intents);
        let dismiss = Rc::clone(&self.intents);
        self.review = Some(
            ModalDialog::new(
                crate::i18n::I18nManager::lookup_plain("standalone-history-restore-title"),
                Text::new("standalone-history-restore-body")
                    .with_arg("version", label)
                    .with_id("standalone-history-restore-body"),
            )
            .with_test_id("standalone-history-restore-review")
            .open(true)
            .primary_action("standalone-history-restore-confirm", move || {
                confirm.borrow_mut().push_back(QueuedIntent::ConfirmRestore);
            })
            .secondary_action("standalone-history-restore-cancel", move || {
                cancel.borrow_mut().push_back(QueuedIntent::CancelRestore);
            })
            .on_dismiss(move || {
                dismiss.borrow_mut().push_back(QueuedIntent::CancelRestore);
            }),
        );
    }

    pub fn close_review(&mut self) {
        self.review = None;
    }

    pub fn review_is_open(&self) -> bool {
        self.review.is_some()
    }

    #[cfg(test)]
    pub(crate) fn review_dialog(&self) -> Option<&ModalDialog<Text>> {
        self.review.as_ref()
    }

    fn selected_ids(&self) -> Option<Vec<HistoryEntryId>> {
        let (first, last) = self.timeline.selection()?;
        Some(
            self.timeline
                .entries()
                .get(first..=last)?
                .iter()
                .map(|entry| entry.id)
                .collect(),
        )
    }

    /// Drain after input and semantic action dispatch, including pointer range
    /// selection (which intentionally does not activate timeline rows).
    pub fn take_intent(&mut self) -> Option<HistoryPaneIntent> {
        let queued = self.intents.borrow_mut().pop_front();
        match queued {
            Some(QueuedIntent::Close) => return Some(HistoryPaneIntent::Close),
            Some(QueuedIntent::ReviewRestore) => return Some(HistoryPaneIntent::ReviewRestore),
            Some(QueuedIntent::ConfirmRestore) => return Some(HistoryPaneIntent::ConfirmRestore),
            Some(QueuedIntent::CancelRestore) => return Some(HistoryPaneIntent::CancelRestore),
            Some(QueuedIntent::Activate(id)) => {
                self.observed_selection = self.selected_ids();
                return Some(HistoryPaneIntent::Select(id));
            }
            None => {}
        }
        let selection = self.selected_ids();
        if selection == self.observed_selection {
            return None;
        }
        let intent = match selection.as_deref() {
            Some([id]) => HistoryPaneIntent::Select(*id),
            Some(ids) if !ids.is_empty() => HistoryPaneIntent::Compare(ids.to_vec()),
            _ => HistoryPaneIntent::ClearSelection,
        };
        self.observed_selection = selection;
        Some(intent)
    }
}

impl Widget for StandaloneHistoryPane {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        if let Some(dialog) = &mut self.review {
            self.child_nodes = vec![dialog.build_layout(taffy, theme, node_map)];
        } else {
            let header = self.header.build_layout(taffy, theme, node_map);
            let timeline = self.timeline.build_layout(taffy, theme, node_map);
            let summary = self.summary.build_layout(taffy, theme, node_map);
            let restore = self.restore.build_layout(taffy, theme, node_map);
            if let Ok(style) = taffy.style(timeline) {
                let mut style = style.clone();
                style.size.height = Dimension::auto();
                style.min_size.height = Dimension::length(0.0);
                style.flex_grow = 1.0;
                style.flex_basis = Dimension::length(0.0);
                let _ = taffy.set_style(timeline, style);
            }
            if let Ok(style) = taffy.style(restore) {
                let mut style = style.clone();
                style.align_self = Some(taffy::AlignSelf::FlexStart);
                let _ = taffy.set_style(restore, style);
            }
            self.child_nodes = vec![header, timeline, summary, restore];
        }
        let node = taffy.parent_node(
            Style {
                flex_direction: taffy::FlexDirection::Column,
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                gap: Size {
                    width: taffy::LengthPercentage::length(theme.gap_standard),
                    height: taffy::LengthPercentage::length(theme.gap_standard),
                },
                ..Default::default()
            },
            &self.child_nodes,
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        let origin = origin + (f64::from(layout.location.x), f64::from(layout.location.y));
        self.rect = Rect::from_origin_size(
            origin,
            (f64::from(layout.size.width), f64::from(layout.size.height)),
        );
        let nodes = self.child_nodes.clone();
        for (child, node) in self.children_mut().into_iter().zip(nodes) {
            child.apply_layout(taffy, node, origin);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        for child in self.children_mut() {
            child.draw(scene, transform, text, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        let nodes = self.child_nodes.clone();
        for (child, node) in self.children_mut().into_iter().zip(nodes) {
            child.handle_event(event, &mut ctx.reborrow(ctx.path, node));
            if *ctx.handled {
                break;
            }
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        if let Some(dialog) = &self.review {
            return vec![dialog];
        }
        vec![&self.header, &self.timeline, &self.summary, &self.restore]
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if let Some(dialog) = &mut self.review {
            return vec![dialog];
        }
        vec![
            &mut self.header,
            &mut self.timeline,
            &mut self.summary,
            &mut self.restore,
        ]
    }
    fn name(&self) -> &'static str {
        "StandaloneHistoryPane"
    }
    fn test_id(&self) -> Option<&str> {
        Some("standalone-history")
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a11y::{Action, Role};
    use crate::widget::history::HistoryAuthor;
    use automerge::AutoCommit;
    use automerge::transaction::Transactable;

    fn entry(id: u8) -> HistoryEntry {
        HistoryEntry {
            id: HistoryEntryId([id; 16]),
            author: HistoryAuthor::new("Alice", 120.0, "alice"),
            op_class: collab_types::OpClass::Splice,
            artifact_kind: collab_types::ArtifactKind::Note,
            timestamp: crate::time::SystemTime::UNIX_EPOCH,
            summary: format!("Change {id}"),
            text_delta: Some((1, 0)),
            diff_payload: None,
            redaction: None,
            context: None,
        }
    }

    #[test]
    fn history_pane_selection_is_exact_and_never_commits_or_echoes() {
        // Case `semantic_activation_selects_without_committing_pending_edits`.
        {
            let mut pane = StandaloneHistoryPane::new();
            let entry = entry(1);
            pane.set_entries(std::slice::from_ref(&entry));
            let semantics = pane.timeline.accessibility().expect("history semantics");
            let target = semantics
                .children
                .iter()
                .find(|node| node.role == Role::ListItem)
                .and_then(|node| node.action_target.as_deref())
                .expect("published selectable history entry");
            let mut doc = AutoCommit::new();
            doc.put(automerge::ROOT, "draft", "typing")
                .expect("pending edit");
            let pending = doc.pending_ops();
            assert!(pane.timeline.perform_semantic_action(
                target,
                &Action::Activate,
                &mut doc,
                &automerge::ROOT
            ));
            assert_eq!(
                pane.take_intent(),
                Some(HistoryPaneIntent::Select(entry.id))
            );
            assert_eq!(
                pane.take_intent(),
                None,
                "one activation must not duplicate pointer selection"
            );
            assert_eq!(doc.pending_ops(), pending);
        }

        // Case `removed_selection_clears_detail_instead_of_selecting_replacement_row`.
        {
            let mut pane = StandaloneHistoryPane::new();
            pane.set_entries(&[entry(1), entry(2)]);
            pane.timeline.set_selection(Some((0, 0)));
            assert_eq!(
                pane.take_intent(),
                Some(HistoryPaneIntent::Select(entry(1).id))
            );
            pane.set_entries(&[entry(2)]);
            assert_eq!(pane.take_intent(), Some(HistoryPaneIntent::ClearSelection));
        }

        // Case `controller_selection_is_exact_and_never_echoes_as_input`.
        {
            let mut pane = StandaloneHistoryPane::new();
            pane.set_entries(&[entry(1), entry(2), entry(3)]);
            assert!(pane.set_selected_entries(&[entry(2).id]));
            assert_eq!(pane.selected_ids(), Some(vec![entry(2).id]));
            assert_eq!(pane.take_intent(), None);
            assert!(pane.set_selected_entries(&[entry(1).id, entry(2).id, entry(3).id]));
            assert_eq!(
                pane.selected_ids(),
                Some(vec![entry(1).id, entry(2).id, entry(3).id])
            );
            assert_eq!(pane.take_intent(), None);
            assert!(pane.set_selected_entries(&[]));
            assert_eq!(pane.selected_ids(), None);
            assert_eq!(pane.take_intent(), None);
            for invalid in [
                vec![entry(1).id, entry(3).id],
                vec![entry(9).id],
                vec![entry(2).id, entry(2).id],
            ] {
                assert!(!pane.set_selected_entries(&invalid));
                assert_eq!(pane.selected_ids(), None);
                assert_eq!(pane.take_intent(), None);
            }
        }
    }

    #[test]
    fn history_pane_range_selection_compares_the_exact_before_state() {
        // Case `complete_selected_range_materializes_the_state_before_all_three_changes`.
        {
            use crate::history_source::{HistorySource, compare_entries, entry_id_from_hash};
            use automerge::ReadDoc;

            let mut doc = AutoCommit::new();
            doc.put(automerge::ROOT, "value", "before").unwrap();
            doc.commit();
            let mut ids = Vec::new();
            for value in ["first", "middle", "last"] {
                doc.put(automerge::ROOT, "value", value).unwrap();
                ids.push(entry_id_from_hash(&doc.commit().unwrap().to_string()));
            }
            let entries = HistorySource::new().entries(&mut doc);
            let selected_entries: Vec<_> = ids
                .iter()
                .map(|id| {
                    entries
                        .iter()
                        .find(|entry| entry.id == *id)
                        .unwrap()
                        .clone()
                })
                .collect();
            let mut pane = StandaloneHistoryPane::new();
            pane.set_entries(&selected_entries);
            pane.timeline.set_selection(Some((0, 2)));
            let Some(HistoryPaneIntent::Compare(selected)) = pane.take_intent() else {
                panic!("range must request a comparison");
            };
            assert_eq!(selected, ids);
            let comparison = compare_entries(&mut doc, &selected).unwrap();
            let before = AutoCommit::load(&comparison.before_document).unwrap();
            let after = AutoCommit::load(&comparison.after_document).unwrap();
            assert_eq!(
                before
                    .get(automerge::ROOT, "value")
                    .unwrap()
                    .unwrap()
                    .0
                    .as_str(),
                Some("before")
            );
            assert_eq!(
                after
                    .get(automerge::ROOT, "value")
                    .unwrap()
                    .unwrap()
                    .0
                    .as_str(),
                Some("last")
            );
            // Omitting the interior change contaminates the before frontier with
            // the last change's dependency: this is why endpoints are insufficient.
            let endpoints = compare_entries(&mut doc, &[ids[0], ids[2]]).unwrap();
            let endpoint_before = AutoCommit::load(&endpoints.before_document).unwrap();
            assert_eq!(
                endpoint_before
                    .get(automerge::ROOT, "value")
                    .unwrap()
                    .unwrap()
                    .0
                    .as_str(),
                Some("middle")
            );
        }

        // Case `changed_interior_selection_emits_a_fresh_comparison`.
        {
            let mut pane = StandaloneHistoryPane::new();
            pane.set_entries(&[entry(1), entry(2), entry(3)]);
            pane.timeline.set_selection(Some((0, 2)));
            assert_eq!(
                pane.take_intent(),
                Some(HistoryPaneIntent::Compare(vec![
                    entry(1).id,
                    entry(2).id,
                    entry(3).id
                ]))
            );
            pane.set_entries(&[entry(1), entry(4), entry(3)]);
            pane.timeline.set_selection(Some((0, 2)));
            assert_eq!(
                pane.take_intent(),
                Some(HistoryPaneIntent::Compare(vec![
                    entry(1).id,
                    entry(4).id,
                    entry(3).id
                ]))
            );
            assert_eq!(pane.take_intent(), None);
        }

        // Case `range_selection_compares_stable_ids_and_clear_discards_queued_activation`.
        {
            let mut pane = StandaloneHistoryPane::new();
            pane.set_entries(&[entry(1), entry(2)]);
            pane.timeline.set_selection(Some((0, 1)));
            assert_eq!(
                pane.take_intent(),
                Some(HistoryPaneIntent::Compare(vec![entry(1).id, entry(2).id]))
            );
            pane.timeline.set_all_expanded(true);
            let row = (0..pane.timeline.row_count())
                .find(|row| pane.timeline.row_entry(*row).is_some())
                .expect("entry row");
            assert!(pane.timeline.activate_row(row));
            pane.clear();
            assert_eq!(pane.take_intent(), None);
            assert!(pane.timeline.entries().is_empty());
        }
    }
}
