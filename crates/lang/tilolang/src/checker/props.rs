//! The props a view gives a described component are the ones that component declares.
//!
//! A component's description is the contract between a view and the widget the UI host builds
//! (`crates/dev/ui-graph`): a prop the description does not have is a batch the host refuses, so
//! the window goes without what the app sent, at run time, after the app has landed. The
//! checker is the only writer of those names — an app names arguments, and the checker lowers
//! them — so what it wrote is read back here, over the whole module: a name the catalogue does
//! not know is the compiler's own defect (`E000`), and it fails the compile that would have sent
//! it, not the window that would have shown it.

use std::collections::HashMap;

use crate::abi::Imp;
use crate::diag::{Diagnostic, Span};
use crate::ir::{Ek, Expr, LocalId, Module, Stmt};
use crate::prims;
use crate::toolkit;

/// Reports every prop a module sets on a node of a described component that the component does
/// not declare.
pub(super) fn undeclared(module: &Module, diags: &mut Vec<Diagnostic>) {
    for func in &module.funcs {
        let mut nodes = Nodes::default();
        nodes.stmts(&func.body, func, diags);
    }
}

/// The kind of each node a function builds so far, by the local that holds it.
#[derive(Default)]
struct Nodes {
    kinds: HashMap<LocalId, String>,
}

impl Nodes {
    fn stmts(&mut self, stmts: &[Stmt], func: &crate::ir::Func, diags: &mut Vec<Diagnostic>) {
        for s in stmts {
            match s {
                Stmt::Set(local, e) => {
                    if let Some(kind) = node_kind(e) {
                        self.kinds.insert(*local, kind.to_owned());
                    }
                    self.expr(e, func, diags);
                }
                Stmt::Eval(e) | Stmt::Return(Some(e)) => self.expr(e, func, diags),
                Stmt::If(c, then, els) => {
                    self.expr(c, func, diags);
                    self.stmts(then, func, diags);
                    self.stmts(els, func, diags);
                }
                Stmt::ForList(list) => {
                    self.expr(&list.list, func, diags);
                    self.stmts(&list.body, func, diags);
                }
                Stmt::Return(None) | Stmt::Break => {}
            }
        }
    }

    fn expr(&mut self, e: &Expr, func: &crate::ir::Func, diags: &mut Vec<Diagnostic>) {
        match &e.kind {
            Ek::Host(imp, args) => {
                if let Some(name) = prop_name(*imp, args)
                    && let Some(kind) = args.first().and_then(|node| self.kind(node))
                {
                    declared(kind, name, func, diags);
                }
                for a in args {
                    self.expr(a, func, diags);
                }
            }
            Ek::Call(_, args) => {
                for a in args {
                    self.expr(a, func, diags);
                }
            }
            Ek::Bin(_, a, b) => {
                self.expr(a, func, diags);
                self.expr(b, func, diags);
            }
            Ek::Neg(a) | Ek::Not(a) | Ek::Conv(_, a) => self.expr(a, func, diags),
            Ek::If(c, a, b) => {
                self.expr(c, func, diags);
                self.expr(a, func, diags);
                self.expr(b, func, diags);
            }
            Ek::Seq(stmts, v) => {
                self.stmts(stmts, func, diags);
                self.expr(v, func, diags);
            }
            _ => {}
        }
    }

    /// The kind of the node an expression holds, when it is one this function built.
    fn kind(&self, node: &Expr) -> Option<&str> {
        let Ek::Local(local) = node.kind else {
            return None;
        };
        self.kinds.get(&local).map(String::as_str)
    }
}

/// The components a view places actions on itself: an item's node and the window's shell.
const PLACES: [&str; 4] = ["ListItem", "TreeViewNode", "BoardCard", "NavShell"];

/// Reports `name` on a node of the described component `kind` unless the component declares it,
/// or it is a name every node may carry: a sizing argument, the binding a control takes, or the
/// companion of a prop the component declares (`value:as`, `meta:time`).
fn declared(kind: &str, name: &str, func: &crate::ir::Func, diags: &mut Vec<Diagnostic>) {
    let Some(component) = kind.strip_prefix(toolkit::KIND_PREFIX) else {
        return;
    };
    let Some(description) = toolkit::catalogue().get(component) else {
        return;
    };
    let carried =
        |prop: &str| description.prop(prop).is_some() || description.state(prop).is_some();
    if carried(name)
        || description.binding(name).is_some()
        || name == "bind"
        || prims::NODE_ARGS.contains(&sizing(name))
        || tilo_ui_graph::component::companion(name).is_some_and(|(prop, _)| carried(prop))
        // The actions a view places on what it shows: the host reads them while it builds the
        // item's surfaces and takes them before the tree is sent, as it takes a `show`'s own
        // props, so the widget it hands over carries only what its description declares.
        || (name == "actions" && PLACES.contains(&component))
    {
        return;
    }
    diags.push(Diagnostic::new(
        "E000",
        Span::default(),
        format!(
            "internal: `{}` gives `{kind}` the prop `{name}`, which the component does not \
             declare: the window would refuse what the app sent",
            func.name
        ),
    ));
}

/// The kind of the node `e` builds (`ui.ListRow`), when it builds one.
fn node_kind(e: &Expr) -> Option<&str> {
    let Ek::Host(Imp::UiNode, args) = &e.kind else {
        return None;
    };
    match &args.first()?.kind {
        Ek::Str(kind) => Some(kind),
        _ => None,
    }
}

/// The name a prop-setting call sets, when it sets one.
fn prop_name(imp: Imp, args: &[Expr]) -> Option<&str> {
    if !matches!(
        imp,
        Imp::UiPropH | Imp::UiPropI | Imp::UiPropF | Imp::UiPropPic
    ) {
        return None;
    }
    match &args.get(1)?.kind {
        Ek::Str(name) => Some(name),
        _ => None,
    }
}

/// A sizing argument's name without its unit (`min.rem` is `min`).
fn sizing(name: &str) -> &str {
    name.strip_suffix(".rem")
        .or_else(|| name.strip_suffix(".dp"))
        .unwrap_or(name)
}
