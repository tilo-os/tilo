//! Package metadata, the JSON in the module's `tilolang.meta` section: the package's types,
//! enums, actions (with their effect class and what they reach), views and fns, by id
//! and name, with their doc comments. Tools, the runtime, the platform and agents read it;
//! compiled packages are imported through it. The platform derives the agent's catalogue, the
//! published actions and the app's surface from it.

use crate::types::{EffectClass, MergeKind};
use serde::{Deserialize, Serialize};

/// Name of the wasm custom section that carries the metadata as JSON.
pub const META_SECTION: &str = "tilolang.meta";

/// The custom section a package's catalogues travel in: JSON `[[locale, Fluent text], …]`
/// (i18n.md). Outside the module hash, as the metadata is.
pub const LOCALES_SECTION: &str = "tilolang.locales";

/// The custom section naming where each function's declaration is written: JSON
/// `[{export, file, line}, …]`. Outside the module hash, as the metadata is: what a profile
/// reads a unit's line from, not what the module runs.
pub const LINES_SECTION: &str = "tilolang.lines";

/// Where a function is written: the file of the package its declaration is in, and the line in
/// it, counted from 1. A block, or a function made without a declaration, is written nowhere and
/// has no entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Line {
    /// The function's export (`a_<id>`, `v_<id>`, `q_<id>`, …).
    pub export: String,
    /// The file's path within the package (`/` when it was compiled from one source string).
    pub file: String,
    pub line: u32,
}

/// Appends `lines` to the compiled module `wasm` as its [`LINES_SECTION`]; nothing when there
/// are none.
pub fn append_lines(wasm: &mut Vec<u8>, lines: &[Line]) {
    if lines.is_empty() {
        return;
    }
    append_json_section(wasm, LINES_SECTION, lines);
}

/// Where the functions `wasm` was compiled from are written; none for a module compiled without
/// them.
#[must_use]
pub fn lines_from_wasm(wasm: &[u8]) -> Vec<Line> {
    custom_section(wasm, LINES_SECTION)
        .and_then(|data| serde_json::from_slice(data).ok())
        .unwrap_or_default()
}

/// Appends `value` as JSON in the custom section `name`. A custom section may follow every other
/// section, so the module stays valid.
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

/// Appends `locales` to the compiled module `wasm` as its [`LOCALES_SECTION`]; nothing when
/// there are none. A custom section may follow every other section, so the module stays valid.
pub fn append_locales(wasm: &mut Vec<u8>, locales: &[(String, String)]) {
    if locales.is_empty() {
        return;
    }
    append_json_section(wasm, LOCALES_SECTION, locales);
}

/// The catalogues a compiled module carries, `(locale, Fluent text)`; none when it has none.
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

/// The record kind of each person's own state of an app's workspace: the view state
/// they set in views and what they have seen, kept in the workspace's record of the kind
/// (`tilo.host`'s, `$personal/<person>`), on every device of theirs and readable by every
/// member.
pub const PERSONAL_RECORD_KIND: &str = "tilolang.personal";

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
    /// The ids the app was published under before (`app ... (previously: so.tilo.old)`): the
    /// platform reads its records, grants and windows under them as the app's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub previously: Vec<String>,
    /// Each member keeps the app's records in their own record of the workspace
    /// (`app ... (records: .own)`), which only that person's devices read.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub own_records: bool,
    /// The hosts the app's `uses web` requests may reach, as its header declares them
    /// (`app ... (web: ["example.com", "*.rss.org"])`). A request to any other host is refused
    /// as a value, and recorded with the invocation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub web: Vec<String>,
    /// SHA-256 (hex) of the wasm module without the metadata section.
    pub module_hash: String,
    pub types: Vec<TypeMeta>,
    /// Declared enums. Types that name one are written `enum Name` in metadata.
    #[serde(default)]
    pub enums: Vec<EnumMeta>,
    /// The words the package's views show as written.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<WordMeta>,
    pub actions: Vec<ActionMeta>,
    pub views: Vec<ViewMeta>,
    pub fns: Vec<FnMeta>,
    /// Ids of the root types (`root T`): one resource each per workspace, which the launcher
    /// lists and opens.
    #[serde(default)]
    pub roots: Vec<String>,
    /// The packages it uses (`use app.id`), by their app ids now (a `use` of an id one had
    /// before names it by its current one), and the standard library every module compiles
    /// with (`tilo.std`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uses: Vec<String>,
    /// The named roots (`root notes: Notes`): what other packages reach as `alias.notes`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub named_roots: Vec<NamedRootMeta>,
    /// `type Name = T` of a type that is not a container: another name for `T`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<AliasMeta>,
    /// The view a window of each type uses unless changed (`default view v for T`).
    #[serde(default)]
    pub default_views: Vec<DefaultViewMeta>,
    /// The row a list of each type shows per item (`row view v for T`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub row_views: Vec<DefaultViewMeta>,
    /// The views a window of each type switches between (`views a, b for T`), in order. A type
    /// not named here has no switch: its views are helpers the app places itself.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub views_of: Vec<ViewsMeta>,
    /// Named seed data (`scenario "name" { ... }`), each run by its hidden action.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scenarios: Vec<ScenarioMeta>,
    /// The actions the package runs by itself (`trigger due(t: Task) at t.at: notify_due(t)`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub triggers: Vec<TriggerMeta>,
    /// The arguments of actions views apply that compute from view state (`set(open, not
    /// open)`): functions the runtime runs when the control's event fires, on the view state
    /// as it is then. Each is exported as `e_<id>`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub at_event: Vec<FnMeta>,
}

