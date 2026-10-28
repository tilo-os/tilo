//! Agents' programs (agents.md section 1): a session that compiles each program against every
//! loaded app, runs it as a flow the agent wrote, and keeps its top-level `let`s for the next.
//!
//! A program is code by its agent: its module's frames carry the agent's principal identity
//! (`ModuleHash::principal`), so what it applies is decided as the agent's code, as a block's
//! is. Its output is what it `print`s. Values print as source: a resource as its reference
//! literal (`@notes.Note:root-1/notes:n2`), so output pastes back into the next program.

use std::fmt::Write as _;
use std::rc::Rc;

use tilolang::{Binding, CompiledProgram, PackageMeta};

use crate::core::{Core, HResult, trap};
use crate::flow::{FlowOutcome, Suggestion};
use crate::ids::Principal;
use crate::meta::{FieldKind, Ty};
use crate::query::{Query, find, resolve};
use crate::registry::FieldInfo;
use crate::runtime::Runtime;
use crate::value::{RefValue, ResourceRef, Value};

/// How many resources `find` answers at most.
pub const FIND_LIMIT: usize = 50;

/// Items of a list a print shows before it says how many more there are.
const PRINT_ITEMS: usize = 50;

/// Characters of one text a read shows before it says it was cut.
const READ_TEXT_CHARS: usize = 4_000;

/// What finds resources by text for `find`: the words, how many at most, the best first.
pub type TextSearch = dyn Fn(&str, usize) -> Vec<ResourceRef>;

/// What one program run collects: its lines of output and the values its `let`s bound.
#[derive(Debug, Default)]
pub struct ProgramIo {
    pub output: Vec<String>,
    pub kept: Vec<(String, Value)>,
}

/// How a program ended.
#[derive(Debug)]
pub enum ProgramOutcome {
    /// It ran to its end.
    Done,
    /// It did not compile: one line per diagnostic, `line:col: code message`.
    Rejected(Vec<String>),
    /// An action it applied asks a person first; the run waits for their answer.
    Suspended(Suggestion),
    /// It stopped: a trap, or an action that failed.
    Failed(String),
}

/// What a program run answers.
#[derive(Debug)]
pub struct ProgramReply {
    pub output: Vec<String>,
    pub outcome: ProgramOutcome,
}

/// A REPL session: the names earlier programs bound, with their values.
#[derive(Debug, Default)]
pub struct Session {
    bound: Vec<(Binding, Value)>,
}

impl Session {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The names bound so far, with their types.
    #[must_use]
    pub fn bindings(&self) -> Vec<Binding> {
        self.bound.iter().map(|(b, _)| b.clone()).collect()
    }

    /// The value bound to `name`.
    #[must_use]
    pub fn value(&self, name: &str) -> Option<&Value> {
        self.bound
            .iter()
            .find(|(b, _)| b.name == name)
            .map(|(_, v)| v)
    }

    /// Compile `source` against every app `runtime` holds and run it as `agent`.
    pub fn run(&mut self, runtime: &Runtime, agent: &Principal, source: &str) -> ProgramReply {
        let visible = runtime.visible_packages();
        let bindings = self.bindings();
        let program = match tilolang::compile_program(source, &visible, &bindings) {
            Ok(program) => program,
            Err(diagnostics) => {
                return ProgramReply {
                    output: Vec::new(),
                    outcome: ProgramOutcome::Rejected(
                        diagnostics
                            .iter()
                            .map(|d| format!("{}:{}: {} {}", d.line, d.col, d.code, d.message))
                            .collect(),
                    ),
                };
            }
        };
        let (io, outcome) =
            runtime.run_program(agent, &program, |param| self.value(param).cloned());
        for (name, value) in io.kept {
            let Some(declared) = program.lets.iter().find(|b| b.name == name) else {
                continue;
            };
            let value = unboxed(value, &declared.ty, &program.meta.app);
            self.bound.retain(|(b, _)| b.name != name);
            self.bound.push((declared.clone(), value));
        }
        ProgramReply {
            output: io.output,
            outcome,
        }
    }
}

/// A kept scalar arrives boxed (`.some(v)`), as a handle carries it; the binding holds `v`.
fn unboxed(value: Value, ty: &str, app: &str) -> Value {
    let scalar = Ty::parse_meta(ty, app).is_some_and(|t| t.is_scalar());
    match value {
        Value::Opt(Some(inner)) if scalar => *inner,
        other => other,
    }
}

