//! Package metadata, the JSON in the module's `tilolang.meta` section: the package's types,
//! enums, actions with their effect class and what they reach, views and fns, by id and name, with
//! their doc comments. Tools, the runtime, the platform and agents read it.

use crate::types::{EffectClass, MergeKind};
use serde::{Deserialize, Serialize};

pub const META_SECTION: &str = "tilolang.meta";

/// The custom section a package's catalogues travel in: JSON `[[locale, Fluent text], …]`. Outside
/// the module hash.
pub const LOCALES_SECTION: &str = "tilolang.locales";

/// The custom section naming where each function's declaration is written: JSON
/// `[{export, file, line}, …]`. Outside the module hash.
pub const LINES_SECTION: &str = "tilolang.lines";

/// Where a function is declared: 1-based, and no entry for a block or a function written without a
/// declaration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Line {
    pub export: String,
    /// The file's path in the package; `/` when it was compiled from one source string.
    pub file: String,
    pub line: u32,
}

pub fn append_lines(wasm: &mut Vec<u8>, lines: &[Line]) {
    if lines.is_empty() {
        return;
    }
    append_json_section(wasm, LINES_SECTION, lines);
}

#[must_use]
pub fn lines_from_wasm(wasm: &[u8]) -> Vec<Line> {
    custom_section(wasm, LINES_SECTION)
        .and_then(|data| serde_json::from_slice(data).ok())
        .unwrap_or_default()
}

/// Appends `value` as JSON in the custom section `name`; a custom section may follow every other,
/// so the module stays valid.
fn append_json_section<T: Serialize + ?Sized>(wasm: &mut Vec<u8>, name: &str, value: &T) {
    let data = serde_json::to_vec(value).unwrap_or_default();
    let mut payload = Vec::new();
    write_leb(&mut payload, name.len());
    payload.extend_from_slice(name.as_bytes());
    payload.extend_from_slice(&data);
    wasm.push(0);
    write_leb(wasm, payload.len());
    wasm.extend_from_slice(&payload);
}

pub fn append_locales(wasm: &mut Vec<u8>, locales: &[(String, String)]) {
    if locales.is_empty() {
        return;
    }
    append_json_section(wasm, LOCALES_SECTION, locales);
}

#[must_use]
pub fn locales_from_wasm(wasm: &[u8]) -> Vec<(String, String)> {
    custom_section(wasm, LOCALES_SECTION)
        .and_then(|data| serde_json::from_slice(data).ok())
        .unwrap_or_default()
}

