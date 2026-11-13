//! The package the host itself provides: the types that exist in every module. `Invocation` records
//! what happened to an action a block applied: the host creates and changes these resources, and code
//! only reads them. Every module sees the package as `host`.

use crate::meta::{EnumMeta, FieldMeta, PackageMeta, TypeMeta, VariantMeta};
use crate::types::MergeKind;

pub const HOST_APP: &str = "tilo.host";
pub const INVOCATION_TYPE: &str = "host-invocation";
pub const NOTICE_TYPE: &str = "host-notice";
pub const NOTICE_TO: &str = "notice-to";
pub const NOTICE_TITLE: &str = "notice-title";
pub const NOTICE_BODY: &str = "notice-body";
pub const NOTICE_OPENS: &str = "notice-opens";
pub const NOTICE_SEEN: &str = "notice-seen";
pub const LINK_HOW_TYPE: &str = "host-link-how";

/// The ids of the built-in enum `Result<T, E>`: its type, its variants and their payload fields. `try
/// act(args)` builds one from an invocation's outcome.
pub const RESULT_TYPE: &str = "result";
pub const RESULT_OK: &str = "ok";
pub const RESULT_ERR: &str = "err";
pub const RESULT_VALUE: &str = "value";
pub const RESULT_ERROR: &str = "error";

/// A type of the host whose values are made by the host and read by code, field by field: what the
/// platform says about a resource, an action or a lens. Such a value is not a resource and is never
/// stored; it is held as a tuple of its fields, in this order.
pub struct ValueType {
    pub name: &'static str,
    pub id: &'static str,
    pub doc: &'static str,
    pub fields: &'static [(&'static str, &'static str, &'static str)],
}

/// A closed set of the host's surface: a type whose values code names by case, so that a case the host
/// never answers is a compile error.
pub struct HostEnum {
    pub name: &'static str,
    pub id: &'static str,
    pub doc: &'static str,
    pub variants: &'static [(&'static str, &'static str, &'static str)],
}

pub const HOST_ENUMS: &[HostEnum] = &[HostEnum {
    name: "LinkHow",
    id: LINK_HOW_TYPE,
    doc: "How a resource is linked to another, as `linked(r)` answers: made from it, naming it in a field, or mentioning it in its rich text.",
    variants: &[
        (
            "created_from",
            "Made from it",
            "An action made it from the resource it links to.",
        ),
        (
            "field",
            "Named in a field",
            "A reference field of it points at the resource it links to.",
        ),
        (
            "mention",
            "Mentioned in its text",
            "Its rich text names the resource it links to.",
        ),
    ],
}];