impl Runtime {
    /// The newest package of each app, apart from the host and agents' blocks and programs:
    /// what a program sees.
    #[must_use]
    pub fn visible_packages(&self) -> Vec<PackageMeta> {
        let pkgs = self.core.registry.borrow().packages();
        let mut visible: Vec<PackageMeta> = Vec::new();
        for p in pkgs.iter().rev() {
            let app = &p.meta.app;
            if app == tilolang::HOST_APP
                || app.starts_with("agent.block.")
                || app.starts_with(tilolang::PROGRAM_APP_PREFIX)
            {
                continue;
            }
            if !visible.iter().any(|m| m.app == *app) {
                visible.push(p.meta.clone());
            }
        }
        visible.reverse();
        visible
    }

    /// Run a compiled program as `agent`, its parameters given by `bound` by name. Returns what
    /// it printed and kept, and how it ended.
    pub fn run_program(
        &self,
        agent: &Principal,
        program: &CompiledProgram,
        bound: impl Fn(&str) -> Option<Value>,
    ) -> (ProgramIo, ProgramOutcome) {
        let started = (|| {
            self.core.load_block(&program.wasm, &program.meta)?;
            let params = program
                .meta
                .flows
                .first()
                .map(|f| f.params.clone())
                .unwrap_or_default();
            let mut args = Vec::with_capacity(params.len());
            for p in &params {
                args.push(bound(&p.name).ok_or_else(|| {
                    crate::RuntimeError::BadArgs(format!("`{}` is not bound", p.name))
                })?);
            }
            Ok::<_, crate::RuntimeError>(args)
        })();
        let args = match started {
            Ok(args) => args,
            Err(e) => return (ProgramIo::default(), ProgramOutcome::Failed(e.to_string())),
        };
        let outer = self.core.program.replace(Some(ProgramIo::default()));
        let ran = self.core.start_flow(agent, &program.flow, args, Vec::new());
        let io = self.core.program.replace(outer).unwrap_or_default();
        let outcome = match ran {
            Ok(FlowOutcome::Done(_)) => ProgramOutcome::Done,
            Ok(FlowOutcome::Suspended(s)) => ProgramOutcome::Suspended(s),
            Err(e) => ProgramOutcome::Failed(e.to_string()),
        };
        (io, outcome)
    }

    /// Find resources by text for programs' `find`: the host's search, which answers words
    /// with the resources that hold them, the best first.
    pub fn set_text_search(&self, search: impl Fn(&str, usize) -> Vec<ResourceRef> + 'static) {
        *self.core.text_search.borrow_mut() = Some(Box::new(search));
    }

    /// `value` as a program prints it.
    #[must_use]
    pub fn show(&self, value: &Value) -> String {
        self.core.show(value)
    }
}

impl Core {
    pub(crate) fn program_import(
        &self,
        imp: crate::abi::Imp,
        a: &crate::core::Args<'_>,
    ) -> HResult<Option<crate::engine::Wasm>> {
        use crate::abi::Imp;
        match imp {
            Imp::Print => {
                let said = match self.val(a.h(0)?)? {
                    Value::Tuple(items) => items
                        .iter()
                        .map(|v| self.show(v))
                        .collect::<Vec<_>>()
                        .join(" "),
                    other => self.show(&other),
                };
                if let Some(io) = self.program.borrow_mut().as_mut() {
                    io.output.push(said);
                }
                Ok(None)
            }
            Imp::Keep => {
                let name = self.text(a.h(0)?)?;
                let value = self.val(a.h(1)?)?;
                if let Some(io) = self.program.borrow_mut().as_mut() {
                    io.kept.retain(|(kept, _)| *kept != name);
                    io.kept.push((name, value));
                }
                Ok(None)
            }
            Imp::Find => {
                let (query, ty) = (self.text(a.h(0)?)?, self.text(a.h(1)?)?);
                let found = self.journaled_read(|| self.find_import(&query, &ty))?;
                self.ret_h(found)
            }
            Imp::ReadText => {
                let Value::Ref(r) = self.val(a.h(0)?)? else {
                    return trap("read takes a resource");
                };
                let text = self.journaled_read(|| Ok(Value::Text(self.read_text(&r.target))))?;
                self.ret_h(text)
            }
            Imp::OfType => {
                let ty = self.text(a.h(1)?)?;
                let Value::List(items) = self.val(a.h(0)?)? else {
                    return trap("of takes a list");
                };
                let kept = items
                    .into_iter()
                    .filter(|v| matches!(v, Value::Ref(r) if r.target.ty == ty))
                    .collect();
                self.ret_h(Value::List(kept))
            }
            _ => trap("not a program import"),
        }
    }

