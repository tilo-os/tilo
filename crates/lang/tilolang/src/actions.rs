//! What tilolang code reads of the workspace's action registry, and does through it: the
//! catalogue every app and the workspace publish, and one call of an action named by its address.
//!
//! Both are projections of the one registry the dispatch keeps: the catalogue is the same pages
//! the command line, MCP and the SDKs read, and a call is the same child invocation the app's own
//! code makes of another app's action, decided as the caller it runs for. The language knows this
//! package as `tilo.actions` (`use tilo.actions as actions`); the host answers it over its
//! action connection, never by walking a registry of its own.
//!
//! An address names resources separated by `/`, then the action after `#`:
//! `<app id>/<resource id>#<action>` (`so.tilo.notes/notes/<id>#write`). It is written and read
//! with [`tilo_actions::tree::Address`], the one address type every caller uses.

use crate::meta::{ActionMeta, EnumMeta, FieldMeta, PackageMeta, ParamMeta, TypeMeta, VariantMeta};
use crate::types::{EffectClass, MergeKind};

/// The package, as a program names it: `use tilo.actions as actions`.
pub const ACTIONS_APP: &str = "tilo.actions";

/// One action the workspace's apps and the workspace itself publish, as `actions.catalogue`
/// answers it. Its name is not `Action`: a package's `Action` is the language's own invocable
/// action a view shows, and a record type of that name reads as that instead.
pub const ACTION_TYPE: &str = "actions-action";
/// One parameter of such an action.
pub const PARAM_TYPE: &str = "actions-param";
/// What an action does when it runs: it reads, or it changes something.
pub const KIND_ENUM: &str = "actions-kind";

/// The action that reads the catalogue.
pub const CATALOGUE_ACTION: &str = "catalogue";
/// The action that calls one action by its address.
pub const CALL_ACTION: &str = "call";

fn field(id: &str, name: &str, ty: &str, doc: &str) -> FieldMeta {
    FieldMeta {
        id: id.to_string(),
        name: name.to_string(),
        ty: ty.to_string(),
        merge: MergeKind::Register,
        doc: doc.to_string(),
        default: None,
    }
}

fn param(name: &str, ty: &str, doc: &str) -> ParamMeta {
    ParamMeta {
        name: name.to_string(),
        ty: ty.to_string(),
        default: None,
        place: false,
        entry: false,
        secret: false,
        label: Some(doc.to_string()),
    }
}

/// `actions.catalogue(query, app)`: the actions the caller is offered, narrowed to those whose
/// words hold `query` when it names some, and to one app's when `app` names one.
fn catalogue_action() -> ActionMeta {
    ActionMeta {
        id: CATALOGUE_ACTION.to_string(),
        name: CATALOGUE_ACTION.to_string(),
        verb: String::new(),
        doc: "The actions your apps and this workspace offer you, as the dispatch lists them: \
              each one's address, app, label, resource and parameters. A read: code reads it \
              where it runs and never stores a copy."
            .to_string(),
        params: vec![
            param(
                "query",
                "Option<Text>",
                "Only the actions whose words hold this, when given",
            ),
            param("app", "Option<Text>", "Only this app's actions, when named"),
        ],
        result: Some("List<Offered>".to_string()),
        class: EffectClass::Read,
        leaves: false,
        reaches: Vec::new(),
        label: "Read the actions you are offered".to_string(),
        icon: None,
        role: None,
        scenario: false,
        did: None,
        did_many: None,
        keywords: Vec::new(),
        present: None,
        toggles: None,
        settles: None,
        field: None,
        key: None,
        group: None,
        export: None,
        uses: Vec::new(),
        guard: false,
        applies: false,
        invokes: false,
        outside: Vec::new(),
        notice: false,
        search: None,
        opens: Vec::new(),
        shows: Vec::new(),
        takes: Vec::new(),
    }
}

/// `actions.call(address, arguments)`: one published action, called as the code that runs for
/// the person, and the invocation it was. A call that needs the person's yes waits in its asked
/// record; a refusal comes back as one.
fn call_action() -> ActionMeta {
    ActionMeta {
        id: CALL_ACTION.to_string(),
        name: CALL_ACTION.to_string(),
        verb: String::new(),
        doc: "Call one action the catalogue names, at its address, with its arguments: the call \
              runs as the person the code runs for, inside the same authority, and answers the \
              invocation it was. A call that needs a yes waits for it, and reads as waiting."
            .to_string(),
        params: vec![
            param(
                "address",
                "Text",
                "Its address: <app id>/<resource id>#<action>",
            ),
            param(
                "arguments",
                "json.Value",
                "What it is called with, an object of its parameters by name",
            ),
        ],
        result: Some("tilo.invocations.Invocation".to_string()),
        class: EffectClass::Outside,
        leaves: false,
        reaches: Vec::new(),
        label: "Call an action".to_string(),
        icon: None,
        role: None,
        scenario: false,
        did: None,
        did_many: None,
        keywords: Vec::new(),
        present: None,
        toggles: None,
        settles: None,
        field: None,
        key: None,
        group: None,
        export: None,
        uses: Vec::new(),
        guard: false,
        applies: false,
        invokes: true,
        outside: Vec::new(),
        notice: false,
        search: None,
        opens: Vec::new(),
        shows: Vec::new(),
        takes: Vec::new(),
    }
}

