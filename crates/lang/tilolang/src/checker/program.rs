//! An agent's program (agents.md, "One language, one tool"): statements run as a flow written by
//! the agent, over every mounted app. It reads, finds, loops and applies actions like a flow, and
//! adds the three words a session needs: `print` says something back, `find` takes the palette's
//! query string, `read` gives a resource as text. Its top-level `let`s are kept for the next
//! program of the session ([`ProgramResult::lets`]).

use super::{Checker, Ctx, Ex, FnState, conv, store_op};
use crate::abi::Imp;
use crate::ast::{self, ExprKind, StmtKind};
use crate::diag::{Diagnostic, Span};
use crate::ir::{self, Ek, FuncKind, Stmt};
use crate::resolve::{Env, ParamInfo, TypeInfo};
use crate::types::{Ty, TypeKey};

/// The words only a program has.
pub(crate) const PROGRAM_BUILTINS: &[&str] = &["print", "find", "read", "grep", "describe"];

pub use crate::host::RESOURCE_TYPE_NAME;

/// The type key of [`RESOURCE_TYPE_NAME`].
#[must_use]
pub fn resource_key() -> TypeKey {
    TypeKey {
        app: crate::host::HOST_APP.to_owned(),
        name: RESOURCE_TYPE_NAME.to_owned(),
    }
}

/// A checked program.
pub struct ProgramResult {
    pub module: ir::Module,
    /// The names its top-level `let`s bound, with their types, in the order last bound.
    pub lets: Vec<(String, Ty)>,
}

/// Checks a program whose earlier bindings are `params`, lowered to the flow `export`.
pub fn check_program(
    env: &Env,
    stmts: &[ast::Stmt],
    params: &[ParamInfo],
    export: &str,
    diags: &mut Vec<Diagnostic>,
) -> ProgramResult {
    let mut c = Checker {
        env,
        diags,
        st: FnState::default(),
        synth: Vec::new(),
        synth_base: 0,
        states: Vec::new(),
        at_event: Vec::new(),
    };
    c.begin(Ctx::Flow, params, Ty::Unit, export);
    c.st.program = true;
    let mut lets: Vec<(String, Ty)> = Vec::new();
    for (at, s) in stmts.iter().enumerate() {
        let last = at + 1 == stmts.len();
        match &s.kind {
            StmtKind::Let { name, .. } => {
                c.stmt(s);
                if let Some((id, ty)) = c.lookup(&name.name)
                    && ty != Ty::Error
                {
                    let kept = Checker::handle_of(Ex::local(id, ty.clone()));
                    c.emit(Stmt::Eval(Ex::host(
                        Imp::Keep,
                        vec![Ex::str(&name.name), kept],
                        Ty::Unit,
                    )));
                    lets.retain(|(bound, _)| *bound != name.name);
                    lets.push((name.name.clone(), ty));
                }
            }
            // The last line of a program says its value, as a REPL does, unless it is a
            // statement (an `if`, a `match`, a block) or has none.
            StmtKind::Expr(e) if last && echoes(e) => {
                let ex = c.expr(e, None);
                if matches!(ex.ty, Ty::Unit | Ty::Error) {
                    c.emit(Stmt::Eval(ex));
                } else {
                    let said = c.print_values(vec![ex]);
                    c.emit(Stmt::Eval(said));
                }
            }
            _ => c.stmt(s),
        }
    }
    let mut f = c.end(FuncKind::Flow, "main", export);
    f.nparams = params.len();
    ProgramResult {
        module: ir::Module {
            funcs: vec![f],
            views: Vec::new(),
            states: Vec::new(),
            at_event: Vec::new(),
        },
        lets,
    }
}

/// Whether a last line is a value to say: not a statement, not a `print`.
fn echoes(e: &ast::Expr) -> bool {
    match &e.kind {
        ExprKind::If { .. } | ExprKind::Match { .. } | ExprKind::Block(_) => false,
        ExprKind::Call { callee, .. } => {
            !matches!(&callee.kind, ExprKind::Ident(name) if name == "print")
        }
        _ => true,
    }
}

