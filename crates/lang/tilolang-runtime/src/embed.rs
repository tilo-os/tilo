//! `embed(source, author: who)`: a block's source rendered inline, as in a chat message.
//!
//! The node carries the block's source, its author as a `Principal`, and for an action block
//! the `Invocation` the host recorded when the message was posted. The host compiles the source
//! against the loaded packages' metadata (cached by the source's hash and the package set),
//! runs it as a nested boundary whose code author is the principal, and replaces the node's
//! children with what the block's value shows. The node's `state` property says which:
//!
//! * `view`: the block was a view, or the source a written view (`crate::written`) that takes
//!   nothing; its tree is the only child.
//! * `definition`: the source is a written view that takes a resource, which is drawn where
//!   that resource is shown; the child says what it shows.
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
use tilolang::{BlockKind, CompiledBlock, Diagnostic, INVOCATION_TYPE, Span};

use crate::core::Core;
use crate::error::RuntimeError;
use crate::host_ui::shown_node;
use crate::ids::{ModuleHash, Principal};
use crate::project::{ProjectOptions, project};
use crate::registry::Package;
use crate::render::{Acc, Build, ViewTarget, scope};
use crate::value::{ResourceRef, Value, from_json};
use crate::view::UiNode;
use crate::written::diagnostic_line;

/// A compiled and registered block.
pub struct EmbedBlock {
    pub(crate) block: CompiledBlock,
    pub(crate) pkg: Rc<Package>,
}

pub(crate) type Cached = Result<Rc<EmbedBlock>, Rc<Vec<Diagnostic>>>;

