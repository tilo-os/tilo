//! Name resolution: the module's declarations, its imports, and the signatures the checker
//! needs before it looks at any body.

use crate::ast::{self, Expr, ExprKind, Item, MergeSpec, TypeExpr, TypeExprKind, UnOp};
use crate::diag::{Diagnostic, Span};
use crate::ids::IdsLock;
use crate::meta::PackageMeta;
use crate::prims;
use crate::types::{MergeKind, Ty, TypeKey};
use std::collections::{BTreeSet, HashMap, HashSet};

mod effects;
mod model;
mod text;
mod triggers;
mod views;

pub use model::*;
pub use text::*;

/// A view expanded where it is called ([`crate::checker`]'s templates): generic, or taking
/// view state, functions or views of an item. It has no function of its own.
#[derive(Clone, Debug)]
pub struct Template {
    /// The package whose names its body sees: `None` for the module's own, `std` for the
    /// standard library's.
    pub ns: Option<String>,
    pub decl: ast::ViewDecl,
}

#[derive(Clone, Debug)]
pub struct Import {
    pub alias: String,
    pub app: String,
    /// The ids of the package's root types: what a scenario may reach with `root alias.T`.
    pub roots: Vec<String>,
    /// Its named roots (`root notes: Notes`) and their type ids: `alias.notes` reaches one.
    pub named_roots: Vec<(String, String)>,
    /// The ids the package was published under before (`previously:`): `use` and a full path
    /// under one of them name it.
    pub previously: Vec<String>,
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
    pub enums: Vec<EnumInfo>,
    /// The package's root types (`root T`), in declaration order.
    pub roots: Vec<TypeKey>,
    /// The package's named roots (`root notes: Notes`): the name code reaches each by.
    pub named_roots: Vec<(String, TypeKey)>,
    /// `type Name = T` naming a type that is not a container, as its written form.
    pub aliases: Vec<(TypeKey, Ty)>,
    /// `default view v for T`: the type and the view's name.
    pub default_views: Vec<(TypeKey, String)>,
    /// The view state every view declares.
    pub states: Vec<StateInfo>,
    /// `row view v for T`: the type and the view's name.
    pub row_views: Vec<(TypeKey, String)>,
    /// `views a, b for T`: the type, the views in the order written, and where the
    /// declaration stands.
    pub views_of: Vec<(TypeKey, Vec<crate::resolve::ViewsInfo>, Span)>,
    /// The functions of action parameters' defaults, numbered after the presentation views.
    pub defaults: Vec<DefaultFn>,
    /// The toolkit's components, the package `ui`.
    pub toolkit: Option<&'static crate::toolkit::Catalogue>,
    /// The views expanded where they are called: the module's and the standard library's.
    pub templates: Vec<Template>,
    /// The toolkit component each of the module's views makes, by the view's name: a view whose
    /// body is one `ui.X(...)` is an `X` wherever one goes (views are components).
    pub view_makes: HashMap<String, String>,
    /// The apps a program may read beyond those it holds, each by the name programs call it and
    /// its id: what `grep(re, in: chat)` and `describe(chat)` name.
    pub sources: Vec<(String, String)>,
    /// A session's environment: the agent library's words ([`crate::agent`]) are in scope
    /// without a `use`, as `std` is in every package. The agent door and the REPL build one.
    pub session: bool,
    /// The actions the package runs by itself, in declaration order.
    pub triggers: Vec<TriggerInfo>,
}

/// A trigger resolved: what runs, for which resources, and when.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TriggerInfo {
    pub name: String,
    pub id: String,
    pub doc: String,
    /// The action it runs, by id.
    pub action: String,
    /// The type of the resources it runs for, by id.
    pub ty: String,
    /// The action's arguments, in order.
    pub args: Vec<TriggerArg>,
    pub when: TriggerWhen,
}

/// What a trigger passes its action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TriggerArg {
    /// The record it runs for.
    Subject,
    /// A root record of the package, by its type's id.
    Root(String),
}

/// When a trigger runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TriggerWhen {
    /// At each multiple of these seconds, counted from the epoch.
    Every(i64),
    /// At the instant the field (by id) of the resource holds.
    At(FieldId),
}

/// The id of a field a trigger reads its instant from.
pub type FieldId = String;

impl Env {
    pub fn type_info(&self, key: &TypeKey) -> Option<&TypeInfo> {
        self.types.iter().find(|t| &t.key == key)
    }

    pub fn enum_info(&self, key: &TypeKey) -> Option<&EnumInfo> {
        self.enums.iter().find(|t| &t.key == key)
    }

    pub fn own_enum(&self, name: &str) -> Option<&EnumInfo> {
        self.enums
            .iter()
            .find(|t| t.key.app == self.app && t.key.name == name)
    }

    pub fn own_type(&self, name: &str) -> Option<&TypeInfo> {
        self.types
            .iter()
            .find(|t| t.key.app == self.app && t.key.name == name)
    }

    pub fn own_callable(&self, name: &str) -> Option<&Callable> {
        self.callables
            .iter()
            .find(|c| c.own && !c.scenario && c.name == name)
    }

    /// The hidden action of the scenario `name`.
    pub fn scenario(&self, name: &str) -> Option<&Callable> {
        self.callables
            .iter()
            .find(|c| c.own && c.scenario && c.name == name)
    }

    /// The row view of the record type `key`, when its package declares one.
    pub fn row_view(&self, key: &TypeKey) -> Option<&Callable> {
        let (_, name) = self.row_views.iter().find(|(k, _)| k == key)?;
        self.callables
            .iter()
            .find(|c| c.app == key.app && !c.scenario && c.kind == CKind::View && &c.name == name)
    }

    /// The template `name` of the package `ns` (`None`: the module's own).
    pub fn template(&self, ns: Option<&str>, name: &str) -> Option<&Template> {
        self.templates
            .iter()
            .find(|t| t.ns.as_deref() == ns && t.decl.name.name == name)
    }

    /// Adds the standard library, `std`, as the package it is: the declarations its own module
    /// makes, and its views, expanded where a package calls them.
    ///
    /// The library's own module is not an import of itself: it calls its views by its own name.
    pub fn add_std(&mut self) {
        if self.app == crate::stdlib::APP {
            self.imports.push(Import {
                alias: crate::stdlib::NS.to_string(),
                app: self.app.clone(),
                roots: Vec::new(),
                named_roots: Vec::new(),
                previously: Vec::new(),
            });
            return;
        }
        self.add_import(crate::stdlib::NS, crate::stdlib::meta());
        self.templates
            .extend(crate::stdlib::templates().iter().map(|decl| Template {
                ns: Some(crate::stdlib::APP.to_string()),
                decl: decl.clone(),
            }));
    }

    /// Adds the agent library, `tilo.agent`, as the package a session has by default: its
    /// words are host imports the checker lowers (see [`crate::agent`]).
    pub fn add_agent(&mut self) {
        self.add_import(crate::agent::NS, crate::agent::meta());
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
            .or_else(|| {
                self.imports
                    .iter()
                    .find(|i| i.previously.iter().any(|old| old == path))
            })
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

    /// Adds the toolkit's package, `ui`: its components, described by the toolkit.
    pub fn add_toolkit(&mut self) {
        self.imports.push(Import {
            alias: crate::toolkit::UI_ALIAS.to_string(),
            app: crate::toolkit::UI_APP.to_string(),
            roots: Vec::new(),
            named_roots: Vec::new(),
            previously: Vec::new(),
        });
        self.toolkit = Some(crate::toolkit::catalogue());
        self.enums.extend(crate::toolkit::enums());
    }

    /// The component `name` of the toolkit, when `path` names the toolkit's package.
    pub fn component(
        &self,
        path: &str,
        name: &str,
    ) -> Option<&'static crate::toolkit::Description> {
        let import = self.find_import(path)?;
        if import.app != crate::toolkit::UI_APP {
            return None;
        }
        self.toolkit?.get(name)
    }