pub const VALUE_TYPES: &[ValueType] = &[
    ValueType {
        name: "Action",
        id: "host-action",
        doc: "An action offered on a resource to the person looking, as the platform presents it.",
        fields: &[
            (
                "label",
                "Text",
                "What it does, as a button or a menu names it.",
            ),
            (
                "icon",
                "Option<Text>",
                "The name of its icon, when it declares one.",
            ),
            ("role", "Text", "`primary`, `destructive`, or empty."),
            (
                "primary",
                "Bool",
                "Whether it is the resource's primary action.",
            ),
            (
                "destructive",
                "Bool",
                "Whether it takes something away: it is undone, not confirmed.",
            ),
            (
                "on",
                "Option<Bool>",
                "Whether the state it toggles is on for the resource, when it says.",
            ),
            (
                "key",
                "Option<Text>",
                "The plain key that runs it while its window has the keyboard (and, on a row's \
                 action, while that row is selected), when it declares one.",
            ),
            (
                "params",
                "List<Param>",
                "What it takes besides the resource, each with its default.",
            ),
            (
                "run",
                "Action",
                "The action applied to the resource: what a control runs.",
            ),
            (
                "group",
                "Option<Text>",
                "The run of the resource's actions it stands in, as the app names it \
                 (`group: \"Reply\"`); empty for one in no run.",
            ),
            (
                "available",
                "Bool",
                "Whether it can run on the resource now: the requirements its subject decides \
                 hold. A control for one that cannot is disabled, and says `reason`.",
            ),
            (
                "reason",
                "Text",
                "Why it cannot run now, as the `require` or `applies` that does not hold says \
                 it, in the person's language; empty while it can.",
            ),
            (
                "applies",
                "Bool",
                "Whether it can apply to the item where it is offered at all: the `applies` \
                 conditions its subject and place decide hold. A menu leaves one that does not \
                 apply out; a palette lists it, disabled with `reason`.",
            ),
        ],
    },
    ValueType {
        name: "Param",
        id: "host-param",
        doc: "A parameter of an action.",
        fields: &[
            ("name", "Text", "Its name."),
            ("type_name", "Text", "Its type, as source writes it."),
            (
                "default",
                "Option<Text>",
                "Its default as written, when it has one.",
            ),
        ],
    },
    ValueType {
        name: "Version",
        id: "host-version",
        doc: "A version of a resource, as history keeps it: who made it, when, by which action, in which run, and what it did.",
        fields: &[
            ("who", "Principal", "Who made it: a person, or an agent."),
            ("at", "Time", "When it was made."),
            (
                "what",
                "Text",
                "The action that made it, by its label, or by its name read as words when it declares none.",
            ),
            (
                "run",
                "Option<Run>",
                "The run that made it; `.none` for a change made outside a run.",
            ),
            (
                "kind",
                "Text",
                "What it did to the resource: `created`, `changed` or `removed`. A removed resource keeps its versions, so who removed it stays answerable.",
            ),
            (
                "fields",
                "List<Text>",
                "The fields it changed, by name; empty for one that made or removed the resource.",
            ),
            ("of", "Resource", "The resource it is a version of."),
            (
                "from",
                "Option<Resource>",
                "The resource it was made from, for the version that made it: the invocation that made it was made from that resource. `.none` for a change that made none.",
            ),
            (
                "mine",
                "Bool",
                "Whether the person looking made it, outside any run: one they may undo on its own.",
            ),
            (
                "actor",
                "Text",
                "Who made it, as a sentence names them: you for the person looking, everyone else by their name.",
            ),
            (
                "did",
                "Text",
                "What it did, in words a sentence goes on with after who did it \
                 (`moved this to`): the sentence of the fields it changed, derived from the \
                 change itself; what its action declares (`did:`); or, for one that made or \
                 removed the resource, `created this` or `removed this`. Empty when there is \
                 nothing to say but what its action is called (`what`).",
            ),
            (
                "icon",
                "Option<Text>",
                "The icon of the action that made it, when it declares one.",
            ),
            (
                "changes",
                "List<Change>",
                "Each field it changed, with what the field held before and after, read from the change itself; empty for one that made or removed the resource.",
            ),
            (
                "suggested",
                "Text",
                "Who proposed the action the change made, as a sentence names them, when the person making it accepted their suggestion; empty for a change made on its own.",
            ),
            (
                "count",
                "Int",
                "How many items the change was made over: `1` for an action applied to its \
                 subject, and for one action applied to many items at once (`many`), as many \
                 as the items it changed.",
            ),
        ],
    },
    ValueType {
        name: "Link",
        id: "host-link",
        doc: "A resource linked to another, as `linked(r)` answers: what links to it and how.",
        fields: &[
            (
                "of",
                "Resource",
                "The linked resource: one made from the resource the query named, one whose text mentions it, or one whose field refers to it.",
            ),
            (
                "how",
                "enum LinkHow",
                "How it is linked: `created_from`, `field` or `mention`.",
            ),
        ],
    },
    ValueType {
        name: "Step",
        id: "host-step",
        doc: "A step of a resource's history as people read it: consecutive versions by the same person, or by the same run, folded into one.",
        fields: &[
            ("who", "Principal", "Who made it: a person, or an agent."),
            ("at", "Time", "When its last version was made."),
            (
                "run",
                "Option<Run>",
                "The run that made it; `.none` for changes made outside a run.",
            ),
            ("versions", "List<Version>", "Its versions, oldest first."),
            (
                "mine",
                "Bool",
                "Whether the person looking made it, outside any run.",
            ),
            (
                "changes",
                "Int",
                "How many changes it folds: one for one command, however many items it \
                 changed at once, and more for a step of several.",
            ),
            (
                "opens",
                "Bool",
                "Whether it opens a visit: the newest step, or one whose hand, or the moment \
                 it was made at, is not the step above it's. A history draws the face of the \
                 step that opens a visit, and the steps below it that open none join it.",
            ),
            (
                "latest",
                "Bool",
                "Whether it is the newest step: the change whose result the page shows.",
            ),
        ],
    },
    ValueType {
        name: "Change",
        id: "host-change",
        doc: "A field a version changed: which, what it held before and after, each shown as the field's type shows it, and the words history says it with.",
        fields: &[
            ("field", "Text", "The field, by its name in the source."),
            ("label", "Text", "The field, in words: `Due` for `due`."),
            (
                "before",
                "View",
                "What the field held before the change, shown as its type shows it: a day as a day, a person as a person.",
            ),
            (
                "after",
                "View",
                "What the field holds after the change, shown as its type shows it.",
            ),
            (
                "said",
                "Text",
                "The change in words a sentence goes on with (`moved this to`, `assigned this \
                 to`), decided by the field's type and by what it held before and after, from \
                 the words the platform keeps; empty when the field reads as nothing of its \
                 own. Who made it stands in the version, not in each change; a change after \
                 the first of its version opens with `and`.",
            ),
            (
                "shows",
                "Text",
                "Which of the two values the words speak of: `after` (`assigned this to`), \
                 `before` (`moved this out of`), or empty when they speak of neither \
                 (`unassigned this`).",
            ),
        ],
    },
    ValueType {
        name: "Run",
        id: "host-run",
        doc: "One run: what an agent or a script did for the person who started it, kept and undone as one unit.",
        fields: &[
            (
                "id",
                "Text",
                "The run's group, which every change it made is stamped with.",
            ),
            (
                "of",
                "Text",
                "The id of the work it is a run of, which its group opens with.",
            ),
            (
                "title",
                "Text",
                "What the run was asked, as it was titled when it ran.",
            ),
        ],
    },
    ValueType {
        name: "EntryPart",
        id: "host-entry-part",
        doc: "One thing the text of an entry will make of the action it runs, as `entry_parts` \
              read it: a day, a time, a person or a record, taken out of the title.",
        fields: &[
            (
                "label",
                "Text",
                "What it says: `Fri 30 Oct`, `10:00`, `@Bo`, `#Move flat`.",
            ),
            (
                "icon",
                "Option<Text>",
                "Its icon, a kebab-case name of the platform's icon set.",
            ),
            (
                "without",
                "Text",
                "What is typed once it is removed: the text without its words.",
            ),
        ],
    },
    ValueType {
        name: "Delivery",
        id: "host-delivery",
        doc: "How an outside effect a resource's action queued stands: a mail being sent.",
        fields: &[
            (
                "state",
                "Text",
                "`waiting` (to go online), `sending`, `sent`, `rejected` or `unknown` (the device \
                 stopped while handing it over, so it is not tried again).",
            ),
            (
                "reason",
                "Text",
                "Why it waits or was rejected, as the service said; empty otherwise.",
            ),
        ],
    },
    ValueType {
        name: "Lens",
        id: "host-lens",
        doc: "Someone's lens on a resource: a person or an agent whose window shows it now.",
        fields: &[
            ("who", "Principal", "Whose lens it is."),
            (
                "composing",
                "Bool",
                "Whether they are typing a message in it now: a draft under way, never what \
                 it says (what a conversation shows as typing).",
            ),
            (
                "mine",
                "Bool",
                "Whether it is the lens of the person looking.",
            ),
        ],
    },
    ValueType {
        name: "Meta",
        id: "host-meta",
        doc: "What the platform shows of a resource or of its type.",
        fields: &[
            (
                "title",
                "Text",
                "Its title: the first line of its first text field; else its app's title for a root, or its type's name.",
            ),
            (
                "type_name",
                "Text",
                "Its type's name, in words: `New note` for `NewNote`.",
            ),
            ("noun", "Text", "What one is called in a sentence: `note`."),
            (
                "plural",
                "Text",
                "What several are called in a sentence: `notes`.",
            ),
            ("icon", "Option<Text>", "The name of its app's icon."),
            (
                "opens",
                "Bool",
                "Whether it opens in a window of its own: its type has a default view.",
            ),
            (
                "row",
                "Bool",
                "Whether its type declares a row view, the one a list of its type draws each item through.",
            ),
        ],
    },
    ValueType {
        name: "Filter",
        id: "host-filter",
        doc: "A filter a type declares, as code names it and as a list's places and search's chips show it: one per case of an enum's.",
        fields: &[
            ("key", "Text", "What a query names it by: `done`, `status`."),
            ("label", "Text", "What its control shows."),
            (
                "icon",
                "Option<Text>",
                "The name of its icon, when its case declares one.",
            ),
            (
                "query",
                "Text",
                "What choosing it asks, as people type it in search: `is:done`, `status:open`, `project:<id>`; empty for a filter whose value is still to be picked.",
            ),
            (
                "toggle",
                "Bool",
                "Whether it is on or off: a place of a list, a chip of search.",
            ),
        ],
    },
    ValueType {
        name: "Sort",
        id: "host-sort",
        doc: "The order by one field of a type, as code names it and as a window's sort menu shows it: `Note.title` where the window's sorts are declared.",
        fields: &[
            (
                "label",
                "Text",
                "What its menu entry says: the field's label.",
            ),
            (
                "query",
                "Text",
                "The order it asks, as people type it in search: `sort:title`, `sort:-received`.",
            ),
        ],
    },
    ValueType {
        name: "Match",
        id: "host-match",
        doc: "A match of `grep` in the text of a resource: where it is, what it says, and the facts the system knows of it.",
        fields: &[
            (
                "doc",
                "Resource",
                "The resource whose text holds it: a note, a todo, a message's part.",
            ),
            (
                "anchor",
                "Text",
                "Where in it: the paragraph (`¶4`, `¶2.1` for a list's first item), or the field (`title`).",
            ),
            ("text", "Text", "What matched."),
            ("line", "Text", "The whole paragraph or field it is in."),
            (
                "groups",
                "List<Text>",
                "The regular expression's groups: the whole match first, then each group, empty where one took no part.",
            ),
            (
                "kind",
                "Text",
                "The kind of block it is in: `paragraph`, `heading`, `quote`, `list`, `code`, or `field` for a text field of one line.",
            ),
            (
                "written_by",
                "Option<Principal>",
                "Who wrote that run of text, when the system knows: an authored run, or a resource only one person changed.",
            ),
            (
                "changed",
                "Option<Time>",
                "When the resource last changed, from the log.",
            ),
        ],
    },
];

