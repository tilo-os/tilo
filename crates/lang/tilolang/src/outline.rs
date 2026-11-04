//! The actions an item type standing under its own kind gets, derived from what its fields say.
//!
//! A type whose items nest, and that one of its package's roots holds, needs the same tree
//! operations every nested type needs: move an item into a place, indent and outdent it, copy it
//! with the items under it, delete it with them, check it off with everything under it, and add a
//! subtask under it. They are the type's fields read out, not the app's code:
//!
//! - `(parent)` says an item stands under another, and by which field its items nest;
//! - `(done)` says which flag finishes an item, so checking one off finishes the items under it
//!   and opening one opens the items it is under;
//! - `(title)` says what names an item, which is what a subtask is made by;
//! - `(inherit)` says what an item keeps of the one it stands under, which a new subtask starts
//!   with: a task in a project's section makes its subtasks in the same project and section,
//!   while who it is assigned to is the subtask's own. It is the app's model of what nesting
//!   shares, marked where the model is, rather than the platform guessing at every reference
//!   field;
//! - a named root holding the type's items (`root tasks: Tasks` with `items: Collection<Task>`)
//!   is the list the actions read: the descendants they finish, the ancestors they open, the
//!   siblings they count from.
//!
//! The actions are the app's own declarations once derived, so the host offers them in menus,
//! palettes and keys and writes their changes in history as it does any declaration. An app that
//! declares one of the names itself overrides that one: the derived actions call each other by
//! name, and find the app's.
//!
//! One type of a package has the actions: the first declared that meets the rules, since they
//! are named for the outline the person sees, not for a type. An app with another nested type
//! declares that one's actions itself, as it declares a type's any other action.

use std::collections::HashSet;
use std::fmt::Write as _;

use crate::ast::{self, Item, TypeExprKind};
use crate::parser::parse_file_at;

/// The actions, in the words the placeholders name them by.
const TEMPLATE: &str = include_str!("outline.tl");

/// What stands before the app's own declarations: what an item's state speaks of, which a menu
/// offers before the work an item can be given. Everything else stands after them, where the
/// moves, the copy and the delete belong.
const EARLY: [&str; 2] = ["check", "add_subtask"];

/// Where the derived items' spans start, apart from every module's own nodes. Node ids key UI
/// nodes, which an action has none of.
const NODE: ast::NodeId = 1 << 31;

/// Derives the tree actions of the module's nested item type, if it has one, and stands them
/// among its declarations: what an item's state speaks of before the app's own actions, and its
/// moves, copy and delete after them.
pub fn derive(module: &mut ast::Module) {
    let declared: HashSet<String> = crate::ids::declared_names(module).into_iter().collect();
    let Some(plan) = plan(module) else { return };
    let text = plan.text();
    let (parsed, diags) = parse_file_at(&text, plan.at, NODE);
    debug_assert!(
        diags.is_empty(),
        "the derived outline actions parse: {diags:?}\n{text}"
    );
    if !diags.is_empty() {
        return;
    }
    let mut early = Vec::new();
    let mut late = Vec::new();
    for item in parsed.items {
        let Some(name) = declared_name(&item).map(str::to_owned) else {
            continue;
        };
        if declared.contains(&name) {
            continue;
        }
        if EARLY.contains(&name.as_str()) {
            early.push(item);
        } else {
            late.push(item);
        }
    }
    let mut items = early;
    items.append(&mut module.items);
    items.extend(late);
    module.items = items;
}

/// The module's first type the outline actions are derived for: the item type, the fields the
/// derivation writes them with, and where in the module its `(parent)` marker stands, which is
/// where a diagnostic about the derived actions points.
struct Plan {
    item: String,
    holder: String,
    root: String,
    items: String,
    parent: String,
    done: String,
    title: String,
    /// Every field of the item type, in declaration order, as an item's copy writes it.
    fields: Vec<String>,
    /// The fields an item keeps of the one it stands under (`(inherit)`).
    inherits: Vec<String>,
    at: usize,
}

