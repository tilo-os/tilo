use ui::resource::{DocumentCheckout, ResourceType};

/// Checked package records preserve tab identity; Chat is the workspace package-primary record.
pub fn component_resource(app_id: &str, tab: u64, kind: ResourceType) -> DocumentCheckout {
    DocumentCheckout::for_component_tab(app_id, tab, kind)
}
