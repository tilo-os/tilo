//! Name resolution: the module's declarations, its imports, and the signatures the checker
//! needs before it looks at any body.

use crate::ast::{self, Expr, ExprKind, Item, MergeSpec, TypeExpr, TypeExprKind, UnOp};
use crate::diag::{Diagnostic, Span};
use crate::ids::IdsLock;
use crate::meta::PackageMeta;
use crate::prims;
use crate::types::{EffectClass, MergeKind, Ty, TypeKey};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug)]
pub struct FieldInfo {
    pub name: String,
    pub id: String,
    pub ty: Ty,
    pub merge: MergeKind,
    pub doc: String,
    pub default: Option<serde_json::Value>,
}

#[derive(Clone, Debug)]
pub struct TypeInfo {
    pub key: TypeKey,
    pub id: String,
    pub doc: String,
    pub fields: Vec<FieldInfo>,
}

impl TypeInfo {
    pub fn field(&self, name: &str) -> Option<&FieldInfo> {
        self.fields.iter().find(|f| f.name == name)
    }

    pub fn field_names(&self) -> String {
        self.fields
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[derive(Clone, Debug)]
pub struct ParamInfo {
    pub name: String,
    pub ty: Ty,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CKind {
    Fn,
    Action,
    Flow,
    View,
}

impl CKind {
    pub const fn word(self) -> &'static str {
        match self {
            Self::Fn => "fn",
            Self::Action => "action",
            Self::Flow => "flow",
            Self::View => "view",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Callable {
    pub kind: CKind,
    pub name: String,
    pub id: String,
    pub app: String,
    pub own: bool,
    /// Index into the module's functions when declared here.
    pub func: Option<usize>,
    pub params: Vec<ParamInfo>,
    pub result: Ty,
    /// The class recorded in foreign metadata.
    pub class: Option<EffectClass>,
}

impl Callable {
    pub fn signature(&self, own_app: &str) -> String {
        let params: Vec<String> = self
            .params
            .iter()
            .map(|p| format!("{}: {}", p.name, p.ty.display(own_app)))
            .collect();
        let result = if matches!(self.result, Ty::Unit) {
            String::new()
        } else {
            format!(" -> {}", self.result.display(own_app))
        };
        format!("{}({}){result}", self.name, params.join(", "))
    }
}

#[derive(Clone, Debug)]
pub struct Import {
    pub alias: String,
    pub app: String,
}

/// Everything the checker knows about the module and the packages it imports.
#[derive(Clone, Debug, Default)]
pub struct Env {
    pub app: String,
    pub types: Vec<TypeInfo>,
    pub callables: Vec<Callable>,
    pub imports: Vec<Import>,
    /// `view#anchor` to id.
    pub anchors: HashMap<String, String>,
}

impl Env {
    pub fn type_info(&self, key: &TypeKey) -> Option<&TypeInfo> {
        self.types.iter().find(|t| &t.key == key)
    }

    pub fn own_type(&self, name: &str) -> Option<&TypeInfo> {
        self.types
            .iter()
            .find(|t| t.key.app == self.app && t.key.name == name)
    }

    pub fn own_callable(&self, name: &str) -> Option<&Callable> {
        self.callables.iter().find(|c| c.own && c.name == name)
    }

    pub fn foreign_callable(&self, app: &str, name: &str) -> Option<&Callable> {
        self.callables
            .iter()
            .find(|c| !c.own && c.app == app && c.name == name)
    }

    /// An import named by alias or by full app id.
    pub fn find_import(&self, path: &str) -> Option<&Import> {
        self.imports
            .iter()
            .find(|i| i.alias == path)
            .or_else(|| self.imports.iter().find(|i| i.app == path))
    }

    /// Resolves the app part of a reference literal and its type name.
    pub fn resolve_ref(&self, app: &str, ty: &str) -> Option<&TypeInfo> {
        let want_app: Option<&str> = if app.is_empty() {
            None
        } else if app == self.app {
            Some(self.app.as_str())
        } else {
            self.find_import(app).map(|i| i.app.as_str())
        };
        if !app.is_empty() && want_app.is_none() {
            return None;
        }
        let matches_name = |t: &&TypeInfo, exact: bool| {
            want_app.is_none_or(|a| t.key.app == a)
                && if exact {
                    t.key.name == ty
                } else {
                    t.key.name.eq_ignore_ascii_case(ty)
                }
        };
        self.types
            .iter()
            .find(|t| matches_name(t, true))
            .or_else(|| self.types.iter().find(|t| matches_name(t, false)))
    }

    pub fn imported_types(&self) -> impl Iterator<Item = &TypeInfo> {
        self.types.iter().filter(|t| t.key.app != self.app)
    }

    /// Adds a compiled package's types and callables under `alias`.
    pub fn add_import(&mut self, alias: &str, meta: &PackageMeta) {
        self.imports.push(Import {
            alias: alias.to_string(),
            app: meta.app.clone(),
        });
        let app = meta.app.clone();
        for t in &meta.types {
            let fields = t
                .fields
                .iter()
                .map(|f| FieldInfo {
                    name: f.name.clone(),
                    id: f.id.clone(),
                    ty: Ty::parse_meta(&f.ty, &app).unwrap_or(Ty::Error),
                    merge: f.merge,
                    doc: f.doc.clone(),
                    default: f.default.clone(),
                })
                .collect();
            self.types.push(TypeInfo {
                key: TypeKey {
                    app: app.clone(),
                    name: t.name.clone(),
                },
                id: t.id.clone(),
                doc: t.doc.clone(),
                fields,
            });
        }
        let params = |ps: &[crate::meta::ParamMeta]| -> Vec<ParamInfo> {
            ps.iter()
                .map(|p| ParamInfo {
                    name: p.name.clone(),
                    ty: Ty::parse_meta(&p.ty, &app).unwrap_or(Ty::Error),
                })
                .collect()
        };
        let result = |r: &Option<String>| {
            r.as_ref()
                .map_or(Ty::Unit, |t| Ty::parse_meta(t, &app).unwrap_or(Ty::Error))
        };
        let mut add = |kind, name: &str, id: &str, params, result, class| {
            self.callables.push(Callable {
                kind,
                name: name.to_string(),
                id: id.to_string(),
                app: app.clone(),
                own: false,
                func: None,
                params,
                result,
                class,
            });
        };
        for a in &meta.actions {
            add(
                CKind::Action,
                &a.name,
                &a.id,
                params(&a.params),
                result(&a.result),
                Some(a.class),
            );
        }
        for f in &meta.flows {
            add(
                CKind::Flow,
                &f.name,
                &f.id,
                params(&f.params),
                result(&f.result),
                Some(f.class),
            );
        }
        for v in &meta.views {
            add(
                CKind::View,
                &v.name,
                &v.id,
                params(&v.params),
                Ty::View,
                None,
            );
        }
        for f in &meta.fns {
            add(
                CKind::Fn,
                &f.name,
                &f.id,
                params(&f.params),
                Ty::parse_meta(&f.result, &app).unwrap_or(Ty::Error),
                None,
            );
        }
    }
}

/// The last segment of an app id, the default alias.
pub fn default_alias(app: &str) -> String {
    app.rsplit('.').next().unwrap_or(app).to_string()
}

struct Resolver<'a> {
    env: Env,
    diags: &'a mut Vec<Diagnostic>,
    own_types: HashSet<String>,
}

/// Builds the environment of a parsed module. Errors go to `diags`.
pub fn resolve(
    module: &ast::Module,
    lock: &IdsLock,
    imports: &[PackageMeta],
    diags: &mut Vec<Diagnostic>,
) -> Env {
    let env = Env {
        app: module.app.id.clone(),
        ..Env::default()
    };
    let mut r = Resolver {
        env,
        diags,
        own_types: HashSet::new(),
    };
    r.env.add_import("host", &crate::host::host_meta());
    r.imports(module, imports);
    r.types(module, lock);
    r.callables(module, lock);
    r.env
}

fn mentions_principal(ty: &Ty) -> bool {
    match ty {
        Ty::Principal => true,
        Ty::List(t) | Ty::Set(t) | Ty::Map(t) | Ty::Collection(t) | Ty::Option(t) => {
            mentions_principal(t)
        }
        Ty::Tuple(ts) => ts.iter().any(mentions_principal),
        _ => false,
    }
}

fn id_of(lock: &IdsLock, name: &str) -> String {
    lock.get(name).unwrap_or_default().to_string()
}

fn name_case_ok(name: &str, upper: bool) -> bool {
    let first = name.chars().next().unwrap_or('a');
    if upper {
        first.is_ascii_uppercase()
    } else {
        !first.is_ascii_uppercase()
    }
}

impl Resolver<'_> {
    fn err(&mut self, code: &str, span: Span, msg: impl Into<String>) {
        self.diags.push(Diagnostic::new(code, span, msg));
    }

    fn imports(&mut self, module: &ast::Module, available: &[PackageMeta]) {
        for item in &module.items {
            let Item::Use(u) = item else { continue };
            let Some(meta) = available.iter().find(|m| m.app == u.app) else {
                let known: Vec<&str> = available.iter().map(|m| m.app.as_str()).collect();
                let known = if known.is_empty() {
                    "none were provided".to_string()
                } else {
                    known.join(", ")
                };
                self.err(
                    "E015",
                    u.span,
                    format!(
                        "no package `{}` is available to import; available packages: {known}",
                        u.app
                    ),
                );
                continue;
            };
            let alias = u
                .alias
                .as_ref()
                .map_or_else(|| default_alias(&u.app), |a| a.name.clone());
            if self.env.imports.iter().any(|i| i.alias == alias) {
                self.err(
                    "E012",
                    u.span,
                    format!("the import name `{alias}` is used twice; give one `use` an `as` name"),
                );
                continue;
            }
            self.env.add_import(&alias, meta);
        }
    }

    fn types(&mut self, module: &ast::Module, lock: &IdsLock) {
        let mut decls: Vec<&ast::TypeDecl> = Vec::new();
        for item in &module.items {
            let Item::Type(t) = item else { continue };
            if !name_case_ok(&t.name.name, true) {
                self.err(
                    "E016",
                    t.name.span,
                    format!(
                        "type names start with an uppercase letter; rename `{}`",
                        t.name.name
                    ),
                );
            }
            if !self.own_types.insert(t.name.name.clone()) {
                self.err(
                    "E011",
                    t.name.span,
                    format!("the type `{}` is declared twice", t.name.name),
                );
                continue;
            }
            decls.push(t);
        }
        for t in decls {
            let mut fields: Vec<FieldInfo> = Vec::new();
            for f in &t.fields {
                if fields.iter().any(|x| x.name == f.name.name) {
                    self.err(
                        "E012",
                        f.name.span,
                        format!("`{}` already has a field `{}`", t.name.name, f.name.name),
                    );
                    continue;
                }
                fields.push(self.field(&t.name.name, f, lock));
            }
            self.env.types.push(TypeInfo {
                key: TypeKey {
                    app: self.env.app.clone(),
                    name: t.name.name.clone(),
                },
                id: id_of(lock, &t.name.name),
                doc: t.doc.join("\n"),
                fields,
            });
        }
    }

    fn field(&mut self, owner: &str, f: &ast::FieldDecl, lock: &IdsLock) -> FieldInfo {
        let ty = self.resolve_type(&f.ty, true);
        if matches!(ty, Ty::Action | Ty::View) {
            self.err("E023", f.ty.span, format!("a field cannot hold `{ty}`: it is not stored state; fields hold scalars, Text, records, List, Option, Set, Map and Collection"));
        }
        let merge = self.merge(&ty, f.merge.as_ref(), &f.name.name);
        let default = f.default.as_ref().and_then(|d| self.default_value(&ty, d));
        FieldInfo {
            name: f.name.name.clone(),
            id: id_of(lock, &format!("{owner}.{}", f.name.name)),
            ty,
            merge,
            doc: f.doc.join("\n"),
            default,
        }
    }

    /// The merge kind of a field (implementation.md section 3.2).
    fn merge(&mut self, ty: &Ty, spec: Option<&MergeSpec>, field: &str) -> MergeKind {
        let (default, allowed): (MergeKind, &[MergeKind]) = match ty {
            Ty::Int | Ty::Float => (
                MergeKind::Register,
                &[MergeKind::Register, MergeKind::Counter],
            ),
            Ty::Text => (MergeKind::Text, &[MergeKind::Text, MergeKind::Register]),
            Ty::Set(_) | Ty::Map(_) | Ty::Collection(_) => (MergeKind::Structural, &[]),
            _ => (MergeKind::Register, &[MergeKind::Register]),
        };
        let Some(spec) = spec else { return default };
        if allowed.contains(&spec.kind) {
            return spec.kind;
        }
        let options = if allowed.is_empty() {
            "containers merge structurally and take no merge kind".to_string()
        } else {
            format!(
                "`{field}` can be {}",
                allowed
                    .iter()
                    .map(|m| format!("`{}`", m.name()))
                    .collect::<Vec<_>>()
                    .join(" or ")
            )
        };
        self.err(
            "E032",
            spec.span,
            format!(
                "`merge {}` does not apply to a field of type {ty}: {options}",
                spec.kind.name()
            ),
        );
        default
    }

    fn default_value(&mut self, ty: &Ty, e: &Expr) -> Option<serde_json::Value> {
        fn lit(e: &Expr) -> Option<(Option<Ty>, serde_json::Value)> {
            match &e.kind {
                ExprKind::Int(v) => Some((Some(Ty::Int), serde_json::json!(v))),
                ExprKind::Float(v) => Some((Some(Ty::Float), serde_json::json!(v))),
                ExprKind::Bool(v) => Some((Some(Ty::Bool), serde_json::json!(v))),
                ExprKind::None => Some((None, serde_json::Value::Null)),
                ExprKind::Str(parts) => {
                    let mut s = String::new();
                    for p in parts {
                        match p {
                            ast::StrPart::Lit(t) => s.push_str(t),
                            ast::StrPart::Interp(_) => return None,
                        }
                    }
                    Some((Some(Ty::Text), serde_json::Value::String(s)))
                }
                ExprKind::Unary {
                    op: UnOp::Neg,
                    operand,
                } => match lit(operand)? {
                    (Some(Ty::Int), serde_json::Value::Number(n)) => {
                        Some((Some(Ty::Int), serde_json::json!(-n.as_i64()?)))
                    }
                    (Some(Ty::Float), serde_json::Value::Number(n)) => {
                        Some((Some(Ty::Float), serde_json::json!(-n.as_f64()?)))
                    }
                    _ => None,
                },
                _ => None,
            }
        }
        let Some((lt, value)) = lit(e) else {
            self.err(
                "E019",
                e.span,
                "a field default must be a literal: a number, a string, true, false or none",
            );
            return None;
        };
        let ok = match (&lt, ty) {
            (None, Ty::Option(_) | Ty::Error) => true,
            (None, _) => false,
            (Some(t), Ty::Option(inner)) => t.same(inner),
            (Some(t), want) => t.same(want),
        };
        if !ok {
            let found = lt.map_or_else(|| "none".to_string(), |t| t.to_string());
            self.err(
                "E020",
                e.span,
                format!("the default is {found} but the field has type {ty}"),
            );
            return None;
        }
        Some(value)
    }

    /// Resolves a written type. Set, Map and Collection are only valid as field types.
    pub fn resolve_type(&mut self, te: &TypeExpr, field_position: bool) -> Ty {
        match &te.kind {
            TypeExprKind::Tuple(items) => {
                let tys = items.iter().map(|t| self.resolve_type(t, false)).collect();
                Ty::Tuple(tys)
            }
            TypeExprKind::Generic(name, args) => {
                if args.len() != 1 {
                    self.err(
                        "E013",
                        te.span,
                        format!(
                            "`{}` takes one type argument, written `{}<T>`",
                            name.name, name.name
                        ),
                    );
                    return Ty::Error;
                }
                let inner = args
                    .first()
                    .map_or(Ty::Error, |a| self.resolve_type(a, false));
                let boxed = Box::new(inner);
                let ty = match name.name.as_str() {
                    "List" => Ty::List(boxed),
                    "Option" => Ty::Option(boxed),
                    "Set" => Ty::Set(boxed),
                    "Map" => Ty::Map(boxed),
                    _ => Ty::Collection(boxed),
                };
                if ty.is_container() && !field_position {
                    self.err("E023", te.span, format!("`{}` can only be the type of a field; use `List<T>` for values you pass around", name.name));
                    return Ty::Error;
                }
                ty
            }
            TypeExprKind::Named(path) => self.named_type(path, te.span),
        }
    }

    fn named_type(&mut self, path: &[ast::Ident], span: Span) -> Ty {
        let dotted: Vec<&str> = path.iter().map(|i| i.name.as_str()).collect();
        if let [single] = dotted.as_slice() {
            match *single {
                "Int" => return Ty::Int,
                "Float" => return Ty::Float,
                "Bool" => return Ty::Bool,
                "Text" => return Ty::Text,
                "Principal" => return Ty::Principal,
                "Action" => return Ty::Action,
                "View" => return Ty::View,
                name if self.own_types.contains(name) => {
                    return Ty::Record(TypeKey {
                        app: self.env.app.clone(),
                        name: name.to_string(),
                    });
                }
                _ => {}
            }
            let mut known: Vec<String> = self.own_types.iter().cloned().collect();
            known.sort();
            let known = if known.is_empty() {
                "this module declares none".to_string()
            } else {
                known.join(", ")
            };
            self.err("E013", span, format!("unknown type `{single}`; types here: Int, Float, Bool, Text, Principal, Action, View and {known}"));
            return Ty::Error;
        }
        let (prefix, name) = dotted.split_at(dotted.len().saturating_sub(1));
        let prefix = prefix.join(".");
        let name = name.first().copied().unwrap_or("");
        let Some(import) = self.env.find_import(&prefix) else {
            self.err("E013", span, format!("unknown type `{}`: `{prefix}` is not an imported package; add `use <app id> as {prefix}`", dotted.join(".")));
            return Ty::Error;
        };
        let key = TypeKey {
            app: import.app.clone(),
            name: name.to_string(),
        };
        if self.env.type_info(&key).is_none() {
            let names: Vec<&str> = self
                .env
                .types
                .iter()
                .filter(|t| t.key.app == key.app)
                .map(|t| t.key.name.as_str())
                .collect();
            self.err(
                "E013",
                span,
                format!(
                    "package `{prefix}` has no type `{name}`; its types: {}",
                    names.join(", ")
                ),
            );
            return Ty::Error;
        }
        Ty::Record(key)
    }

    fn params(&mut self, params: &[ast::Param]) -> Vec<ParamInfo> {
        let mut out: Vec<ParamInfo> = Vec::new();
        for p in params {
            if out.iter().any(|x| x.name == p.name.name) {
                self.err(
                    "E012",
                    p.name.span,
                    format!("the parameter `{}` is declared twice", p.name.name),
                );
                continue;
            }
            let ty = self.resolve_type(&p.ty, false);
            out.push(ParamInfo {
                name: p.name.name.clone(),
                ty,
            });
        }
        out
    }

    fn callables(&mut self, module: &ast::Module, lock: &IdsLock) {
        let mut func = 0usize;
        for item in &module.items {
            let (kind, name, params, result) = match item {
                Item::Action(a) => (CKind::Action, &a.name, &a.params, a.result.as_ref()),
                Item::Flow(f) => (CKind::Flow, &f.name, &f.params, f.result.as_ref()),
                Item::View(v) => (CKind::View, &v.name, &v.params, None),
                Item::Fn(f) => (CKind::Fn, &f.name, &f.params, Some(&f.result)),
                Item::Use(_) | Item::Type(_) => continue,
            };
            let index = func;
            func += 1;
            if !name_case_ok(&name.name, false) {
                self.err("E016", name.span, format!("`{}` names a {}, which starts with a lowercase letter (uppercase is for types)", name.name, kind.word()));
            }
            if prims::prim(&name.name).is_some() || prims::BUILTINS.contains(&name.name.as_str()) {
                self.err(
                    "E011",
                    name.span,
                    format!(
                        "`{}` is a built-in name; choose another name for this {}",
                        name.name,
                        kind.word()
                    ),
                );
            }
            if self
                .env
                .callables
                .iter()
                .any(|c| c.own && c.name == name.name)
            {
                self.err(
                    "E011",
                    name.span,
                    format!(
                        "`{}` is declared twice; actions, flows, views and fns share one namespace",
                        name.name
                    ),
                );
            }
            let params = self.params(params);
            if matches!(kind, CKind::Action | CKind::Flow)
                && params.iter().any(|p| mentions_principal(&p.ty))
            {
                self.err("E048", name.span, format!("{} `{}` takes a Principal parameter; actions and flows are what callers invoke, and a caller could pass anyone. Read `invoker` inside the action instead", kind.word(), name.name));
            }
            let result = match (kind, result) {
                (CKind::View, _) => Ty::View,
                (_, Some(t)) => self.resolve_type(t, false),
                (_, None) => Ty::Unit,
            };
            self.env.callables.push(Callable {
                kind,
                name: name.name.clone(),
                id: id_of(lock, &name.name),
                app: self.env.app.clone(),
                own: true,
                func: Some(index),
                params,
                result,
                class: None,
            });
            if let Item::View(v) = item {
                self.anchors(v, lock);
            }
        }
    }

    fn anchors(&mut self, v: &ast::ViewDecl, lock: &IdsLock) {
        let mut names = Vec::new();
        collect(&v.body, &mut names);
        let mut seen: HashSet<String> = HashSet::new();
        for a in names {
            if !seen.insert(a.name.clone()) {
                self.err(
                    "E011",
                    a.span,
                    format!(
                        "the anchor `#{}` is used twice in view `{}`",
                        a.name, v.name.name
                    ),
                );
                continue;
            }
            let key = format!("{}#{}", v.name.name, a.name);
            let id = id_of(lock, &key);
            self.env.anchors.insert(key, id);
        }
    }
}

/// Resolves a type written inside a body (a `let` annotation).
pub fn resolve_type_expr(env: &Env, te: &TypeExpr, diags: &mut Vec<Diagnostic>) -> Ty {
    let own_types = env
        .types
        .iter()
        .filter(|t| t.key.app == env.app)
        .map(|t| t.key.name.clone())
        .collect();
    let mut r = Resolver {
        env: env.clone(),
        diags,
        own_types,
    };
    r.resolve_type(te, false)
}

fn collect(stmts: &[ast::UiStmt], out: &mut Vec<ast::Ident>) {
    for s in stmts {
        match &s.kind {
            ast::UiKind::Call(c) => {
                out.extend(c.anchor.clone());
                if let Some(b) = &c.block {
                    collect(b, out);
                }
            }
            ast::UiKind::For { body, .. } => collect(body, out),
            ast::UiKind::If { then, els, .. } => {
                collect(then, out);
                if let Some(e) = els {
                    collect(e, out);
                }
            }
            ast::UiKind::Let { .. } => {}
        }
    }
}
