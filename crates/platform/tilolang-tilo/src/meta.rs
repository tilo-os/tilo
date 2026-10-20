//! The package metadata (`tilolang.meta`, implementation design 5) as this crate reads it.
//!
//! The compiler emits the JSON; the runtime parses it into its own types. The platform needs
//! only the declarations (what the types are, which actions exist, their parameters and effect
//! classes), so this module reads just that and ignores the rest.

use serde::{Deserialize, Serialize};

use crate::schema::{FieldSchema, MergeKind, TypeSchema};

/// Effect class of an action (spec 3.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EffectClass {
    Read,
    Undoable,
    Outside,
}

impl From<EffectClass> for tilo_actions::ActionKind {
    fn from(class: EffectClass) -> Self {
        match class {
            EffectClass::Read => Self::Read,
            EffectClass::Undoable => Self::Undoable,
            EffectClass::Outside => Self::Outside,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PackageMeta {
    pub app: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub module_hash: String,
    #[serde(default)]
    pub types: Vec<TypeMeta>,
    #[serde(default)]
    pub actions: Vec<ActionMeta>,
    #[serde(default)]
    pub views: Vec<ViewMeta>,
    #[serde(default)]
    pub flows: Vec<FlowMeta>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TypeMeta {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub doc: Option<String>,
    #[serde(default)]
    pub fields: Vec<FieldMeta>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FieldMeta {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
    #[serde(default)]
    pub merge: Option<String>,
    #[serde(default)]
    pub doc: Option<String>,
    #[serde(default)]
    pub default: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ParamMeta {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActionMeta {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub doc: Option<String>,
    #[serde(default)]
    pub params: Vec<ParamMeta>,
    #[serde(default)]
    pub result: Option<String>,
    pub class: EffectClass,
    #[serde(default)]
    pub proposal: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ViewMeta {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub doc: Option<String>,
    #[serde(default)]
    pub params: Vec<ParamMeta>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FlowMeta {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub doc: Option<String>,
    #[serde(default)]
    pub params: Vec<ParamMeta>,
}

impl PackageMeta {
    /// Parse the metadata JSON.
    ///
    /// # Errors
    /// The text is not package metadata.
    pub fn from_json(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }

    /// The declared type named `name`, or whose id is `name`.
    #[must_use]
    pub fn type_named(&self, name: &str) -> Option<&TypeMeta> {
        self.types
            .iter()
            .find(|ty| ty.name == name || ty.id == name)
    }

    /// The record schemas of every declared type, ready to register.
    #[must_use]
    pub fn schemas(&self) -> Vec<TypeSchema> {
        self.types
            .iter()
            .map(|ty| {
                let mut schema = TypeSchema::new(&ty.id, &self.app, &ty.name);
                for field in &ty.fields {
                    schema.fields.push(self.field_schema(field));
                }
                schema
            })
            .collect()
    }

    fn field_schema(&self, field: &FieldMeta) -> FieldSchema {
        let ty = field.ty.trim();
        let mut kind = MergeKind::of(field.merge.as_deref(), ty);
        let mut record = None;
        if kind == MergeKind::Collection {
            record = inner(ty, "Collection").and_then(|item| self.type_named(item));
        } else if kind == MergeKind::Register
            && let Some(declared) = self.type_named(ty)
        {
            kind = MergeKind::Record;
            record = Some(declared);
        }
        let mut schema = FieldSchema::new(&field.id, kind);
        if let Some(declared) = record {
            schema = schema.of_record(&declared.id);
        }
        if let Some(default) = &field.default {
            schema = schema.with_default(default.clone());
        }
        schema
    }

    /// Register the schemas of every declared type; peers that replay this package's actions
    /// resolve its records through them.
    pub fn register_schemas(&self) {
        for schema in self.schemas() {
            crate::schema::register(schema);
        }
    }

    /// The resource kind of the type `name`: `tl.<app>.<Type>`.
    #[must_use]
    pub fn kind_of(&self, type_name: &str) -> String {
        format!("tl.{}.{}", self.app, type_name)
    }

    /// The kind a package-level action (one with no record parameter) is published under.
    #[must_use]
    pub fn package_kind(&self) -> String {
        format!("tl.{}", self.app)
    }

    /// The type an action is on: that of its first parameter, when it is a declared record.
    #[must_use]
    pub fn subject_of(&self, params: &[ParamMeta]) -> Option<&TypeMeta> {
        params.first().and_then(|param| self.type_named(&param.ty))
    }
}

/// `T` of `Wrapper<T>`.
fn inner<'a>(ty: &'a str, wrapper: &str) -> Option<&'a str> {
    ty.strip_prefix(wrapper)?
        .strip_prefix('<')?
        .strip_suffix('>')
        .map(str::trim)
}
