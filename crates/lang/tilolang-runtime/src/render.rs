//! Running views and blocks: the view frame, the UI tree built from it, and the resolution of
//! foreign view nodes (`view:<app>:<view id>`) into nested boundaries.

use std::collections::HashSet;

use crate::core::Core;
use crate::error::RuntimeError;
use crate::handles::{FrameKind, Retained, ViewState};
use crate::ids::{ModuleHash, Principal};
use crate::meta::ParamMeta;
use crate::registry::Package;
use crate::space::Space;
use crate::value::Value;
use crate::view::{NodeBuild, UiNode};

/// Nested foreign views deeper than this are refused: a module that applies views in a loop
/// would otherwise exhaust the host's stack.
const MAX_VIEW_DEPTH: usize = 32;

/// The prefix of the kind of a node that applies another package's view.
const FOREIGN_VIEW: &str = "view:";
/// The kind of a node that renders a block's source (`embed(source, author: who)`).
pub(crate) const EMBED: &str = "embed";
/// The prefix of the properties of such a node that carry the view's arguments.
const ARG_PREFIX: &str = "arg.";

/// A view export of a loaded module.
#[derive(Clone, Debug)]
pub struct ViewTarget {
    pub module: ModuleHash,
    pub export: String,
    pub params: Vec<ParamMeta>,
}

/// A rendered view: its tree, what it read, and the action references it retained.
pub struct Rendered {
    pub root: UiNode,
    pub reads: Vec<(String, String)>,
    pub space_reads: Vec<(String, f64)>,
    pub retained: Vec<Retained>,
}

pub(crate) struct Acc {
    pub reads: Vec<(String, String)>,
    pub space_reads: Vec<(String, f64)>,
    pub retained: Vec<Retained>,
}

/// What a view boundary is offered: its space, the person looking at it and how deep the
/// nesting is.
#[derive(Clone, Copy)]
pub(crate) struct Offer<'a> {
    pub space: Space,
    pub viewer: &'a Principal,
    pub depth: usize,
}

impl Offer<'_> {
    pub fn deeper(self) -> Self {
        Self {
            depth: self.depth.saturating_add(1),
            ..self
        }
    }
}

pub(crate) struct Build<'a> {
    pub nodes: &'a [NodeBuild],
    pub offer: Offer<'a>,
}

/// Prefix every key of a nested tree with the key of the node that holds it. A view called
/// per row, or a block embedded in two messages, would otherwise repeat its keys, and a key must name one node in the
/// tree for a press or an edit to find it.
pub(crate) fn scope(prefix: &str, mut tree: UiNode) -> UiNode {
    // An anchored node keeps its key: the anchor is the address people and agents use.
    if tree.anchor.is_none() {
        tree.key = format!("{prefix}~{}", tree.key);
    }
    tree.children = tree
        .children
        .into_iter()
        .map(|c| scope(prefix, c))
        .collect();
    tree
}

impl Acc {
    /// Take on what a nested boundary read and retained.
    pub fn absorb(
        &mut self,
        reads: Vec<(String, String)>,
        space_reads: Vec<(String, f64)>,
        retained: Vec<Retained>,
    ) {
        self.reads.extend(reads);
        self.space_reads.extend(space_reads);
        self.retained.extend(retained);
    }
}

impl Core {
    /// The target of the view with this id.
    ///
    /// # Errors
    ///
    /// `NotFound` for an unknown view.
    pub fn view_target(&self, view: &str) -> Result<ViewTarget, RuntimeError> {
        let pkg = self
            .registry
            .borrow()
            .view_package(view)
            .ok_or_else(|| RuntimeError::NotFound(format!("view {view}")))?;
        let meta = pkg
            .view(view)
            .ok_or_else(|| RuntimeError::NotFound(format!("view {view}")))?;
        Ok(ViewTarget {
            module: pkg.hash.clone(),
            export: format!("v_{view}"),
            params: meta.params.clone(),
        })
    }

    /// Run a view and build its tree.
    ///
    /// # Errors
    ///
    /// Arguments that do not fit, a trap, or a malformed tree.
    pub fn render(
        &self,
        target: &ViewTarget,
        args: &[Value],
        space: Space,
        viewer: &Principal,
    ) -> Result<Rendered, RuntimeError> {
        let offer = Offer {
            space,
            viewer,
            depth: 0,
        };
        self.render_at(target, args, offer, None)
    }

    /// Render a view as its own boundary. `author` replaces the module as the author of what
    /// the view creates: an embedded block is written by its principal, not by its module.
    pub(crate) fn render_at(
        &self,
        target: &ViewTarget,
        args: &[Value],
        offer: Offer<'_>,
        author: Option<&ModuleHash>,
    ) -> Result<Rendered, RuntimeError> {
        let (space, depth) = (offer.space, offer.depth);
        if depth > MAX_VIEW_DEPTH {
            return Err(RuntimeError::Package(format!(
                "views are nested more than {MAX_VIEW_DEPTH} deep"
            )));
        }
        let pkg = self
            .registry
            .borrow()
            .package(&target.module)
            .ok_or_else(|| RuntimeError::NotFound(format!("module {}", target.module)))?;
        self.frames.borrow_mut().push(
            author.unwrap_or(&pkg.hash).clone(),
            FrameKind::View(ViewState {
                space,
                ..ViewState::default()
            }),
        );
        let root = self.call_in_frame(&pkg, &target.export, &target.params, args, Some("View"));
        let frame = self.frames.borrow_mut().pop();
        let Some(FrameKind::View(state)) = frame.map(|f| f.kind) else {
            return Err(RuntimeError::Package("the view frame vanished".into()));
        };
        let Value::Node(root) = root? else {
            return Err(RuntimeError::Package("a view must return a ui node".into()));
        };
        let mut acc = Acc {
            reads: state.reads,
            space_reads: state.space_reads,
            retained: Vec::new(),
        };
        let cx = Build {
            nodes: &state.nodes,
            offer,
        };
        let tree = self.build_node(&cx, root, 0, &mut acc)?;
        Ok(Rendered {
            root: tree,
            reads: acc.reads,
            space_reads: acc.space_reads,
            retained: acc.retained,
        })
    }