/// The views a window of a type switches between: the type's id and the views' ids, in the
/// order the declaration names them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewsMeta {
    /// The type id.
    #[serde(rename = "type")]
    pub ty: String,
    /// The views' ids.
    pub views: Vec<String>,
    /// The views whose declaration says how a window's switch offers them, by id: the rest are
    /// offered by their names, with the type's name left off.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shown: Vec<ViewShownMeta>,
}

/// How a view of a type is offered in a window's switch, when its declaration says
/// (`views project_list (label: "List", icon: .list), project_board for Project`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewShownMeta {
    /// The view's id.
    pub view: String,
    /// The short label the switch shows, in the words the app wrote it in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The icon beside it, as a kebab-case name of the platform's set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

/// A named root: its name and its type's id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedRootMeta {
    pub name: String,
    pub ty: String,
    /// The resource the session publishes it as, for a root of an app this runtime does not
    /// run (`sound`, `settings:settings.appearance`): what `device.sound` names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published: Option<String>,
}

/// `type Name = T`: the name and the type it names, as metadata writes types.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AliasMeta {
    pub name: String,
    pub ty: String,
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

/// A `trigger`: when a package's action runs by itself, for the resources of one type.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerMeta {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub doc: String,
    /// The id of the action it runs.
    pub action: String,
    /// The id of the type of the resources it runs for.
    #[serde(rename = "type")]
    pub ty: String,
    /// The action's arguments, in the order the call passes them.
    pub args: Vec<TriggerArgMeta>,
    /// When it runs.
    pub when: TriggerWhenMeta,
}

/// What a trigger passes its action: the record it runs for, or a root of the package.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerArgMeta {
    /// The resource the trigger runs for.
    Subject,
    /// A root record, by its type's id (the records the platform keeps as `root-<type id>`).
    Root(String),
}

/// When a trigger runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerWhenMeta {
    /// At each multiple of these seconds, counted from the epoch.
    Every(i64),
    /// At the instant the field (by id) of the resource holds.
    At(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefaultViewMeta {
    /// The type id.
    #[serde(rename = "type")]
    pub ty: String,
    /// The view id.
    pub view: String,
}

/// Words a view shows as it wrote them (`"{n} new"` given to a label), or an action tells as
/// it wrote them (`notify`'s title and body): a message of the package's catalogue, an
/// attribute of that declaration's own message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WordMeta {
    /// The id of the declaration whose message the words are an attribute of: the view that
    /// shows them, or the action whose notification says them.
    pub view: String,
    /// The attribute naming it under the view's message: `w-` and a slug of its words.
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
    /// What a control shows for the variant: its doc's first line, or its name in sentence
    /// case.
    #[serde(default)]
    pub label: String,
    /// `(icon: .x)`: what a control shows beside the label, a kebab-case name of the
    /// platform's icon set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// `(tone: .x)`: the tone the case reads in, one of `ui.Tone`'s cases, a theme role rather
    /// than a colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tone: Option<String>,
    /// The payload fields, in order. Their merge is always `register`.
    pub fields: Vec<FieldMeta>,
}

