//! The items a chain's parts keep, named by id or label: what a path naming
//! one asks of the part keeping it, without paging the level or publishing
//! the page.

use super::*;

impl ChainState {
    /// The items `name` names among the parts' items — by its id, whole or
    /// the item part of it (`<kind>/<field>/<item>`), and only when no id
    /// names any, by its label — each built alone from the part keeping it:
    /// what a path naming one costs, however many items the chain holds, and
    /// whether or not the page the chain publishes carries it. Empty when it
    /// names none, or names only what the chain does not show.
    pub(crate) fn items_named(&self, name: &str) -> Vec<tilo_actions::ResourceSpec> {
        let part = self.part();
        let Some(table) = part.table else {
            return Vec::new();
        };
        let parts = self.parts();
        let typed = (part.typed)(&parts);
        let hidden = (part.hidden)(&*typed);
        let mut by_id: Vec<(u64, String)> = Vec::new();
        let mut by_label: Vec<(u64, String)> = Vec::new();
        for (index, state) in parts.iter().rev() {
            for (field, items) in tilo_resource::table::resource_items(table, &**state) {
                for (id, label) in items {
                    let resource = tilo_resource::items::item_resource_id_in(part.kind, field, &id);
                    if hidden.contains(&resource) {
                        continue;
                    }
                    let named = resource == name
                        || resource.rsplit('/').next().is_some_and(|item| item == name);
                    if named {
                        by_id.push((*index, resource));
                    } else if label.eq_ignore_ascii_case(name) {
                        by_label.push((*index, resource));
                    }
                }
            }
        }
        let wanted = if by_id.is_empty() { by_label } else { by_id };
        let mut found = Vec::with_capacity(wanted.len());
        for (index, resource) in wanted {
            let Some(state) = parts
                .iter()
                .rev()
                .find(|(at, _)| *at == index)
                .map(|(_, state)| state)
            else {
                continue;
            };
            let Some(child) = (part.child)(&**state, &resource) else {
                continue;
            };
            found.push(
                self.core
                    .carried_item(route_at(child, &self.core.id, true), Some(table)),
            );
        }
        found
    }
}
