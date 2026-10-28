//! Loaded packages and the id lookups the host needs.

use std::collections::HashMap;
use std::rc::Rc;

use serde_json::Value as Json;

use crate::ids::{ActionId, FlowId, ModuleHash, TypeId, ViewId};
use crate::meta::{
    ActionMeta, EnumMeta, FieldKind, FlowMeta, Meta, StateMeta, Ty, TypeKey, TypeMeta, ViewMeta,
    field_kind, parse_ty,
};
use crate::store::{FieldSpec, NewRecord};

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
}

impl Package {
    /// # Errors
    ///
    /// A message naming the field whose type or merge kind is not understood.
    pub fn new(hash: ModuleHash, meta: Meta) -> Result<Self, String> {
        let types = meta
            .types
            .iter()
            .map(|t| type_info(t, &meta.app).map(Rc::new))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { hash, meta, types })
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

    pub fn flow(&self, id: &str) -> Option<&FlowMeta> {
        self.meta.flows.iter().find(|f| f.id == id)
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
    flows: HashMap<FlowId, ModuleHash>,
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
        for f in &pkg.meta.flows {
            Self::claim(&mut self.flows, &f.id, &hash, "flow")?;
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
            self.flows.clone(),
        );
        for hash in &old {
            self.packages.remove(hash);
            self.order.retain(|h| h != hash);
            for map in [
                &mut self.types,
                &mut self.actions,
                &mut self.views,
                &mut self.flows,
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
                self.flows,
            ) = saved;
        })
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

    pub fn flow_package(&self, id: &str) -> Option<Rc<Package>> {
        self.package(self.flows.get(id)?)
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
    ///
    /// # Errors
    ///
    /// A message when no loaded package declares the type, or several do.
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

    /// The `window` or `lens` declaration with this id.
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

    /// The id of the view a window of type `ty` (an id) shows unless another is asked for: the
    /// default view the type's package declares.
    pub fn default_view(&self, ty: &str) -> Option<ViewId> {
        let pkg = self.package(self.types.get(ty)?)?;
        pkg.meta.default_view(ty).map(str::to_owned)
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