/// The type that stands for any resource, of whatever app: what `find` and `grep` answer when they read
/// across apps. `.of(T)` keeps those of one type.
pub const RESOURCE_TYPE_NAME: &str = "Resource";

#[must_use]
pub fn value_type(name: &str) -> Option<&'static ValueType> {
    VALUE_TYPES.iter().find(|t| t.name == name)
}

/// Whether the record type `key` is a value type of the host: held as a tuple, never a resource, so it
/// has no id.
#[must_use]
pub fn is_value(key: &crate::types::TypeKey) -> bool {
    key.app == HOST_APP && value_type(&key.name).is_some()
}

#[must_use]
pub fn value_field(ty: &str, field: &str) -> Option<usize> {
    value_type(ty)?
        .fields
        .iter()
        .position(|(name, ..)| *name == field)
}

/// The value types that name an action, `Version` (the one that made it) and `Action` (the one offered),
/// which `v.is(edit)` asks about by the action itself rather than its label.
pub const NAMES_AN_ACTION: &[&str] = &["Version", "Action"];

/// Where the tuple of the value type `ty` holds the id of the action it names: after its fields, which
/// code reads by name, so the id is compared only through `.is(action)`.
#[must_use]
pub fn action_id_at(ty: &str) -> Option<usize> {
    NAMES_AN_ACTION
        .contains(&ty)
        .then(|| value_type(ty).map(|t| t.fields.len()))
        .flatten()
}

