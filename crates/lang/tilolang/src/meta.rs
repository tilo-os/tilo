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
    /// SHA-256 (hex) of the wasm module without the metadata section.
    pub module_hash: String,
    pub types: Vec<TypeMeta>,
    pub actions: Vec<ActionMeta>,
    pub views: Vec<ViewMeta>,
    pub flows: Vec<FlowMeta>,
    pub fns: Vec<FnMeta>,
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
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParamMeta {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
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
