//! The items a chain's parts keep, named by id or label: what a path or an
//! address naming one asks of the part keeping it, without paging the level
//! or publishing the page.

use super::*;

impl ChainState {
    /// The item `id` (`<kind>/<field>/<item>`) built alone from the part
    /// keeping it, as the chain carries it: what an action addressed to it
    /// resolves, however many items the chain holds, and whether or not the
    /// page the chain publishes carries it. `None` when no part held here
    /// keeps it, or the chain does not show it.
    pub(crate) fn item(&self, id: &str) -> Option<tilo_actions::ResourceSpec> {
        let part = self.part();
        tilo_resource::items::item_of_resource_in(part.kind, id)?;
        let parts = self.parts();
        let child = parts
            .iter()
            .rev()
            .find_map(|(_, state)| (part.child)(&**state, id))?;
        if (part.hidden)(&*(part.typed)(&parts)).contains(id) {
            return None;
        }
        Some(
            self.core
                .carried_item(route_at(child, &self.core.id, true), part.table),
        )
    }

    /// The items `name` names among the parts' items — by its id, whole or
    /// the item part of it (`<kind>/<field>/<item>`), each found in the part
    /// keeping it, and only when no id names any, by its label — each built
    /// alone: what a path naming one costs, and an id costs the same however
    /// many items the chain holds. Empty when it names none, or names only
    /// what the chain does not show.
    pub(crate) fn items_named(&self, name: &str) -> Vec<tilo_actions::ResourceSpec> {
        let part = self.part();
        let Some(table) = part.table else {
            return Vec::new();
        };
        let ids: Vec<String> =
            if tilo_resource::items::item_of_resource_in(part.kind, name).is_some() {
                vec![name.to_owned()]
            } else if name.contains('/') {
                Vec::new()
            } else {
                tilo_resource::table::item_fields(table)
                    .into_iter()
                    .map(|field| {
                        tilo_resource::items::item_resource_id_in(
                            part.kind,
                            field,
                            &tilo_resource::Id::from_raw(name),
                        )
                    })
                    .collect()
            };
        let by_id: Vec<_> = ids.iter().filter_map(|id| self.item(id)).collect();
        if !by_id.is_empty() {
            return by_id;
        }
        let parts = self.parts();
        let typed = (part.typed)(&parts);
        let hidden = (part.hidden)(&*typed);
        let mut found = Vec::new();
        for (_, state) in parts.iter().rev() {
            for (field, items) in tilo_resource::table::resource_items(table, &**state) {
                for (id, label) in items {
                    if !label.eq_ignore_ascii_case(name) {
                        continue;
                    }
                    let resource = tilo_resource::items::item_resource_id_in(part.kind, field, &id);
                    if hidden.contains(&resource) {
                        continue;
                    }
                    if let Some(child) = (part.child)(&**state, &resource) {
                        found.push(
                            self.core
                                .carried_item(route_at(child, &self.core.id, true), Some(table)),
                        );
                    }
                }
            }
        }
        found
    }
}