    /// `find(query)`: the resources the palette's query finds, of the type `ty` (an id) when
    /// it is not empty. Words are searched as text by the host's search; filters and the order
    /// are the type's, as a typed query's are.
    fn find_import(&self, text: &str, ty: &str) -> HResult<Value> {
        let query: Query = find(text).map_err(|why| format!("find `{text}`: {why}"))?;
        if query.scope.is_some() {
            return trap(format!(
                "find `{text}`: `in:` is not supported by find yet; filter the results: `hits.filter(n -> ...)`"
            ));
        }
        let ty = (!ty.is_empty()).then(|| ty.to_owned());
        let author = self.module_of_frame()?;
        let as_value = |target: ResourceRef| {
            Value::Ref(RefValue {
                target: Rc::new(target),
                author: Some(author.clone()),
            })
        };
        let words = query.text.join(" ");
        let filtered = !query.terms.is_empty() || query.sort.is_some();
        let Some(ty) = ty else {
            if filtered {
                return trap(format!(
                    "find `{text}`: filters belong to a type; name it first: `find(\"Note {text}\")`"
                ));
            }
            if words.is_empty() {
                return trap(
                    "find needs words to search for, or a type first: `find(\"Note is:pinned\")`",
                );
            }
            let found = self.search_text(&words)?;
            return Ok(Value::List(found.into_iter().map(as_value).collect()));
        };
        let (pkg, info) =
            self.registry.borrow().type_info(&ty).ok_or_else(|| {
                format!("find: type `{ty}` is not declared by any loaded package")
            })?;
        let Some(declared) = pkg.meta.types.iter().find(|t| t.id == ty) else {
            return trap(format!("find: type `{ty}` is not declared"));
        };
        let filters = Query {
            text: Vec::new(),
            ty: None,
            ..query
        };
        let resolved = resolve(&filters, declared, &pkg.meta.enums, &self.program_clock()?)?;
        let Some(pred) = crate::colls::field_reads(&resolved.pred, &info) else {
            return trap(format!(
                "find: {} cannot be read as its filters ask",
                declared.name
            ));
        };
        if words.is_empty() {
            return self.query_all(&ty, &pred, resolved.order.as_ref());
        }
        let found: Vec<Value> = self
            .search_text(&words)?
            .into_iter()
            .filter(|r| r.ty == ty)
            .map(as_value)
            .collect();
        if filtered {
            self.query_list(found, &pred, resolved.order.as_ref())
        } else {
            Ok(Value::List(found))
        }
    }

    fn search_text(&self, words: &str) -> HResult<Vec<ResourceRef>> {
        let search = self.text_search.borrow();
        let Some(search) = search.as_ref() else {
            return trap(
                "this host searches no text; find by a type and its filters: `find(\"Note is:pinned\")`",
            );
        };
        Ok(search(words, FIND_LIMIT))
    }

    fn program_clock(&self) -> HResult<crate::query::Clock> {
        let now = self
            .clock
            .borrow()
            .as_ref()
            .map_or_else(|| crate::core::EPOCH.to_owned(), |clock| clock());
        Ok(crate::query::Clock::at(&now)?)
    }

    /// `value` as a program prints it: text as it is, a resource as its reference literal and
    /// its title, a list one item per line.
    pub(crate) fn show(&self, value: &Value) -> String {
        match value {
            Value::List(items) => {
                let mut lines: Vec<String> = items
                    .iter()
                    .take(PRINT_ITEMS)
                    .map(|v| self.show(v))
                    .collect();
                if items.len() > PRINT_ITEMS {
                    lines.push(format!(
                        "… {} more",
                        items.len().saturating_sub(PRINT_ITEMS)
                    ));
                }
                if lines.is_empty() {
                    "(none)".to_owned()
                } else {
                    lines.join("\n")
                }
            }
            Value::Coll(c) => match self.coll_items(c) {
                Ok(items) => self.show(&Value::List(items)),
                Err(error) => format!("error: {error}"),
            },
            Value::Ref(r) => {
                let title = self.program_title(&r.target);
                let literal = self.ref_literal(&r.target);
                match title {
                    Some(title) => format!("{literal} {title:?}"),
                    None => literal,
                }
            }
            other => self.show_inline(other),
        }
    }

