//! The named-style table both office codecs resolve body content against:
//! character styles as inline marks, paragraph styles as a parent chain.

use std::collections::{BTreeMap, HashMap};
use tilo_doc::document::MarkValue;
use tilo_doc::schema::{
    STYLE_ALIGN, STYLE_BOLD, STYLE_COLOR, STYLE_FONT, STYLE_INDENT, STYLE_ITALIC, STYLE_NAME,
    STYLE_SIZE, STYLE_SPACE_AFTER, STYLE_SPACE_BEFORE,
};
use tilo_doc::snapshot::AttrVal;

const MAX_CHAIN: usize = 8;

#[derive(Default, Clone)]
pub(crate) struct ParaStyle {
    pub parent: Option<String>,
    pub align: Option<String>,
    pub outline_level: Option<i64>,
    pub heading_name: bool,
    pub name_level: Option<i64>,
    pub text: Vec<(String, MarkValue)>,
    pub display_name: Option<String>,
    /// A user-facing *named* style, as opposed to a resolution-only or
    /// automatic one; only named styles become container styles and explicit
    /// block `style` attrs.
    pub named: bool,
    pub size: Option<f64>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub color: Option<String>,
    pub font: Option<String>,
    pub space_before: Option<f64>,
    pub space_after: Option<f64>,
    pub indent: Option<f64>,
    /// Line spacing as a unitless multiple (1.5).
    pub line_height: Option<f64>,
}

#[derive(Default)]
pub(crate) struct StyleTable {
    pub text: HashMap<String, Vec<(String, MarkValue)>>,
    pub para: HashMap<String, ParaStyle>,
}

impl StyleTable {
    fn walk(&self, name: &str) -> (Vec<&ParaStyle>, Option<String>) {
        let mut chain = Vec::new();
        let mut cur = Some(name.to_string());
        for _ in 0..MAX_CHAIN {
            let Some(n) = cur.take() else { break };
            let Some(p) = self.para.get(&n) else {
                return (chain, Some(n));
            };
            chain.push(p);
            cur.clone_from(&p.parent);
        }
        (chain, None)
    }

    fn inherited<T>(&self, name: &str, field: impl Fn(&ParaStyle) -> Option<T>) -> Option<T> {
        self.walk(name).0.into_iter().find_map(field)
    }

    pub(crate) fn heading_level(
        &self,
        name: &str,
        unregistered: impl FnOnce(&str) -> Option<i64>,
    ) -> Option<i64> {
        let (chain, missing) = self.walk(name);
        let level = chain.into_iter().find_map(|p| {
            p.outline_level
                .or_else(|| p.heading_name.then(|| p.name_level.unwrap_or(1)))
        });
        level
            .or_else(|| missing.and_then(|n| unregistered(&n)))
            .map(|l| l.clamp(1, 6))
    }

    pub(crate) fn para_align(&self, name: &str) -> Option<String> {
        self.inherited(name, |p| p.align.clone())
    }

    pub(crate) fn para_line_height(&self, name: &str) -> Option<f64> {
        self.inherited(name, |p| p.line_height)
    }

    pub(crate) fn para_text_marks(&self, name: &str) -> Vec<(String, MarkValue)> {
        let mut out: Vec<(String, MarkValue)> = Vec::new();
        for p in self.walk(name).0.iter().rev() {
            for (k, v) in &p.text {
                out.retain(|(k2, _)| k2 != k);
                out.push((k.clone(), v.clone()));
            }
        }
        out
    }

    pub(crate) fn text_marks(&self, name: &str) -> Vec<(String, MarkValue)> {
        self.text.get(name).cloned().unwrap_or_default()
    }

    pub(crate) fn is_named_para(&self, name: &str) -> bool {
        self.para.get(name).is_some_and(|p| p.named)
    }

    /// Resolve a named paragraph style into the scalar map the CRDT styles
    /// container stores: self overrides parent per field, and unset fields fall
    /// back to neutral defaults so the round-trip stays idempotent. A style
    /// without a display name is called `unnamed(name)`.
    pub(crate) fn style_props(
        &self,
        name: &str,
        unnamed: impl FnOnce(&str) -> String,
    ) -> Option<BTreeMap<String, AttrVal>> {
        let chain = self.walk(name).0;
        if chain.is_empty() {
            return None;
        }
        let first = |field: fn(&ParaStyle) -> Option<f64>, default| {
            chain.iter().find_map(|p| field(p)).unwrap_or(default)
        };
        let first_str = |field: fn(&ParaStyle) -> &Option<String>, default: &str| {
            chain
                .iter()
                .find_map(|p| field(p).clone())
                .unwrap_or_else(|| default.to_string())
        };
        let flag = |field: fn(&ParaStyle) -> Option<bool>| {
            AttrVal::Bool(chain.iter().find_map(|p| field(p)).unwrap_or(false))
        };
        let display = chain
            .iter()
            .find_map(|p| p.display_name.clone())
            .unwrap_or_else(|| unnamed(name));
        Some(BTreeMap::from([
            (STYLE_NAME.to_string(), AttrVal::Str(display)),
            (
                STYLE_FONT.to_string(),
                AttrVal::Str(first_str(|p| &p.font, "sans")),
            ),
            (
                STYLE_SIZE.to_string(),
                AttrVal::Float(first(|p| p.size, 16.0)),
            ),
            (STYLE_BOLD.to_string(), flag(|p| p.bold)),
            (STYLE_ITALIC.to_string(), flag(|p| p.italic)),
            (
                STYLE_COLOR.to_string(),
                AttrVal::Str(first_str(|p| &p.color, "#000000")),
            ),
            (
                STYLE_ALIGN.to_string(),
                AttrVal::Str(first_str(|p| &p.align, "left")),
            ),
            (
                STYLE_SPACE_BEFORE.to_string(),
                AttrVal::Float(first(|p| p.space_before, 0.0)),
            ),
            (
                STYLE_SPACE_AFTER.to_string(),
                AttrVal::Float(first(|p| p.space_after, 0.0)),
            ),
            (
                STYLE_INDENT.to_string(),
                AttrVal::Float(first(|p| p.indent, 0.0)),
            ),
        ]))
    }
}
