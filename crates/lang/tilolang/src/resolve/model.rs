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
    /// Its filters, default order and hidden fields (search.md, "What a type declares").
    pub search: crate::meta::SearchMeta,
    /// `(origin: imap.message)`: the kind of an outside service's resource it holds
    /// (`crate::origin`).
    pub origin: Option<String>,
    /// `(title: subject)`: the field a record of the type is named by, when the type says which.
    pub title: Option<String>,
    /// `(untitled: "Untitled")`: what one of its records is called while its title is empty.
    pub untitled: Option<String>,
}

impl TypeInfo {
    pub fn field(&self, name: &str) -> Option<&FieldInfo> {
        self.fields.iter().find(|f| f.name == name)
    }

    /// The field a record of the type is named by: the one it says (`title: subject`), else its
    /// first `Text` or `RichText` field that merges as text. A register of text is a code or a
    /// label, never what the record is called. A type with no such field names its records by the
    /// type's name and the record's `created`.
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

/// One view of a `views` declaration: its name, and the short label and icon it is offered by
/// in a window's switch (`views project_list, project_board (icon: .columns) for Project`).
#[derive(Clone, Debug)]
pub struct ViewsInfo {
    pub name: String,
    /// `(label: "Board")`: what the switch names it, when that is not the view's label with the
    /// type's name left off.
    pub label: Option<String>,
    /// `(icon: .columns)`: the platform's icon for it.
    pub icon: Option<String>,
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
    /// `(tone: .x)`: the tone the case reads in (`ui.Tone`'s cases: `neutral`, `success`,
    /// `warning`, `danger`, `info`), a theme role rather than a colour.
    pub tone: Option<String>,
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

/// A `state` declaration of a view.
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
    /// An action parameter's invocation-time default (`done: Bool = not item.done`).
    pub default: Option<ParamDefault>,
    /// Whether the view offering the action supplies it (`above: Option<Task> (above)`): the
    /// place the item stands in, as a row of a list gives the row above it.
    pub place: bool,
    /// Whether the text of an entry fills it: an action's one `Text` parameter is what the
    /// words left after the other parameters take theirs title the item with
    /// (`crate::entry`).
    pub entry: bool,
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

impl ParamDefault {
    /// Whether the default is `.none`, a constant: see [`crate::meta::DefaultMeta::is_none`].
    #[must_use]
    pub fn is_none(&self) -> bool {
        self.text == crate::meta::DefaultMeta::NONE
    }
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
    /// Index into the module's functions when declared here.
    pub func: Option<usize>,
    pub params: Vec<ParamInfo>,
    pub result: Ty,
    /// The class recorded in foreign metadata.
    pub class: Option<EffectClass>,
    /// `uses web, time`: the effects its body has, as the declaration says them. An action
    /// reads nothing else that a replay would compute differently ([`crate::effects::Effect`]).
    pub uses: BTreeSet<crate::effects::Effect>,
    /// How the platform presents an action: its label, icon and role.
    pub present: Presentation,
    /// The hidden action of a `scenario`; its name is the scenario's, which source cannot call.
    pub scenario: bool,
}

/// What a button or a menu shows for an action, from its declaration.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Presentation {
    pub label: String,
    /// A kebab-case name of the platform's icon set.
    pub icon: Option<String>,
    /// `primary` or `destructive`.
    pub role: Option<String>,
    /// The id of the hidden view that presents the action's state for its subject, when its
    /// `label:` or `icon:` is an `if`, or it declares `on:`.
    pub view: Option<String>,
    /// `label: "Move to {col.title}"` or `label: if pinned { "Pin" } else { "Unpin" }`: the
    /// label as written, read where the action is applied with its arguments
    /// (`button(move_to(b, card, col))`, `button(pin(n))`), so it follows them as they change.
    pub label_text: Option<Expr>,
    /// `icon: if n.pinned { .pin_off } else { .pin }`: read where the action is applied, as
    /// `label_text` is.
    pub icon_when: Option<Expr>,
    /// `on: n.pinned`: whether the state it toggles is on, read where the action is applied: a
    /// button for it shows as toggled.
    pub on: Option<Expr>,
    /// `key: "Ctrl+Enter"`: the shortcut that runs the action while its window has the keyboard,
    /// and (on an item's action) while that item is selected or its view shows. One spelling
    /// (`Ctrl+Shift+enter`: modifiers in one order, then the key's token), read through the
    /// person's keymap and the platform's own command modifier, never as a key code.
    pub key: Option<String>,
    /// `group: "Reply"`: the run of the resource's actions this one stands in, as the app names
    /// it. A surface that shows the resource's actions — a window bar, the desktop's action
    /// bar — draws the runs in the order the app declared them, so an app decides which
    /// commands read as one without any surface keeping a list of its own.
    pub group: Option<String>,
    /// `did: "moved this"`: what the action did to its subject, as history's sentence reads it
    /// after who did it ("Ana moved this"); empty for one that says nothing, which history then
    /// names by the action and its description.
    pub did: Option<String>,
}

/// The shortcut an action's `key:` declares, as it is published and compared: the modifiers in
/// one order and the key's own token (`Ctrl+Shift+enter`), so `Ctrl+Enter` and `control+Return`
/// are one shortcut. The error says what is wrong with it, for the person who wrote it.
pub(super) fn declared_key(e: &Expr) -> Result<String, String> {
    let text = plain_text(e).unwrap_or_default();
    tilo_ui_graph::keys::Chord::parse(&text)
        .map(|chord| chord.text())
        .map_err(|said| format!("`key:` takes a shortcut: {said}"))
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
pub(super) fn doc_line(doc: &str) -> Option<String> {
    let line = doc.lines().next()?.trim();
    let line = line.strip_suffix('.').unwrap_or(line).trim_end();
    (!line.is_empty()).then(|| line.to_string())
}

/// An action's label: its name in sentence case, or its doc's first line when that is
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