/// A `state` declaration of a view: what its person picks there, kept as theirs.
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
    /// What search, a list's filters and agents offer of the type: its `search` block, or the
    /// defaults when it has none.
    #[serde(default, skip_serializing_if = "SearchMeta::is_empty")]
    pub search: SearchMeta,
    /// `(origin: imap.message)`: the kind of an outside service's resource the type holds,
    /// which the system keeps in step with the service (`crate::origin`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// `(title: subject)`: the field a record of the type is named by, as the field's name. A
    /// type that says none is named by its first text field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// `(untitled: "Untitled")`: what one of its records is called while its title is empty,
    /// as the app wrote it. Surfaces that name a record fall back to it, and the control that
    /// edits the record's title shows it while the title holds nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub untitled: Option<String>,
}

/// What search, a list's filters and agents offer of a type (search.md, "What a type declares"):
/// its filters in order (the first are the visible chips), its default order and the fields kept
/// out of filters.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SearchMeta {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filters: Vec<FilterMeta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<SortMeta>,
    /// The fields kept out of filters, by id.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden: Vec<String>,
    /// Whether the type declares these (a `search` block) rather than having the defaults.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub declared: bool,
}

impl SearchMeta {
    pub fn is_empty(&self) -> bool {
        self.filters.is_empty() && self.sort.is_none() && self.hidden.is_empty() && !self.declared
    }

    /// The filter a query names `key`.
    pub fn filter(&self, key: &str) -> Option<&FilterMeta> {
        self.filters.iter().find(|f| f.key == key)
    }
}

/// One filter of a type.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FilterMeta {
    /// What a query names it by: the field's name (`done`), or a named filter's label in snake
    /// case (`has_attachment`).
    pub key: String,
    /// What a control shows: the label written, or the field's name in sentence case.
    pub label: String,
    /// What it is, for agents: the field's doc comment's first line.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub doc: String,
    pub kind: FilterKind,
    /// The field it is on, by id; none for a named filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// The field's type as metadata writes it (`Option<Date>`, `Set<Text>`, `enum Status`).
    #[serde(rename = "type", default, skip_serializing_if = "String::is_empty")]
    pub ty: String,
    /// A named filter's predicate over the resource's own fields, by field id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pred: Option<crate::pred::Pred<String>>,
    /// Whether it may hold no value, an option or a set: `none` and `any` apply.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub optional: bool,
    /// Whether it holds several values, a set: a value asks whether it holds that one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub many: bool,
    /// What its control shows beside its label: the icon its declaration names
    /// (`filter done (icon: .circle_check)`).
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
    /// Whether the filter is a toggle, on or off, as a list's places and the palette's chips
    /// show it.
    pub const fn is_toggle(self) -> bool {
        matches!(self, Self::Bool | Self::Predicate)
    }
}

/// A type's default order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SortMeta {
    /// The field's name.
    pub key: String,
    /// The field, by id.
    pub field: String,
    pub order: SortOrder,
}

/// Which way an order runs.
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

    /// Whether the order runs from the largest, the latest or the last in the alphabet.
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
    /// Whether the field holds text as it is written: a `Text`, or a `RichText`, which the
    /// store reads as the text it is written as (search.md, "What a type declares").
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
    /// An action parameter's invocation-time default: an invocation that leaves the argument
    /// out gets it, computed then over the arguments before it and recorded as the argument.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<DefaultMeta>,
    /// Whether the view offering the action supplies it (`above: Option<Task> (above)`): the
    /// place the item stands in, which no invocation gives. A surface offering the action with
    /// no such view (a palette over the item alone) supplies `.none`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub place: bool,
    /// Whether the text of an entry fills it: an action's one `Text` parameter is what the
    /// words before the entry type say into its other parameters, the rest titles the item
    /// (`crate::entry`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub entry: bool,
}

/// How an action parameter's default is computed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefaultMeta {
    /// The module export computing it, which takes the parameters before this one.
    pub export: String,
    /// The default as written (`not item.done`), for descriptions.
    pub text: String,
}

impl DefaultMeta {
    /// The default `.none` as written.
    pub const NONE: &str = ".none";

    /// `= .none`, the default of an optional parameter of an app that publishes its actions
    /// without a module (a native app's, the session's): no code computes it, so it has no
    /// export.
    #[must_use]
    pub fn none() -> Self {
        Self {
            export: String::new(),
            text: Self::NONE.to_owned(),
        }
    }

    /// Whether the default is `.none`: a constant, which needs no code to compute. A call that
    /// leaves the parameter out passes `.none`, wherever the parameter stands.
    #[must_use]
    pub fn is_none(&self) -> bool {
        self.text == Self::NONE
    }
}

