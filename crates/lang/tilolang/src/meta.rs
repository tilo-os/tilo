//! Package metadata (`tilolang.meta`, implementation.md section 5). Tools, the runtime, the
//! platform and agents read it; compiled packages are imported through it.

use crate::types::{EffectClass, MergeKind};
use serde::{Deserialize, Serialize};

/// Name of the wasm custom section that carries the metadata as JSON.
pub const META_SECTION: &str = "tilolang.meta";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PackageMeta {
    pub app: String,
    pub title: String,
    /// The platform icon the app is shown with (`app ... (icon: .note)`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// The launcher category (`app ... (category: .office)`), lower case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// SHA-256 (hex) of the wasm module without the metadata section.
    pub module_hash: String,
    pub types: Vec<TypeMeta>,
    /// Declared enums. Types that name one are written `enum Name` in metadata.
    #[serde(default)]
    pub enums: Vec<EnumMeta>,
    pub actions: Vec<ActionMeta>,
    pub views: Vec<ViewMeta>,
    pub flows: Vec<FlowMeta>,
    pub fns: Vec<FnMeta>,
    /// Ids of the root types (`root T`): one resource each per workspace, which the launcher
    /// lists and opens.
    #[serde(default)]
    pub roots: Vec<String>,
    /// The view a window of each type uses unless changed (`default view v for T`).
    #[serde(default)]
    pub default_views: Vec<DefaultViewMeta>,
    /// The row a list of each type shows per item (`row view v for T`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub row_views: Vec<DefaultViewMeta>,
    /// Named seed data (`scenario "name" { ... }`), each run by its hidden action.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scenarios: Vec<ScenarioMeta>,
}

