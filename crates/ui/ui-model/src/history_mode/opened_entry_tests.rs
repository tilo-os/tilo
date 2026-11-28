//! A change a person chose to open reaches the live app only once History has returned to it.
use super::*;
use crate::widget::history::HistoryEntryId;
use crate::widget::history::{EntryKind, EntryTarget, HistoryAuthor};
use std::rc::Rc;

/// A live app that counts the changes it was asked to open.
struct Opens(Rc<Cell<usize>>);

impl crate::history_mode::capability::PresentsHistoryEntries for Opens {
    fn history_entry_presenter(&self) -> Option<Rc<dyn crate::widget::HistoryEntryPresenter>> {
        None
    }

    fn open_history_entry(&mut self, _entry: &crate::widget::HistoryEntry) -> bool {
        self.0.set(self.0.get() + 1);
        true
    }
}

impl Widget for Opens {
    tilo_ui_base::provides!(dyn crate::history_mode::capability::PresentsHistoryEntries);
    tilo_ui::testing::inert_widget_methods!(name = "opens history entries");

    fn build_layout(
        &mut self,
        tree: &mut taffy::TaffyTree<()>,
        _theme: &tilo_ui_base::Theme,
        _nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        tree.new_leaf(taffy::Style::default()).unwrap()
    }
}

fn chosen() -> crate::widget::HistoryEntry {
    crate::widget::HistoryEntry {
        id: HistoryEntryId([1; 16]),
        author: HistoryAuthor::new("Ada", 0.0, "Ada"),
        entry_kind: EntryKind::Details,
        artifact_kind: collab_types::ArtifactKind::File,
        timestamp: std::time::SystemTime::UNIX_EPOCH,
        summary: "Created legal".into(),
        text_delta: None,
        diff_payload: None,
        redaction: None,
        context: None,
        target: Some(EntryTarget {
            action: "Created".into(),
            label: "legal".into(),
            id: "legal".into(),
        }),
    }
}

/// Until the return settles, every pass discards what the live app queues, so opening the change
/// then would lose what it asked for: it waits, and is opened once, after.
#[test]
fn a_chosen_change_opens_once_history_has_returned() {
    let state = crate::State::new();
    let mut history =
        HistoryHost::new(crate::history_mode::testing::registration(), &state.doc).unwrap();
    let opened = Rc::new(Cell::new(0));
    let mut live = Opens(opened.clone());
    history.opened = Some(chosen());

    history.open = true;
    assert!(
        !history.deliver_opened_entry(&mut live),
        "History is still open"
    );
    history.open = false;
    history.returning_live = true;
    assert!(
        !history.deliver_opened_entry(&mut live),
        "the return has not settled"
    );
    assert_eq!(opened.get(), 0);

    history.returning_live = false;
    assert!(history.deliver_opened_entry(&mut live));
    assert!(
        !history.deliver_opened_entry(&mut live),
        "a change opens once"
    );
    assert_eq!(opened.get(), 1);
}