    fn build_node(
        &self,
        cx: &Build<'_>,
        at: usize,
        level: usize,
        acc: &mut Acc,
    ) -> Result<UiNode, RuntimeError> {
        if level > cx.nodes.len() {
            return Err(RuntimeError::Package("the ui tree contains a cycle".into()));
        }
        let n = cx
            .nodes
            .get(at)
            .ok_or_else(|| RuntimeError::Package("unknown ui node".into()))?;
        let mut keys = HashSet::new();
        let mut children = Vec::new();
        for c in &n.children {
            let child = self.build_node(cx, *c, level.saturating_add(1), acc)?;
            if !keys.insert(child.key.clone()) {
                return Err(RuntimeError::Package(format!(
                    "duplicate key `{}` among the children of `{}`",
                    child.key, n.key
                )));
            }
            children.push(child);
        }
        let events = n
            .events
            .iter()
            .map(|(name, v)| {
                let r = self.frames.borrow_mut().retain(v.clone());
                acc.retained.push(r);
                (name.clone(), r)
            })
            .collect();
        let mut node = UiNode {
            kind: n.kind.clone(),
            key: n.key.clone(),
            props: n.props.clone(),
            binding: n.binding.clone(),
            events,
            anchor: n.anchor.clone(),
            children,
        };
        if let Some(target) = n.kind.strip_prefix(FOREIGN_VIEW) {
            self.resolve_foreign(cx, target, &mut node, acc)?;
        } else if n.kind == EMBED {
            self.resolve_embed(cx, &mut node, acc);
        }
        if node.kind == "fragment" {
            // A view call: the callee's keys are the same at every call site.
            let prefix = node.key.clone();
            node.children = node
                .children
                .into_iter()
                .map(|c| scope(&prefix, c))
                .collect();
        }
        Ok(node)
    }

    /// Replace a node applying another package's view with a `fragment` holding that view's
    /// tree, rendered as its own boundary. The nested view's code is its own package's, so
    /// its action references are authored by that package.
    fn resolve_foreign(
        &self,
        cx: &Build<'_>,
        target: &str,
        node: &mut UiNode,
        acc: &mut Acc,
    ) -> Result<(), RuntimeError> {
        let (app, view) = target
            .split_once(':')
            .ok_or_else(|| RuntimeError::Package(format!("malformed view node `{target}`")))?;
        let vt = self.view_target(view)?;
        let pkg: std::rc::Rc<Package> = self
            .registry
            .borrow()
            .package(&vt.module)
            .ok_or_else(|| RuntimeError::NotFound(format!("module {}", vt.module)))?;
        if pkg.meta.app != app {
            return Err(RuntimeError::Package(format!(
                "view {view} belongs to app `{}`, not `{app}`",
                pkg.meta.app
            )));
        }
        let args = vt
            .params
            .iter()
            .map(|p| {
                node.props
                    .get(&format!("{ARG_PREFIX}{}", p.name))
                    .cloned()
                    .ok_or_else(|| {
                        RuntimeError::Package(format!(
                            "the view node for `{view}` is missing the argument `{}`",
                            p.name
                        ))
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let nested = self.render_at(&vt, &args, cx.offer.deeper(), None)?;
        acc.absorb(nested.reads, nested.space_reads, nested.retained);
        "fragment".clone_into(&mut node.kind);
        node.props.retain(|name, _| !name.starts_with(ARG_PREFIX));
        node.children = vec![nested.root];
        Ok(())
    }

    // ----- blocks ------------------------------------------------------------------------

    /// Compile and register the module of a block.
    ///
    /// # Errors
    ///
    /// `Package` for a module that does not link.
    pub fn load_block(
        &self,
        wasm: &[u8],
        meta: &tilolang::PackageMeta,
    ) -> Result<std::rc::Rc<Package>, RuntimeError> {
        let hash = self.engine.compile(wasm)?;
        let pkg = Package::new(hash, meta.clone()).map_err(RuntimeError::Package)?;
        self.registry
            .borrow_mut()
            .add(pkg)
            .map_err(RuntimeError::Package)
    }

    /// Run a block's `b_main`, which takes no arguments, and read its value.
    ///
    /// # Errors
    ///
    /// A trap or a value of the wrong type.
    pub fn eval_block(
        &self,
        pkg: &Package,
        value_type: &str,
        author: Option<&ModuleHash>,
    ) -> Result<Value, RuntimeError> {
        self.frames
            .borrow_mut()
            .push(author.unwrap_or(&pkg.hash).clone(), FrameKind::Pure);
        let out = self.call_in_frame(pkg, "b_main", &[], &[], Some(value_type));
        self.frames.borrow_mut().pop();
        out
    }
}