/// An action that code can reach, with its effect class. `own` is relative to the
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
    /// `did: "moved this"`: what the action did to its subject, as a history entry reads it
    /// after who did it. An attribute of the action's message in the app's catalogue
    /// (`d-<id>.did`), so a translation says it in the reader's language.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub did: Option<String>,
    /// The id of the view that presents the action's state for a subject, when its `label:` or
    /// `icon:` follows the subject or it declares `on:`. The view takes the action's first
    /// parameter and builds one node of kind `present` with the props given dynamically:
    /// `label` (Text), `icon` (Text, a kebab-case icon name) and `on` (Bool). `label` and `icon`
    /// above are what shows without evaluating it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub present: Option<String>,
    /// The id of the subject's `Bool` field whose state the action toggles, when its `on:` is
    /// that field (`on: n.pinned`). A control bound to the field stands for the action where
    /// both would show.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toggles: Option<String>,
    /// `key: "Ctrl+Enter"`: the shortcut that runs the action while its window has the keyboard,
    /// and (on an item's action) while that item is selected or its view shows. One spelling —
    /// the modifiers in one order, then the key's token (`Ctrl+Shift+enter`) — that the toolkit
    /// matches a press against, with the platform's own command modifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// `group: "Reply"`: the run of the resource's actions this one stands in, as the app names
    /// it. A surface showing the resource's actions draws the runs in the order the app
    /// declared them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// `uses web, random, time`: the effects the action's body reads, each of which its
    /// invocation records and a replay answers from the record ([`crate::effects::Effect`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uses: Vec<String>,
    /// Whether the action keeps a requirement its subject alone decides, exported
    /// as [`crate::ir::guard_export`]: the first reason it cannot run on the subject now, or empty
    /// while it can. What a surface offering the action reads its availability from.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub guard: bool,
    /// Whether the action keeps an `applies` condition, exported
    /// as [`crate::ir::applies_export`]: the first reason it cannot apply to the item where it is
    /// offered, or empty while it can. A menu leaves such an action out; a palette lists it,
    /// disabled with the reason.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub applies: bool,
    /// Whether the action's body invokes another action, itself included. A run that does is
    /// run again from its start whenever a call waits for a person, so it journals its reads and
    /// its writes; one that does not is one change and is never replayed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub invokes: bool,
}

impl ActionMeta {
    /// Whether it is a `migration action`: never offered, run by the platform once on each
    /// resource of its subject's type that an earlier version wrote.
    #[must_use]
    pub fn is_migration(&self) -> bool {
        self.role.as_deref() == Some("migration")
    }

    /// Whether the action takes a `File` — the person's own file, which the chooser a control
    /// runs fills ([`Self::unbound`]'s exception): it is then no command, only a control's.
    #[must_use]
    pub fn takes_a_file(&self) -> bool {
        self.params.iter().any(|param| param.ty.trim() == "File")
    }

    /// The parameters an invocation that has given `bound` arguments still lacks: those after
    /// them that have no default, each with its place, which its form asks the person for
    /// before the action runs. Empty when it runs at once on what it was given.
    ///
    /// What an offer binds — the subject, the places its view supplies, the resource holding
    /// the subject — is what `bound` counts, so this is the one rule for what a command still
    /// asks: the form an invocation opens on (`missing_arguments`) and the "…" its label says
    /// so with ([`crate::host::Action`]'s label) both read it.
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
    /// The view's name, as it is declared: what its window is called and what a screen reader
    /// says. A loop body is a part of the view it stands in and takes that view's name, never a
    /// binding's.
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
    /// What one is called in a sentence: its name in words, lower case (`TodoList` is a
    /// `todo list`).
    #[must_use]
    pub fn noun(&self) -> String {
        in_words(&self.name)
    }

    /// What several are called in a sentence: [`plural`] of its [`noun`](Self::noun).
    #[must_use]
    pub fn plural(&self) -> String {
        plural(&self.noun())
    }
}