fn write_leb(out: &mut Vec<u8>, mut value: usize) {
    loop {
        let byte = u8::try_from(value & 0x7f).unwrap_or(0);
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// The record kind of each person's own state of an app: the view state they set and what they have
/// seen, on every device of theirs and readable by every member.
pub const PERSONAL_RECORD_KIND: &str = "tilolang.personal";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PackageMeta {
    pub app: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// The ids the app was published under before (`previously: so.tilo.old`): the platform reads
    /// its records, grants and windows under them as the app's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub previously: Vec<String>,
    /// Each member keeps the app's records in their own record (`records: .own`), which only that
    /// person's devices read.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub own_records: bool,
    /// The hosts the app's `uses web` requests may reach (`web: ["example.com"]`). A request to any
    /// other host is refused as a value.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub web: Vec<String>,
    /// SHA-256 (hex) of the wasm module without the metadata section.
    pub module_hash: String,
    pub types: Vec<TypeMeta>,
    #[serde(default)]
    pub enums: Vec<EnumMeta>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<WordMeta>,
    pub actions: Vec<ActionMeta>,
    pub views: Vec<ViewMeta>,
    pub fns: Vec<FnMeta>,
    /// Ids of the root types (`root T`): one resource each per workspace, listed and opened by the
    /// launcher.
    #[serde(default)]
    pub roots: Vec<String>,
    /// The packages it uses (`use app.id`), by their current app ids, and `tilo.std`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uses: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub named_roots: Vec<NamedRootMeta>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<AliasMeta>,
    #[serde(default)]
    pub default_views: Vec<DefaultViewMeta>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub row_views: Vec<DefaultViewMeta>,
    /// The views a window of each type switches between (`views a, b for T`), in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub views_of: Vec<ViewsMeta>,
    /// Named seed data (`scenario "name" { ... }`), each run by its hidden action.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scenarios: Vec<ScenarioMeta>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub triggers: Vec<TriggerMeta>,
    /// The arguments views apply that compute from view state (`set(open, not open)`): functions the
    /// runtime runs when the control's event fires. Each is exported as `e_<id>`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub at_event: Vec<FnMeta>,
    /// The package's `template view`s, as source: a template is expanded where a caller writes the
    /// call, in this package's namespace, so an importer needs its text. It travels in the metadata
    /// JSON; the module hash leaves this section out.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub templates: Vec<TemplateMeta>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateMeta {
    /// The name a caller writes after the package's alias (`shell.desk_style`).
    pub name: String,
    /// The file it is declared in, for a diagnostic that names it.
    pub file: String,
    /// The declaration, from `template view` to its closing brace.
    pub source: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewsMeta {
    #[serde(rename = "type")]
    pub ty: String,
    /// The views' ids, in the order the declaration names them.
    pub views: Vec<String>,
    /// The views whose declaration says how the switch offers them, by id; the rest are offered by
    /// name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shown: Vec<ViewShownMeta>,
}

/// How a view is offered in a window's switch (`views project_list (label: "List", icon:
/// .list), project_board for Project`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewShownMeta {
    pub view: String,
    /// The short label the switch shows, in the words the app wrote it in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The icon beside it, as a kebab-case name of the platform's set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedRootMeta {
    pub name: String,
    pub ty: String,
    /// The resource the session publishes it as, for a root of an app this runtime does not run
    /// (`sound`): what `device.sound` names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AliasMeta {
    pub name: String,
    pub ty: String,
}

/// A `scenario`: its name and the hidden action that fills a workspace with its data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScenarioMeta {
    pub id: String,
    pub name: String,
    pub action: String,
}

/// A `trigger`: when a package's action runs by itself, for the resources of one type.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerMeta {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub doc: String,
    pub action: String,
    #[serde(rename = "type")]
    pub ty: String,
    /// The action's arguments, in the order the call passes them.
    pub args: Vec<TriggerArgMeta>,
    pub when: TriggerWhenMeta,
}

/// What a trigger passes its action: the record it runs for, or a root of the package.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerArgMeta {
    Subject,
    /// A root record, by its type's id (the platform keeps it as `root-<type id>`).
    Root(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerWhenMeta {
    /// At each multiple of these seconds, counted from the epoch.
    Every(i64),
    /// At the instant the resource's field (by id) holds.
    At(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefaultViewMeta {
    #[serde(rename = "type")]
    pub ty: String,
    pub view: String,
}

/// Words a view shows as written, or an action tells as written: a message of the package's
/// catalogue, with an attribute of the declaration's message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WordMeta {
    /// The id of the declaration whose message the words belong to.
    pub view: String,
    /// The attribute naming it: `w-` and a slug of its words.
    pub key: String,
    /// The words with each interpolation as its variable: `{n} new`.
    pub text: String,
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
    /// What a control shows for the variant: its doc's first line, or its name in sentence case.
    #[serde(default)]
    pub label: String,
    /// `(icon: .x)`: a kebab-case name of the platform's icon set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// `(tone: .x)`: a theme role, one of `ui.Tone`'s cases.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tone: Option<String>,
    /// The payload fields, in order. Their merge is always `register`.
    pub fields: Vec<FieldMeta>,
}

/// A view's `state` declaration: what its person picks there, kept as theirs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateMeta {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TypeMeta {
    pub id: String,
    pub name: String,
    pub doc: String,
    pub fields: Vec<FieldMeta>,
    /// What search, a list's filters and agents offer of the type; the defaults when it declares no
    /// `search` block.
    #[serde(default, skip_serializing_if = "SearchMeta::is_empty")]
    pub search: SearchMeta,
    /// `(origin: imap.message)`: the outside service kind of the resource, which the system keeps in
    /// step with the service (`crate::origin`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// `(title: subject)`: the field a record is named by. A type that says none is named by its
    /// first text field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// `(untitled: "Untitled")`: what a record is called while its title is empty, as the app wrote
    /// it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub untitled: Option<String>,
}

/// What search, a list's filters and agents offer of a type: its filters in order (the first are the
/// visible chips), the default order and the fields kept out of filters.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SearchMeta {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filters: Vec<FilterMeta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<SortMeta>,
    /// The fields kept out of filters, by id.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden: Vec<String>,
    /// Whether the type declares these with a `search` block rather than taking the defaults.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub declared: bool,
}

