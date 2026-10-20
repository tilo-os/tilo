//! `embed(source, author: who)` (implementation design 7.8): a block's source rendered inline.
//!
//! The node carries the block's source and its author as `Text`. The host compiles the source
//! against the loaded packages' metadata (cached by the source's hash and the package set),
//! runs it as a nested boundary whose code author is the principal, and replaces the node's
//! children with what the block's value shows. The node's `state` property says which:
//!
//! * `view`: the block was a view; its tree is the only child.
//! * `invocation`: the block was an action application. The child is an `invocation` node
//!   (`state` of `done`, `suggested`, `declined`, `failed` or `denied`, plus `action`, `app`,
//!   and for a suggestion its `run`), holding the action's proposal view when it has one.
//! * `data`: the block was a value; the child is its default rendering.
//! * `error`: the source did not compile or the block failed; the child is an `error` node
//!   with one `text` child per diagnostic.

use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use serde_json::Value as Json;
use tilolang::{BlockKind, CompiledBlock, Diagnostic, PackageMeta, Span};

use crate::core::Core;
use crate::error::RuntimeError;
use crate::exec::InvokeOutcome;
use crate::flow::JournalEntry;
use crate::ids::ModuleHash;
use crate::project::{ProjectOptions, project};
use crate::registry::Package;
use crate::render::{Acc, Build, ViewTarget, scope};
use crate::value::{ActionRef, Value};
use crate::view::UiNode;

/// The app prefix of block modules: they are never visible to other blocks.
const BLOCK_APP_PREFIX: &str = "agent.block.";

/// A compiled and registered block.
pub struct EmbedBlock {
    block: CompiledBlock,
    pkg: Rc<Package>,
}

type Cached = Result<Rc<EmbedBlock>, Rc<Vec<Diagnostic>>>;

/// What an embedded action application did, kept so a re-render does not repeat it.
struct Invocation {
    aref: ActionRef,
    outcome: InvokeOutcome,
}

