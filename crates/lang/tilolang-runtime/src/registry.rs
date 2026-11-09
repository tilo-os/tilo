//! Loaded packages and the id lookups the host needs.

use std::collections::HashMap;
use std::rc::Rc;

use serde_json::Value as Json;

use crate::ids::{ActionId, ModuleHash, TypeId, ViewId};
use crate::meta::{
    ActionMeta, EnumMeta, FieldKind, FnMeta, Meta, StateMeta, Ty, TypeKey, TypeMeta, ViewMeta,
    field_kind, parse_ty,
};
use crate::store::{FieldSpec, NewRecord};
use crate::value::EnumValue;

#[derive(Clone, Debug)]
pub struct FieldInfo {
    pub id: String,
    pub name: String,
    pub ty: Ty,
    pub kind: FieldKind,
    pub default: Option<Json>,
}

#[derive(Clone, Debug)]
pub struct TypeInfo {
    pub id: TypeId,
    pub name: String,
    pub fields: Vec<FieldInfo>,
}

impl TypeInfo {
    /// The field a record of the type is named by: its first text field, as a row, a menu, a
    /// link and a history entry show it. A register of text is a code or a label, never what
    /// the record is called. A type with no such field names its records by the type's name
    /// and the record's `created`.
    #[must_use]
    pub fn name_field(&self) -> Option<&FieldInfo> {
        self.fields
            .iter()
            .find(|f| matches!(f.kind, FieldKind::Text | FieldKind::RichText))
    }

    pub fn field(&self, id: &str) -> Option<&FieldInfo> {
        self.fields.iter().find(|f| f.id == id)
    }

    /// The field by id, or by name (for callers outside guest code).
    pub fn field_by_id_or_name(&self, key: &str) -> Option<&FieldInfo> {
        self.field(key)
            .or_else(|| self.fields.iter().find(|f| f.name == key))
    }

    pub fn specs(&self) -> Vec<FieldSpec> {
        self.fields
            .iter()
            .map(|f| FieldSpec {
                id: f.id.clone(),
                kind: f.kind,
            })
            .collect()
    }