    /// Adds a compiled package's types and callables under `alias`.
    pub fn add_import(&mut self, alias: &str, meta: &PackageMeta) {
        self.imports.push(Import {
            alias: alias.to_string(),
            app: meta.app.clone(),
            roots: meta.roots.clone(),
            named_roots: meta
                .named_roots
                .iter()
                .map(|r| (r.name.clone(), r.ty.clone()))
                .collect(),
            previously: meta.previously.clone(),
        });
        let app = meta.app.clone();
        let field_info = |f: &crate::meta::FieldMeta| FieldInfo {
            name: f.name.clone(),
            id: f.id.clone(),
            ty: Ty::parse_meta(&f.ty, &app).unwrap_or(Ty::Error),
            merge: f.merge,
            doc: f.doc.clone(),
            default: f.default.clone(),
        };
        for e in &meta.enums {
            self.enums.push(EnumInfo {
                key: TypeKey {
                    app: app.clone(),
                    name: e.name.clone(),
                },
                id: e.id.clone(),
                doc: e.doc.clone(),
                variants: e
                    .variants
                    .iter()
                    .map(|v| VariantInfo {
                        name: v.name.clone(),
                        id: v.id.clone(),
                        doc: v.doc.clone(),
                        label: if v.label.is_empty() {
                            variant_label(&v.name, &v.doc)
                        } else {
                            v.label.clone()
                        },
                        icon: v.icon.clone(),
                        tone: v.tone.clone(),
                        fields: v.fields.iter().map(field_info).collect(),
                    })
                    .collect(),
            });
        }
        for a in &meta.aliases {
            if let Some(ty) = Ty::parse_meta(&a.ty, &app) {
                let key = TypeKey {
                    app: app.clone(),
                    name: a.name.clone(),
                };
                self.aliases.push((key, ty));
            }
        }
        for t in &meta.types {
            let fields = t.fields.iter().map(field_info).collect();
            self.types.push(TypeInfo {
                key: TypeKey {
                    app: app.clone(),
                    name: t.name.clone(),
                },
                id: t.id.clone(),
                doc: t.doc.clone(),
                fields,
                search: t.search.clone(),
                origin: t.origin.clone(),
            });
        }
        let params = |ps: &[crate::meta::ParamMeta]| -> Vec<ParamInfo> {
            ps.iter()
                .map(|p| ParamInfo {
                    name: p.name.clone(),
                    ty: Ty::parse_meta(&p.ty, &app).unwrap_or(Ty::Error),
                    default: p.default.as_ref().map(|d| ParamDefault {
                        export: d.export.clone(),
                        func: None,
                        text: d.text.clone(),
                    }),
                    place: p.place,
                    entry: p.entry,
                })
                .collect()
        };
        let result = |r: &Option<String>| {
            r.as_ref()
                .map_or(Ty::Unit, |t| Ty::parse_meta(t, &app).unwrap_or(Ty::Error))
        };
        let mut add = |kind, name: &str, id: &str, params, result, class, uses, present| {
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
                uses,
                present,
                scenario: false,
            });
        };
        for a in meta.callable_actions() {
            let present = Presentation {
                label: if a.label.is_empty() {
                    action_label(&a.name, &a.doc)
                } else {
                    a.label.clone()
                },
                icon: a.icon.clone(),
                role: a.role.clone(),
                view: a.present.clone(),
                label_text: None,
                icon_when: None,
                on: None,
                key: a.key.clone(),
                group: a.group.clone(),
                did: a.did.clone(),
            };
            add(
                CKind::Action,
                &a.name,
                &a.id,
                params(&a.params),
                result(&a.result),
                Some(a.class),
                effects::declared(&a.uses),
                present,
            );
        }
        for v in meta.offered_views() {
            add(
                CKind::View,
                &v.name,
                &v.id,
                params(&v.params),
                Ty::View,
                None,
                BTreeSet::new(),
                Presentation::default(),
            );
        }
        for rv in &meta.row_views {
            let ty = meta.types.iter().find(|t| t.id == rv.ty);
            let view = meta.views.iter().find(|v| v.id == rv.view);
            if let (Some(ty), Some(view)) = (ty, view) {
                let key = TypeKey {
                    app: app.clone(),
                    name: ty.name.clone(),
                };
                self.row_views.push((key, view.name.clone()));
            }
        }
        for f in &meta.fns {
            add(
                CKind::Fn,
                &f.name,
                &f.id,
                params(&f.params),
                Ty::parse_meta(&f.result, &app).unwrap_or(Ty::Error),
                None,
                BTreeSet::new(),
                Presentation::default(),
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
    own_enums: HashSet<String>,
    /// `type Name = T` of a type that is not a container: resolved where it is named.
    own_aliases: HashMap<String, TypeExpr>,
    /// The aliases being resolved, to refuse one that names itself.
    resolving: Vec<String>,
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
        own_enums: HashSet::new(),
        own_aliases: HashMap::new(),
        resolving: Vec::new(),
    };
    r.env.add_import("host", &crate::host::host_meta());
    r.env.add_toolkit();
    r.env.add_std();
    r.imports(module, imports);
    r.types(module, lock);
    r.callables(module, lock);
    r.triggers(module, lock);
    for item in &module.items {
        if let Item::View(v) = item
            && let Some(made) = view_root_component(&v.body)
        {
            r.env.view_makes.insert(v.name.name.clone(), made);
        }
    }
    r.package_decls(module);
    r.one_shortcut_each(module);
    r.env
}

