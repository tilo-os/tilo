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
/// The prefix of the kind of the node a UI loop lowers to (`for:<app>:<row view id>`): one row
/// per item of its `items`, each the row view called with the item and the node's `arg.<name>`
/// properties, and a boundary of its own.
const FOR_EACH: &str = "for:";
/// The property of a loop's node that holds its items.
const ITEMS: &str = "items";
/// The property of a loop's node that names the parameter of its row view the item goes to.
const EACH: &str = "each";
/// The property of a loop's node that holds the key suffix of the rows around it.
const SUFFIX: &str = "suffix";
/// The parameter of a row view that takes the row's key suffix (`<suffix>/<item key>`).
const ROW_KEY: &str = "$key";

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
            let rows = match cx.nodes.get(*c) {
                Some(loop_node) if loop_node.kind.starts_with(FOR_EACH) => {
                    self.build_rows(cx, loop_node, below, acc, hook.as_deref_mut())?
                }
                _ => vec![self.build_node(cx, *c, below, acc, hook.as_deref_mut())?],
            };
            for child in rows {
                if !keys.insert(child.key.clone()) {
                    return Err(RuntimeError::Package(format!(
                        "duplicate key `{}` among the children of `{}`",
                        child.key, n.key
                    )));
                }
                children.push(child);
            }
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

    /// The rows of a loop's node, in its place among its parent's children: per item, a
    /// `fragment` keyed by the item that stands for the row view called on it. With
    /// `boundaries` each row is a boundary of its own (rendered when shown, for a row of a
    /// `list`); without, each is rendered here and its reads join this view's.
    fn build_rows<'h>(
        &self,
        cx: &Build<'_>,
        n: &NodeBuild,
        walk: Walk<'_>,
        acc: &mut Acc,
        mut hook: Option<&mut (dyn Boundaries + 'h)>,
    ) -> Result<Vec<UiNode>, RuntimeError> {
        let target = n.kind.strip_prefix(FOR_EACH).unwrap_or_default();
        let key = match walk.prefix {
            Some(prefix) => format!("{prefix}~{}", n.key),
            None => n.key.clone(),
        };
        let Some(Value::List(items)) = n.props.get(ITEMS) else {
            return Err(RuntimeError::Package(format!(
                "the loop `{}` has no list of items",
                n.key
            )));
        };
        let suffix = match n.props.get(SUFFIX) {
            Some(Value::Text(suffix)) => suffix.as_str(),
            _ => "",
        };
        let (vt, args_of) = self.rows_call(target, &n.props)?;
        // The sizing and placement the compiler took out of a one-node body: each row's.
        let layout: std::collections::BTreeMap<String, Value> = n
            .props
            .iter()
            .filter(|(name, _)| {
                ![ITEMS, SUFFIX, EACH].contains(&name.as_str()) && !name.starts_with(ARG_PREFIX)
            })
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        let mut rows = Vec::with_capacity(items.len());
        for (i, item) in items.iter().enumerate() {
            let item_key = item_key(item, i);
            let row_key = format!("{key}/{item_key}");
            let args: Vec<Value> = args_of
                .iter()
                .map(|arg| match arg {
                    RowArg::Item => item.clone(),
                    RowArg::Key => Value::Text(format!("{suffix}/{item_key}")),
                    RowArg::Value(value) => value.clone(),
                })
                .collect();
            let children = if let Some(boundaries) = hook.as_deref_mut() {
                boundaries.boundary(Stub {
                    key: &row_key,
                    target: vt.clone(),
                    args,
                    lazy: walk.in_list,
                    depth: cx.offer.depth.saturating_add(1),
                })?
            } else {
                let nested = self.render_at(&vt, &args, cx.offer.deeper(), None)?;
                vec![scope(&row_key, acc.absorb(nested))]
            };
            rows.push(UiNode {
                kind: "fragment".to_owned(),
                key: row_key,
                props: layout.clone(),
                binding: None,
                events: std::collections::BTreeMap::new(),
                anchor: None,
                children,
            });
        }
        Ok(rows)
    }

    /// The view a loop's node calls per row and what it passes each parameter: the item to
    /// the one the node's `each` names, the row's key suffix to `$key`, and the node's
    /// `arg.<name>` properties to the rest.
    fn rows_call(
        &self,
        target: &str,
        props: &std::collections::BTreeMap<String, Value>,
    ) -> Result<(ViewTarget, Vec<RowArg>), RuntimeError> {
        let (app, view) = target
            .split_once(':')
            .ok_or_else(|| RuntimeError::Package(format!("malformed loop node `{target}`")))?;
        let vt = self.view_target(view)?;
        let own = self
            .registry
            .borrow()
            .package(&vt.module)
            .is_some_and(|pkg| pkg.meta.app == app);
        if !own {
            return Err(RuntimeError::Package(format!(
                "the row view {view} does not belong to app `{app}`"
            )));
        }
        let each = match props.get(EACH) {
            Some(Value::Text(each)) => each.as_str(),
            _ => vt.params.first().map_or("", |p| p.name.as_str()),
        };
        let around = vt
            .params
            .iter()
            .map(|p| {
                if p.name == each {
                    return Ok(RowArg::Item);
                }
                if p.name == ROW_KEY {
                    return Ok(RowArg::Key);
                }
                props
                    .get(&format!("{ARG_PREFIX}{}", p.name))
                    .cloned()
                    .map(RowArg::Value)
                    .ok_or_else(|| {
                        RuntimeError::Package(format!(
                            "the loop node for `{view}` is missing the value `{}`",
                            p.name
                        ))
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok((vt, around))
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

/// What keys the row of `item`, the `index`th of its loop: a resource by its id, any other
/// value by its place.
fn item_key(item: &Value, index: usize) -> String {
    match item {
        Value::Ref(r) => r.target.id_string(),
        _ => index.to_string(),
    }
}

/// What a row view's parameter takes.
enum RowArg {
    Item,
    /// The row's key suffix.
    Key,
    Value(Value),
}
