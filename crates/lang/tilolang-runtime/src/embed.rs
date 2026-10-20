//! `embed(source, author: who)` (implementation design 7.8): a block's source rendered inline.
//!
//! The node carries the block's source, its author as a `Principal`, and for an action block
//! the `Invocation` the host recorded when the message was posted. The host compiles the source
//! against the loaded packages' metadata (cached by the source's hash and the package set),
//! runs it as a nested boundary whose code author is the principal, and replaces the node's
//! children with what the block's value shows. The node's `state` property says which:
//!
//! * `view`: the block was a view; its tree is the only child.
//! * `invocation`: the block was an action application. Nothing runs here: the child is an
//!   `invocation` node drawn from the recorded resource (`state` of `pending` before the host
//!   has recorded one, then `done`, `suggested`, `declined`, `failed` or `denied`, plus
//!   `action`, `app`, `result`, the `invocation` reference and `can_resolve`), holding the
//!   action's proposal view while it is a suggestion and the action has one.
//! * `data`: the block was a value; the child is its default rendering.
//! * `error`: the source did not compile or the block failed; the child is an `error` node
//!   with one `text` child per diagnostic.

use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use serde_json::Value as Json;
use tilolang::{BlockKind, CompiledBlock, Diagnostic, PackageMeta, Span};

use crate::core::Core;
use crate::error::RuntimeError;
use crate::ids::{ModuleHash, Principal};
use crate::project::{ProjectOptions, project};
use crate::registry::Package;
use crate::render::{Acc, Build, ViewTarget, scope};
use crate::value::{ResourceRef, Value, from_json};
use crate::view::UiNode;

/// The app prefix of block modules: they are never visible to other blocks.
const BLOCK_APP_PREFIX: &str = "agent.block.";

/// A compiled and registered block.
pub struct EmbedBlock {
    pub(crate) block: CompiledBlock,
    pub(crate) pkg: Rc<Package>,
}

pub(crate) type Cached = Result<Rc<EmbedBlock>, Rc<Vec<Diagnostic>>>;

#[derive(Default)]
pub struct EmbedState {
    blocks: HashMap<String, Cached>,
}

fn hex(h: &blake3::Hash) -> String {
    h.to_hex().to_string()
}

fn text_prop(node: &UiNode, name: &str) -> Option<String> {
    match node.props.get(name) {
        Some(Value::Text(t)) => Some(t.clone()),
        _ => None,
    }
}

fn node(kind: &str, key: String, props: &[(&str, Value)], children: Vec<UiNode>) -> UiNode {
    UiNode {
        kind: kind.to_owned(),
        key,
        props: props
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect::<BTreeMap<_, _>>(),
        binding: None,
        events: BTreeMap::new(),
        anchor: None,
        children,
    }
}

fn text_node(key: String, value: &str) -> UiNode {
    node("text", key, &[("value", Value::text(value))], Vec::new())
}

fn error_node(key: &str, lines: &[String]) -> UiNode {
    let children = lines
        .iter()
        .enumerate()
        .map(|(i, l)| text_node(format!("{key}/e{i}"), l))
        .collect();
    let count = i64::try_from(lines.len()).unwrap_or(i64::MAX);
    node(
        "error",
        format!("{key}/error"),
        &[("count", Value::Int(count))],
        children,
    )
}

fn diagnostic_line(d: &Diagnostic) -> String {
    format!("{}:{}: {} {}", d.line, d.col, d.code, d.message)
}