#[derive(Default)]
pub struct EmbedState {
    blocks: HashMap<String, Cached>,
    /// Written views (`crate::written`), by their source and the packages they saw.
    pub(crate) views: HashMap<String, crate::written::CachedView>,
    /// The writer of each written view's module, as its code author.
    pub(crate) authors: HashMap<ModuleHash, ModuleHash>,
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

/// The kind of a text: the toolkit's `Text` component.
const TEXT: &str = "ui.Text";

/// `proposals(of)`: the proposals waiting on a resource, each as the card a person accepts or
/// dismisses it with.
pub(crate) const PROPOSALS: &str = "proposals";

fn text_node(key: String, value: &str) -> UiNode {
    node(TEXT, key, &[("value", Value::text(value))], Vec::new())
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
                TEXT,
                format!("{key}/label"),
                &[
                    ("value", Value::text(label)),
                    ("role", Value::text("heading")),
                ],
                Vec::new(),
            )];
            kids.push(node(
                TEXT,
                format!("{key}/type"),
                &[("value", Value::text(ty)), ("role", Value::text("caption"))],
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
                                TEXT,
                                format!("{fk}/name"),
                                &[
                                    ("value", Value::text(name)),
                                    ("role", Value::text("caption")),
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
                Value::Ref(r) => Some(ResourceRef::clone(&r.target)),
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
        if tilolang::written::is_declaration(source) {
            return self.embed_written(cx, key, source, author, acc);
        }
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
                    view: String::new(),
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
        let (visible, mut fingerprint) = self.visible_metas();
        fingerprint.update(source.as_bytes());
        let cache_key = hex(&fingerprint.finalize());
        if let Some(hit) = self.embeds.borrow().blocks.get(&cache_key) {
            return hit.clone();
        }
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

    /// An embedded written view (`crate::written`): one that takes nothing is drawn, with its
    /// writer as its code author; one that takes a resource is drawn where that resource is
    /// shown, so here it says what it shows.
    fn embed_written(
        &self,
        cx: &Build<'_>,
        key: &str,
        source: &str,
        author: &Principal,
        acc: &mut Acc,
    ) -> Result<(&'static str, Vec<UiNode>), RuntimeError> {
        let view = match self.written_view(source, author) {
            Ok(view) => view,
            Err(diags) => {
                let lines: Vec<String> = diags.iter().map(diagnostic_line).collect();
                return Ok(("error", vec![error_node(key, &lines)]));
            }
        };
        if !view.view.params.is_empty() {
            let shows = view
                .view
                .params
                .iter()
                .map(|p| p.ty.rsplit('.').next().unwrap_or(&p.ty).to_owned())
                .collect::<Vec<_>>()
                .join(", ");
            return Ok((
                "definition",
                vec![text_node(
                    format!("{key}/shows"),
                    &format!("Shows a {shows}"),
                )],
            ));
        }
        let nested = self.render_at(&view.target(), &[], cx.offer.deeper(), None)?;
        let root = acc.absorb(nested);
        Ok(("view", vec![scope(key, root)]))
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
            ("label", Value::text(self.action_label(&rec.name))),
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
                props.push(("view", Value::Bool(true)));
                children.push(scope(&card_key, root));
            }
        }
        Ok(node("invocation", card_key, &props, children))
    }

    /// The proposals waiting on `of`: invocation records a person proposed that nobody has
    /// accepted or dismissed yet, with what each says, in the order the store keeps what links
    /// to `of`. Reading them reads what links to `of`, so a view that draws them runs again when
    /// one arrives.
    pub(crate) fn proposals_of(
        &self,
        of: &ResourceRef,
    ) -> Vec<(ResourceRef, crate::InvocationRecord)> {
        self.read_links_of(of);
        let Ok(links) = self.store.borrow().linked(of) else {
            return Vec::new();
        };
        links
            .into_iter()
            .map(|link| link.of)
            .filter(|at| at.ty == INVOCATION_TYPE)
            .filter_map(|at| match self.read_invocation(&at) {
                Ok(rec) if rec.proposed && rec.state == "suggested" => Some((at, rec)),
                _ => None,
            })
            .collect()
    }

    /// The proposals waiting on `of`, as the resources that hold them.
    pub(crate) fn pending_proposals(&self, of: &ResourceRef) -> Vec<ResourceRef> {
        self.proposals_of(of)
            .into_iter()
            .map(|(inv, _)| inv)
            .collect()
    }

    /// A proposal's card: the invocation card the host draws for a waiting action, and what the
    /// action would do in words, when it declares no view of it.
    fn proposal_card(
        &self,
        cx: &Build<'_>,
        key: &str,
        inv: &ResourceRef,
        acc: &mut Acc,
    ) -> Result<UiNode, RuntimeError> {
        let rec = self.read_invocation(inv)?;
        let mut card = self.invocation_card(cx, key, Some(inv), acc)?;
        card.props.insert("proposed".to_owned(), Value::Bool(true));
        let args = serde_json::from_str::<serde_json::Value>(&rec.args)
            .ok()
            .and_then(|json| from_json(&json).ok())
            .and_then(|value| match value {
                Value::List(args) => Some(args),
                _ => None,
            })
            .unwrap_or_default();
        // When the action declares no view of what it would do, its arguments say it: the
        // words a change to them reads with, and each value as its type shows it (a day in
        // the person's format), so a sentence carries no identifier and no stored text.
        if self.action_facts(&rec.action, &args).2.is_none() {
            let lines = self.proposal_lines(&rec.action, &args);
            if lines.is_empty() {
                card.children.push(text_node(
                    format!("{key}/says"),
                    &self.action_label(&rec.action),
                ));
            }
            let phrase = |id: &str| self.word_args(id, &[], id);
            for (i, line) in lines.iter().enumerate() {
                card.children
                    .push(text_node(format!("{key}/says{i}/w"), &line.words));
                if let Some(value) = &line.value {
                    card.children
                        .push(shown_node(&format!("{key}/says{i}/v"), value, &phrase));
                }
            }
        }
        Ok(card)
    }

    /// `proposals(of)` as the fragment of one card per proposal waiting on the resource.
    pub(crate) fn resolve_proposals(&self, cx: &Build<'_>, node: &mut UiNode, acc: &mut Acc) {
        let Some(Value::Ref(of)) = node.props.get("of").cloned() else {
            node.kind = String::from("fragment");
            node.children = Vec::new();
            return;
        };
        let mut cards = Vec::new();
        for (i, inv) in self.pending_proposals(&of.target).iter().enumerate() {
            let key = format!("{}/{i}", node.key);
            if let Ok(card) = self.proposal_card(cx, &key, inv, acc) {
                cards.push(card);
            }
        }
        node.kind = String::from("fragment");
        node.children = cards;
    }

    /// The app and name of an action by id, and the proposal view with the arguments it takes.
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
        (String::new(), action.to_owned(), None)
    }
}
