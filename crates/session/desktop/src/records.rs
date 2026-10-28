use ui::record::{RecordCheckout, RecordId, RecordKind, RecordStore};
use ui::widget::WindowGroup as UiGroup;

/// Record type for a tab whose content is the app's own, unsaved
/// document — a scratch note, an untitled buffer. The shell only needs
/// one name here since it never interprets the content.
pub const TAB_RECORD_KIND: &str = "tab";

/// The record a scratch tab is. Not `RecordId::minted()`: the tab id is
/// already a written-down identity — allocated once, peer-salted, and
/// persisted with the workspace tree — so a second one beside it could
/// only drift.
pub fn scratch_tab_record(tab_id: u64) -> RecordCheckout {
    RecordCheckout::new(RecordId::for_tab(tab_id), RecordKind::new(TAB_RECORD_KIND))
}

/// Called only by the action that minted this new tab identity, before its
/// first widget-tree rebuild. Never infer creation from an absent record.
pub fn create_scratch_tab_record(
    store: &RecordStore,
    tab_id: u64,
) -> Result<(), ui::record::RecordOpenError> {
    store.create_record(&scratch_tab_record(tab_id))
}

/// Failures belong to the exact fresh creation ticket, never to the saved
/// resource globally. Apply before syncing the newly constructed group.
pub fn apply_creation_failures(
    group: &mut UiGroup,
    failures: &std::collections::BTreeMap<u64, String>,
) {
    for (tab, reason) in failures {
        if let Some(host) = group.host_for_tab_mut(*tab) {
            host.refuse_record_binding(reason.clone());
        }
    }
}

/// Give every tab in `group` its own document. Called once per group as
/// the shell (re)builds the widget tree. The binding travels with the
/// tile, so a tab dragged into another group keeps its document.
pub fn bind_group_records(group: &mut UiGroup, store: &RecordStore) {
    group.bind_tab_records(store, scratch_tab_record);
}

/// Checked package records preserve tab identity; Chat is the room package-primary record.
pub fn component_record(app_id: &str, tab: u64, kind: RecordKind) -> RecordCheckout {
    RecordCheckout::for_component_tab(app_id, tab, kind)
}