/// The default rendering of a projected value.
fn data_node(key: &str, v: &Json) -> UiNode {
    match v {
        Json::Null => text_node(key.to_owned(), "none"),
        Json::Bool(b) => text_node(key.to_owned(), &b.to_string()),
        Json::Number(n) => text_node(key.to_owned(), &n.to_string()),
        Json::String(s) => text_node(key.to_owned(), s),
        Json::Array(items) => column(key, items.iter().collect()),
        Json::Object(m) if m.contains_key("fields") || m.contains_key("label") => {
            let label = m.get("label").and_then(Json::as_str).unwrap_or_default();
            let ty = m.get("type").and_then(Json::as_str).unwrap_or_default();
            let mut kids = vec![node(
                "heading",
                format!("{key}/label"),
                &[("value", Value::text(label))],
                Vec::new(),
            )];
            kids.push(node(
                "text",
                format!("{key}/type"),
                &[
                    ("value", Value::text(ty)),
                    ("style", Value::text("caption")),
                ],
                Vec::new(),
            ));
            if let Some(Json::Object(fields)) = m.get("fields") {
                for (name, value) in fields {
                    let fk = format!("{key}/f:{name}");
                    kids.push(node(
                        "row",
                        fk.clone(),
                        &[],
                        vec![
                            node(
                                "text",
                                format!("{fk}/name"),
                                &[
                                    ("value", Value::text(name)),
                                    ("style", Value::text("caption")),
                                ],
                                Vec::new(),
                            ),
                            data_node(&format!("{fk}/value"), value),
                        ],
                    ));
                }
            }
            node("column", key.to_owned(), &[], kids)
        }
        Json::Object(m) if m.contains_key("items") => {
            let items = m.get("items").and_then(Json::as_array);
            column(key, items.map(|a| a.iter().collect()).unwrap_or_default())
        }
        Json::Object(m) => {
            let kids = m
                .iter()
                .map(|(name, value)| {
                    let fk = format!("{key}/f:{name}");
                    node(
                        "row",
                        fk.clone(),
                        &[],
                        vec![
                            text_node(format!("{fk}/name"), name),
                            data_node(&format!("{fk}/value"), value),
                        ],
                    )
                })
                .collect();
            node("column", key.to_owned(), &[], kids)
        }
    }
}

fn column(key: &str, items: Vec<&Json>) -> UiNode {
    let kids = items
        .into_iter()
        .enumerate()
        .map(|(i, v)| data_node(&format!("{key}/d{i}"), v))
        .collect();
    node("column", key.to_owned(), &[], kids)
}

impl Core {
    /// Replace the children of an `embed` node with what its block shows. Nothing here fails
    /// the surrounding view: a block that does not compile or run is an `error` child.
    pub(crate) fn resolve_embed(&self, cx: &Build<'_>, node: &mut UiNode, acc: &mut Acc) {
        let source = text_prop(node, "source");
        let author = match node.props.get("author") {
            Some(Value::Principal(p)) => Some(p.clone()),
            _ => None,
        };
        let invocation = match node.props.get("invocation") {
            Some(Value::Opt(Some(inner))) => match &**inner {
                Value::Ref(r) => Some(r.target.clone()),
                _ => None,
            },
            _ => None,
        };
        let key = node.key.clone();
        let (state, children) = match (source, author) {
            (Some(s), Some(a)) => {
                match self.embed_content(cx, &key, &s, &a, invocation.as_ref(), acc) {
                    Ok(done) => done,
                    Err(e) => ("error", vec![error_node(&key, &[e.to_string()])]),
                }
            }
            _ => (
                "error",
                vec![error_node(
                    &key,
                    &[
                        "an embed needs its source as text and its author as a principal"
                            .to_owned(),
                    ],
                )],
            ),
        };
        node.props.insert("state".into(), Value::text(state));
        node.children = children;
    }

    fn embed_content(
        &self,
        cx: &Build<'_>,
        key: &str,
        source: &str,
        author: &Principal,
        invocation: Option<&ResourceRef>,
        acc: &mut Acc,
    ) -> Result<(&'static str, Vec<UiNode>), RuntimeError> {
        let block = match self.embed_block(source) {
            Ok(b) => b,
            Err(diags) => {
                let lines: Vec<String> = diags.iter().map(diagnostic_line).collect();
                return Ok(("error", vec![error_node(key, &lines)]));
            }
        };
        let identity = ModuleHash::principal(&author.0);
        match block.block.kind {
            BlockKind::View => {
                let target = ViewTarget {
                    module: block.pkg.hash.clone(),
                    export: "b_main".to_owned(),
                    params: Vec::new(),
                };
                let nested = self.render_at(&target, &[], cx.offer.deeper(), Some(&identity))?;
                let root = acc.absorb(nested);
                Ok(("view", vec![scope(key, root)]))
            }
            BlockKind::Action => {
                let child = self.invocation_card(cx, key, invocation, acc)?;
                Ok(("invocation", vec![child]))
            }
            BlockKind::Data => {
                let value =
                    self.eval_block(&block.pkg, &block.block.value_type, Some(&identity))?;
                let shown = project(self, &value, ProjectOptions::default());
                Ok(("data", vec![data_node(&format!("{key}/data"), &shown)]))
            }
        }
    }