fn field(name: &str, ty: &str, doc: &str) -> FieldMeta {
    FieldMeta {
        id: format!("inv-{name}"),
        name: name.to_string(),
        ty: ty.to_string(),
        merge: MergeKind::Register,
        doc: doc.to_string(),
        default: None,
    }
}

fn notice_field(id: &str, name: &str, ty: &str, merge: MergeKind, doc: &str) -> FieldMeta {
    FieldMeta {
        id: id.to_string(),
        name: name.to_string(),
        ty: ty.to_string(),
        merge,
        doc: doc.to_string(),
        default: None,
    }
}

pub fn host_meta() -> PackageMeta {
    PackageMeta {
        app: HOST_APP.to_string(),
        title: "Host".to_string(),
        types: std::iter::once(TypeMeta {
            id: INVOCATION_TYPE.to_string(),
            name: "Invocation".to_string(),
            doc: "An action a block applied, and what became of it.".to_string(),
            fields: vec![
                field("action", "Text", "The id of the action."),
                field("name", "Text", "Its name."),
                field("app", "Text", "The app that declares it."),
                field("args", "Text", "The arguments, as JSON."),
                field("author", "Principal", "Who wrote the block."),
                field(
                    "state",
                    "Text",
                    "done, suggested, declined, failed or denied.",
                ),
                field("result", "Text", "The result, or why it failed."),
                field(
                    "run",
                    "Text",
                    "The suggestion's run on the device that made it.",
                ),
                field(
                    "subject",
                    "Option<Resource>",
                    "The resource the action is about, when a person proposed it: the resource it waits on.",
                ),
                field(
                    "proposed",
                    "Bool",
                    "Whether a person proposed the action, to accept or dismiss, rather than the authority asking about one as it was applied.",
                ),
            ],
            search: crate::meta::SearchMeta::default(),
            origin: None,
            title: None,
            untitled: None,
        })
        .chain(std::iter::once(TypeMeta {
            id: format!("host-{}", RESOURCE_TYPE_NAME.to_lowercase()),
            name: RESOURCE_TYPE_NAME.to_owned(),
            doc: "Any resource of the workspace, of whichever app: what `find` and `grep` answer across apps. `.of(T)` keeps those of one type.".to_owned(),
            fields: Vec::new(),
            search: crate::meta::SearchMeta::default(),
            origin: None,
            title: None,
            untitled: None,
        }))
        .chain(std::iter::once(TypeMeta {
            id: NOTICE_TYPE.to_string(),
            name: "Notice".to_string(),
            doc: "Something a person is told: the record `notify` writes in the change that tells them, addressed to them alone, and kept until they have seen it. Every device of theirs shows it through its notifications; one of them dismissing or opening it marks it seen for all.".to_string(),
            fields: vec![
                notice_field(NOTICE_TO, "to", "Principal", MergeKind::Register, "The person told."),
                notice_field(NOTICE_TITLE, "title", "Text", MergeKind::Text, "What the notification shows: a line."),
                notice_field(NOTICE_BODY, "body", "Text", MergeKind::Text, "What it shows below that line, empty for nothing."),
                notice_field(NOTICE_OPENS, "opens", "Resource", MergeKind::Register, "The resource choosing the notification opens."),
                notice_field(NOTICE_SEEN, "seen", "Bool", MergeKind::Register, "Whether the person has opened or dismissed it."),
            ],
            search: crate::meta::SearchMeta::default(),
            origin: None,
            title: None,
            untitled: None,
        }))
        .chain(VALUE_TYPES.iter().map(|value| {
            TypeMeta {
                id: value.id.to_string(),
                name: value.name.to_string(),
                doc: value.doc.to_string(),
                fields: value
                    .fields
                    .iter()
                    .map(|(name, ty, doc)| FieldMeta {
                        id: format!("{}-{name}", value.id),
                        name: (*name).to_string(),
                        ty: (*ty).to_string(),
                        merge: MergeKind::Register,
                        doc: (*doc).to_string(),
                        default: None,
                    })
                    .collect(),
                search: crate::meta::SearchMeta::default(),
                origin: None,
                title: None,
                untitled: None,
            }
        }))
        .collect(),
        enums: HOST_ENUMS
            .iter()
            .map(|e| EnumMeta {
                id: e.id.to_string(),
                name: e.name.to_string(),
                doc: e.doc.to_string(),
                variants: e
                    .variants
                    .iter()
                    .map(|(name, label, doc)| VariantMeta {
                        id: (*name).to_string(),
                        name: (*name).to_string(),
                        doc: (*doc).to_string(),
                        label: (*label).to_string(),
                        icon: None,
                        tone: None,
                        fields: Vec::new(),
                    })
                    .collect(),
            })
            .collect(),
        ..PackageMeta::default()
    }
}
