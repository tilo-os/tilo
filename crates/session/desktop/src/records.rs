use ui::record::{RecordCheckout, RecordKind};

/// Checked package records preserve tab identity; Chat is the room package-primary record.
pub fn component_record(app_id: &str, tab: u64, kind: RecordKind) -> RecordCheckout {
    RecordCheckout::for_component_tab(app_id, tab, kind)
}