/// The toolkit component a view body makes: its one call (after its state and `let`s) when
/// that is `ui.X(...)`.
pub(crate) fn view_root_component(body: &[ast::UiStmt]) -> Option<String> {
    let mut calls = body
        .iter()
        .filter(|s| !matches!(s.kind, ast::UiKind::State(_) | ast::UiKind::Let { .. }));
    let one = calls.next()?;
    if calls.next().is_some() {
        return None;
    }
    let ast::UiKind::Call(c) = &one.kind else {
        return None;
    };
    let callee = match &c.call.kind {
        ExprKind::Call { callee, .. } => callee,
        _ => &c.call,
    };
    match &callee.kind {
        ExprKind::Field { base, name } if matches!(&base.kind, ExprKind::Ident(b) if b == "ui") => {
            Some(name.name.clone())
        }
        _ => None,
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

mod types;

impl Resolver<'_> {
    fn err(&mut self, code: &str, span: Span, msg: impl Into<String>) {
        self.diags.push(Diagnostic::new(code, span, msg));
    }

    fn imports(&mut self, module: &ast::Module, available: &[PackageMeta]) {
        for item in &module.items {
            let Item::Use(u) = item else { continue };
            let Some(meta) = find_package(available, &u.app) else {
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

    /// Each type with an origin names a kind the platform has, and declares the kind's fields
    /// it shows with the kind's types (`crate::origin`).
    fn check_origins(&mut self, decls: &[&ast::TypeDecl]) {
        for t in decls {
            let Some(origin) = &t.origin else {
                continue;
            };
            let Some(kind) = crate::origin::kind(&origin.name) else {
                self.err(
                    "E112",
                    origin.span,
                    format!(
                        "no outside service has the kind `{}`; the kinds are: {}",
                        origin.name,
                        crate::origin::kind_names()
                    ),
                );
                continue;
            };
            let Some(info) = self
                .env
                .types
                .iter()
                .find(|x| x.key.app == self.env.app && x.key.name == t.name.name)
                .cloned()
            else {
                continue;
            };
            for f in &t.fields {
                let Some(want) = kind.field(&f.name.name) else {
                    continue;
                };
                let Some(have) = info.fields.iter().find(|x| x.name == f.name.name) else {
                    continue;
                };
                if !self.origin_type_fits(&have.ty, want.ty) {
                    let shown = have.ty.meta_text(&self.env.app);
                    self.err(
                        "E112",
                        f.name.span,
                        format!(
                            "`{}` of `{}` is the service's `{}`, a {}; it is declared {shown}",
                            f.name.name, kind.name, want.name, want.ty
                        ),
                    );
                }
            }
        }
    }

    /// Whether a declared field type is the origin kind's `want` (`Text`, `Set<Text>`,
    /// `Collection<mail.thread>`, ...).
    fn origin_type_fits(&self, have: &Ty, want: &str) -> bool {
        if let Some(inner) = want
            .strip_prefix("Collection<")
            .and_then(|r| r.strip_suffix('>'))
        {
            return match have {
                Ty::Collection(item) => match item.as_ref() {
                    Ty::Record(key) => self
                        .env
                        .type_info(key)
                        .is_some_and(|t| t.origin.as_deref() == Some(inner)),
                    _ => false,
                },
                _ => false,
            };
        }
        match want {
            "Text" => *have == Ty::Text,
            "Bool" => *have == Ty::Bool,
            "Int" => *have == Ty::Int,
            "Time" => *have == Ty::Time,
            "Set<Text>" => *have == Ty::Set(Box::new(Ty::Text)),
            _ => false,
        }
    }

    fn enum_decl(&mut self, e: &ast::EnumDecl, lock: &IdsLock) {
        let name = &e.name.name;
        let mut variants: Vec<VariantInfo> = Vec::new();
        if e.variants.is_empty() {
            self.err(
                "E013",
                e.name.span,
                format!("the enum `{name}` has no variants; write `enum {name} {{ a, b }}`"),
            );
        }
        for v in &e.variants {
            if variants.iter().any(|x| x.name == v.name.name) {
                self.err(
                    "E012",
                    v.name.span,
                    format!("`{name}` already has a variant `{}`", v.name.name),
                );
                continue;
            }
            if !name_case_ok(&v.name.name, false) {
                self.err(
                    "E016",
                    v.name.span,
                    format!(
                        "variant names start with a lowercase letter; rename `{}`",
                        v.name.name
                    ),
                );
            }
            if let Some(first) = v.fields.first() {
                let case = &v.name.name;
                let upper: String = case
                    .chars()
                    .next()
                    .map(|c| c.to_uppercase().chain(case.chars().skip(1)).collect())
                    .unwrap_or_default();
                self.err(
                    "E110",
                    first.name.span,
                    format!(
                        "an enum's cases are plain names; a case that holds data is a type of its own, and a union of the types takes any of them: `type {upper} {{ ... }}`, then `type {name} = {upper} | ...`, matched as `match x {{ v: {upper} => ..., }}`"
                    ),
                );
            }
            let mut fields: Vec<FieldInfo> = Vec::new();
            for p in &v.fields {
                if let Some(d) = &p.default {
                    self.err("E028", d.span, format!("`.{}` is a variant's field, which takes no default; only an action's parameter does", p.name.name));
                }
                if fields.iter().any(|x| x.name == p.name.name) {
                    self.err(
                        "E012",
                        p.name.span,
                        format!("`.{}` already has a field `{}`", v.name.name, p.name.name),
                    );
                    continue;
                }
                let ty = self.resolve_type(&p.ty, false);
                if matches!(ty, Ty::Action | Ty::View) {
                    self.err(
                        "E023",
                        p.ty.span,
                        format!(
                            "a variant's field cannot hold `{ty}`: enum values are stored state"
                        ),
                    );
                }
                fields.push(FieldInfo {
                    name: p.name.name.clone(),
                    id: id_of(lock, &format!("{name}.{}.{}", v.name.name, p.name.name)),
                    ty,
                    merge: MergeKind::Register,
                    doc: String::new(),
                    default: None,
                });
            }
            let doc = v.doc.join("\n");
            let (icon, tone) = self.variant_options(v);
            variants.push(VariantInfo {
                name: v.name.name.clone(),
                id: id_of(lock, &format!("{name}.{}", v.name.name)),
                label: variant_label(&v.name.name, &doc),
                icon,
                tone,
                doc,
                fields,
            });
        }
        self.env.enums.push(EnumInfo {
            key: TypeKey {
                app: self.env.app.clone(),
                name: name.clone(),
            },
            id: id_of(lock, name),
            doc: e.doc.join("\n"),
            variants,
        });
    }

    /// A variant's options: `(icon: .x)`, what a control shows beside its label, and
    /// `(tone: .x)`, the tone it reads in.
    fn variant_options(&mut self, v: &ast::VariantDecl) -> (Option<String>, Option<String>) {
        let mut icon = None;
        let mut tone = None;
        let mut icons = false;
        let mut tones = false;
        for opt in &v.opts {
            let case = match &opt.value.kind {
                ExprKind::Variant { name, args: None } => Some(name),
                _ => None,
            };
            match (opt.name.name.as_str(), case) {
                ("icon", _) if icons => self.err("E090", opt.name.span, "`icon:` is given twice"),
                ("icon", Some(name)) => {
                    icons = true;
                    icon = self.icon(name);
                }
                ("icon", None) => self.err(
                    "E090",
                    opt.value.span,
                    "`icon:` takes an icon written with a dot, such as `icon: .pin`",
                ),
                ("tone", _) if tones => self.err("E090", opt.name.span, "`tone:` is given twice"),
                ("tone", Some(name)) => {
                    tones = true;
                    tone = self.tone(name);
                }
                ("tone", None) => self.err(
                    "E090",
                    opt.value.span,
                    "`tone:` takes a tone written with a dot, such as `tone: .warning`",
                ),
                (other, _) => self.err(
                    "E090",
                    opt.name.span,
                    format!("a variant takes the options `icon:` and `tone:`; `{other}:` is not one. Its label is its doc comment's first line"),
                ),
            }
        }
        (icon, tone)
    }

    /// The tone a variant reads in, one of the toolkit's `ui.Tone` cases: a theme role, not a
    /// colour.
    fn tone(&mut self, name: &ast::Ident) -> Option<String> {
        const TONES: [&str; 5] = ["neutral", "success", "warning", "danger", "info"];
        if let Some(tone) = TONES.iter().find(|tone| **tone == name.name) {
            return Some((*tone).to_owned());
        }
        self.err(
            "E090",
            name.span,
            format!(
                "`{}` is not a tone; the tones are {}, theme roles a control reads a mark in rather than colours",
                name.name,
                TONES.join(", ")
            ),
        );
        None
    }

    fn field(&mut self, owner: &str, f: &ast::FieldDecl, lock: &IdsLock) -> FieldInfo {
        let ty = self.resolve_type(&f.ty, true);
        if crate::types::is_transient(&ty) {
            self.err("E023", f.ty.span, format!("a field cannot hold `{ty}`: it is not stored state; fields hold scalars, Text, records, enums, List, Option, Set, Map and Collection"));
        } else if ty == Ty::Duration {
            self.err("E023", f.ty.span, "a field cannot hold a `Duration`: a stored length of time keeps the number the app measures it in (`Int`), and `Duration(n)` reads it as one where a moment moves by it".to_string());
        }
        let merge = self.merge(&ty, f.merge.as_ref(), &f.name.name);
        let default = match &f.default {
            Some(d) => self.default_value(&ty, d),
            None => self.implicit_default(&ty, f),
        };
        FieldInfo {
            name: f.name.name.clone(),
            id: id_of(lock, &format!("{owner}.{}", f.name.name)),
            ty,
            merge,
            doc: f.doc.join("\n"),
            default,
        }
    }

    /// The merge kind of a field. `Int` and `Float` are registers unless declared
    /// `merge counter`; `Text` merges as text unless declared `merge register` (an atom, like a
    /// tag); sets, maps and collections merge per element; every other value is a register.
    fn merge(&mut self, ty: &Ty, spec: Option<&MergeSpec>, field: &str) -> MergeKind {
        let (default, allowed): (MergeKind, &[MergeKind]) = match ty {
            Ty::Int | Ty::Float => (
                MergeKind::Register,
                &[MergeKind::Register, MergeKind::Counter],
            ),
            Ty::Text => (MergeKind::Text, &[MergeKind::Text, MergeKind::Register]),
            // Rich text merges by splices and marks, never as a whole value.
            Ty::RichText => (MergeKind::Text, &[MergeKind::Text]),
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

    /// An enum field without a default starts as the enum's first variant, which must then
    /// carry no payload.
    fn implicit_default(&mut self, ty: &Ty, f: &ast::FieldDecl) -> Option<serde_json::Value> {
        let Ty::Enum(key) = ty else { return None };
        let info = self.env.enum_info(key)?;
        let first = info.variants.first()?;
        if first.fields.is_empty() {
            return Some(enum_json(&info.id, &first.id, &serde_json::Map::new()));
        }
        let msg = format!(
            "the field `{}` needs a default: its enum's first variant `.{}` carries a payload; write `{}: {} = .<variant>`",
            f.name.name, first.name, f.name.name, key.name
        );
        self.err("E019", f.name.span, msg);
        None
    }

    /// `.name` or `.name(field: literal)` as the default of an Option or enum field.
    fn variant_default(
        &mut self,
        ty: &Ty,
        name: &ast::Ident,
        args: Option<&Vec<ast::Arg>>,
        span: Span,
    ) -> Option<serde_json::Value> {
        match ty {
            Ty::Option(_) if name.name == "none" && args.is_none() => Some(serde_json::Value::Null),
            Ty::Enum(key) => {
                let info = self.env.enum_info(key)?.clone();
                let Some(v) = info.variant(&name.name) else {
                    self.err(
                        "E036",
                        name.span,
                        format!(
                            "`{}` has no variant `.{}`; its variants: {}",
                            key.name,
                            name.name,
                            info.variant_names()
                        ),
                    );
                    return None;
                };
                let mut fields = serde_json::Map::new();
                let given = args.map_or(&[][..], Vec::as_slice);
                if given.len() != v.fields.len() {
                    self.err(
                        "E022",
                        span,
                        format!(
                            "`.{}` has {} payload field(s): {}",
                            v.name,
                            v.fields.len(),
                            v.fields
                                .iter()
                                .map(|f| f.name.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    );
                    return None;
                }
                for (i, a) in given.iter().enumerate() {
                    let f = match &a.name {
                        Some(n) => v.fields.iter().find(|f| f.name == n.name),
                        None => v.fields.get(i),
                    }?;
                    let value = self.default_value(&f.ty, &a.value)?;
                    fields.insert(f.id.clone(), value);
                }
                Some(enum_json(&info.id, &v.id, &fields))
            }
            _ => {
                self.err(
                    "E020",
                    span,
                    format!("the default is a variant but the field has type {ty}"),
                );
                None
            }
        }
    }

    fn default_value(&mut self, ty: &Ty, e: &Expr) -> Option<serde_json::Value> {
        fn lit(e: &Expr) -> Option<(Option<Ty>, serde_json::Value)> {
            match &e.kind {
                ExprKind::Int(v) => Some((Some(Ty::Int), serde_json::json!(v))),
                ExprKind::Float(v) => Some((Some(Ty::Float), serde_json::json!(v))),
                ExprKind::Bool(v) => Some((Some(Ty::Bool), serde_json::json!(v))),
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
        if let ExprKind::Variant { name, args } = &e.kind {
            return self.variant_default(ty, name, args.as_ref(), e.span);
        }
        if let ExprKind::Call { callee, args, .. } = &e.kind
            && let ExprKind::Ident(kind) = &callee.kind
            && matches!(kind.as_str(), "Date" | "Time")
        {
            return match date_value(kind, args) {
                Ok((lt, text)) if lt.same(ty) => Some(serde_json::Value::String(text)),
                Ok((lt, _)) => {
                    self.err(
                        "E020",
                        e.span,
                        format!("the default is {lt} but the field has type {ty}"),
                    );
                    None
                }
                Err(msg) => {
                    self.err("E099", e.span, msg);
                    None
                }
            };
        }
        if let Some(args) = std_identifier_call(e) {
            return match std_identifier_value(args) {
                Ok(text) if ty.same(&Ty::Identifier) => Some(serde_json::Value::String(text)),
                Ok(_) => {
                    self.err(
                        "E020",
                        e.span,
                        format!("the default is std.Identifier but the field has type {ty}"),
                    );
                    None
                }
                Err(msg) => {
                    self.err("E099", e.span, msg);
                    None
                }
            };
        }
        let Some((lt, value)) = lit(e) else {
            self.err(
                "E019",
                e.span,
                "a field default must be a literal: a number, a string, true, false, or a variant such as `.none`",
            );
            return None;
        };
        let ok = match (&lt, ty) {
            (None, _) => false,
            (Some(t), want) => t.same(want),
        };
        if !ok {
            let found = lt.map_or_else(|| "?".to_string(), |t| t.to_string());
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
            TypeExprKind::Tuple(items) if items.is_empty() => Ty::Unit,
            TypeExprKind::Tuple(items) => {
                let tys = items.iter().map(|t| self.resolve_type(t, false)).collect();
                Ty::Tuple(tys)
            }
            TypeExprKind::Generic(name, args) if name.name == "Result" => {
                let [ok, err] = args.as_slice() else {
                    self.err(
                        "E013",
                        te.span,
                        "`Result` takes two type arguments, written `Result<T, E>`",
                    );
                    return Ty::Error;
                };
                let ok = self.resolve_type(ok, false);
                let err = self.resolve_type(err, false);
                Ty::Result(Box::new(ok), Box::new(err))
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
                // A field's map may hold a set per key (`Map<Set<Principal>>`, who reacted with
                // each emoji): stored element by element, so it is a field's container too.
                let set_per_key = field_position
                    && name.name == "Map"
                    && args.first().is_some_and(|a| {
                        matches!(&a.kind, TypeExprKind::Generic(inner, _) if inner.name == "Set")
                    });
                let inner = args
                    .first()
                    .map_or(Ty::Error, |a| self.resolve_type(a, set_per_key));
                if name.name == "Filter" {
                    return match inner {
                        Ty::Record(key) if field_position => {
                            self.err("E023", te.span, format!("`Filter<{}>` is a value code holds and a query takes, not a stored field's type", key.name));
                            Ty::Error
                        }
                        Ty::Record(key) => Ty::Filter(key),
                        Ty::Error => Ty::Error,
                        other => {
                            let t = other.display(&self.env.app);
                            self.err("E013", te.span, format!("`Filter<T>` is the filters of a type's items; `{t}` is no resource type"));
                            Ty::Error
                        }
                    };
                }
                let boxed = Box::new(inner);
                let ty = match name.name.as_str() {
                    "List" => Ty::List(boxed),
                    "Option" => Ty::Option(boxed),
                    "Set" => Ty::Set(boxed),
                    "Map" => Ty::Map(boxed),
                    _ => Ty::Collection(boxed),
                };
                if ty.is_container() && !field_position {
                    self.err("E023", te.span, format!("`{}` can only be the type of a field or of a view's parameter; use `List<T>` for values you pass around", name.name));
                    return Ty::Error;
                }
                ty
            }
            TypeExprKind::Named(path) => self.named_type(path, te.span),
            TypeExprKind::Union(cases) => self.union_type(cases),
            TypeExprKind::Fn(..) => {
                self.err("E013", te.span, "a function type `(T) -> R` is the type of a parameter of a view only: such a view is expanded where it is called");
                Ty::Error
            }
        }
    }

    /// What the alias `name` names, resolved once where it is used.
    fn alias_type(&mut self, name: &str, te: &TypeExpr) -> Ty {
        if self.resolving.iter().any(|n| n == name) {
            self.err(
                "E013",
                te.span,
                format!("`{name}` names itself; a type alias names another type"),
            );
            return Ty::Error;
        }
        self.resolving.push(name.to_owned());
        let ty = self.resolve_type(te, false);
        self.resolving.pop();
        ty
    }

    /// `A | B`: a resource of any of the cases, each a resource type.
    fn union_type(&mut self, cases: &[TypeExpr]) -> Ty {
        let mut keys: Vec<TypeKey> = Vec::new();
        for case in cases {
            match self.resolve_type(case, false) {
                Ty::Record(key) => keys.push(key),
                Ty::Union(more) => keys.extend(more),
                Ty::Error => return Ty::Error,
                other => {
                    let t = other.display(&self.env.app);
                    self.err("E109", case.span, format!("a union's cases are resource types, and {t} is not one; for named cases without data declare an `enum`"));
                    return Ty::Error;
                }
            }
        }
        keys.sort();
        keys.dedup();
        match keys.as_slice() {
            [one] => Ty::Record(one.clone()),
            _ => Ty::Union(keys),
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
                "Date" => return Ty::Date,
                "Time" => return Ty::Time,
                "Duration" => return Ty::Duration,
                "RichText" => return Ty::RichText,
                "Bytes" => return Ty::Bytes,
                "Response" => return Ty::Response,
                "Action" => return Ty::Action,
                "View" => return Ty::View,
                name if self.own_types.contains(name) => {
                    return Ty::Record(TypeKey {
                        app: self.env.app.clone(),
                        name: name.to_string(),
                    });
                }
                name if self.own_enums.contains(name) => {
                    return Ty::Enum(TypeKey {
                        app: self.env.app.clone(),
                        name: name.to_string(),
                    });
                }
                name if self.own_aliases.contains_key(name) => {
                    let te = self.own_aliases.get(name).cloned();
                    return te.map_or(Ty::Error, |te| self.alias_type(name, &te));
                }
                name => {
                    let app = &self.env.app;
                    if let Some((_, ty)) = self
                        .env
                        .aliases
                        .iter()
                        .find(|(k, _)| k.app == *app && k.name == name)
                    {
                        return ty.clone();
                    }
                }
            }
            let mut known: Vec<String> = self
                .own_types
                .iter()
                .chain(self.own_enums.iter())
                .cloned()
                .collect();
            known.sort();
            let known = if known.is_empty() {
                "this module declares none".to_string()
            } else {
                known.join(", ")
            };
            let held = if *single == "Identifier" {
                format!("; the standard library has `std.{single}`")
            } else {
                String::new()
            };
            self.err("E013", span, format!("unknown type `{single}`; types here: Int, Float, Bool, Text, RichText, Bytes, Principal, Date, Time, Duration, Action, View and {known}{held}"));
            return Ty::Error;
        }
        if let Some(ty) = crate::types::named(&dotted) {
            return ty;
        }
        let (prefix, name) = dotted.split_at(dotted.len().saturating_sub(1));
        let prefix = prefix.join(".");
        let name = name.first().copied().unwrap_or("");
        let Some(import) = self.env.find_import(&prefix) else {
            self.err("E013", span, format!("unknown type `{}`: `{prefix}` is not an imported package; add `use <app id> as {prefix}`", dotted.join(".")));
            return Ty::Error;
        };
        // The library declares no types: what it names under `std.` are the language's
        // text-held types.
        if import.app == crate::stdlib::APP {
            self.err("E013", span, format!("the standard library has no type `{name}`; its types are `std.Identifier` and `std.RichText`"));
            return Ty::Error;
        }
        let key = TypeKey {
            app: import.app.clone(),
            name: name.to_string(),
        };
        if self.env.enum_info(&key).is_some() {
            return Ty::Enum(key);
        }
        if let Some((_, ty)) = self.env.aliases.iter().find(|(k, _)| *k == key) {
            return ty.clone();
        }
        if self.env.type_info(&key).is_none() {
            let names: Vec<&str> = self
                .env
                .types
                .iter()
                .map(|t| &t.key)
                .chain(self.env.enums.iter().map(|e| &e.key))
                .filter(|k| k.app == key.app)
                .map(|k| k.name.as_str())
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

    /// Checks the parameters a view supplies (`above: Option<Task> (above)`): only an action's,
    /// starting right after its subject, each optional and with no default, and each a place a
    /// view knows (`above`).
    fn places(&mut self, written: &[ast::Param], params: &[ParamInfo], kind: CKind, _span: Span) {
        let mut in_run = true;
        for (i, p) in written.iter().enumerate() {
            let Some(place) = &p.place else {
                if i > 0 {
                    in_run = false;
                }
                continue;
            };
            if kind != CKind::Action {
                self.err("E052", p.name.span, format!("`{}` is a parameter of the {}; only an action takes a place a view supplies (`(above)`)", p.name.name, kind.word()));
                continue;
            }
            if i == 0 {
                self.err("E053", place.span, format!("`{}` is a place a view supplies: it follows the action's subject directly, as `{}: Option<T> (above)`", p.name.name, p.name.name));
                continue;
            }
            if place.name != "above" {
                self.err("E054", place.span, format!("`{}` names no place a view supplies; the one there is is `above`, the item just above the one the action is offered on", place.name));
                continue;
            }
            if !in_run {
                self.err("E053", place.span, format!("`{}` is a place a view supplies: it follows the action's subject directly, as `{}: Option<T> (above)`", p.name.name, p.name.name));
                continue;
            }
            if !matches!(params.get(i).map(|p| &p.ty), Some(Ty::Option(_))) {
                self.err("E055", p.ty.span, format!("`{}` is a place a view supplies, so it is `Option<…>`: a view may have nothing above the item it offers the action on", p.name.name));
            }
            if let Some(default) = &p.default {
                self.err("E056", default.span, format!("`{}` is a place a view supplies: no caller gives it, so it takes no default", p.name.name));
            }
        }
    }

    /// The parameters of a callable. A view's may be a collection, a set or a map, as a field
    /// is: a view is handed it live (views are components, and these are their props).
    ///
    /// An action's one `Text` parameter is the one an entry fills with the words nothing else
    /// takes (`crate::entry`), so it is the parameter an entry's action is read by.
    fn params(&mut self, params: &[ast::Param], view: bool, action: bool) -> Vec<ParamInfo> {
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
            let ty = self.resolve_type(&p.ty, view);
            out.push(ParamInfo {
                name: p.name.name.clone(),
                ty,
                default: None,
                place: action && p.place.is_some(),
                entry: false,
            });
        }
        if action {
            let mut texts = out.iter_mut().filter(|p| p.ty == Ty::Text);
            if let (Some(one), None) = (texts.next(), texts.next()) {
                one.entry = true;
            }
        }
        out
    }

    fn callables(&mut self, module: &ast::Module, lock: &IdsLock) {
        let mut func = 0usize;
        // The presentation views of actions, which get functions after every declared one.
        let mut presents: Vec<(String, ParamInfo)> = Vec::new();
        // The defaults of action parameters, which get functions after the presentation views:
        // the callable's index and the parameter's.
        let mut defaults: Vec<(usize, usize)> = Vec::new();
        for item in &module.items {
            if let Item::Scenario(sc) = item {
                let index = func;
                func += 1;
                self.scenario(sc, lock, index);
                continue;
            }
            if let Item::View(v) = item
                && crate::checker::is_template(v)
            {
                self.template(v);
                continue;
            }
            let (kind, name, params, result) = match item {
                Item::Action(a) => (CKind::Action, &a.name, &a.params, a.result.as_ref()),
                Item::View(v) => (CKind::View, &v.name, &v.params, None),
                Item::Fn(f) => (CKind::Fn, &f.name, &f.params, Some(&f.result)),
                Item::Use(_)
                | Item::Type(_)
                | Item::Alias(_)
                | Item::Enum(_)
                | Item::Root(_)
                | Item::DefaultView(_)
                | Item::RowView(_)
                | Item::Views(_)
                | Item::Scenario(_)
                | Item::Trigger(_) => continue,
            };
            let index = func;
            func += 1;
            if !name_case_ok(&name.name, false) {
                self.err("E016", name.span, format!("`{}` names a {}, which starts with a lowercase letter (uppercase is for types)", name.name, kind.word()));
            }
            // A view's name is a UI statement, so it cannot be a primitive's; the others are
            // called in expressions, where the built-in functions and values are taken. A view
            // is not called in an expression, so it may be named after one: Today's is `today`.
            let taken = if kind == CKind::View {
                prims::prim(&name.name).is_some()
            } else {
                prims::BUILTINS.contains(&name.name.as_str())
            };
            if taken {
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
            if self.env.own_callable(&name.name).is_some() {
                self.err(
                    "E011",
                    name.span,
                    format!(
                        "`{}` is declared twice; actions, views and fns share one namespace",
                        name.name
                    ),
                );
            }
            let ast_params = params;
            let params = self.params(params, kind == CKind::View, kind == CKind::Action);
            self.places(ast_params, &params, kind, name.span);
            if kind == CKind::Action {
                let at = self.env.callables.len();
                defaults.extend(
                    ast_params
                        .iter()
                        .enumerate()
                        .filter(|(_, p)| p.default.is_some())
                        .map(|(i, _)| (at, i)),
                );
            } else {
                for p in ast_params {
                    if let Some(d) = &p.default {
                        self.err("E028", d.span, format!("`{}` is a parameter of the {} `{}`; only an action's parameters take a default, which the action's invoker gets when they leave it out", p.name.name, kind.word(), name.name));
                    }
                }
            }
            let result = match (kind, result) {
                (CKind::View, _) => Ty::View,
                (_, Some(t)) => self.resolve_type(t, false),
                (_, None) => Ty::Unit,
            };
            let present = match item {
                Item::Action(a) => {
                    let mut p = self.presentation(a, &params);
                    if a.presents_state() {
                        match params.first() {
                            Some(subject) => {
                                let key = crate::ids::present_key(&name.name);
                                p.view = Some(id_of(lock, &key));
                                presents.push((key, subject.clone()));
                            }
                            None => self.err("E090", name.span, format!("`{}` presents a state of its subject, its first parameter, and has none; take the resource first, or write `label:` and `icon:` as plain values", name.name)),
                        }
                    }
                    p
                }
                _ => Presentation::default(),
            };
            let uses = self.uses_of(item);
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
                uses,
                present,
                scenario: false,
            });
            if let Item::View(v) = item {
                self.anchors(v, lock);
                self.states(v, lock);
            }
        }
        for (key, subject) in presents {
            self.env.callables.push(Callable {
                kind: CKind::View,
                id: id_of(lock, &key),
                name: key,
                app: self.env.app.clone(),
                own: true,
                func: Some(func),
                params: vec![subject],
                result: Ty::View,
                class: None,
                uses: BTreeSet::new(),
                present: Presentation::default(),
                scenario: false,
            });
            func += 1;
        }
        let actions: Vec<&ast::ActionDecl> = module
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Action(a) => Some(a),
                _ => None,
            })
            .collect();
        for (at, index) in defaults {
            let Some(callable) = self.env.callables.get_mut(at) else {
                continue;
            };
            let written = actions
                .iter()
                .find(|a| a.name.name == callable.name)
                .and_then(|a| a.params.get(index))
                .and_then(|p| p.default.as_ref());
            let (Some(written), Some(param)) = (written, callable.params.get_mut(index)) else {
                continue;
            };
            param.default = Some(ParamDefault {
                export: default_export(&callable.id, &param.name),
                func: Some(func),
                text: crate::print::expr_source(written),
            });
            self.env.defaults.push(DefaultFn {
                action: callable.name.clone(),
                param: index,
                func,
            });
            func += 1;
        }
        self.one_primary(module);
    }

    /// A view of the module expanded where it is called: its name taken, its body checked at
    /// each call, its `state` declarations made the view's own state.
    fn template(&mut self, v: &ast::ViewDecl) {
        if self.env.own_callable(&v.name.name).is_some()
            || self.env.template(None, &v.name.name).is_some()
        {
            self.err(
                "E011",
                v.name.span,
                format!(
                    "`{}` is declared twice; actions, views and fns share one namespace",
                    v.name.name
                ),
            );
            return;
        }
        self.env.templates.push(Template {
            ns: None,
            decl: v.clone(),
        });
    }

    /// A scenario's hidden action: no parameters, named by the scenario, returning what it
    /// declares (the resource a window shows once the scenario is staged).
    fn scenario(&mut self, sc: &ast::ScenarioDecl, lock: &IdsLock, index: usize) {
        let name = &sc.name;
        if name.trim().is_empty() || name.trim() != name || name.contains(['=', '\n']) {
            self.err("E094", sc.name_span, "a scenario's name is a short phrase without `=`, line breaks or spaces at its ends, such as `scenario \"a week of notes\"`");
        }
        if self.env.scenario(name).is_some() {
            self.err(
                "E094",
                sc.name_span,
                format!(
                    "the scenario \"{name}\" is declared twice; give each scenario its own name"
                ),
            );
        }
        let result = match &sc.result {
            Some(t) => self.resolve_type(t, false),
            None => Ty::Unit,
        };
        self.env.callables.push(Callable {
            kind: CKind::Action,
            name: name.clone(),
            id: id_of(lock, &crate::ids::scenario_key(name)),
            app: self.env.app.clone(),
            own: true,
            func: Some(index),
            params: Vec::new(),
            result,
            class: None,
            // Seed data runs where a developer runs it, and is never replayed: it reads any
            // effect it likes.
            uses: crate::effects::Effect::ALL.iter().copied().collect(),
            present: Presentation {
                label: name.clone(),
                ..Presentation::default()
            },
            scenario: true,
        });
    }

    /// An action's label, icon and role, from its modifiers and its `(icon: ..., label: ...)`.
    /// A `label:` or `icon:` written as an `if` follows the subject; the static one is then its
    /// `else` branch's value (or its first), what a menu shows without evaluating anything.
    fn presentation(&mut self, a: &ast::ActionDecl, params: &[ParamInfo]) -> Presentation {
        let subject = params.first().and_then(|p| match &p.ty {
            Ty::Record(key) => self.env.type_info(key).cloned(),
            _ => None,
        });
        let at_rest = |cond: &Expr| at_rest(cond, a, subject.as_ref(), 0);
        let mut out = Presentation {
            label: action_label(&a.name.name, &a.doc.join("\n")),
            icon: None,
            role: a.role.map(|(r, _)| r.word().to_string()),
            view: None,
            label_text: None,
            icon_when: None,
            on: None,
            key: None,
            group: None,
            did: None,
        };
        let mut seen: Vec<&str> = Vec::new();
        for opt in &a.opts {
            let name = opt.name.name.as_str();
            if seen.contains(&name) {
                self.err("E090", opt.name.span, format!("`{name}:` is given twice"));
                continue;
            }
            seen.push(name);
            match (name, &opt.value.kind) {
                ("icon", ExprKind::Variant { name: icon, args: None }) => {
                    out.icon = self.icon(icon);
                }
                ("icon", ExprKind::If { .. }) => {
                    if let Some(icon) = fallback_icon(&opt.value, &at_rest) {
                        out.icon = prims::icon(&icon.name).map(str::to_string);
                    }
                    out.icon_when = Some(opt.value.clone());
                }
                ("icon", _) => self.err(
                    "E090",
                    opt.value.span,
                    "`icon:` takes an icon written with a dot, such as `icon: .plus`, or an `if` over the subject: `icon: if n.pinned { .pin_off } else { .pin }`",
                ),
                ("label", ExprKind::Str(_)) if plain_text(&opt.value).is_some() => {
                    out.label = plain_text(&opt.value).unwrap_or_default();
                }
                // `"Move to {col.title}"`: read with the arguments where it is applied; the
                // action's name stands in elsewhere.
                ("label", ExprKind::Str(_)) => out.label_text = Some(opt.value.clone()),
                // `if n.pinned { "Unpin" } else { "Pin" }`: read where it is applied, as a
                // button for it draws; the label at rest stands in elsewhere.
                ("label", ExprKind::If { .. }) => {
                    if let Some(label) = fallback_label(&opt.value, &at_rest) {
                        out.label = label;
                    }
                    out.label_text = Some(opt.value.clone());
                }
                ("label", _) => self.err(
                    "E090",
                    opt.value.span,
                    "`label:` takes text in quotes, without `{ }`, such as `label: \"New note\"`, or an `if` over the subject: `label: if n.pinned { \"Unpin\" } else { \"Pin\" }`",
                ),
                ("on", _) => out.on = Some(opt.value.clone()),
                // `key: "Ctrl+Enter"`: the shortcut that runs the action while its window has the
                // keyboard, and (on a row's action) while that row is selected.
                ("key", ExprKind::Str(_)) if plain_text(&opt.value).is_some() => {
                    match declared_key(&opt.value) {
                        Ok(key) => out.key = Some(key),
                        Err(said) => self.err("E090", opt.value.span, said),
                    }
                }
                ("key", _) => self.err(
                    "E090",
                    opt.value.span,
                    "`key:` takes a shortcut in quotes, without `{ }`: `key: \"Ctrl+Enter\"`, `key: \"F2\"`, `key: \"e\"`",
                ),
                // `group: "Reply"`: the run of the resource's actions this one stands in, by
                // the app's own name for it.
                ("group", ExprKind::Str(_)) if plain_text(&opt.value).is_some() => {
                    let group = plain_text(&opt.value).unwrap_or_default();
                    if group.trim().is_empty() {
                        self.err(
                            "E090",
                            opt.value.span,
                            "`group:` names a run of the resource's actions: `group: \"Reply\"`",
                        );
                    } else {
                        out.group = Some(group);
                    }
                }
                ("group", _) => self.err(
                    "E090",
                    opt.value.span,
                    "`group:` takes the run's name in quotes, without `{ }`: `group: \"Reply\"`",
                ),
                // `did: "moved this"`: what the action did, said after who did it, so history
                // reads as people say it ("Ana moved this"). A phrase of the app's own, one
                // verb for each thing a person does.
                ("did", ExprKind::Str(_)) if plain_text(&opt.value).is_some() => {
                    let said = plain_text(&opt.value).unwrap_or_default();
                    if said.trim().is_empty() {
                        self.err(
                            "E090",
                            opt.value.span,
                            "`did:` says what the action did, after who did it: `did: \"moved this\"`",
                        );
                    } else {
                        out.did = Some(said);
                    }
                }
                ("did", _) => self.err(
                    "E090",
                    opt.value.span,
                    "`did:` takes words in quotes, without `{ }`: `did: \"moved this\"`",
                ),
                (other, _) => self.err(
                    "E090",
                    opt.name.span,
                    format!("an action takes the options `icon:`, `label:`, `on:`, `key:`, `group:` and `did:`; `{other}:` is not one. Mark the main action with `primary action` and a destructive one with `destructive action`"),
                ),
            }
        }
        out
    }

    /// The kebab-case name of the icon `.name`, or an error naming close ones.
    fn icon(&mut self, name: &ast::Ident) -> Option<String> {
        if let Some(kebab) = prims::icon(&name.name) {
            return Some(kebab.to_string());
        }
        self.err("E091", name.span, prims::unknown_icon(&name.name));
        None
    }

    /// At most one `primary` action per subject type (its first parameter's record type).
    fn one_primary(&mut self, module: &ast::Module) {
        let mut seen: Vec<(TypeKey, String)> = Vec::new();
        for item in &module.items {
            let Item::Action(a) = item else { continue };
            let Some((ast::ActionRole::Primary, span)) = a.role else {
                continue;
            };
            let subject = self
                .env
                .own_callable(&a.name.name)
                .and_then(|c| c.params.first())
                .and_then(|p| match &p.ty {
                    Ty::Record(key) => Some(key.clone()),
                    _ => None,
                });
            let Some(subject) = subject else {
                self.err("E090", span, format!("a primary action is the main thing to do with a resource, so its first parameter is a record; `{}` has none. Remove `primary`, or take the resource first", a.name.name));
                continue;
            };
            if let Some((_, first)) = seen.iter().find(|(k, _)| *k == subject) {
                self.err("E090", span, format!("`{}` already has a primary action, `{first}`; a resource has one. Remove `primary` from `{}` or from `{first}`", subject.name, a.name.name));
                continue;
            }
            seen.push((subject, a.name.name.clone()));
        }
    }

    /// Two actions of one subject that declare the same shortcut: one would silently take the
    /// other's key wherever both are offered, so the app is told which action holds it. An
    /// action on an item is answered where that item is selected or its window shows it; one on
    /// the app's root is answered window-wide.
    fn one_shortcut_each(&mut self, module: &ast::Module) {
        let mut seen: Vec<(Option<TypeKey>, String, String)> = Vec::new();
        for item in &module.items {
            let Item::Action(a) = item else { continue };
            let Some(opt) = a.opts.iter().find(|o| o.name.name == "key") else {
                continue;
            };
            let Ok(key) = declared_key(&opt.value) else {
                continue;
            };
            let scope = self
                .env
                .own_callable(&a.name.name)
                .and_then(|c| self.shortcut_scope(c));
            if let Some((_, _, holder)) = seen.iter().find(|(s, k, _)| *s == scope && *k == key) {
                let said = match &scope {
                    Some(ty) => format!(
                        "`{key}` is already `{holder}`'s shortcut on `{}`: two of its actions cannot answer the same keys",
                        ty.name
                    ),
                    None => format!(
                        "`{key}` is already `{holder}`'s shortcut: two actions of the window cannot answer the same keys"
                    ),
                };
                self.err("E090", opt.value.span, said);
                continue;
            }
            seen.push((scope, key, a.name.name.clone()));
        }
    }

    /// What an action's shortcut is answered on: the first record it takes that is not the app's
    /// root, or `None` for one offered on the root alone, which its window answers wherever the
    /// person is.
    fn shortcut_scope(&self, c: &Callable) -> Option<TypeKey> {
        c.params.iter().find_map(|p| match &p.ty {
            Ty::Record(key) if !self.env.roots.contains(key) => Some(key.clone()),
            _ => None,
        })
    }

    /// The `state` declarations at the top of a view body.
    fn states(&mut self, v: &ast::ViewDecl, lock: &IdsLock) {
        for s in &v.body {
            let ast::UiKind::State(d) = &s.kind else {
                continue;
            };
            if self
                .env
                .states
                .iter()
                .any(|x| x.view == v.name.name && x.name == d.name.name)
            {
                self.err(
                    "E012",
                    d.name.span,
                    format!("view `{}` already declares `{}`", v.name.name, d.name.name),
                );
                continue;
            }
            let ty = self.resolve_type(&d.ty, false);
            if matches!(ty, Ty::Action | Ty::View) {
                self.err("E066", d.ty.span, format!("view state cannot hold `{ty}`; it holds values: scalars, Text, records, enums, List and Option"));
            }
            self.env.states.push(StateInfo {
                view: v.name.name.clone(),
                name: d.name.name.clone(),
                id: id_of(lock, &format!("{}.{}", v.name.name, d.name.name)),
                ty,
            });
        }
    }

    /// `root T`, `default view v for T` and `row view v for T` (workspace-model.md,
    /// "Apps"), and the app's icon.
    fn package_decls(&mut self, module: &ast::Module) {
        if let Some(icon) = &module.app.icon
            && prims::icon(icon).is_none()
        {
            self.err("E091", module.app.span, prims::unknown_icon(icon));
        }
        // Roots first, so a default view may name one.
        let roots_first = module
            .items
            .iter()
            .filter(|i| matches!(i, Item::Root(_)))
            .chain(module.items.iter().filter(|i| !matches!(i, Item::Root(_))));
        for item in roots_first {
            match item {
                Item::Root(r) => {
                    let Some(key) = self.own_record(&r.ty, "root") else {
                        continue;
                    };
                    if let Some(name) = &r.name {
                        if !name_case_ok(&name.name, false) {
                            self.err(
                                "E016",
                                name.span,
                                format!(
                                    "a root's name starts with a lowercase letter; rename `{}`",
                                    name.name
                                ),
                            );
                        }
                        if self.env.named_roots.iter().any(|(n, _)| *n == name.name) {
                            self.err(
                                "E011",
                                name.span,
                                format!("the root `{}` is declared twice", name.name),
                            );
                            continue;
                        }
                        self.env.named_roots.push((name.name.clone(), key.clone()));
                    }
                    if self.env.roots.contains(&key) {
                        self.err(
                            "E011",
                            r.span,
                            format!("`{}` is declared a root twice", key.name),
                        );
                        continue;
                    }
                    self.env.roots.push(key);
                }
                Item::RowView(d) => self.row_view_decl(d),
                Item::DefaultView(d) => {
                    // `default view talk for talks`: for the type of the root `talks`.
                    let named = match &d.ty.kind {
                        TypeExprKind::Named(path) => match path.as_slice() {
                            [one] => self
                                .env
                                .named_roots
                                .iter()
                                .find(|(n, _)| *n == one.name)
                                .map(|(_, k)| k.clone()),
                            _ => None,
                        },
                        _ => None,
                    };
                    let Some(key) = named.or_else(|| self.own_record(&d.ty, "default view")) else {
                        continue;
                    };
                    if self.env.default_views.iter().any(|(k, _)| *k == key) {
                        self.err(
                            "E011",
                            d.span,
                            format!("`{}` has two default views", key.name),
                        );
                        continue;
                    }
                    let fits = self
                        .env
                        .own_callable(&d.view.name)
                        .filter(|c| c.kind == CKind::View)
                        .map(|c| matches!(c.params.as_slice(), [p] if p.ty == Ty::Record(key.clone())));
                    match fits {
                        Some(true) => self.env.default_views.push((key, d.view.name.clone())),
                        Some(false) => self.err("E018", d.view.span, format!("the default view of `{}` takes the resource and nothing else: `view {}(x: {})`", key.name, d.view.name, key.name)),
                        None => self.err("E018", d.view.span, format!("`{}` is not a view of this package", d.view.name)),
                    }
                }
                Item::Views(d) => self.views_decl(d),
                _ => {}
            }
        }
        let unnamed: Vec<(Span, String)> = self
            .env
            .views_of
            .iter()
            .filter_map(|(key, views, span)| {
                let (_, default) = self.env.default_views.iter().find(|(k, _)| k == key)?;
                (!views.iter().any(|v| &v.name == default)).then(|| {
                    (
                        *span,
                        format!(
                            "the views of `{}` leave out its default view `{default}`, the one a window opens on",
                            key.name
                        ),
                    )
                })
            })
            .collect();
        for (span, what) in unnamed {
            self.err("E018", span, what);
        }
        let missing: Vec<TypeKey> = self
            .env
            .roots
            .iter()
            .filter(|k| !self.env.default_views.iter().any(|(d, _)| d == *k))
            .cloned()
            .collect();
        for key in missing {
            let span = module
                .items
                .iter()
                .find_map(|i| match i {
                    Item::Root(r) => Some(r.span),
                    _ => None,
                })
                .unwrap_or_default();
            self.err("E018", span, format!("the root `{0}` has no default view, which is what opening it shows; add `default view <view> for {0}`", key.name));
        }
    }

    /// The own record type a package declaration names.
    fn own_record(&mut self, te: &TypeExpr, what: &str) -> Option<TypeKey> {
        match self.resolve_type(te, false) {
            Ty::Record(key) if key.app == self.env.app => Some(key),
            Ty::Error => None,
            other => {
                let t = other.display(&self.env.app);
                self.err(
                    "E018",
                    te.span,
                    format!("a {what} is one of this package's record types, not {t}"),
                );
                None
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
    let own_enums = env
        .enums
        .iter()
        .filter(|t| t.key.app == env.app)
        .map(|t| t.key.name.clone())
        .collect();
    let mut r = Resolver {
        env: env.clone(),
        diags,
        own_types,
        own_enums,
        own_aliases: HashMap::new(),
        resolving: Vec::new(),
    };
    r.resolve_type(te, false)
}

/// The one expression of a block written as `{ e }`.
fn block_expr(b: &ast::Block) -> Option<&Expr> {
    match b.stmts.as_slice() {
        [
            ast::Stmt {
                kind: ast::StmtKind::Expr(e),
                ..
            },
        ] => Some(e),
        _ => None,
    }
}

/// What a dynamic option shows without evaluation: the value of its `else` branch, or of its
/// first branch that has one.
/// What an `if` of `label:` or `icon:` shows where it is not evaluated (a menu of every item,
/// History): the branch a subject at rest takes, when `at_rest` can tell (see [`at_rest`]),
/// else the `else` branch.
fn fallback<T>(
    e: &Expr,
    leaf: &impl Fn(&Expr) -> Option<T>,
    at_rest: &impl Fn(&Expr) -> Option<bool>,
) -> Option<T> {
    match &e.kind {
        ExprKind::If { cond, then, els } => {
            let then = || block_expr(then).and_then(|x| fallback(x, leaf, at_rest));
            let els = || els.as_deref().and_then(|x| fallback(x, leaf, at_rest));
            if at_rest(cond) == Some(true) {
                then().or_else(els)
            } else {
                els().or_else(then)
            }
        }
        ExprKind::Block(b) => block_expr(b).and_then(|x| fallback(x, leaf, at_rest)),
        _ => leaf(e),
    }
}

fn fallback_label(e: &Expr, at_rest: &impl Fn(&Expr) -> Option<bool>) -> Option<String> {
    fallback(e, &plain_text, at_rest)
}

fn fallback_icon(e: &Expr, at_rest: &impl Fn(&Expr) -> Option<bool>) -> Option<ast::Ident> {
    fallback(
        e,
        &|x: &Expr| match &x.kind {
            ExprKind::Variant { name, args: None } => Some(name.clone()),
            _ => None,
        },
        at_rest,
    )
}

/// The value of an action option's condition for a subject at rest (each Bool field at its
/// default) invoked with every parameter's default: `n.pinned` is `false`, and `done` with
/// `done: Bool = not item.done` is `true`. `None` for a condition that reads anything else.
fn at_rest(
    cond: &Expr,
    a: &ast::ActionDecl,
    subject: Option<&TypeInfo>,
    depth: u8,
) -> Option<bool> {
    if depth > 8 {
        return None;
    }
    let deeper = |e: &Expr| at_rest(e, a, subject, depth.saturating_add(1));
    match &cond.kind {
        ExprKind::Bool(b) => Some(*b),
        ExprKind::Unary {
            op: UnOp::Not,
            operand,
        } => deeper(operand).map(|b| !b),
        ExprKind::Block(b) => block_expr(b).and_then(deeper),
        ExprKind::Ident(name) => a
            .params
            .iter()
            .skip(1)
            .find(|p| &p.name.name == name)
            .and_then(|p| p.default.as_ref())
            .and_then(deeper),
        ExprKind::Field { base, name } => {
            let ExprKind::Ident(base) = &base.kind else {
                return None;
            };
            if a.params.first().is_none_or(|p| &p.name.name != base) {
                return None;
            }
            let field = subject?.field(&name.name)?;
            match (&field.ty, &field.default) {
                (Ty::Bool, None) => Some(false),
                (Ty::Bool, Some(value)) => value.as_bool(),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Names the language gives types of its own; a package cannot declare them.
fn is_builtin_type(name: &str) -> bool {
    matches!(
        name,
        "Int"
            | "Float"
            | "Bool"
            | "Text"
            | "Principal"
            | "Date"
            | "Time"
            | "Duration"
            | "Action"
            | "View"
            | "List"
            | "Set"
            | "Map"
            | "Collection"
            | "Option"
            | "Result"
    )
}

/// An enum value as the runtime stores it in a register (see the runtime's `value::to_json`).
pub fn enum_json(
    ty: &str,
    variant: &str,
    fields: &serde_json::Map<String, serde_json::Value>,
) -> serde_json::Value {
    serde_json::json!({ "enum": ty, "variant": variant, "fields": fields })
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
            ast::UiKind::For { body, .. } | ast::UiKind::Slot { body, .. } => collect(body, out),
            ast::UiKind::If { then, els, .. } | ast::UiKind::IfLet { then, els, .. } => {
                collect(then, out);
                if let Some(e) = els {
                    collect(e, out);
                }
            }
            ast::UiKind::Match { arms, .. } => {
                for arm in arms {
                    collect(&arm.body, out);
                }
            }
            ast::UiKind::Let { .. }
            | ast::UiKind::State(_)
            | ast::UiKind::Fact { .. }
            | ast::UiKind::On { .. } => {}
        }
    }
}

/// The package `use <app>` imports among `available`: the one whose id it is, else the one
/// published under it before (`previously:`). An id a package still has as its own is that
/// package's, whoever else names it as an earlier one.
pub fn find_package<'a>(available: &'a [PackageMeta], app: &str) -> Option<&'a PackageMeta> {
    available
        .iter()
        .find(|m| m.app == app)
        .or_else(|| {
            available
                .iter()
                .find(|m| m.previously.iter().any(|old| old == app))
        })
        // The agent library ships with the compiler, as the standard library does: a package
        // `use`s it without it being one of the packages around it.
        .or_else(|| (app == crate::agent::APP).then(crate::agent::meta))
}
