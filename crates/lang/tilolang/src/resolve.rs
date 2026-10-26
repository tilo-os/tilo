//! Name resolution: the module's declarations, its imports, and the signatures the checker
//! needs before it looks at any body.

use crate::ast::{self, Expr, ExprKind, Item, MergeSpec, StateLayer, TypeExpr, TypeExprKind, UnOp};
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
    /// `title` or `body`, from the field's marker.
    pub role: Option<String>,
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

/// One variant of a declared enum and its payload fields.
#[derive(Clone, Debug)]
pub struct VariantInfo {
    pub name: String,
    pub id: String,
    pub doc: String,
    pub label: String,
    /// `(icon: .x)`: a kebab-case name of the platform's icon set.
    pub icon: Option<String>,
    pub fields: Vec<FieldInfo>,
}

/// A declared `enum`.
#[derive(Clone, Debug)]
pub struct EnumInfo {
    pub key: TypeKey,
    pub id: String,
    pub doc: String,
    pub variants: Vec<VariantInfo>,
}

impl EnumInfo {
    pub fn variant(&self, name: &str) -> Option<&VariantInfo> {
        self.variants.iter().find(|v| v.name == name)
    }

    pub fn variant_names(&self) -> String {
        self.variants
            .iter()
            .map(|v| format!(".{}", v.name))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// A `window` or `lens` declaration of a view.
#[derive(Clone, Debug)]
pub struct StateInfo {
    pub view: String,
    pub name: String,
    pub id: String,
    pub layer: StateLayer,
    pub ty: Ty,
}

#[derive(Clone, Debug)]
pub struct ParamInfo {
    pub name: String,
    pub ty: Ty,
    /// An action parameter's invocation-time default (`done: Bool = not item.done`).
    pub default: Option<ParamDefault>,
}

/// How an action parameter's default is computed: when the action is invoked without the
/// argument, over the arguments before it, on the invoker's state, and recorded as the argument.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParamDefault {
    /// The module export computing it: it takes the parameters before this one.
    pub export: String,
    /// The function computing it, when the action is declared in this module.
    pub func: Option<usize>,
    /// The default as written (`not item.done`), for descriptions.
    pub text: String,
}

/// The function of an action parameter's default, which the checker lowers.
#[derive(Clone, Debug)]
pub struct DefaultFn {
    /// The action's name.
    pub action: String,
    /// The index of the parameter in the action's parameters.
    pub param: usize,
    /// Index into the module's functions.
    pub func: usize,
}

/// The export name of the default of the parameter `param` of the action `action` (an id).
pub fn default_export(action: &str, param: &str) -> String {
    format!("d_{action}_{param}")
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
    /// How the platform presents an action or flow: its label, icon and role.
    pub present: Presentation,
    /// The hidden action of a `scenario`; its name is the scenario's, which source cannot call.
    pub scenario: bool,
}

/// What a button or a menu shows for an action or flow, from its declaration.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Presentation {
    pub label: String,
    /// A kebab-case name of the platform's icon set.
    pub icon: Option<String>,
    /// `primary` or `destructive`.
    pub role: Option<String>,
    /// The id of the hidden view that presents the action's state for its subject, when its
    /// `label:` or `icon:` is an `if`, or it declares `on:`.
    pub view: Option<String>,
}