impl SearchMeta {
    pub fn is_empty(&self) -> bool {
        self.filters.is_empty() && self.sort.is_none() && self.hidden.is_empty() && !self.declared
    }

    pub fn filter(&self, key: &str) -> Option<&FilterMeta> {
        self.filters.iter().find(|f| f.key == key)
    }
}

/// One filter of a type.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FilterMeta {
    /// What a query names it by: the field's name, or a named filter's label in snake case.
    pub key: String,
    /// What a control shows: the label as written, or the field's name in sentence case.
    pub label: String,
    /// For agents: the field's doc comment's first line.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub doc: String,
    pub kind: FilterKind,
    /// The field it is on, by id; none for a named filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// The field's type as metadata writes it (`Option<Date>`, `Set<Text>`, `enum Status`).
    #[serde(rename = "type", default, skip_serializing_if = "String::is_empty")]
    pub ty: String,
    /// A named filter's predicate over the resource's fields, by field id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pred: Option<crate::pred::Pred<String>>,
    /// Whether it may hold no value: `none` and `any` apply.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub optional: bool,
    /// Whether it holds a set: a value asks whether the set holds that one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub many: bool,
    /// The icon its declaration names (`filter done (icon: .circle_check)`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

/// What a filter compares, and so the control it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterKind {
    /// A toggle: `is:unread`.
    Bool,
    /// Chips of the cases: `status:open`.
    Enum,
    /// Presets and a range: `due:today`, `due:-7d`.
    Date,
    Time,
    /// A range: `size:>10`.
    Number,
    /// A picker of the resources of a type: `label:<ref>`.
    Reference,
    /// A picker of the identifiers that occur, and patterns: `from:*@x.com`.
    Identifier,
    /// Tags: `tag:urgent`.
    Text,
    /// A named filter, a toggle.
    Predicate,
    /// A person or agent of the workspace: `assignee:me`, `assignee:<id>`. A field of
    /// `Principal` is one without declaring it.
    Person,
}

impl FilterKind {
    pub const fn is_toggle(self) -> bool {
        matches!(self, Self::Bool | Self::Predicate)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SortMeta {
    pub key: String,
    pub field: String,
    pub order: SortOrder,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortOrder {
    /// The latest (or largest) first.
    Newest,
    Oldest,
    #[serde(rename = "a_z")]
    AZ,
    #[serde(rename = "z_a")]
    ZA,
}

impl SortOrder {
    pub const ALL: [Self; 4] = [Self::Newest, Self::Oldest, Self::AZ, Self::ZA];

    /// The word a `sort` line and a query write it as.
    pub const fn word(self) -> &'static str {
        match self {
            Self::Newest => "newest",
            Self::Oldest => "oldest",
            Self::AZ => "a_z",
            Self::ZA => "z_a",
        }
    }

    pub fn from_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|o| o.word() == word)
    }