#[derive(Default)]
pub struct EmbedState {
    blocks: HashMap<String, Cached>,
    invocations: HashMap<String, Rc<Invocation>>,
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
        let (source, author) = (text_prop(node, "source"), text_prop(node, "author"));
        let key = node.key.clone();
        let (state, children) = match (source, author) {
            (Some(s), Some(a)) if !a.is_empty() => {
                match self.embed_content(cx, &key, &s, &a, acc) {
                    Ok(done) => done,
                    Err(e) => ("error", vec![error_node(&key, &[e.to_string()])]),
                }
            }
            _ => (
                "error",
                vec![error_node(
                    &key,
                    &["an embed needs its source and its author as text".to_owned()],
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
        author: &str,
        acc: &mut Acc,
    ) -> Result<(&'static str, Vec<UiNode>), RuntimeError> {
        let block = match self.embed_block(source) {
            Ok(b) => b,
            Err(diags) => {
                let lines: Vec<String> = diags.iter().map(diagnostic_line).collect();
                return Ok(("error", vec![error_node(key, &lines)]));
            }
        };
        let identity = ModuleHash::principal(author);
        match block.block.kind {
            BlockKind::View => {
                let target = ViewTarget {
                    module: block.pkg.hash.clone(),
                    export: "b_main".to_owned(),
                    params: Vec::new(),
                };
                let nested = self.render_at(&target, &[], cx.offer.deeper(), Some(&identity))?;
                acc.absorb(
                    nested.reads,
                    nested.space_reads,
                    nested.retained,
                    nested.runs,
                );
                Ok(("view", vec![scope(key, nested.root)]))
            }
            BlockKind::Action => {
                let child = self.embed_action(cx, key, source, author, &block, acc)?;
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
    fn embed_block(&self, source: &str) -> Cached {
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

    /// Invoke (or suggest) the action a block applies, once for this message and author, and
    /// render the outcome. A re-render shows the recorded outcome; a suggestion's state
    /// follows its journal, so deciding it shows as done or declined.
    fn embed_action(
        &self,
        cx: &Build<'_>,
        key: &str,
        source: &str,
        author: &str,
        block: &EmbedBlock,
        acc: &mut Acc,
    ) -> Result<UiNode, RuntimeError> {
        let memo = format!(
            "{key}\u{0}{}\u{0}{author}",
            hex(&blake3::hash(source.as_bytes()))
        );
        let known = self.embeds.borrow().invocations.get(&memo).cloned();
        let inv = if let Some(inv) = known {
            inv
        } else {
            let identity = ModuleHash::principal(author);
            let value = self.eval_block(&block.pkg, &block.block.value_type, Some(&identity))?;
            let Value::Action(aref) = value else {
                return Err(RuntimeError::Package(
                    "the block did not produce an action".into(),
                ));
            };
            let outcome = self.invoke_direct(cx.offer.viewer, &aref)?;
            let inv = Rc::new(Invocation { aref, outcome });
            self.embeds
                .borrow_mut()
                .invocations
                .insert(memo, Rc::clone(&inv));
            inv
        };
        acc.runs = true;
        let (state, run) = self.invocation_state(&inv.outcome);
        let (app, name, proposal) = self.action_facts(&inv.aref);
        let mut props = vec![
            ("state", Value::text(state)),
            ("action", Value::text(&name)),
            ("app", Value::text(&app)),
        ];
        if let Some(run) = &run {
            props.push(("run", Value::text(run)));
        }
        if let InvokeOutcome::Done(v) = &inv.outcome
            && *v != Value::Unit
        {
            let shown = project(self, v, ProjectOptions::default());
            props.push(("result", Value::text(shown.to_string())));
        }
        let mut children = Vec::new();
        if state == "suggested"
            && let Some((view, args)) = proposal
        {
            let target = self.view_target(&view)?;
            let nested = self.render_at(&target, &args, cx.offer.deeper(), None)?;
            acc.absorb(
                nested.reads,
                nested.space_reads,
                nested.retained,
                nested.runs,
            );
            children.push(scope(&format!("{key}/invocation"), nested.root));
        }
        Ok(node(
            "invocation",
            format!("{key}/invocation"),
            &props,
            children,
        ))
    }

    /// How an invocation stands now, and the run waiting on a decision.
    fn invocation_state(&self, outcome: &InvokeOutcome) -> (&'static str, Option<String>) {
        match outcome {
            InvokeOutcome::Done(_) => ("done", None),
            InvokeOutcome::Denied => ("denied", None),
            InvokeOutcome::Suggested(s) => {
                let journal = self.store.borrow_mut().load_journal(&s.run);
                match journal {
                    Ok(Some(j)) if j.pending.is_none() => {
                        let state = match j.entries.last() {
                            Some(JournalEntry::Denied { .. }) => "declined",
                            Some(JournalEntry::Failed { .. }) => "failed",
                            _ => "done",
                        };
                        (state, None)
                    }
                    _ => ("suggested", Some(s.run.clone())),
                }
            }
        }
    }

    /// The app and name of an applied action or flow, and the proposal view with the
    /// arguments it takes.
    fn action_facts(&self, aref: &ActionRef) -> (String, String, Option<(String, Vec<Value>)>) {
        let reg = self.registry.borrow();
        if let Some(pkg) = reg.action_package(&aref.action)
            && let Some(meta) = pkg.action(&aref.action)
        {
            let proposal = meta.proposal.as_ref().map(|p| {
                let args = p
                    .args
                    .iter()
                    .filter_map(|n| meta.params.iter().position(|x| &x.name == n))
                    .filter_map(|i| aref.args.get(i).cloned())
                    .collect();
                (p.view.clone(), args)
            });
            return (pkg.meta.app.clone(), meta.name.clone(), proposal);
        }
        if let Some(pkg) = reg.flow_package(&aref.action)
            && let Some(meta) = pkg.flow(&aref.action)
        {
            return (pkg.meta.app.clone(), meta.name.clone(), None);
        }
        (String::new(), aref.action.clone(), None)
    }
}