/// `new_note` as "New note".
pub fn sentence_case(name: &str) -> String {
    let spaced = name.replace('_', " ");
    let mut chars = spaced.trim().chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The first line of a doc comment without its closing period, when there is one.
fn doc_line(doc: &str) -> Option<String> {
    let line = doc.lines().next()?.trim();
    let line = line.strip_suffix('.').unwrap_or(line).trim_end();
    (!line.is_empty()).then(|| line.to_string())
}

/// An action's or flow's label: its name in sentence case, or its doc's first line when that is
/// shorter.
pub fn action_label(name: &str, doc: &str) -> String {
    let named = sentence_case(name);
    match doc_line(doc) {
        Some(line) if line.chars().count() < named.chars().count() => line,
        _ => named,
    }
}

/// A variant's label: its doc's first line, or its name in sentence case.
pub fn variant_label(name: &str, doc: &str) -> String {
    doc_line(doc).unwrap_or_else(|| sentence_case(name))
}

impl Callable {
    /// A callable source never names: a scenario's action or an action's presentation view.
    pub fn hidden(&self) -> bool {
        self.scenario || self.name.ends_with(crate::ids::PRESENT_SUFFIX)
    }

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
    /// The ids of the package's root types: what a scenario may reach with `root alias.T`.
    pub roots: Vec<String>,
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
    /// `default view v for T`: the type and the view's name.
    pub default_views: Vec<(TypeKey, String)>,
    /// The view state every view declares.
    pub states: Vec<StateInfo>,
    /// `row view v for T`: the type and the view's name.
    pub row_views: Vec<(TypeKey, String)>,
    /// The functions of action parameters' defaults, numbered after the presentation views.
    pub defaults: Vec<DefaultFn>,
}

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
            roots: meta.roots.clone(),
        });
        let app = meta.app.clone();
        let field_info = |f: &crate::meta::FieldMeta| FieldInfo {
            name: f.name.clone(),
            id: f.id.clone(),
            ty: Ty::parse_meta(&f.ty, &app).unwrap_or(Ty::Error),
            merge: f.merge,
            doc: f.doc.clone(),
            default: f.default.clone(),
            role: f.role.clone(),
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
                        fields: v.fields.iter().map(field_info).collect(),
                    })
                    .collect(),
            });
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
                })
                .collect()
        };
        let result = |r: &Option<String>| {
            r.as_ref()
                .map_or(Ty::Unit, |t| Ty::parse_meta(t, &app).unwrap_or(Ty::Error))
        };
        let mut add = |kind, name: &str, id: &str, params, result, class, present| {
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
                present,
                scenario: false,
            });
        };
        for a in meta.offered_actions() {
            let present = Presentation {
                label: if a.label.is_empty() {
                    action_label(&a.name, &a.doc)
                } else {
                    a.label.clone()
                },
                icon: a.icon.clone(),
                role: a.role.clone(),
                view: a.present.clone(),
            };
            add(
                CKind::Action,
                &a.name,
                &a.id,
                params(&a.params),
                result(&a.result),
                Some(a.class),
                present,
            );
        }
        for f in &meta.flows {
            let present = Presentation {
                label: action_label(&f.name, &f.doc),
                ..Presentation::default()
            };
            add(
                CKind::Flow,
                &f.name,
                &f.id,
                params(&f.params),
                result(&f.result),
                Some(f.class),
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
    };
    r.env.add_import("host", &crate::host::host_meta());
    r.imports(module, imports);
    r.types(module, lock);
    r.callables(module, lock);
    r.package_decls(module);
    r.env
}

