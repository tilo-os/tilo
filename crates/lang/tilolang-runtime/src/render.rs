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
use crate::view_state::StateKey;

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
    /// The view state slots read, with their generations.
    pub state_reads: Vec<(StateKey, u64)>,
    /// The value each slot read had, its default included.
    pub state_seen: Vec<(StateKey, Value)>,
}

pub(crate) struct Acc {
    pub reads: Vec<(String, String)>,
    pub space_reads: Vec<(String, f64)>,
    pub retained: Vec<Retained>,
    pub state_reads: Vec<(StateKey, u64)>,
    pub state_seen: Vec<(StateKey, Value)>,
}

/// What a view boundary is offered: its space, the window and the person looking at it, and
/// how deep the nesting is.
#[derive(Clone, Copy)]
pub(crate) struct Offer<'a> {
    pub space: Space,
    pub viewer: &'a Principal,
    pub window: &'a str,
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

/// Where the walk over a view's nodes is.
#[derive(Clone, Copy)]
pub(crate) struct Walk<'a> {
    pub level: usize,
    /// The key the nodes here go under, `None` at the root of a view.
    pub prefix: Option<&'a str>,
    /// Whether the nodes here are the rows of a `list`.
    pub in_list: bool,
}

/// A view call met while building a tree: the boundary the host renders.
pub(crate) struct Stub<'a> {
    /// The key the call's node has in the tree.
    pub key: &'a str,
    pub target: ViewTarget,
    pub args: Vec<Value>,
    /// A row of a `list`: rendered when the host shows it, not now.
    pub lazy: bool,
    pub depth: usize,
}

/// Renders the view calls of a tree as boundaries of their own.
pub(crate) trait Boundaries {
    /// The children of the `fragment` that stands for the call: the callee's tree, or nothing
    /// for a row that is not shown yet, or for a boundary whose tree the caller keeps.
    fn boundary(&mut self, stub: Stub<'_>) -> Result<Vec<UiNode>, RuntimeError>;
}

/// How a boundary is rendered: under which key, and who renders its view calls.
pub(crate) struct Scoped<'p, 'b> {
    pub prefix: Option<&'p str>,
    pub boundaries: Option<&'b mut dyn Boundaries>,
}

/// Prefix every key of a nested tree with the key of the node that holds it. A view called
/// per row, or a block embedded in two messages, would otherwise repeat its keys, and a key
/// must name one node in the tree for a press or an edit to find it.
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
    /// Take on what a nested boundary read and retained, and hand back its tree.
    pub fn absorb(&mut self, nested: Rendered) -> UiNode {
        self.reads.extend(nested.reads);
        self.space_reads.extend(nested.space_reads);
        self.retained.extend(nested.retained);
        self.state_reads.extend(nested.state_reads);
        self.state_seen.extend(nested.state_seen);
        nested.root
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

    /// Render a view as its own boundary. `author` replaces the module as the author of what
    /// the view creates: an embedded block is written by its principal, not by its module.
    /// View calls inside are resolved in place, their reads joining this view's.
    pub(crate) fn render_at(
        &self,
        target: &ViewTarget,
        args: &[Value],
        offer: Offer<'_>,
        author: Option<&ModuleHash>,
    ) -> Result<Rendered, RuntimeError> {
        self.render_boundary(target, args, offer, author, None)
    }

    /// Render a view as its own boundary, its keys under `scope` (the key of the node that
    /// holds it, `None` at the root). With `boundaries`, a view call inside is not resolved
    /// here but handed to it: the node becomes a `fragment` holding what it answers, and the
    /// callee's reads are its own, not this view's.
    pub(crate) fn render_boundary(
        &self,
        target: &ViewTarget,
        args: &[Value],
        offer: Offer<'_>,
        author: Option<&ModuleHash>,
        scoped: Option<Scoped<'_, '_>>,
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
                window: offer.window.to_owned(),
                viewer: offer.viewer.clone(),
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
            state_reads: state.state_reads,
            state_seen: state.state_seen,
        };
        let cx = Build {
            nodes: &state.nodes,
            offer,
        };
        let (prefix, hook) = match scoped {
            Some(Scoped { prefix, boundaries }) => (prefix, boundaries),
            None => (None, None),
        };
        let walk = Walk {
            level: 0,
            prefix,
            in_list: false,
        };
        let tree = self.build_node(&cx, root, walk, &mut acc, hook)?;
        Ok(Rendered {
            root: tree,
            reads: acc.reads,
            space_reads: acc.space_reads,
            retained: acc.retained,
            state_reads: acc.state_reads,
            state_seen: acc.state_seen,
        })
    }

