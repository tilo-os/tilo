//! `compose(app, record: id, view: name) { fallback }`: another package's view of one of its
//! resources, drawn inline in the caller's tree (shell.md §8, "The canvas composes views").
//!
//! The caller names the resource and the view as text, the way a person's arrangement keeps
//! them (`ArrangementPlace`'s `shows`, `record` and `shown_as`), so the shell needs no `use` of
//! every app it may show. The runtime finds the app's package among those it holds code for,
//! the view among the package's offered views (by id or name), and the resource's type from the
//! view's first parameter. The view runs here, in the caller's runtime, as its own package's
//! code: what it creates and what its presses invoke carry that package's module as their
//! author, never the caller's ([`Core::render_at`]). A package loaded as another app's
//! ([`crate::Runtime::load_foreign_package`]) reads the resource from the reading its app
//! publishes ([`crate::foreign`]) and invokes its actions at that app's door.
//!
//! The node's block is what stands there when the view can't be composed here: no package of
//! the app is loaded (a window of an app that runs elsewhere), or the arrangement names no view
//! or no record. A view that is named and doesn't fit (unknown, or one that takes more than the
//! resource) is an error the node shows, never a silent fallback.

use std::rc::Rc;

use crate::core::Core;
use crate::meta::Ty;
use crate::registry::Package;
use crate::render::{Acc, Build, ViewTarget, scope};
use crate::value::{RefValue, ResourceRef, Value};
use crate::view::UiNode;

/// The kind of the node `compose(...)` builds.
pub(crate) const COMPOSE: &str = "compose";

/// A view of another package and the resource it is applied to.
struct Composed {
    target: ViewTarget,
    resource: ResourceRef,
    /// Whether the resource is another app's, drawn from the reading its app publishes.
    reading: bool,
}

fn text_prop<'n>(node: &'n UiNode, name: &str) -> &'n str {
    match node.props.get(name) {
        Some(Value::Text(t)) => t.as_str(),
        _ => "",
    }
}

impl Core {
    /// Replace a `compose` node with a `fragment` holding the named view's tree, rendered in
    /// place (its reads join the caller's), or holding the node's own children when the view
    /// can't be composed here.
    pub(crate) fn resolve_compose(&self, cx: &Build<'_>, node: &mut UiNode, acc: &mut Acc) {
        let app = text_prop(node, "app").to_owned();
        let record = text_prop(node, "record").to_owned();
        let view = text_prop(node, "view").to_owned();
        "fragment".clone_into(&mut node.kind);
        for name in ["app", "record", "view"] {
            node.props.remove(name);
        }
        let composed = match self.composed(&app, &record, &view) {
            Ok(Some(composed)) => composed,
            // Nothing of the app's runs here: the node's own children stand in its place.
            Ok(None) => return,
            Err(why) => {
                node.children = vec![crate::embed::error_node(&node.key, &[why])];
                return;
            }
        };
        // Another app's resource is drawn from its reading: until the first one arrives the
        // place is empty, and the reading arriving runs the caller again.
        if composed.reading && self.foreign.borrow().generation(&composed.resource) == 0 {
            let key = crate::foreign::reading_key(&composed.resource);
            let generation = self.view_states.borrow().generation(&key);
            let seen = Value::Int(i64::try_from(generation).unwrap_or(i64::MAX));
            acc.state_reads.push((key.clone(), generation));
            acc.state_seen.push((key, seen));
            node.children = Vec::new();
            return;
        }
        let arg = Value::Ref(RefValue {
            target: Rc::new(composed.resource),
            author: None,
        });
        // A composed view that fails shows why in its place; the rest of the caller stands.
        match self.render_at(&composed.target, &[arg], cx.offer.deeper(), None) {
            Ok(nested) => {
                let root = acc.absorb(nested);
                node.children = vec![scope(&node.key, root)];
            }
            Err(why) => {
                node.children = vec![crate::embed::error_node(&node.key, &[why.to_string()])];
            }
        }
    }

    /// The view `view` of the app `app` over its resource `record`: `None` when no package of
    /// the app with code is loaded, or no view or record is named.
    ///
    /// # Errors
    ///
    /// A message when the view is named and the package has no such view, the view does not
    /// take one of the app's resources and nothing else, or `record` is not a resource id.
    fn composed(&self, app: &str, record: &str, view: &str) -> Result<Option<Composed>, String> {
        if app.is_empty() || record.is_empty() || view.is_empty() {
            return Ok(None);
        }
        let Some(pkg) = self.package_of_app(app) else {
            return Ok(None);
        };
        let meta = pkg
            .meta
            .offered_views()
            .find(|v| v.id == view || v.name == view)
            .ok_or_else(|| format!("`{app}` has no view `{view}`"))?;
        let [param] = meta.params.as_slice() else {
            return Err(format!(
                "`{app}`'s view `{view}` takes {} values; a composed view takes its resource only",
                meta.params.len()
            ));
        };
        let key = match crate::meta::parse_ty(&param.ty, &pkg.meta.app) {
            Ok(Ty::Record(key)) if key.app == pkg.meta.app => key,
            _ => {
                return Err(format!(
                    "`{app}`'s view `{view}` does not take one of `{app}`'s resources"
                ));
            }
        };
        // A package of an app that runs elsewhere names its resources by app and type, as
        // their readings come in; one held here names them by its type's id.
        let ty = if pkg.foreign {
            format!("{}#{}", key.app, key.name)
        } else {
            self.registry
                .borrow()
                .type_for(&key)
                .ok_or_else(|| format!("`{app}` declares no type `{}`", key.name))?
        };
        let resource = ResourceRef::parse_id(record, ty)
            .ok_or_else(|| format!("`{record}` is not a resource id"))?;
        Ok(Some(Composed {
            target: ViewTarget {
                module: pkg.hash.clone(),
                export: format!("v_{}", meta.id),
                params: meta.params.clone(),
            },
            resource,
            reading: pkg.foreign,
        }))
    }

    /// The most recently loaded package of `app` whose code this runtime runs: one of its own,
    /// or another app's loaded with its module.
    fn package_of_app(&self, app: &str) -> Option<Rc<Package>> {
        let registry = self.registry.borrow();
        registry
            .packages()
            .into_iter()
            .rev()
            .find(|pkg| pkg.meta.app == app && pkg.code)
    }
}