fn mentions_principal(ty: &Ty) -> bool {
    match ty {
        Ty::Principal => true,
        Ty::List(t) | Ty::Set(t) | Ty::Map(t) | Ty::Collection(t) | Ty::Option(t) => {
            mentions_principal(t)
        }
        Ty::Result(a, b) => mentions_principal(a) || mentions_principal(b),
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
        let mut enums: Vec<&ast::EnumDecl> = Vec::new();
        for item in &module.items {
            let Item::Enum(e) = item else { continue };
            if !name_case_ok(&e.name.name, true) {
                self.err(
                    "E016",
                    e.name.span,
                    format!(
                        "enum names start with an uppercase letter; rename `{}`",
                        e.name.name
                    ),
                );
            }
            if is_builtin_type(&e.name.name) || !self.own_enums.insert(e.name.name.clone()) {
                self.err(
                    "E011",
                    e.name.span,
                    format!("the type `{}` is declared twice", e.name.name),
                );
                continue;
            }
            enums.push(e);
        }
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
            if is_builtin_type(&t.name.name)
                || self.own_enums.contains(&t.name.name)
                || !self.own_types.insert(t.name.name.clone())
            {
                self.err(
                    "E011",
                    t.name.span,
                    format!("the type `{}` is declared twice", t.name.name),
                );
                continue;
            }
            decls.push(t);
        }
        for e in enums {
            self.enum_decl(e, lock);
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
                let mut info = self.field(&t.name.name, f, lock);
                info.role = self.marker(&t.name.name, f, &info, &fields);
                fields.push(info);
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
                    role: None,
                });
            }
            let doc = v.doc.join("\n");
            let icon = self.variant_icon(v);
            variants.push(VariantInfo {
                name: v.name.name.clone(),
                id: id_of(lock, &format!("{name}.{}", v.name.name)),
                label: variant_label(&v.name.name, &doc),
                icon,
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

    /// A variant's `(icon: .x)`: what a control shows beside its label.
    fn variant_icon(&mut self, v: &ast::VariantDecl) -> Option<String> {
        let mut icon = None;
        let mut seen = false;
        for opt in &v.opts {
            match (opt.name.name.as_str(), &opt.value.kind) {
                ("icon", _) if seen => {
                    self.err("E090", opt.name.span, "`icon:` is given twice");
                }
                ("icon", ExprKind::Variant { name, args: None }) => {
                    seen = true;
                    icon = self.icon(name);
                }
                ("icon", _) => self.err(
                    "E090",
                    opt.value.span,
                    "`icon:` takes an icon written with a dot, such as `icon: .pin`",
                ),
                (other, _) => self.err(
                    "E090",
                    opt.name.span,
                    format!("a variant takes the option `icon:`; `{other}:` is not one. Its label is its doc comment's first line"),
                ),
            }
        }
        icon
    }

    fn field(&mut self, owner: &str, f: &ast::FieldDecl, lock: &IdsLock) -> FieldInfo {
        let ty = self.resolve_type(&f.ty, true);
        if matches!(ty, Ty::Action | Ty::View) {
            self.err("E023", f.ty.span, format!("a field cannot hold `{ty}`: it is not stored state; fields hold scalars, Text, records, enums, List, Option, Set, Map and Collection"));
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
            role: None,
        }
    }

    /// The role a field's marker gives it: `(title)` or `(body)`, on a Text field, once per
    /// type.
    fn marker(
        &mut self,
        owner: &str,
        f: &ast::FieldDecl,
        info: &FieldInfo,
        before: &[FieldInfo],
    ) -> Option<String> {
        let m = f.marker.as_ref()?;
        if !matches!(m.name.as_str(), "title" | "body") {
            self.err("E092", m.span, format!("`({})` is not a field marker; a Text field can be marked `(title)`, what names the resource, or `(body)`, its main text", m.name));
            return None;
        }
        if info.ty != Ty::Text {
            let t = info.ty.display(&self.env.app);
            self.err("E092", m.span, format!("only a Text field can be the {}; `{}` is {t}. Mark a Text field instead, or remove `({})`", m.name, f.name.name, m.name));
            return None;
        }
        if let Some(other) = before.iter().find(|x| x.role.as_deref() == Some(&m.name)) {
            self.err("E092", m.span, format!("`{owner}` already has a {} field, `{}`; a type has at most one. Remove `({})` from one of them", m.name, other.name, m.name));
            return None;
        }
        Some(m.name.clone())
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
        if let ExprKind::Call { callee, args } = &e.kind
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
                "Date" => return Ty::Date,
                "Time" => return Ty::Time,
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
                _ => {}
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
            self.err("E013", span, format!("unknown type `{single}`; types here: Int, Float, Bool, Text, Principal, Date, Time, Action, View and {known}"));
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
        if self.env.enum_info(&key).is_some() {
            return Ty::Enum(key);
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
                default: None,
            });
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
            let (kind, name, params, result) = match item {
                Item::Action(a) => (CKind::Action, &a.name, &a.params, a.result.as_ref()),
                Item::Flow(f) => (CKind::Flow, &f.name, &f.params, f.result.as_ref()),
                Item::View(v) => (CKind::View, &v.name, &v.params, None),
                Item::Fn(f) => (CKind::Fn, &f.name, &f.params, Some(&f.result)),
                Item::Use(_)
                | Item::Type(_)
                | Item::Enum(_)
                | Item::Root(_)
                | Item::DefaultView(_)
                | Item::RowView(_)
                | Item::Scenario(_) => continue,
            };
            let index = func;
            func += 1;
            if !name_case_ok(&name.name, false) {
                self.err("E016", name.span, format!("`{}` names a {}, which starts with a lowercase letter (uppercase is for types)", name.name, kind.word()));
            }
            // A view's name is a UI statement, so it cannot be a primitive's; the others are
            // called in expressions, where only the built-in functions are taken.
            let taken = (kind == CKind::View && prims::prim(&name.name).is_some())
                || prims::BUILTINS.contains(&name.name.as_str());
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
                        "`{}` is declared twice; actions, flows, views and fns share one namespace",
                        name.name
                    ),
                );
            }
            let ast_params = params;
            let params = self.params(params);
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
                Item::Flow(f) => Presentation {
                    label: action_label(&f.name.name, &f.doc.join("\n")),
                    ..Presentation::default()
                },
                _ => Presentation::default(),
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
                }
                ("icon", _) => self.err(
                    "E090",
                    opt.value.span,
                    "`icon:` takes an icon written with a dot, such as `icon: .plus`, or an `if` over the subject: `icon: if n.pinned { .pin_off } else { .pin }`",
                ),
                ("label", ExprKind::Str(_)) if plain_text(&opt.value).is_some() => {
                    out.label = plain_text(&opt.value).unwrap_or_default();
                }
                ("label", ExprKind::If { .. }) => {
                    if let Some(label) = fallback_label(&opt.value, &at_rest) {
                        out.label = label;
                    }
                }
                ("label", _) => self.err(
                    "E090",
                    opt.value.span,
                    "`label:` takes text in quotes, without `{ }`, such as `label: \"New note\"`, or an `if` over the subject: `label: if n.pinned { \"Unpin\" } else { \"Pin\" }`",
                ),
                ("on", _) => {}
                (other, _) => self.err(
                    "E090",
                    opt.name.span,
                    format!("an action takes the options `icon:`, `label:` and `on:`; `{other}:` is not one. Mark the main action with `primary action` and a destructive one with `destructive action`"),
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

    /// The `window` and `lens` declarations at the top of a view body.
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
                layer: d.layer,
                ty,
            });
        }
    }

    /// `root T`, `default view v for T` and `row view v for T` (workspace-model.md sections 2
    /// and 7, native-apps.md section 3.2), and the app's icon.
    fn package_decls(&mut self, module: &ast::Module) {
        if let Some(icon) = &module.app.icon
            && prims::icon(icon).is_none()
        {
            self.err("E091", module.app.span, prims::unknown_icon(icon));
        }
        for item in &module.items {
            match item {
                Item::Root(r) => {
                    let Some(key) = self.own_record(&r.ty, "root") else {
                        continue;
                    };
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
                    let Some(key) = self.own_record(&d.ty, "default view") else {
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
                _ => {}
            }
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

    /// `row view v for T`: a view of one `T` whose body is one list row, `row(title, ...)`.
    fn row_view_decl(&mut self, d: &ast::DefaultViewDecl) {
        let Some(key) = self.own_record(&d.ty, "row view") else {
            return;
        };
        if self.env.row_views.iter().any(|(k, _)| *k == key) {
            self.err(
                "E093",
                d.span,
                format!("`{}` has two row views; a type has one", key.name),
            );
            return;
        }
        let fits = self
            .env
            .own_callable(&d.view.name)
            .filter(|c| c.kind == CKind::View)
            .map(|c| matches!(c.params.as_slice(), [p] if p.ty == Ty::Record(key.clone())));
        match fits {
            Some(true) => self.env.row_views.push((key, d.view.name.clone())),
            Some(false) => self.err(
                "E093",
                d.view.span,
                format!(
                    "the row view of `{}` takes the item and nothing else: `view {}(x: {})`",
                    key.name, d.view.name, key.name
                ),
            ),
            None => self.err(
                "E093",
                d.view.span,
                format!("`{}` is not a view of this package", d.view.name),
            ),
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
    };
    r.resolve_type(te, false)
}

/// `Date("2026-10-24")` or `Time("2026-10-21T09:30:00Z")`: the type and the ISO text, checked
/// at compile time. `kind` is `Date` or `Time`.
pub fn date_value(kind: &str, args: &[ast::Arg]) -> Result<(Ty, String), String> {
    let ty = if kind == "Date" { Ty::Date } else { Ty::Time };
    let example = if kind == "Date" {
        "Date(\"2026-10-24\")"
    } else {
        "Time(\"2026-10-21T09:30:00Z\")"
    };
    let text = match args {
        [ast::Arg { name: None, value }] => plain_text(value),
        _ => None,
    };
    let Some(text) = text else {
        return Err(format!(
            "`{kind}(...)` takes one string literal, such as `{example}`; a {kind} cannot be made from computed text"
        ));
    };
    if kind == "Date" {
        crate::types::check_date(&text)?;
    } else {
        crate::types::check_time(&text)?;
    }
    Ok((ty, text))
}

/// The text of a string literal without interpolation.
fn plain_text(e: &Expr) -> Option<String> {
    let ExprKind::Str(parts) = &e.kind else {
        return None;
    };
    parts
        .iter()
        .map(|p| match p {
            ast::StrPart::Lit(t) => Some(t.as_str()),
            ast::StrPart::Interp(_) => None,
        })
        .collect()
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
            ast::UiKind::For { body, .. } => collect(body, out),
            ast::UiKind::If { then, els, .. } => {
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
            ast::UiKind::Let { .. } | ast::UiKind::State(_) | ast::UiKind::Fact { .. } => {}
        }
    }
}