impl Checker<'_> {
    /// The words only a program has, when the code being checked is a program.
    pub(super) fn program_builtins(&self) -> impl Iterator<Item = &'static str> {
        let words: &[&str] = if self.st.program {
            PROGRAM_BUILTINS
        } else {
            &[]
        };
        words.iter().copied()
    }

    /// `print`, `find` or `read` in a program; `None` for any other name or code.
    pub(super) fn program_builtin(
        &mut self,
        name: &str,
        args: &[ast::Arg],
        span: Span,
    ) -> Option<Ex> {
        if !self.st.program {
            return None;
        }
        Some(match name {
            "print" => self.builtin_print(args),
            "find" => self.builtin_find(args, span),
            "read" => self.builtin_read(args, span),
            "grep" => self.builtin_grep(args, span),
            "describe" => self.builtin_describe(args, span),
            _ => return None,
        })
    }

    /// `r.f = v` in a program: the resource's field edit, which the host invokes at the app
    /// that holds `r` as the program's change (agents.md, "How a program runs"). Only a whole
    /// value is set: a collection changes through the app's actions, a counter by what an
    /// action adds.
    pub(super) fn program_edit(
        &mut self,
        base: Ex,
        key: &TypeKey,
        f: &crate::resolve::FieldInfo,
        op: ast::AssignOp,
        value: &ast::Expr,
        span: Span,
    ) {
        use crate::types::MergeKind;
        let app = key.app.rsplit('.').next().unwrap_or(&key.app);
        let refused = match (op, f.merge) {
            (_, MergeKind::Structural) => Some(format!(
                "`{}.{}` is a {}; a program changes it with {app}'s actions",
                key.name,
                f.name,
                f.ty.display(&self.env.app),
            )),
            (ast::AssignOp::Add | ast::AssignOp::Sub, _) => Some(format!(
                "a program sets `{}` to a whole value: `x.{} = ...`",
                f.name, f.name
            )),
            (ast::AssignOp::Set, MergeKind::Counter) => Some(format!(
                "`{}` is a counter; a program changes it with {app}'s actions",
                f.name
            )),
            _ => None,
        };
        if let Some(why) = refused {
            self.err("E042", span, why);
            let _ = self.expr(value, None);
            return;
        }
        let v = self.expr_as(value, &f.ty);
        let v = Checker::handle_of(v);
        self.emit(Stmt::Eval(Ex::host(
            Imp::EditField,
            vec![base, Ex::str(&f.id), v],
            Ty::Unit,
        )));
    }

    /// A tuple of values already checked, as `print` hands them to the host.
    fn tuple_of(&mut self, items: Vec<Ex>) -> Ex {
        let tmp = self.fresh(Ty::Error);
        let count = i32::try_from(items.len()).unwrap_or(i32::MAX);
        let mut stmts = vec![Stmt::Set(
            tmp,
            Ex::host(
                Imp::TupleNew,
                vec![Ex::new(Ek::I32(count), Ty::Bool)],
                Ty::Text,
            ),
        )];
        let mut tys = Vec::new();
        for (i, v) in items.into_iter().enumerate() {
            tys.push(v.ty.clone());
            let (imp, c) = store_op(&v.ty, Imp::TupleSetI, Imp::TupleSetF, Imp::TupleSetH);
            let idx = Ex::new(Ek::I32(i32::try_from(i).unwrap_or(i32::MAX)), Ty::Bool);
            stmts.push(Stmt::Eval(Ex::host(
                imp,
                vec![Ex::local(tmp, Ty::Text), idx, conv(c, v, Ty::Int)],
                Ty::Unit,
            )));
        }
        let ty = Ty::Tuple(tys);
        if let Some(l) = self.st.locals.get_mut(tmp as usize) {
            *l = ty.clone();
        }
        Ex::new(Ek::Seq(stmts, Box::new(Ex::local(tmp, ty.clone()))), ty)
    }

    /// Print `values` on one line, separated by spaces, each as its type shows; a list of a
    /// type's resources alone prints a row per resource with the type's filter fields.
    fn print_values(&mut self, mut values: Vec<Ex>) -> Ex {
        if values.len() == 1
            && let Some(list) = values.pop()
        {
            let kept = list.clone();
            if let Some(rows) = self.print_rows(list, None) {
                return rows;
            }
            values.push(kept);
        }
        let types: Vec<String> = values
            .iter()
            .map(|v| v.ty.meta_text(&self.env.app))
            .collect();
        let said = self.tuple_of(values);
        Ex::host(
            Imp::Print,
            vec![said, Ex::str(&serde_json::json!(types).to_string())],
            Ty::Unit,
        )
    }

    /// `print(a, b, ...)`: one line of output, its values separated by spaces; `print(xs,
    /// fields: [.a, .b])` a row per item.
    fn builtin_print(&mut self, args: &[ast::Arg]) -> Ex {
        let fields = args
            .iter()
            .find(|a| a.name.as_ref().is_some_and(|n| n.name == "fields"));
        if let Some(fields) = fields {
            let listed: Vec<&ast::Arg> = args.iter().filter(|a| a.name.is_none()).collect();
            let [list] = listed.as_slice() else {
                self.err(
                    "E022",
                    fields.value.span,
                    "`fields:` prints one list: `print(xs, fields: [.title, .due])`",
                );
                return Ex::error();
            };
            let list = self.expr(&list.value, None);
            if list.ty == Ty::Error {
                return Ex::error();
            }
            return self
                .print_rows(list, Some(fields))
                .unwrap_or_else(Ex::error);
        }
        let mut values = Vec::new();
        for a in args {
            if let Some(n) = &a.name {
                self.err(
                    "E022",
                    n.span,
                    "`print` takes values, and `fields:` for a list's rows: `print(n, n.title)`",
                );
            }
            let v = self.expr(&a.value, None);
            if v.ty == Ty::Unit {
                self.err(
                    "E020",
                    a.value.span,
                    "this expression has no value to print",
                );
                continue;
            }
            values.push(v);
        }
        self.print_values(values)
    }

    /// `find("Note launch video")`: the resources the palette's query finds. A query that
    /// starts with a type's name finds resources of that type, a `List<T>`; any other finds
    /// resources of every app, a `List<host.Resource>` that `.of(T)` narrows.
    fn builtin_find(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        // `find(Note, pinned: true)`: typed, its arguments the type's filters, as a query's.
        if args
            .first()
            .is_some_and(|a| a.name.is_none() && !matches!(a.value.kind, ExprKind::Str(_)))
        {
            return self.query_named("find", args, span);
        }
        let [query] = args else {
            self.err(
                "E022",
                span,
                "`find` takes the query as people type it: `find(\"Note launch is:pinned\")`",
            );
            return Ex::error();
        };
        let named = match &query.value.kind {
            ExprKind::Str(parts) => match parts.as_slice() {
                [ast::StrPart::Lit(text)] => self.query_type(text, query.value.span),
                _ => Ok(None),
            },
            _ => Ok(None),
        };
        let q = self.expr_as(&query.value, &Ty::Text);
        let Ok(info) = named else {
            return Ex::error();
        };
        let (ty_id, elem) = match info {
            Some(info) => (info.id, Ty::Record(info.key)),
            None => (String::new(), Ty::Record(resource_key())),
        };
        Ex::host(
            Imp::Find,
            vec![q, Ex::str(&ty_id)],
            Ty::List(Box::new(elem)),
        )
    }

    /// The type a query's first word names (`Note`, `notes.Note`), when it names one. A first
    /// word written like a type's name that names none is an error that lists the types.
    fn query_type(&mut self, text: &str, span: Span) -> Result<Option<TypeInfo>, ()> {
        let Some(first) = text.split_whitespace().next() else {
            return Ok(None);
        };
        let (alias, name) = match first.rsplit_once('.') {
            Some((alias, name)) => (Some(alias), name),
            None => (None, first),
        };
        if !name.chars().next().is_some_and(char::is_uppercase)
            || !name.chars().all(|c| c.is_alphanumeric() || c == '_')
        {
            return Ok(None);
        }
        let env = self.env;
        let app = alias
            .and_then(|a| env.find_import(a))
            .map(|i| i.app.clone());
        let found: Vec<&TypeInfo> = env
            .types
            .iter()
            .filter(|t| t.key.app != crate::host::HOST_APP && t.key.name == name)
            .filter(|t| app.as_ref().is_none_or(|app| t.key.app == *app))
            .collect();
        match found.as_slice() {
            [one] => Ok(Some((*one).clone())),
            [] => {
                let names: Vec<&str> = env
                    .types
                    .iter()
                    .filter(|t| t.key.app != crate::host::HOST_APP)
                    .map(|t| t.key.name.as_str())
                    .collect();
                self.err(
                    "E105",
                    span,
                    format!(
                        "no type `{first}` to search{}; types: {}",
                        crate::diag::suggestion(name, names.iter().copied()),
                        names.join(", ")
                    ),
                );
                Err(())
            }
            several => {
                let apps: Vec<String> = several
                    .iter()
                    .map(|t| {
                        format!(
                            "{}.{}",
                            crate::resolve::default_alias(&t.key.app),
                            t.key.name
                        )
                    })
                    .collect();
                self.err(
                    "E105",
                    span,
                    format!(
                        "`{name}` is a type of several apps; name one: {}",
                        apps.join(", ")
                    ),
                );
                Err(())
            }
        }
    }

    /// `read(r)`: the resource as text, each field on a line.
    fn builtin_read(&mut self, args: &[ast::Arg], span: Span) -> Ex {
        let Some((what, options)) = args.split_first().filter(|(what, _)| what.name.is_none())
        else {
            self.err("E022", span, "`read` takes a resource: `read(n)`");
            return Ex::error();
        };
        let r = self.expr(&what.value, None);
        match &r.ty {
            Ty::Record(key) if !crate::host::is_value(key) => {}
            Ty::Error => return Ex::error(),
            other => {
                let t = self.show(other);
                self.err(
                    "E020",
                    what.value.span,
                    format!("`read` takes a resource; this is a {t}"),
                );
                return Ex::error();
            }
        }
        self.read_with(r, options)
    }

    /// `list.of(Note)`: the resources of the list that are of that type, as that type.
    pub(super) fn list_of(&mut self, list: Ex, args: &[ast::Arg], span: Span) -> Ex {
        let named = match args {
            [a] if a.name.is_none() => self.type_named(&a.value).or_else(|| {
                // A program has no types of its own: a bare name is the one app's that has it.
                let ExprKind::Ident(name) = &a.value.kind else {
                    return None;
                };
                let mut found = self
                    .env
                    .types
                    .iter()
                    .filter(|t| t.key.app != crate::host::HOST_APP && t.key.name == *name);
                let one = found.next()?;
                found.next().is_none().then(|| Ty::Record(one.key.clone()))
            }),
            _ => None,
        };
        let Some(Ty::Record(key)) = named else {
            self.err(
                "E022",
                span,
                "`.of` takes the type to keep: `hits.of(Note)`",
            );
            return Ex::error();
        };
        let Some(info) = self.env.type_info(&key) else {
            return Ex::error();
        };
        let id = info.id.clone();
        Ex::host(
            Imp::OfType,
            vec![list, Ex::str(&id)],
            Ty::List(Box::new(Ty::Record(key))),
        )
    }
}