/// A declared name in words, lower case: `TodoList` and `todo_list` are `todo list`.
#[must_use]
pub fn in_words(name: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    for ch in name.chars() {
        if ch == '_' || ch == ' ' {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        } else if ch.is_uppercase() && !word.is_empty() {
            words.push(std::mem::take(&mut word));
            word.extend(ch.to_lowercase());
        } else {
            word.extend(ch.to_lowercase());
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words.join(" ")
}

/// What several of a noun are called, as English writes most: `notes`, `entries`, `boxes`.
#[must_use]
pub fn plural(noun: &str) -> String {
    let consonant_y = noun.strip_suffix('y').is_some_and(|stem| {
        stem.chars()
            .last()
            .is_some_and(|c| !matches!(c, 'a' | 'e' | 'i' | 'o' | 'u'))
    });
    if consonant_y {
        format!("{}ies", noun.strip_suffix('y').unwrap_or(noun))
    } else if ["s", "x", "z", "ch", "sh"]
        .iter()
        .any(|end| noun.ends_with(end))
    {
        format!("{noun}es")
    } else {
        format!("{noun}s")
    }
}

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

    /// The type an action is on: that of its first parameter, when it is a declared record.
    #[must_use]
    pub fn subject_of(&self, params: &[ParamMeta]) -> Option<&TypeMeta> {
        params.first().and_then(|param| self.type_named(&param.ty))
    }

    /// The parameters of an action beyond its subject, which the address carries: what an
    /// invocation gives.
    #[must_use]
    pub fn arguments<'a>(&self, params: &'a [ParamMeta]) -> &'a [ParamMeta] {
        match (self.subject_of(params), params.split_first()) {
            (Some(_), Some((_, rest))) => rest,
            _ => params,
        }
    }

    /// The id of the default view of the type `ty` (an id), when the package declares one.
    pub fn default_view(&self, ty: &str) -> Option<&str> {
        self.default_views
            .iter()
            .find(|d| d.ty == ty)
            .map(|d| d.view.as_str())
    }

    /// The views a window of the type `ty` (an id) switches between, in the order declared;
    /// empty when the type declares none.
    #[must_use]
    pub fn views_of(&self, ty: &str) -> Vec<&str> {
        self.views_of
            .iter()
            .find(|v| v.ty == ty)
            .map(|v| v.views.iter().map(String::as_str).collect())
            .unwrap_or_default()
    }

    /// How the switch offers the view `view` (an id) of the type `ty` (an id): the short label
    /// and the icon its declaration names, `None` for each it does not.
    #[must_use]
    pub fn view_shown(&self, ty: &str, view: &str) -> (Option<&str>, Option<&str>) {
        self.views_of
            .iter()
            .find(|v| v.ty == ty)
            .and_then(|v| v.shown.iter().find(|s| s.view == view))
            .map_or((None, None), |s| (s.label.as_deref(), s.icon.as_deref()))
    }

    /// The id of the row view of the type `ty` (an id), when the package declares one.
    pub fn row_view(&self, ty: &str) -> Option<&str> {
        self.row_views
            .iter()
            .find(|d| d.ty == ty)
            .map(|d| d.view.as_str())
    }

    /// The actions the platform offers: every action but the hidden ones of scenarios and the
    /// migrations, which the platform runs itself, and but the ones that take a `File` — a file
    /// is the person's own, and the chooser a control runs fills that parameter
    /// (`file.choose(a)`), so it is no command a menu, a palette, an agent or a program runs.
    /// This is the one list every surface draws from — the header's buttons and menu, an item's
    /// menu, the palette — so an action is always somewhere a person, an agent or a program can
    /// reach it.
    pub fn offered_actions(&self) -> impl Iterator<Item = &ActionMeta> {
        self.actions
            .iter()
            .filter(|a| !a.scenario && !a.is_migration() && !a.takes_a_file())
    }

    /// The actions code may call by name: every action but the hidden ones of scenarios and the
    /// migrations, which the platform runs itself.
    pub fn callable_actions(&self) -> impl Iterator<Item = &ActionMeta> {
        self.actions
            .iter()
            .filter(|a| !a.scenario && !a.is_migration())
    }

    /// The package's migrations (`migration action`), in declaration order.
    pub fn migrations(&self) -> impl Iterator<Item = &ActionMeta> {
        self.actions.iter().filter(|a| a.is_migration())
    }

    /// The views a person or an agent can open or apply: every view but the hidden ones that
    /// present an action's state or are the rows of a loop.
    pub fn offered_views(&self) -> impl Iterator<Item = &ViewMeta> {
        self.views
            .iter()
            .filter(|v| v.presents.is_none() && v.row_of.is_none())
    }

    /// The views a person or an agent can open a resource of the type `ty` (an id) through: the
    /// declared views that take such a resource and nothing else. An app's helpers are not among
    /// them: they belong to the views that place them.
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

/// The module `wasm` without the custom section `name`: the module apart from what the
/// compiler writes beside its code for tools, which the same code written in one file and in
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
