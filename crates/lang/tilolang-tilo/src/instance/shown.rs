//! The resources a window shows, as the app publishes them.
//!
//! A window shows the resource of each of its levels, and every resource the view it shows
//! refers to (its rows, and what a row's controls name). That set is one notion for both
//! readers: the palette offers the app's actions on it and the app answers their conditions
//! when it publishes, and what the window's connection publishes is bounded by it, so a change
//! in a workspace of ten thousand tasks costs the rows on screen. Every other row is answered,
//! and published, when a caller asks about it.

use std::collections::BTreeSet;

use tilo_render_protocol::patch::Level;

use super::{Instance, Shown};
use crate::naming::resource_id;
use crate::runtime::{ResourceRef, UiNode, Value};

impl Instance {
    /// The root view and, when a row went somewhere, the detail view.
    fn shown(&self) -> impl Iterator<Item = &Shown> {
        [Level::Root, Level::Detail]
            .into_iter()
            .filter_map(|level| self.level(level))
    }

    /// The published resource the window shows in front: its detail level's, else its root's.
    /// What the window's page belongs to, as the page view is the one in front.
    #[must_use]
    pub fn front_resource(&self) -> Option<String> {
        self.shown()
            .last()
            .map(|shown| resource_id(&self.app, &shown.target.id_string()))
    }

    /// The published resources the window shows, as the app publishes them
    /// (`tl:so.tilo.lang.tasks:root-fjqi4yze`): each level's own, every resource the view it
    /// shows refers to, and the items of the rows its lists hold — a row's actions are published
    /// on its resource, whether or not the list has rendered that row yet.
    #[must_use]
    pub fn shown_resources(&self) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for shown in self.shown() {
            let view = shown.view.borrow();
            out.insert(resource_id(&self.app, &shown.target.id_string()));
            for path in view.shown_refs() {
                out.insert(resource_id(&self.app, &path));
            }
            referents(view.tree(), &self.app, &mut out);
        }
        // The facts page says of its resource what its fields hold: the resource is what the
        // window shows.
        if let Some(facts) = &self.facts {
            out.insert(resource_id(&self.app, &facts.target.id_string()));
        }
        // An instance with no window opens no view, and runs for the callers of its actions: the
        // root they act on is what it holds, found without the reads a view of it makes.
        if self.windowless
            && self.root.is_none()
            && let Some(host) = self.host()
            && let Ok(root) = host.root_of_layer(&self.app, &self.root_view)
        {
            out.insert(resource_id(&self.app, &root.id_string()));
        }
        out
    }
}

impl Instance {
    /// Take a step of someone's action on a resource of the app before the window shows it. A
    /// write of text names its field by the id its type declares, and says it renamed the resource
    /// where history does ([`crate::runtime::did::renamed`]). A change that landed where the
    /// window shows its resource was seen landing ([`crate::runtime::Runtime::seen_live`]).
    pub fn heard(&self, interaction: &mut tilo_actions::SemanticInteraction) {
        let Some(app) = self.host() else {
            return;
        };
        let Some(package) = app.package(&self.app) else {
            return;
        };
        let kind = interaction.resource.kind.rsplit_once('.');
        let declared = kind.and_then(|(_, name)| package.meta.type_named(name));
        if let (Some(declared), tilo_actions::InteractionAnchor::Text { region, .. }) =
            (declared, &mut interaction.anchor)
            && let Some(field) = declared.fields.iter().find(|field| field.name == *region)
        {
            region.clone_from(&field.id);
            if let Some(said) = app.runtime().renamed(&declared.id, &field.id) {
                interaction.action.did = Some(self.platform.phrase(said.key, &[]));
            }
        }
        let published = tilo_actions::logical_resource_id(&interaction.resource.id).into_owned();
        // A write's block names the item by the id the document keeps it under; the window
        // draws that text under the resource it publishes the item as.
        if let tilo_actions::InteractionAnchor::Text { ranges, .. } = &mut interaction.anchor {
            for range in ranges {
                if !range.block.is_empty() && published.ends_with(&range.block) {
                    range.block.clone_from(&published);
                }
            }
        }
        let ty = declared.map_or_else(String::new, |declared| declared.id.clone());
        let Some(target) = published
            .strip_prefix(&format!("tl:{}:", self.app))
            .and_then(|id| ResourceRef::parse_id(id, ty))
        else {
            return;
        };
        if matches!(interaction.phase, tilo_actions::InteractionPhase::Committed)
            && self.shown_resources().contains(&published)
            && let Err(error) =
                app.runtime()
                    .seen_live(app.viewer(), &target, &interaction.actor.by)
        {
            tracing::warn!(%error, "a change seen landing could not be marked seen");
        }
    }
}

/// Every resource the props of `node` and its children refer to, as the app publishes it.
fn referents(node: &UiNode, app: &str, out: &mut BTreeSet<String>) {
    for value in node.props.values() {
        referred(value, app, out);
    }
    for child in &node.children {
        referents(child, app, out);
    }
}

/// Every resource `value` refers to, at any depth.
fn referred(value: &Value, app: &str, out: &mut BTreeSet<String>) {
    match value {
        Value::Ref(reference) => {
            out.insert(resource_id(app, &reference.target.id_string()));
        }
        Value::List(items) | Value::Tuple(items) => {
            for item in items {
                referred(item, app, out);
            }
        }
        Value::Opt(Some(item)) => referred(item, app, out),
        Value::Enum(found) => {
            for field in found.fields.values() {
                referred(field, app, out);
            }
        }
        _ => {}
    }
}
