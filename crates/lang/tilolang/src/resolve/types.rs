//! The module's own types: what each declares, and what its fields mean to the platform.

use super::{FieldInfo, HOLDER_FIELD, Resolver, TypeInfo, id_of, is_builtin_type, name_case_ok};
use crate::ast::{self, Item, TypeExpr, TypeExprKind};
use crate::ids::IdsLock;
use crate::types::{MergeKind, TypeKey};

impl Resolver<'_> {
    pub(super) fn types(&mut self, module: &ast::Module, lock: &IdsLock) {
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
        let mut holders: Vec<&ast::AliasDecl> = Vec::new();
        for item in &module.items {
            let Item::Alias(a) = item else { continue };
            let name = &a.name.name;
            if !name_case_ok(name, true) {
                self.err(
                    "E016",
                    a.name.span,
                    format!("type names start with an uppercase letter; rename `{name}`"),
                );
            }
            if is_builtin_type(name)
                || self.own_enums.contains(name)
                || self.own_types.contains(name)
                || self.own_aliases.contains_key(name)
            {
                self.err(
                    "E011",
                    a.name.span,
                    format!("the type `{name}` is declared twice"),
                );
                continue;
            }
            let container = matches!(&a.ty.kind, TypeExprKind::Generic(g, _) if matches!(g.name.as_str(), "Collection" | "Set" | "Map"));
            if container {
                self.own_types.insert(name.clone());
                holders.push(a);
            } else {
                self.own_aliases.insert(name.clone(), a.ty.clone());
            }
        }
        for e in enums {
            self.enum_decl(e, lock);
        }
        for a in holders {
            let name = &a.name.name;
            let ty = self.resolve_type(&a.ty, true);
            self.env.types.push(TypeInfo {
                key: TypeKey {
                    app: self.env.app.clone(),
                    name: name.clone(),
                },
                id: id_of(lock, name),
                doc: a.doc.join("\n"),
                fields: vec![FieldInfo {
                    name: HOLDER_FIELD.to_owned(),
                    id: id_of(lock, &format!("{name}.{HOLDER_FIELD}")),
                    ty,
                    merge: MergeKind::Structural,
                    personal: false,
                    doc: String::new(),
                    default: None,
                    role: None,
                    derived: None,
                }],
                search: crate::meta::SearchMeta::default(),
                holder: true,
                origin: None,
            });
        }
        let aliases: Vec<(String, TypeExpr)> = self
            .own_aliases
            .iter()
            .map(|(n, t)| (n.clone(), t.clone()))
            .collect();
        for (name, te) in aliases {
            let ty = self.alias_type(&name, &te);
            let key = TypeKey {
                app: self.env.app.clone(),
                name,
            };
            self.env.aliases.push((key, ty));
        }
        for t in &decls {
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
                info.role = None;
                fields.push(info);
            }
            self.roles(&t.name.name, t, &mut fields);
            self.derived_keys(&t.name.name, t, &mut fields, lock);
            self.env.types.push(TypeInfo {
                key: TypeKey {
                    app: self.env.app.clone(),
                    name: t.name.name.clone(),
                },
                id: id_of(lock, &t.name.name),
                doc: t.doc.join("\n"),
                fields,
                search: crate::meta::SearchMeta::default(),
                holder: false,
                origin: t.origin.as_ref().map(|o| o.name.clone()),
            });
        }
        self.check_origins(&decls);
        // Every type's fields are known: each one's search reads its own, and the enums.
        for t in decls {
            let Some(at) = self
                .env
                .types
                .iter()
                .position(|x| x.key.app == self.env.app && x.key.name == t.name.name)
            else {
                continue;
            };
            let search = crate::search::resolve(
                &self.env.types[at],
                t.search.as_ref(),
                &self.env,
                self.diags,
            );
            self.env.types[at].search = search;
        }
    }
}