/// The `tilo.actions` package: what a program naming it compiles against. The host registers it
/// beside the packages it runs ([`tilolang_runtime::Core`]).
#[must_use]
pub fn actions_meta() -> PackageMeta {
    PackageMeta {
        app: ACTIONS_APP.to_string(),
        title: "Actions".to_string(),
        types: vec![
            TypeMeta {
                id: ACTION_TYPE.to_string(),
                name: "Offered".to_string(),
                doc: "One action the workspace offers: what it is called, where it is addressed, and what it takes."
                    .to_string(),
                fields: vec![
                    field(
                        "actions-address",
                        "address",
                        "Text",
                        "Where it is called: <app id>/<resource id>#<action>.",
                    ),
                    field(
                        "actions-app",
                        "app",
                        "Text",
                        "The app that publishes it, by its id.",
                    ),
                    field(
                        "actions-app_label",
                        "app_label",
                        "Text",
                        "The app, as a person reads its name.",
                    ),
                    field(
                        "actions-label",
                        "label",
                        "Text",
                        "What the action is called where it is offered.",
                    ),
                    field(
                        "actions-resource",
                        "resource",
                        "Text",
                        "The resource it acts on, as a person reads it.",
                    ),
                    field(
                        "actions-resource_kind",
                        "resource_kind",
                        "Text",
                        "The kind of resource it acts on (`notes.note`), which a step that calls it names.",
                    ),
                    field(
                        "actions-resource_id",
                        "resource_id",
                        "Text",
                        "The resource it was listed on (`notes.note:1`), which a step calls it on.",
                    ),
                    field(
                        "actions-kind",
                        "kind",
                        "enum ActionKind",
                        "Whether it reads or changes something.",
                    ),
                    field(
                        "actions-params",
                        "params",
                        "List<ActionParam>",
                        "What it takes, in the order its API takes it.",
                    ),
                    field(
                        "actions-confirms",
                        "confirms",
                        "Bool",
                        "Whether a person is asked before it runs.",
                    ),
                ],
                ..TypeMeta::default()
            },
            TypeMeta {
                id: PARAM_TYPE.to_string(),
                name: "ActionParam".to_string(),
                doc: "One parameter of an offered action: its name, its type and whether it is required."
                    .to_string(),
                fields: vec![
                    field("actions-param-name", "name", "Text", "Its name."),
                    field(
                        "actions-param-ty",
                        "ty",
                        "Text",
                        "Its type, in the catalogue's words: text, int, bool, a record's kind.",
                    ),
                    field(
                        "actions-param-required",
                        "required",
                        "Bool",
                        "Whether the call must give it.",
                    ),
                ],
                ..TypeMeta::default()
            },
        ],
        enums: vec![EnumMeta {
            id: KIND_ENUM.to_string(),
            name: "ActionKind".to_string(),
            doc: "What an action does when it runs.".to_string(),
            tag: None,
            variants: vec![
                VariantMeta {
                    id: "read".to_string(),
                    name: "read".to_string(),
                    doc: "It reads: nothing of the workspace changes.".to_string(),
                    label: "Reads".to_string(),
                    icon: None,
                    tone: None,
                    fields: Vec::new(),
                },
                VariantMeta {
                    id: "effect".to_string(),
                    name: "effect".to_string(),
                    doc: "It changes something: a record, the workspace, or what is outside."
                        .to_string(),
                    label: "Changes".to_string(),
                    icon: None,
                    tone: None,
                    fields: Vec::new(),
                },
            ],
        }],
        actions: vec![catalogue_action(), call_action()],
        ..PackageMeta::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The package names the two verbs and what they answer, with the types code reads them by.
    #[test]
    fn the_registry_package_declares_its_verbs_its_types_and_their_answers() {
        let meta = actions_meta();
        assert_eq!(meta.app, ACTIONS_APP);
        for ty in ["Offered", "ActionParam"] {
            assert!(
                meta.types.iter().any(|declared| declared.name == ty),
                "{ty}"
            );
        }
        assert!(
            meta.enums
                .iter()
                .any(|declared| declared.name == "ActionKind")
        );
        let catalogue = meta
            .actions
            .iter()
            .find(|action| action.id == CATALOGUE_ACTION)
            .expect("catalogue is declared");
        assert_eq!(catalogue.result.as_deref(), Some("List<Offered>"));
        assert_eq!(catalogue.class, EffectClass::Read);
        let call = meta
            .actions
            .iter()
            .find(|action| action.id == CALL_ACTION)
            .expect("call is declared");
        assert_eq!(
            call.result.as_deref(),
            Some("tilo.invocations.Invocation"),
            "the answer is the workspace's invocation record"
        );
    }
}