    /// A value on one line: a resource as its literal alone, a list as its items' literals.
    fn show_inline(&self, value: &Value) -> String {
        match value {
            Value::Unit | Value::Opt(None) => "none".to_owned(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => f.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Text(t) => t.clone(),
            Value::Opt(Some(v)) => self.show_inline(v),
            Value::Ref(r) => self.ref_literal(&r.target),
            Value::List(items) => {
                let shown: Vec<String> = items
                    .iter()
                    .take(PRINT_ITEMS)
                    .map(|v| match v {
                        Value::Text(t) => format!("{t:?}"),
                        other => self.show_inline(other),
                    })
                    .collect();
                let more = if items.len() > PRINT_ITEMS {
                    format!(", … {} more", items.len().saturating_sub(PRINT_ITEMS))
                } else {
                    String::new()
                };
                format!("[{}{more}]", shown.join(", "))
            }
            Value::Coll(c) => match self.coll_items(c) {
                Ok(items) => self.show_inline(&Value::List(items)),
                Err(error) => format!("error: {error}"),
            },
            Value::Tuple(items) => {
                let shown: Vec<String> = items.iter().map(|v| self.show_inline(v)).collect();
                format!("({})", shown.join(", "))
            }
            Value::Enum(e) => {
                let meta = self.registry.borrow().enum_meta(&e.ty);
                let name = meta
                    .as_ref()
                    .and_then(|m| m.variants.iter().find(|v| v.id == e.variant))
                    .map_or_else(|| e.variant.clone(), |v| v.name.clone());
                format!(".{name}")
            }
            Value::Principal(p) => self
                .names
                .borrow()
                .as_ref()
                .map_or_else(|| p.0.clone(), |host| host.name(p)),
            Value::Error(reason) => format!("error: {reason}"),
            Value::Action(a) => format!("action {}", a.action),
            other => other.kind_name().to_owned(),
        }
    }

    /// `@notes.Note:root-1/notes:n2`: how a program writes the resource.
    fn ref_literal(&self, target: &ResourceRef) -> String {
        let found = self.registry.borrow().type_info(&target.ty);
        match found {
            Some((pkg, info)) => {
                let app = &pkg.meta.app;
                let alias = app.rsplit('.').next().unwrap_or(app);
                format!("@{alias}.{}:{}", info.name, target.id_string())
            }
            None => format!("@{}:{}", target.ty, target.id_string()),
        }
    }

    /// The text a resource is called by: its `title`, `name` or `label` field, else its first
    /// text field.
    fn program_title(&self, target: &ResourceRef) -> Option<String> {
        let (_, info) = self.registry.borrow().type_info(&target.ty)?;
        let texts: Vec<&FieldInfo> = info
            .fields
            .iter()
            .filter(|f| f.kind == FieldKind::Text || f.ty == Ty::Text)
            .collect();
        let named = texts
            .iter()
            .find(|f| matches!(f.name.as_str(), "title" | "name" | "label"))
            .or_else(|| texts.first())?;
        match self.read_value(target, named, None).ok()? {
            Value::Text(t) if !t.trim().is_empty() => Some(t.chars().take(120).collect()),
            _ => None,
        }
    }

    /// A resource as `read` gives it: its literal and title, then each field on a line.
    pub(crate) fn read_text(&self, target: &ResourceRef) -> String {
        let mut out = self.show(&Value::Ref(RefValue {
            target: Rc::new(target.clone()),
            author: None,
        }));
        let Some((_, info)) = self.registry.borrow().type_info(&target.ty) else {
            return out;
        };
        for f in &info.fields {
            let shown = match self.read_value(target, f, None) {
                Ok(Value::Text(t)) => {
                    let chars = t.chars().count();
                    if chars > READ_TEXT_CHARS {
                        let cut: String = t.chars().take(READ_TEXT_CHARS).collect();
                        format!("{cut}… ({chars} characters)")
                    } else {
                        t
                    }
                }
                Ok(Value::List(items)) if f.kind == FieldKind::Collection => {
                    let mut listed = format!("{} items", items.len());
                    for item in items.iter().take(PRINT_ITEMS) {
                        listed.push_str("\n    ");
                        listed.push_str(&self.show(item));
                    }
                    listed
                }
                Ok(value) => self.show_inline(&value),
                Err(error) => format!("error: {error}"),
            };
            let _ = write!(out, "\n  {}: {shown}", f.name);
        }
        out
    }
}