/// A `scenario`: its name and the hidden action (listed in `actions` with `scenario: true`)
/// that fills a workspace with its data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScenarioMeta {
    pub id: String,
    pub name: String,
    /// The id of the scenario's action.
    pub action: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefaultViewMeta {
    /// The type id.
    #[serde(rename = "type")]
    pub ty: String,
    /// The view id.
    pub view: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EnumMeta {
    pub id: String,
    pub name: String,
    pub doc: String,
    pub variants: Vec<VariantMeta>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VariantMeta {
    pub id: String,
    pub name: String,
    pub doc: String,
    /// What a control shows for the variant: its doc's first line, or its name in sentence
    /// case.
    #[serde(default)]
    pub label: String,
    /// `(icon: .x)`: what a control shows beside the label, a kebab-case name of the
    /// platform's icon set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// The payload fields, in order. Their merge is always `register`.
    pub fields: Vec<FieldMeta>,
}

/// A `window` or `lens` declaration of a view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateMeta {
    pub id: String,
    pub name: String,
    /// `window` (shared by the window) or `lens` (one per person per window).
    pub layer: String,
    #[serde(rename = "type")]
    pub ty: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TypeMeta {
    pub id: String,
    pub name: String,
    pub doc: String,
    pub fields: Vec<FieldMeta>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FieldMeta {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
    pub merge: MergeKind,
    pub doc: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    /// `title` or `body`: what the field is to the platform (`title: Text (title)`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParamMeta {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
    /// An action parameter's invocation-time default: an invocation that leaves the argument
    /// out gets it, computed then over the arguments before it and recorded as the argument.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<DefaultMeta>,
}

/// How an action parameter's default is computed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefaultMeta {
    /// The module export computing it, which takes the parameters before this one.
    pub export: String,
    /// The default as written (`not item.done`), for descriptions.
    pub text: String,
}

/// An action or flow that code can reach, with its effect class. `own` is relative to the
/// package whose metadata lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reach {
    pub app: String,
    pub action: String,
    pub name: String,
    pub class: EffectClass,
    pub own: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposalMeta {
    /// Id of the proposal view.
    pub view: String,
    pub name: String,
    pub args: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActionMeta {
    pub id: String,
    pub name: String,
    pub doc: String,
    pub params: Vec<ParamMeta>,
    pub result: Option<String>,
    pub class: EffectClass,
    pub proposal: Option<ProposalMeta>,
    pub reaches: Vec<Reach>,
    /// What the platform calls the action: `label:`, else its doc's first line when that is
    /// shorter than its name in sentence case, else that.
    #[serde(default)]
    pub label: String,
    /// `icon:`, a kebab-case name of the platform's icon set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// `primary` or `destructive`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// The hidden action of a scenario: never offered, run only to seed a workspace.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub scenario: bool,
    /// The id of the view that presents the action's state for a subject, when its `label:` or
    /// `icon:` follows the subject or it declares `on:`. The view takes the action's first
    /// parameter and builds one node of kind `present` with the props given dynamically:
    /// `label` (Text), `icon` (Text, a kebab-case icon name) and `on` (Bool). `label` and `icon`
    /// above are what shows without evaluating it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub present: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnchorMeta {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ViewMeta {
    pub id: String,
    pub name: String,
    pub doc: String,
    pub params: Vec<ParamMeta>,
    pub anchors: Vec<AnchorMeta>,
    pub reaches: Vec<Reach>,
    #[serde(default)]
    pub state: Vec<StateMeta>,
    /// The id of the action this view presents (see [`ActionMeta::present`]): a hidden view,
    /// never offered or listed as a view of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presents: Option<String>,
    /// The id of the declared view this view is a loop body of: a row the host renders per
    /// item, never offered or listed as a view of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row_of: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FlowMeta {
    pub id: String,
    pub name: String,
    pub doc: String,
    pub params: Vec<ParamMeta>,
    pub result: Option<String>,
    pub class: EffectClass,
    pub reaches: Vec<Reach>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FnMeta {
    pub id: String,
    pub name: String,
    pub doc: String,
    pub params: Vec<ParamMeta>,
    pub result: String,
}

impl PackageMeta {
    /// The id of the default view of the type `ty` (an id), when the package declares one.
    pub fn default_view(&self, ty: &str) -> Option<&str> {
        self.default_views
            .iter()
            .find(|d| d.ty == ty)
            .map(|d| d.view.as_str())
    }

    /// The id of the row view of the type `ty` (an id), when the package declares one.
    pub fn row_view(&self, ty: &str) -> Option<&str> {
        self.row_views
            .iter()
            .find(|d| d.ty == ty)
            .map(|d| d.view.as_str())
    }

    /// The actions the platform offers: every action but the hidden ones of scenarios.
    pub fn offered_actions(&self) -> impl Iterator<Item = &ActionMeta> {
        self.actions.iter().filter(|a| !a.scenario)
    }

    /// The views a person or an agent can open or apply: every view but the hidden ones that
    /// present an action's state or are the rows of a loop.
    pub fn offered_views(&self) -> impl Iterator<Item = &ViewMeta> {
        self.views
            .iter()
            .filter(|v| v.presents.is_none() && v.row_of.is_none())
    }

    /// The JSON bytes embedded in the `tilolang.meta` section.
    pub fn to_json_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// Reads the metadata back out of a compiled module.
    pub fn from_wasm(wasm: &[u8]) -> Option<Self> {
        let data = custom_section(wasm, META_SECTION)?;
        serde_json::from_slice(data).ok()
    }
}

fn read_leb(bytes: &[u8], pos: &mut usize) -> Option<usize> {
    let mut result = 0usize;
    let mut shift = 0u32;
    loop {
        let b = *bytes.get(*pos)?;
        *pos += 1;
        result |= usize::from(b & 0x7f).checked_shl(shift)?;
        if b & 0x80 == 0 {
            return Some(result);
        }
        shift += 7;
        if shift > 35 {
            return None;
        }
    }
}

/// The payload of the named custom section of a wasm module.
pub fn custom_section<'a>(wasm: &'a [u8], name: &str) -> Option<&'a [u8]> {
    let mut pos = 8usize;
    while pos < wasm.len() {
        let id = *wasm.get(pos)?;
        pos += 1;
        let size = read_leb(wasm, &mut pos)?;
        let end = pos.checked_add(size)?;
        if id == 0 {
            let mut p = pos;
            let name_len = read_leb(wasm, &mut p)?;
            let name_end = p.checked_add(name_len)?;
            if wasm.get(p..name_end)? == name.as_bytes() {
                return wasm.get(name_end..end);
            }
        }
        pos = end;
    }
    None
}
