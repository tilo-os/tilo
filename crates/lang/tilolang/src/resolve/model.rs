//! What a module's declarations name: its types, their fields and enums, its state, and the
//! callables (actions, functions, views, sorts) with the parameters and presentation they
//! carry.

use crate::ast::Expr;
use crate::resolve::plain_text;
use crate::types::{EffectClass, MergeKind, Ty, TypeKey};
use std::collections::BTreeSet;

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
    pub search: crate::meta::SearchMeta,
    /// `(origin: imap.message)`: the kind of an outside service's resource it holds
    /// (`crate::origin`).
    pub origin: Option<String>,
    pub title: Option<String>,
    pub untitled: Option<String>,
}

impl TypeInfo {
    pub fn field(&self, name: &str) -> Option<&FieldInfo> {
        self.fields.iter().find(|f| f.name == name)
    }

    /// The field a record of the type is named by: the one it says (`title: subject`), else its first
    /// `Text` or `RichText` field that merges as text. A register of text is a code or a label, never
    /// what the record is called. A type with no such field names its records by the type's name and
    /// the record's `created`.
    #[must_use]
    pub fn name_field(&self) -> Option<&FieldInfo> {
        match &self.title {
            Some(name) => self.field(name),
            None => self.fields.iter().find(|f| {
                matches!(f.ty, Ty::Text | Ty::RichText) && f.merge != MergeKind::Register
            }),
        }
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
pub struct ViewsInfo {
    pub name: String,
    /// `(label: "Board")`: what the switch names it, when that is not the view's label with the type's
    /// name left off.
    pub label: Option<String>,
    pub icon: Option<String>,
}

#[derive(Clone, Debug)]
pub struct VariantInfo {
    pub name: String,
    pub id: String,
    pub doc: String,
    pub label: String,
    pub icon: Option<String>,
    /// `(tone: .x)`: a theme role, one of `ui.Tone`'s cases, never a colour.
    pub tone: Option<String>,
    pub fields: Vec<FieldInfo>,
}

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

#[derive(Clone, Debug)]
pub struct StateInfo {
    pub view: String,
    pub name: String,
    pub id: String,
    pub ty: Ty,
}

#[derive(Clone, Debug)]
pub struct ParamInfo {
    pub name: String,
    pub ty: Ty,
    pub default: Option<ParamDefault>,
    /// Whether the view offering the action supplies it (`above: Option<Task> (above)`): the place the
    /// item stands in.
    pub place: bool,
    /// What the parameter is called where a person gives it (`saved: Bool (label: "On the Watch later
    /// list")`).
    pub label: Option<String>,
    /// Whether the text of an entry fills it: an action's one `Text` parameter is what the words left
    /// after the other parameters title the item with (`crate::entry`).
    pub entry: bool,
}

/// How an action parameter's default is computed: when the action is invoked without the argument,
/// over the arguments before it, and recorded as the argument.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParamDefault {
    /// The module export computing it: it takes the parameters before this one.
    pub export: String,
    pub func: Option<usize>,
    pub text: String,
}

impl ParamDefault {
    /// Whether the default is `.none`, a constant: see [`crate::meta::DefaultMeta::is_none`].
    #[must_use]
    pub fn is_none(&self) -> bool {
        self.text == crate::meta::DefaultMeta::NONE
    }
}

#[derive(Clone, Debug)]
pub struct DefaultFn {
    pub action: String,
    pub param: usize,
    pub func: usize,
}

/// The export name of the default of the parameter `param` of the action `action`.
pub fn default_export(action: &str, param: &str) -> String {
    format!("d_{action}_{param}")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CKind {
    Fn,
    Action,
    View,
}

impl CKind {
    pub const fn word(self) -> &'static str {
        match self {
            Self::Fn => "fn",
            Self::Action => "action",
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
    pub func: Option<usize>,
    pub params: Vec<ParamInfo>,
    pub result: Ty,
    pub class: Option<EffectClass>,
    /// `uses web, time`: the effects its body has, as the declaration says them
    /// ([`crate::effects::Effect`]).
    pub uses: BTreeSet<crate::effects::Effect>,
    pub present: Presentation,
    /// The hidden action of a `scenario`; source cannot call it.
    pub scenario: bool,
    /// A view the package does not offer: an action's presentation view, a loop's row view. Only a
    /// `template view` of the same package may call it.
    pub private: bool,
}

/// What a button or a menu shows for an action, from its declaration.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Presentation {
    pub label: String,
    /// A kebab-case name of the platform's icon set.
    pub icon: Option<String>,
    pub role: Option<String>,
    /// The id of the hidden view that presents the action's state for its subject.
    pub view: Option<String>,
    /// `label: "Move to {col.title}"` or `label: if pinned { "Pin" } else { "Unpin" }`: the label as
    /// written, read where the action is applied with its arguments, so it follows them as they change.
    pub label_text: Option<Expr>,
    pub icon_when: Option<Expr>,
    /// `on: n.pinned`: whether the state it toggles is on, read where the action is applied.
    pub on: Option<Expr>,
    /// `key: "Ctrl+Enter"`: the shortcut that runs the action while its window has the keyboard, and
    /// (on an item's action) while that item is selected. One spelling (`Ctrl+Shift+enter`), read
    /// through the person's keymap, never as a key code.
    pub key: Option<String>,
    /// `group: "Reply"`: the run of the resource's actions this one stands in, as the app names it. A
    /// surface draws the runs in the order the app declared them.
    pub group: Option<String>,
    /// `did: "moved this"`: what the action did, as history's sentence reads it after who did it ("Ana
    /// moved this").
    pub did: Option<String>,
}

/// The shortcut an action's `key:` declares, as it is published and compared: the modifiers in one
/// order and the key's own token (`Ctrl+Shift+enter`), so `Ctrl+Enter` and `control+Return` are one
/// shortcut.
pub(super) fn declared_key(e: &Expr) -> Result<String, String> {
    let text = plain_text(e).unwrap_or_default();
    tilo_ui_graph::keys::Chord::parse(&text)
        .map(|chord| chord.text())
        .map_err(|said| format!("`key:` takes a shortcut: {said}"))
}

pub fn sentence_case(name: &str) -> String {
    let spaced = name.replace('_', " ");
    let mut chars = spaced.trim().chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The first line of a doc comment without its closing period.
pub(super) fn doc_line(doc: &str) -> Option<String> {
    let line = doc.lines().next()?.trim();
    let line = line.strip_suffix('.').unwrap_or(line).trim_end();
    (!line.is_empty()).then(|| line.to_string())
}

/// An action's label: its name in sentence case, or its doc's first line when that is shorter.
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

    /// Whether a caller may call this callable by the name it is written: a private view only from its
    /// own package's `template view` (the caller's `from_template_of` names the package).
    pub fn callable_by(&self, from_template_of: Option<&str>) -> bool {
        !self.private || from_template_of == Some(self.app.as_str())
    }

    pub fn signature(&self, own_app: &str) -> String {
        let params: Vec<String> = self
            .params
            .iter()
            .map(|p| {
                // A default says the argument may be left out, and what it then is.
                let default = p
                    .default
                    .as_ref()
                    .map(|d| format!(" = {}", d.text))
                    .unwrap_or_default();
                format!("{}: {}{default}", p.name, p.ty.display(own_app))
            })
            .collect();
        let result = if matches!(self.result, Ty::Unit) {
            String::new()
        } else {
            format!(" -> {}", self.result.display(own_app))
        };
        format!("{}({}){result}", self.name, params.join(", "))
    }
}