    pub const fn descending(self) -> bool {
        matches!(self, Self::Newest | Self::ZA)
    }
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

impl FieldMeta {
    /// Whether the field holds text as written: a `Text` or a `RichText`, which the store reads as
    /// its text.
    #[must_use]
    pub fn holds_text(&self) -> bool {
        matches!(self.ty.trim(), "Text" | "RichText" | "std.RichText")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParamMeta {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
    /// An invocation-time default: an invocation that leaves the argument out gets it, computed then
    /// over the arguments before it and recorded as the argument.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<DefaultMeta>,
    /// Whether the view offering the action supplies the place (`above: Option<Task> (above)`). A
    /// surface with no such view supplies `.none`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub place: bool,
    /// Whether the text of an entry fills it: what the words before the entry type say into this
    /// parameter, while the rest titles the item (`crate::entry`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub entry: bool,
    /// What the parameter is called where a person gives it (`saved: Bool (label: "On the Watch
    /// later list")`): the catalogue's message `d-<action>-<name>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl ParamMeta {
    #[must_use]
    pub fn words(&self) -> String {
        self.label
            .clone()
            .unwrap_or_else(|| crate::messages::label(&self.name))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefaultMeta {
    /// The module export computing it, which takes the parameters before this one.
    pub export: String,
    pub text: String,
}

impl DefaultMeta {
    pub const NONE: &str = ".none";

    /// The default `.none` of an optional parameter of an app that publishes its actions without a
    /// module: no code computes it, so it has no export.
    #[must_use]
    pub fn none() -> Self {
        Self {
            export: String::new(),
            text: Self::NONE.to_owned(),
        }
    }

    /// Whether the default is the constant `.none`: a call that leaves the parameter out passes
    /// `.none`, wherever the parameter stands.
    #[must_use]
    pub fn is_none(&self) -> bool {
        self.text == Self::NONE
    }
}

/// An action that code can reach, with its effect class. `own` is relative to the package whose
/// metadata lists it.
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
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is one part of what a surface offers and is read on its own: whether the action is a scenario's, and the two conditions its subject and its place decide"
)]
pub struct ActionMeta {
    pub id: String,
    pub name: String,
    pub doc: String,
    pub params: Vec<ParamMeta>,
    pub result: Option<String>,
    pub class: EffectClass,
    pub proposal: Option<ProposalMeta>,
    pub reaches: Vec<Reach>,
    /// What the platform calls the action: `label:`, else its doc's first line when shorter than its
    /// name in sentence case, else that.
    #[serde(default)]
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// `primary` or `destructive`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// The hidden action of a scenario: never offered, run only to seed a workspace.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub scenario: bool,
    /// `did: "moved this"`: what the action did, as a history entry reads it after who did it; an
    /// attribute of the action's message (`d-<id>.did`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub did: Option<String>,
    /// The id of the view that presents the action's state for a subject, when its label or icon
    /// follows the subject or it declares `on:`. `label` and `icon` above are what shows without
    /// evaluating it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub present: Option<String>,
    /// The id of the subject's `Bool` field whose state the action toggles (`on: n.pinned`). A
    /// control bound to the field stands for the action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toggles: Option<String>,
    /// `key: "Ctrl+Enter"`: the shortcut that runs the action while its window has the keyboard, and
    /// while its item is selected. One spelling: the modifiers in one order, then the key's token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// `group: "Reply"`: the run of the resource's actions this one stands in. A surface draws the
    /// runs in the order the app declared them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// `uses web, random, time`: the effects the body reads, which its invocation records and a
    /// replay answers from ([`crate::effects::Effect`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uses: Vec<String>,
    /// Whether the action keeps a requirement its subject decides, exported as
    /// [`crate::ir::guard_export`]: the first reason it cannot run, or empty while it can.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub guard: bool,
    /// Whether the action keeps an `applies` condition, exported as [`crate::ir::applies_export`]:
    /// a menu leaves such an action out, a palette lists it disabled with the reason.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub applies: bool,
    /// Whether the body invokes another action, itself included: a run that does is run again from
    /// its start whenever a call waits for a person, so it journals its reads and writes.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub invokes: bool,
    /// The outside capabilities the body's `outside(...)` calls name, as written (`youtube.search`):
    /// what the platform tags the action with.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outside: Vec<String>,
}

impl ActionMeta {
    /// Whether it is a `migration action`: never offered, run once by the platform on each resource
    /// an earlier version wrote.
    #[must_use]
    pub fn is_migration(&self) -> bool {
        self.role.as_deref() == Some("migration")
    }

    /// Whether it takes a `File`, the person's own: then no command, only a control's
    /// ([`Self::unbound`]'s exception).
    #[must_use]
    pub fn takes_a_file(&self) -> bool {
        self.params.iter().any(|param| param.ty.trim() == "File")
    }

    /// The parameters an invocation that has given `bound` arguments still lacks: those after them
    /// with no default, which its form asks the person for. Empty when it runs at once.
    ///
    /// [`missing_arguments`] and the "…" in [`crate::host::Action`]'s label both read it.
    pub fn unbound(&self, bound: usize) -> impl Iterator<Item = (usize, &ParamMeta)> {
        self.params
            .iter()
            .enumerate()
            .skip(bound)
            .filter(|(_, param)| param.default.is_none())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnchorMeta {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ViewMeta {
    pub id: String,
    /// The view's name as declared: what its window is called and what a screen reader says. A loop
    /// body takes its view's name, never a binding's.
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
    /// What the view does when the window is asked to show an item of a type it lists (`on
    /// show(x: T): ...`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show: Option<ShowMeta>,
}

/// What a view answers a request to show an item of a type with: the action it applies in its
/// own state, computed from the item — a list opens the item's row where it stands, a place
/// follows the item. The view's declaration, so no shape of the window is guessed from the data.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShowMeta {
    /// The id of the declared type of item the view answers for.
    pub ty: String,
    /// The function that makes the action, run with the item: `func.id` is its export.
    pub func: FnMeta,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FnMeta {
    pub id: String,
    pub name: String,
    pub doc: String,
    pub params: Vec<ParamMeta>,
    pub result: String,
}

impl TypeMeta {
    /// What one is called in a sentence: its name in words, lower case, in the singular (`TodoList`
    /// is a `todo list`).
    #[must_use]
    pub fn noun(&self) -> String {
        singular(&in_words(&self.name))
    }

    /// What several are called in a sentence, which a name that already names several leaves as it
    /// is.
    #[must_use]
    pub fn plural(&self) -> String {
        plural(&in_words(&self.name))
    }
}

/// A declared name in words, lower case (`TodoList` and `todo_list` are `todo list`), and what one of
/// them and several are. The rules are [`tilo_words`]': a package's catalogue and the action model's
/// nouns agree whichever language an app is written in.
pub use tilo_words::{in_words, plural, singular};

impl PackageMeta {
    /// The declared type named `name` (a qualified name's last segment), or whose id is `name`.
    #[must_use]
    pub fn type_named(&self, name: &str) -> Option<&TypeMeta> {
        if let Some(found) = self.types.iter().find(|ty| ty.id == name) {
            return Some(found);
        }
        // A qualified name (`so.tilo.todo.Item`) names a type of another app.
        let (app, last) = name
            .rsplit_once('.')
            .map_or((None, name), |(app, last)| (Some(app), last));
        if app.is_some_and(|app| app != self.app) {
            return None;
        }
        self.types.iter().find(|ty| ty.name == last)
    }

    #[must_use]
    pub fn subject_of(&self, params: &[ParamMeta]) -> Option<&TypeMeta> {
        params.first().and_then(|param| self.type_named(&param.ty))
    }

    /// The parameters beyond the subject that the address carries: what an invocation gives.
    #[must_use]
    pub fn arguments<'a>(&self, params: &'a [ParamMeta]) -> &'a [ParamMeta] {
        match (self.subject_of(params), params.split_first()) {
            (Some(_), Some((_, rest))) => rest,
            _ => params,
        }
    }

    pub fn default_view(&self, ty: &str) -> Option<&str> {
        self.default_views
            .iter()
            .find(|d| d.ty == ty)
            .map(|d| d.view.as_str())
    }

    /// The views a window of `ty` (an id) switches between, in the order declared.
    #[must_use]
    pub fn views_of(&self, ty: &str) -> Vec<&str> {
        self.views_of
            .iter()
            .find(|v| v.ty == ty)
            .map(|v| v.views.iter().map(String::as_str).collect())
            .unwrap_or_default()
    }

    /// How the switch offers the view `view` of the type `ty`: its label and icon, `None` for each it
    /// does not name.
    #[must_use]
    pub fn view_shown(&self, ty: &str, view: &str) -> (Option<&str>, Option<&str>) {
        self.views_of
            .iter()
            .find(|v| v.ty == ty)
            .and_then(|v| v.shown.iter().find(|s| s.view == view))
            .map_or((None, None), |s| (s.label.as_deref(), s.icon.as_deref()))
    }

    pub fn row_view(&self, ty: &str) -> Option<&str> {
        self.row_views
            .iter()
            .find(|d| d.ty == ty)
            .map(|d| d.view.as_str())
    }

    /// The actions the platform offers: not the hidden ones of scenarios, the migrations or the ones
    /// that take a `File`. This is the one list every surface draws from — the header's buttons and
    /// menu, an item's menu, the palette.
    pub fn offered_actions(&self) -> impl Iterator<Item = &ActionMeta> {
        self.actions
            .iter()
            .filter(|a| !a.scenario && !a.is_migration() && !a.takes_a_file())
    }

    /// The actions code may call by name: not the hidden ones of scenarios and the migrations.
    pub fn callable_actions(&self) -> impl Iterator<Item = &ActionMeta> {
        self.actions
            .iter()
            .filter(|a| !a.scenario && !a.is_migration())
    }

    /// The package's migrations (`migration action`), in declaration order.
    pub fn migrations(&self) -> impl Iterator<Item = &ActionMeta> {
        self.actions.iter().filter(|a| a.is_migration())
    }

    /// The views a person or an agent can open or apply: not the hidden ones that present an
    /// action's state or are rows of a loop.
    pub fn offered_views(&self) -> impl Iterator<Item = &ViewMeta> {
        self.views
            .iter()
            .filter(|v| v.presents.is_none() && v.row_of.is_none())
    }

    /// The views a person or an agent can open a resource of the type `ty` through: the declared
    /// views that take such a resource and nothing else.
    #[must_use]
    pub fn views_showing(&self, ty: &str) -> Vec<&ViewMeta> {
        let of_type = |param: &ParamMeta| {
            matches!(
                crate::types::Ty::parse_meta(&param.ty, &self.app),
                Some(crate::types::Ty::Record(key)) if key.app == self.app && key.name == ty
            )
        };
        self.offered_views()
            .filter(|view| matches!(&view.params[..], [only] if of_type(only)))
            .collect()
    }

    pub fn to_json_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

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

/// The module `wasm` without the custom section `name`: what the same code written in one file and in
/// several does not share (the lines each declaration is on).
#[must_use]
pub fn without_section(wasm: &[u8], name: &str) -> Vec<u8> {
    let mut out = wasm.get(..8).unwrap_or_default().to_vec();
    let mut pos = 8usize;
    while pos < wasm.len() {
        let start = pos;
        let Some(id) = wasm.get(pos).copied() else {
            break;
        };
        pos += 1;
        let Some(size) = read_leb(wasm, &mut pos) else {
            break;
        };
        let Some(end) = pos.checked_add(size) else {
            break;
        };
        if end > wasm.len() {
            break;
        }
        let named = id == 0 && {
            let mut p = pos;
            match read_leb(wasm, &mut p) {
                Some(len) => wasm.get(p..p.saturating_add(len)) == Some(name.as_bytes()),
                None => false,
            }
        };
        if !named {
            out.extend_from_slice(&wasm[start..end]);
        }
        pos = end;
    }
    out
}

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