    pub fn field_names(&self) -> String {
        self.fields
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

pub struct Package {
    pub hash: ModuleHash,
    pub meta: Meta,
    pub types: Vec<Rc<TypeInfo>>,
    /// Each function the module exports, as a profile names it: from the metadata the package
    /// was loaded with, and the lines its module says its declarations are written on.
    units: std::collections::BTreeMap<String, Rc<crate::cost::Unit>>,
    /// A package of an app this runtime does not run: its records are that app's, read from the
    /// readings it publishes, and its actions are invoked at its own door (`crate::delegate`).
    pub foreign: bool,
    /// Whether its module is loaded: its views run here. Only a description of another app
    /// (`Runtime::load_foreign`) has none.
    pub code: bool,
}

impl Package {
    /// # Errors
    ///
    /// A message naming the field whose type or merge kind is not understood.
    pub fn new(hash: ModuleHash, meta: Meta) -> Result<Self, String> {
        Self::with_lines(hash, meta, &[])
    }

    /// [`Package::new`], with the lines the module says its declarations are written on
    /// (`tilolang.lines`): what a profile names a unit's place by.
    ///
    /// # Errors
    ///
    /// As [`Package::new`].
    pub fn with_lines(
        hash: ModuleHash,
        meta: Meta,
        lines: &[tilolang::meta::Line],
    ) -> Result<Self, String> {
        let types = meta
            .types
            .iter()
            .map(|t| type_info(t, &meta.app).map(Rc::new))
            .collect::<Result<Vec<_>, _>>()?;
        let units = crate::cost::units_of(&meta, lines);
        Ok(Self {
            hash,
            meta,
            types,
            units,
            foreign: false,
            code: true,
        })
    }

    /// The unit `export` names; one no declaration names (an agent block) has none.
    #[must_use]
    pub fn unit(&self, export: &str) -> Option<Rc<crate::cost::Unit>> {
        self.units.get(export).map(Rc::clone)
    }

    /// The description of an app this runtime does not run: its types, actions and views, and
    /// no code. Its resources are read from the readings its app publishes and its actions
    /// invoked at its own door (`crate::delegate`).
    ///
    /// # Errors
    ///
    /// As [`Package::new`].
    pub fn foreign(hash: ModuleHash, meta: Meta) -> Result<Self, String> {
        Ok(Self {
            foreign: true,
            code: false,
            ..Self::new(hash, meta)?
        })
    }

    /// A package of an app this runtime does not run, with its module: its views run here, as
    /// that package's own code, over the readings its app publishes; its records and actions
    /// stay that app's (`Runtime::load_foreign_package`).
    ///
    /// # Errors
    ///
    /// As [`Package::new`].
    pub fn foreign_with_code(hash: ModuleHash, meta: Meta) -> Result<Self, String> {
        Ok(Self {
            foreign: true,
            ..Self::new(hash, meta)?
        })
    }

    pub fn type_by_id(&self, id: &str) -> Option<&Rc<TypeInfo>> {
        self.types.iter().find(|t| t.id == id)
    }

    pub fn type_by_name(&self, name: &str) -> Option<&Rc<TypeInfo>> {
        self.types.iter().find(|t| t.name == name)
    }

    pub fn action(&self, id: &str) -> Option<&ActionMeta> {
        self.meta.actions.iter().find(|a| a.id == id)
    }

    pub fn view(&self, id: &str) -> Option<&ViewMeta> {
        self.meta.views.iter().find(|v| v.id == id)
    }

    pub fn fn_(&self, id: &str) -> Option<&FnMeta> {
        self.meta.fns.iter().find(|f| f.id == id)
    }
}

fn type_info(t: &TypeMeta, app: &str) -> Result<TypeInfo, String> {
    let fields = t
        .fields
        .iter()
        .map(|f| {
            let ty = parse_ty(&f.ty, app).map_err(|e| format!("{}.{}: {e}", t.name, f.name))?;
            let kind = field_kind(f, &ty);
            Ok(FieldInfo {
                id: f.id.clone(),
                name: f.name.clone(),
                ty,
                kind,
                default: f.default.clone(),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(TypeInfo {
        id: t.id.clone(),
        name: t.name.clone(),
        fields,
    })
}

#[derive(Default)]
pub struct Registry {
    packages: HashMap<ModuleHash, Rc<Package>>,
    order: Vec<ModuleHash>,
    types: HashMap<TypeId, ModuleHash>,
    actions: HashMap<ActionId, ModuleHash>,
    views: HashMap<ViewId, ModuleHash>,
    fns: HashMap<String, ModuleHash>,
}

impl Registry {
    /// Add a package. Declaration ids must be unused by other packages.
    ///
    /// # Errors
    ///
    /// A message naming an id another package already declares.
    pub fn add(&mut self, pkg: Package) -> Result<Rc<Package>, String> {
        if let Some(existing) = self.packages.get(&pkg.hash) {
            return Ok(Rc::clone(existing));
        }
        let hash = pkg.hash.clone();
        for t in &pkg.meta.types {
            Self::claim(&mut self.types, &t.id, &hash, "type")?;
        }
        for a in &pkg.meta.actions {
            Self::claim(&mut self.actions, &a.id, &hash, "action")?;
        }
        for v in &pkg.meta.views {
            Self::claim(&mut self.views, &v.id, &hash, "view")?;
        }
        for f in &pkg.meta.fns {
            Self::claim(&mut self.fns, &f.id, &hash, "fn")?;
        }
        let rc = Rc::new(pkg);
        self.packages.insert(hash.clone(), Rc::clone(&rc));
        self.order.push(hash);
        Ok(rc)
    }

    /// Add a new version of a package: every package of the same app is removed first, so the
    /// new version takes over their declaration ids. Resources keep their state: they are stored
    /// by those ids, not by module.
    ///
    /// # Errors
    ///
    /// As [`Registry::add`]; the old versions stay when the new one is refused.
    pub fn replace(&mut self, pkg: Package) -> Result<Rc<Package>, String> {
        let old: Vec<ModuleHash> = self
            .order
            .iter()
            .filter(|h| {
                **h != pkg.hash
                    && self
                        .packages
                        .get(*h)
                        .is_some_and(|p| p.meta.app == pkg.meta.app)
            })
            .cloned()
            .collect();
        let saved = (
            self.packages.clone(),
            self.order.clone(),
            self.types.clone(),
            self.actions.clone(),
            self.views.clone(),
            self.fns.clone(),
        );
        for hash in &old {
            self.packages.remove(hash);
            self.order.retain(|h| h != hash);
            for map in [
                &mut self.types,
                &mut self.actions,
                &mut self.views,
                &mut self.fns,
            ] {
                map.retain(|_, owner| owner != hash);
            }
        }
        self.add(pkg).inspect_err(|_| {
            (
                self.packages,
                self.order,
                self.types,
                self.actions,
                self.views,
                self.fns,
            ) = saved;
        })
    }

    /// Whether a package of the app `app` is loaded here: its types are declared, and a type of
    /// it no loaded package declares is a fault rather than one waiting on the app's package.
    #[must_use]
    pub fn holds_app(&self, app: &str) -> bool {
        self.packages
            .values()
            .any(|package| package.meta.app == app)
    }

    /// Remove every package of the app `app` this runtime does not run (its records are that
    /// app's): its types, actions and views are declared here no more. Whether one was.
    pub fn remove_foreign(&mut self, app: &str) -> bool {
        let gone: Vec<ModuleHash> = self
            .order
            .iter()
            .filter(|h| {
                self.packages
                    .get(*h)
                    .is_some_and(|p| p.foreign && p.meta.app == app)
            })
            .cloned()
            .collect();
        for hash in &gone {
            self.packages.remove(hash);
            self.order.retain(|h| h != hash);
            for map in [
                &mut self.types,
                &mut self.actions,
                &mut self.views,
                &mut self.fns,
            ] {
                map.retain(|_, owner| owner != hash);
            }
        }
        !gone.is_empty()
    }

    fn claim(
        map: &mut HashMap<String, ModuleHash>,
        id: &str,
        owner: &ModuleHash,
        what: &str,
    ) -> Result<(), String> {
        match map.get(id) {
            Some(other) if other != owner => Err(format!(
                "{what} id `{id}` is already declared by module {other}"
            )),
            _ => {
                map.insert(id.to_owned(), owner.clone());
                Ok(())
            }
        }
    }

    pub fn package(&self, hash: &ModuleHash) -> Option<Rc<Package>> {
        self.packages.get(hash).cloned()
    }

    pub fn packages(&self) -> Vec<Rc<Package>> {
        self.order
            .iter()
            .filter_map(|h| self.packages.get(h).cloned())
            .collect()
    }

    pub fn type_owner(&self, ty: &str) -> Option<&ModuleHash> {
        self.types.get(ty)
    }

    /// Every type every loaded package declares, as the runtime loads and reloads them.
    pub fn loaded_types(&self) -> Vec<Rc<TypeInfo>> {
        self.packages
            .values()
            .flat_map(|p| p.types.iter().cloned())
            .collect()
    }

    pub fn type_info(&self, ty: &str) -> Option<(Rc<Package>, Rc<TypeInfo>)> {
        let pkg = self.package(self.types.get(ty)?)?;
        let info = Rc::clone(pkg.type_by_id(ty)?);
        Some((pkg, info))
    }

    pub fn action_package(&self, id: &str) -> Option<Rc<Package>> {
        self.package(self.actions.get(id)?)
    }

    pub fn view_package(&self, id: &str) -> Option<Rc<Package>> {
        self.package(self.views.get(id)?)
    }

    pub fn fn_package(&self, id: &str) -> Option<Rc<Package>> {
        self.package(self.fns.get(id)?)
    }

    /// The id of the record type `key` names: declared by the most recently loaded package of
    /// that app.
    pub fn type_for(&self, key: &TypeKey) -> Option<TypeId> {
        self.order
            .iter()
            .rev()
            .filter_map(|h| self.packages.get(h))
            .filter(|p| p.meta.app == key.app)
            .find_map(|p| p.type_by_name(&key.name).map(|t| t.id.clone()))
    }

    /// Resolve a type name given by a caller outside any package: an id, or a name that
    /// exactly one loaded package declares.
    pub fn resolve_global(&self, name: &str) -> Result<TypeId, String> {
        if self.types.contains_key(name) {
            return Ok(name.to_owned());
        }
        if let Some((app, ty)) = name.rsplit_once('.')
            && let Some(id) = self.type_for(&TypeKey {
                app: app.to_owned(),
                name: ty.to_owned(),
            })
        {
            return Ok(id);
        }
        let hits: Vec<TypeId> = self
            .packages
            .values()
            .filter_map(|p| p.type_by_name(name).map(|t| t.id.clone()))
            .collect();
        match hits.as_slice() {
            [one] => Ok(one.clone()),
            [] => Err(format!("no loaded package declares a type `{name}`")),
            _ => Err(format!("type name `{name}` is ambiguous; use its id")),
        }
    }

    /// The `state` declaration with this id.
    pub fn state_meta(&self, id: &str) -> Option<StateMeta> {
        self.packages().iter().find_map(|p| {
            p.meta
                .views
                .iter()
                .flat_map(|v| v.state.iter())
                .find(|s| s.id == id)
                .cloned()
        })
    }

    /// The enum with this type id.
    pub fn enum_meta(&self, id: &str) -> Option<EnumMeta> {
        self.packages()
            .iter()
            .find_map(|p| p.meta.enums.iter().find(|e| e.id == id).cloned())
    }

    /// The enum `key` names, as the package of its app declares it.
    pub fn enum_of(&self, key: &TypeKey) -> Option<EnumMeta> {
        self.order
            .iter()
            .rev()
            .filter_map(|hash| self.packages.get(hash))
            .filter(|pkg| pkg.meta.app == key.app)
            .find_map(|pkg| {
                pkg.meta
                    .enums
                    .iter()
                    .find(|declared| declared.name == key.name)
                    .cloned()
            })
    }

    /// The value of `case`, named by its name or its id, as a value of `key`'s enum holds it:
    /// the enum's id, the case's id, and no fields.
    pub fn enum_value(&self, key: &TypeKey, case: &str) -> Result<EnumValue, String> {
        let declared = self.enum_of(key).ok_or_else(|| {
            format!(
                "`{}.{}` is not declared by any loaded package",
                key.app, key.name
            )
        })?;
        let variant = declared
            .variants
            .iter()
            .find(|variant| {
                variant.fields.is_empty() && (variant.name == case || variant.id == case)
            })
            .map(|variant| variant.id.clone())
            .ok_or_else(|| format!("`{case}` is not a case of `{}`", key.name))?;
        Ok(EnumValue {
            ty: declared.id,
            variant,
            fields: std::collections::BTreeMap::new(),
        })
    }

    /// The id of the view a window of type `ty` (an id) shows unless another is asked for: the
    /// default view the type's package declares.
    pub fn default_view(&self, ty: &str) -> Option<ViewId> {
        let ty = self.type_id(ty)?;
        let pkg = self.package(self.types.get(&ty)?)?;
        pkg.meta.default_view(&ty).map(str::to_owned)
    }

    /// The id of the row view a list of `ty` (an id) draws each item through: the one the
    /// type's package declares as its row.
    pub fn row_view(&self, ty: &str) -> Option<ViewId> {
        let ty = self.type_id(ty)?;
        let pkg = self.package(self.types.get(&ty)?)?;
        pkg.meta.row_view(&ty).map(str::to_owned)
    }

    /// The id of the type `ty` names: a type id, or another app's type as its resources name
    /// it (`app#Type`, [`crate::foreign`]).
    pub fn type_id(&self, ty: &str) -> Option<TypeId> {
        match crate::foreign::foreign_type(ty) {
            Some(key) => self.type_for(&key),
            None => self.types.contains_key(ty).then(|| ty.to_owned()),
        }
    }

    /// The views a window of `ty` switches between, in the order the package declares them
    /// (`views a, b for T`), each with the name it is declared by: empty when the type declares
    /// none, whose views are then the app's own helpers.
    pub fn views_of_type(&self, ty: &str) -> Vec<(ViewId, String)> {
        let Some(ty) = self.type_id(ty) else {
            return Vec::new();
        };
        let Some(pkg) = self.types.get(&ty).and_then(|hash| self.package(hash)) else {
            return Vec::new();
        };
        pkg.meta
            .views_of(&ty)
            .into_iter()
            .filter_map(|id| {
                let view = pkg.meta.views.iter().find(|view| view.id == id)?;
                Some((view.id.clone(), view.name.clone()))
            })
            .collect()
    }

    /// How a window's switch offers the view `view` (its id) of the type `ty`: the short label
    /// and icon its declaration names for it, `None` for each it does not say.
    pub fn view_shown(&self, ty: &str, view: &str) -> (Option<String>, Option<String>) {
        let Some(ty) = self.type_id(ty) else {
            return (None, None);
        };
        let Some(pkg) = self.types.get(&ty).and_then(|hash| self.package(hash)) else {
            return (None, None);
        };
        let (label, icon) = pkg.meta.view_shown(&ty, view);
        (label.map(str::to_owned), icon.map(str::to_owned))
    }

    /// The views of `ty` a person or an agent can apply or name (`shown_as`), whether or not a
    /// window switches between them: a declared view of one resource and nothing else, but not a
    /// row. A view no declaration names is the app's own helper, which the app places itself and
    /// no window offers.
    pub fn offered_views_of_type(&self, ty: &str) -> Vec<(ViewId, String)> {
        let Some(ty) = self.type_id(ty) else {
            return Vec::new();
        };
        let Some(pkg) = self.types.get(&ty).and_then(|hash| self.package(hash)) else {
            return Vec::new();
        };
        let Some(name) = pkg.type_by_id(&ty).map(|info| info.name.clone()) else {
            return Vec::new();
        };
        pkg.meta
            .views_showing(&name)
            .into_iter()
            // A row draws one item of a list, not a resource whole, so a window cannot show
            // the resource through it.
            .filter(|v| !pkg.meta.row_views.iter().any(|row| row.view == v.id))
            .map(|v| (v.id.clone(), v.name.clone()))
            .collect()
    }

    /// The offered view `view` (its id or name) of the package declaring `ty` that takes a
    /// resource of `ty` and nothing else: what a place names as `shown_as`.
    pub fn view_of_type(&self, ty: &str, view: &str) -> Option<ViewId> {
        self.offered_views_of_type(ty)
            .into_iter()
            .find(|(id, name)| id == view || name == view)
            .map(|(id, _)| id)
    }

    /// The offered view `named` (its id or name, optionally written `package:view`) that draws
    /// a resource of `ty`, when `ty` holds the session's arrangement (`origin:
    /// session.arrangement`) and the view is declared by a package other than the one holding
    /// the type: a *desk style* (`shell.md` §6). A style is a view of the arrangement, and the
    /// arrangement's type belongs to `so.tilo.shell`, so a style in any installed package — the
    /// shipped four, a person's own, an agent's — is found here by the type its view takes,
    /// never by a list in code. `None` for a type that is not the arrangement's, or a name that
    /// names no style: `show` then reports what it always did.
    pub fn style_view(&self, ty: &str, named: &str) -> Option<ViewId> {
        if !self.arrangement_type(ty) {
            return None;
        }
        // The type as the view's parameter names it, when its type is one this runtime holds:
        // a style of a package other than the arrangement's still writes `shell.Arrangement`,
        // the type the shell's package declares.
        let (own_app, name) = if let Some(key) = crate::foreign::foreign_type(ty) {
            (key.app, key.name)
        } else {
            let (pkg, info) = self.type_info(ty)?;
            (pkg.meta.app.clone(), info.name.clone())
        };
        let (app, view) = named
            .split_once(':')
            .map_or((None, named), |(a, v)| (Some(a), v));
        // A named package is searched first, then the rest: a style of the shell's own package
        // (`shell.md` §6: the shipped four) is a view `views_of_type` found, and one of another
        // package is found here.
        let searched = |pkg: &Rc<Package>| -> Option<ViewId> {
            let shows_it = |param: &crate::meta::ParamMeta| {
                matches!(
                    crate::meta::parse_ty(&param.ty, &pkg.meta.app),
                    Ok(Ty::Record(ref key)) if key.app == own_app && key.name == name
                )
            };
            pkg.meta
                .offered_views()
                .filter(|v| matches!(&v.params[..], [only] if shows_it(only)))
                .filter(|v| !pkg.meta.row_views.iter().any(|row| row.view == v.id))
                .find(|v| v.id == view || v.name == view)
                .map(|v| v.id.clone())
        };
        if let Some(app) = app
            && let Some(pkg) = self
                .order
                .iter()
                .rev()
                .filter_map(|hash| self.packages.get(hash))
                .find(|pkg| pkg.meta.app == app)
            && let Some(found) = searched(pkg)
        {
            return Some(found);
        }
        let mine = self.types.get(ty).cloned();
        self.order
            .iter()
            .rev()
            .filter_map(|hash| self.packages.get(hash))
            .filter(|pkg| Some(&pkg.hash) != mine.as_ref())
            .find_map(searched)
    }

    /// Whether `ty` (a type id or an `app#Type`) holds the session's arrangement, the type a
    /// desk style is a view of (`shell.md` §6).
    fn arrangement_type(&self, ty: &str) -> bool {
        let id = match crate::foreign::foreign_type(ty) {
            Some(_) => return true,
            None => self.type_id(ty),
        };
        let Some((pkg, info)) = id.and_then(|id| self.type_info(&id)) else {
            return false;
        };
        pkg.meta
            .types
            .iter()
            .find(|t| t.id == info.id)
            .and_then(|t| t.origin.as_deref())
            == Some("session.arrangement")
    }

    /// What a new resource of this type starts out with.
    pub fn new_record(&self, ty: &str) -> Option<NewRecord> {
        let (pkg, info) = self.type_info(ty)?;
        Some(NewRecord {
            ty: info.id.clone(),
            app: pkg.meta.app.clone(),
            fields: info.specs(),
        })
    }
}