    /// The block for a source, compiled against the packages loaded now. The cache key is the
    /// source's hash and the set of packages it was compiled against, so loading another
    /// package makes the same source compile again.
    pub(crate) fn embed_block(&self, source: &str) -> Cached {
        let pkgs: Vec<Rc<Package>> = self
            .registry
            .borrow()
            .packages()
            .into_iter()
            .filter(|p| !p.meta.app.starts_with(BLOCK_APP_PREFIX))
            .collect();
        let mut fingerprint = blake3::Hasher::new();
        fingerprint.update(source.as_bytes());
        for p in &pkgs {
            fingerprint.update(p.hash.0.as_bytes());
        }
        let cache_key = hex(&fingerprint.finalize());
        if let Some(hit) = self.embeds.borrow().blocks.get(&cache_key) {
            return hit.clone();
        }
        // The newest package of each app is the one a block sees.
        let mut visible: Vec<PackageMeta> = Vec::new();
        for p in pkgs.iter().rev() {
            if !visible.iter().any(|m| m.app == p.meta.app) {
                visible.push(p.meta.clone());
            }
        }
        visible.reverse();
        let built = match tilolang::compile_block(source, &visible) {
            Ok(block) => match self.load_block(&block.wasm, &block.meta) {
                Ok(pkg) => Ok(Rc::new(EmbedBlock { block, pkg })),
                Err(e) => Err(Rc::new(vec![Diagnostic::new(
                    "E000",
                    Span::default(),
                    e.to_string(),
                )])),
            },
            Err(diags) => Err(Rc::new(diags)),
        };
        self.embeds
            .borrow_mut()
            .blocks
            .insert(cache_key, built.clone());
        built
    }

    /// Draw the invocation the host recorded for an action block. This only reads: the action
    /// ran, or was suggested, when the message was posted.
    fn invocation_card(
        &self,
        cx: &Build<'_>,
        key: &str,
        invocation: Option<&ResourceRef>,
        acc: &mut Acc,
    ) -> Result<UiNode, RuntimeError> {
        let card_key = format!("{key}/invocation");
        let Some(inv) = invocation else {
            return Ok(node(
                "invocation",
                card_key,
                &[("state", Value::text("pending"))],
                Vec::new(),
            ));
        };
        acc.reads.extend(self.invocation_reads(inv));
        let rec = self.read_invocation(inv)?;
        let mut props = vec![
            ("state", Value::text(&rec.state)),
            ("action", Value::text(&rec.name)),
            ("app", Value::text(&rec.app)),
            ("author", Value::Principal(rec.author.clone())),
            ("invocation", Value::resource(inv)),
        ];
        if !rec.result.is_empty() {
            props.push(("result", Value::text(&rec.result)));
        }
        let mut children = Vec::new();
        if rec.state == "suggested" {
            props.push(("run", Value::text(&rec.run)));
            props.push((
                "can_resolve",
                Value::Bool(self.may_decide(cx.offer.viewer, &rec.action)),
            ));
            let args = serde_json::from_str::<serde_json::Value>(&rec.args)
                .ok()
                .and_then(|j| from_json(&j).ok());
            if let Some(Value::List(args)) = args
                && let (_, _, Some((view, view_args))) = self.action_facts(&rec.action, &args)
            {
                let target = self.view_target(&view)?;
                let nested = self.render_at(&target, &view_args, cx.offer.deeper(), None)?;
                let root = acc.absorb(nested);
                children.push(scope(&card_key, root));
            }
        }
        Ok(node("invocation", card_key, &props, children))
    }

    /// The app and name of an action or flow by id, and the proposal view with the arguments
    /// it takes.
    pub(crate) fn action_facts(
        &self,
        action: &str,
        args: &[Value],
    ) -> (String, String, Option<(String, Vec<Value>)>) {
        let reg = self.registry.borrow();
        if let Some(pkg) = reg.action_package(action)
            && let Some(meta) = pkg.action(action)
        {
            let proposal = meta.proposal.as_ref().map(|p| {
                let shown = p
                    .args
                    .iter()
                    .filter_map(|n| meta.params.iter().position(|x| &x.name == n))
                    .filter_map(|i| args.get(i).cloned())
                    .collect();
                (p.view.clone(), shown)
            });
            return (pkg.meta.app.clone(), meta.name.clone(), proposal);
        }
        if let Some(pkg) = reg.flow_package(action)
            && let Some(meta) = pkg.flow(action)
        {
            return (pkg.meta.app.clone(), meta.name.clone(), None);
        }
        (String::new(), action.to_owned(), None)
    }
}