    fn build_node<'h>(
        &self,
        cx: &Build<'_>,
        at: usize,
        walk: Walk<'_>,
        acc: &mut Acc,
        mut hook: Option<&mut (dyn Boundaries + 'h)>,
    ) -> Result<UiNode, RuntimeError> {
        if walk.level > cx.nodes.len() {
            return Err(RuntimeError::Package("the ui tree contains a cycle".into()));
        }
        let n = cx
            .nodes
            .get(at)
            .ok_or_else(|| RuntimeError::Package("unknown ui node".into()))?;
        // An anchored node keeps its key: the anchor is the address people and agents use.
        let key = match (walk.prefix, &n.anchor) {
            (Some(prefix), None) => format!("{prefix}~{}", n.key),
            _ => n.key.clone(),
        };
        let events: std::collections::BTreeMap<String, Retained> = n
            .events
            .iter()
            .map(|(name, v)| {
                let r = self.frames.borrow_mut().retain(v.clone());
                acc.retained.push(r);
                (name.clone(), r)
            })
            .collect();
        if let (Some(target), Some(boundaries)) =
            (n.kind.strip_prefix(FOREIGN_VIEW), hook.as_deref_mut())
        {
            let (vt, args) = self.call_of(target, &n.props)?;
            let mut props = n.props.clone();
            props.retain(|name, _| !name.starts_with(ARG_PREFIX));
            let children = boundaries.boundary(Stub {
                key: &key,
                target: vt,
                args,
                lazy: walk.in_list,
                depth: cx.offer.depth.saturating_add(1),
            })?;
            return Ok(UiNode {
                kind: "fragment".to_owned(),
                key,
                props,
                binding: n.binding.clone(),
                events,
                anchor: n.anchor.clone(),
                children,
            });
        }
        // A view call's keys are the same at every call site, so they go under the call's key.
        let calls = n.kind == "fragment" || n.kind.starts_with(FOREIGN_VIEW);
        let below = Walk {
            level: walk.level.saturating_add(1),
            prefix: if calls {
                Some(key.as_str())
            } else {
                walk.prefix
            },
            in_list: n.kind == "list",
        };
        let mut keys = HashSet::new();
        let mut children = Vec::with_capacity(n.children.len());
        for c in &n.children {
            let child = self.build_node(cx, *c, below, acc, hook.as_deref_mut())?;
            if !keys.insert(child.key.clone()) {
                return Err(RuntimeError::Package(format!(
                    "duplicate key `{}` among the children of `{}`",
                    child.key, n.key
                )));
            }
            children.push(child);
        }
        let mut node = UiNode {
            kind: n.kind.clone(),
            key,
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
        Ok(node)
    }

    /// The view a `view:<app>:<view id>` node applies and its arguments, from the node's
    /// `arg.<name>` properties.
    pub(crate) fn call_of(
        &self,
        target: &str,
        props: &std::collections::BTreeMap<String, Value>,
    ) -> Result<(ViewTarget, Vec<Value>), RuntimeError> {
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
                props
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
        Ok((vt, args))
    }

    /// Replace a node applying a view with a `fragment` holding that view's tree, rendered in
    /// place: its reads join the outer view's. The nested view's code is its own package's,
    /// so its action references are authored by that package.
    fn resolve_foreign(
        &self,
        cx: &Build<'_>,
        target: &str,
        node: &mut UiNode,
        acc: &mut Acc,
    ) -> Result<(), RuntimeError> {
        let (vt, args) = self.call_of(target, &node.props)?;
        let nested = self.render_at(&vt, &args, cx.offer.deeper(), None)?;
        let root = acc.absorb(nested);
        "fragment".clone_into(&mut node.kind);
        node.props.retain(|name, _| !name.starts_with(ARG_PREFIX));
        node.children = vec![scope(&node.key, root)];
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