impl Plan {
    /// The template with the plan's names written in.
    fn text(&self) -> String {
        let copy: Vec<String> = self
            .fields
            .iter()
            .map(|field| {
                if field == &self.parent {
                    format!("{parent}: parent", parent = self.parent)
                } else {
                    format!("{field}: t.{field}")
                }
            })
            .collect();
        let mut inherit = String::new();
        for field in &self.inherits {
            let _ = write!(inherit, ", {field}: t.{field}");
        }
        TEMPLATE
            .replace("%item%", &self.item)
            .replace("%holder%", &self.holder)
            .replace("%root%", &self.root)
            .replace("%items%", &self.items)
            .replace("%parent%", &self.parent)
            .replace("%done%", &self.done)
            .replace("%title%", &self.title)
            .replace("%copy%", &copy.join(", "))
            .replace("%inherit%", &inherit)
    }
}

/// The plan for the module's first nested item type, when the module has one its actions can
/// serve: the type marks what nests (`(parent)`), what finishes (`(done)`) and what names an
/// item (`(title)`), and a named root of the package holds the collection its items are in.
fn plan(module: &ast::Module) -> Option<Plan> {
    let named: Vec<&ast::TypeDecl> = module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Type(decl) => Some(decl),
            _ => None,
        })
        .collect();
    let roots = named_roots(module);
    for item in &named {
        let Some(parent) = item.fields.iter().find(|f| marks(f, "parent")) else {
            continue;
        };
        if !has_role(item, "done") || !has_role(item, "title") {
            continue;
        }
        let name = item.name.name.as_str();
        let Some((holder, items)) = named.iter().find_map(|h| {
            if h.name.name == name {
                return None;
            }
            let field = h.fields.iter().find(|f| holds_collection(&f.ty, name))?;
            Some((h.name.name.clone(), field.name.name.clone()))
        }) else {
            continue;
        };
        let Some(root) = roots.iter().find(|(_, ty)| *ty == holder) else {
            continue;
        };
        return Some(Plan {
            item: name.to_owned(),
            holder,
            root: root.0.clone(),
            items,
            parent: parent.name.name.clone(),
            done: role_field(item, "done"),
            title: role_field(item, "title"),
            fields: item.fields.iter().map(|f| f.name.name.clone()).collect(),
            inherits: item
                .fields
                .iter()
                .filter(|f| marks(f, "inherit"))
                .map(|f| f.name.name.clone())
                .collect(),
            at: parent.span.start,
        });
    }
    None
}

/// The package's named roots (`root tasks: Tasks`), each with the type it holds.
fn named_roots(module: &ast::Module) -> Vec<(String, String)> {
    module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Root(root) => {
                let name = root.name.as_ref()?.name.clone();
                let TypeExprKind::Named(path) = &root.ty.kind else {
                    return None;
                };
                let [ty] = path.as_slice() else { return None };
                Some((name, ty.name.clone()))
            }
            _ => None,
        })
        .collect()
}

/// Whether the type marks a field `marker`: `(parent)`, `(done)`, `(title)` or `(inherit)`.
fn has_role(decl: &ast::TypeDecl, marker: &str) -> bool {
    decl.fields.iter().any(|f| marks(f, marker))
}

/// Whether the field's marker is `marker`.
fn marks(field: &ast::FieldDecl, marker: &str) -> bool {
    field.marker.as_ref().is_some_and(|m| m.name.name == marker)
}

/// The field of `decl` its marker `marker` is on.
fn role_field(decl: &ast::TypeDecl, marker: &str) -> String {
    decl.fields
        .iter()
        .find(|f| marks(f, marker))
        .map(|f| f.name.name.clone())
        .unwrap_or_default()
}

/// Whether `ty` is `Collection<T>`, `T` being `name`.
fn holds_collection(ty: &ast::TypeExpr, name: &str) -> bool {
    let TypeExprKind::Generic(generic, args) = &ty.kind else {
        return false;
    };
    if generic.name != "Collection" {
        return false;
    }
    matches!(args.as_slice(), [arg] if matches!(&arg.kind, TypeExprKind::Named(path) if matches!(path.as_slice(), [one] if one.name == name)))
}

/// The name the item declares, when it declares one.
fn declared_name(item: &Item) -> Option<&str> {
    match item {
        Item::Action(decl) => Some(&decl.name.name),
        Item::Fn(decl) => Some(&decl.name.name),
        Item::Flow(decl) => Some(&decl.name.name),
        _ => None,
    }
}
