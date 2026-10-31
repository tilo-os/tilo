//! One window's own search: what the window holds and what can be done in
//! it, read from the window itself.
//!
//! An app's search (its search control, the window bar's "Search `<app>`")
//! is about that window: the resources it publishes, what its own search
//! providers find, and its actions. [`WindowSearch`] answers it from what
//! the window publishes now ([`ui::widget::published_resources`] and
//! [`ui::widget::published_search_providers`]): the same catalogue its own
//! controls, agents and remote peers invoke, never a copy kept by the
//! desktop. Every desktop that hosts windows (the native shell, the
//! portable desktop surface) shows an app's search through this.
//!
//! - What the window holds is what its own search providers list, as the
//!   app lists it (Files' places with where they are, a notebook's notes):
//!   everything they list unasked before anything is typed, their hits once
//!   it is. The resource tree itself names records and their parts for
//!   agents, often twice over, so it is not listed row by row.
//! - Then what can be done in the window, each action read by its short
//!   label; an item's actions answer only to the item's name.
//! - Only what a person can take now is listed, as each action's own
//!   declaration says: offered to people (its audience), available in the
//!   window's state (an app marks a toggle's other side, or an action on an
//!   empty selection, unavailable), and asking no more than a row can
//!   collect. An action on the selection names it in its label.
//! - Actions that act on what the person selected lead the other actions.

use tilo_actions::{ActionSpec, ContextRequirement, FlatAction, ResourceSpec};
use ui::search::system::{MAX_PROVIDER_SCORE, ResourceKind};

use crate::model::{ActionTarget, Hit, Source, Target};
use crate::providers::Content;
use crate::rank;

/// The provider id of the window's actions.
pub const ACTIONS: &str = "window-actions";

/// Whether a person may pick `spec` from a palette now: offered to people,
/// available, asking at most what a row can collect, and not the opening of
/// a search hit (which a hit of the app's own search stands for).
fn offered(spec: &ActionSpec) -> bool {
    spec.reaches_people()
        && spec.contract.availability.is_available()
        && spec.takes_a_row_answer()
        && spec.operation_type.as_deref() != Some(tilo_actions::remote::APPLICATION_OPEN_TYPE)
}

/// The picture `spec` declares for itself, when the toolkit draws it.
#[must_use]
pub fn action_icon(spec: &ActionSpec) -> Option<ui::icons::Icon> {
    spec.icon.as_deref().and_then(ui::icons::Icon::from_name)
}

/// Whether resources of `kind` are the toolkit's mirrors of controls the
/// window already shows (its buttons, segmented controls, the host's search
/// field), published so agents and assistive technology can press them:
/// what the window's search lists is what the app does, not its controls.
fn mirrors_a_control(kind: &str) -> bool {
    kind == "tilo.control"
        || ["tilo.control.", "tilo.ui.", "tilo.host."]
            .iter()
            .any(|prefix| kind.starts_with(prefix))
}

/// Whether `spec` acts on what the person selected: its contract declares
/// the selection as its precondition ([`ContextRequirement::Selection`]).
#[must_use]
pub fn acts_on_selection(spec: &ActionSpec) -> bool {
    spec.contract
        .requirements()
        .contains(&ContextRequirement::Selection)
}

/// What can be done in the window, each action once, read as a person reads
/// it ([`tilo_actions::listed_row`]): its short label, and the item it acts
/// on under it only where the label alone would not say which. Before
/// anything is typed every action is listed, those acting on the selection
/// first.
struct Actions<'a> {
    resources: &'a [ResourceSpec],
}

impl Source for Actions<'_> {
    fn id(&self) -> &'static str {
        ACTIONS
    }

    fn query(&self, needle: &str) -> Vec<Hit> {
        let flat: Vec<FlatAction<'_>> = tilo_actions::flatten(self.resources)
            .into_iter()
            .filter(|flat| offered(flat.action) && !mirrors_a_control(&flat.resource_kind))
            // An item's own view is what its row among what the window
            // holds opens: it is not listed again as something to do.
            .filter(|flat| flat.depth == 0 || flat.action.kind != tilo_actions::OperationKind::View)
            .collect();
        // Two items offering the same action read alike: only then is the
        // item named under each. The same label on one item names nothing.
        let mut items: std::collections::HashMap<tilo_actions::ListedRow, Vec<&str>> =
            std::collections::HashMap::new();
        for flat in &flat {
            let named = items.entry(flat.row()).or_default();
            if !named.contains(&flat.resource_label.as_str()) {
                named.push(&flat.resource_label);
            }
        }
        let browsing = needle.trim().is_empty();
        let count = i32::try_from(flat.len()).unwrap_or(i32::MAX);
        let mut seen = std::collections::HashSet::new();
        flat.iter()
            .zip(0_i32..)
            .filter_map(|(flat, at)| {
                if !seen.insert(flat.key.clone()) {
                    return None;
                }
                let mut row = flat.row();
                if items.get(&row).is_some_and(|named| named.len() > 1) {
                    row = tilo_actions::listed_row(&flat.action.label, Some(&flat.resource_label));
                }
                let score = if browsing {
                    // Unasked, an item's actions would be listed once per
                    // item: the window's own come, the items' answer to
                    // their names.
                    if flat.depth > 0 {
                        return None;
                    }
                    MAX_PROVIDER_SCORE.saturating_sub(at.min(count))
                } else if flat.depth == 0 {
                    // The window's own actions: the window is what was
                    // searched, so its name finds none of them.
                    rank::action_match_score(needle, &row.title, &flat.action.keywords, "")?
                } else {
                    rank::item_action_match_score(needle, &flat.resource_label)?
                };
                Some(Hit {
                    faces: Vec::new(),
                    outcome: None,
                    kind: ResourceKind::Action,
                    provider: ACTIONS.into(),
                    id: flat.key.clone(),
                    title: row.title,
                    subtitle: row.subtitle,
                    facts: Vec::new(),
                    score: score.clamp(0, MAX_PROVIDER_SCORE),
                    icon: action_icon(flat.action),
                    target: Target::Action(Box::new(ActionTarget {
                        resource: flat.resource_id.clone(),
                        resource_kind: flat.resource_kind.clone(),
                        resource_label: flat.resource_label.clone(),
                        spec: flat.action.clone(),
                        preset: None,
                    })),
                })
            })
            .collect()
    }
}

/// Order a group of action hits so those acting on the selection lead the
/// others, each part keeping its order: the person's selection is what an
/// app's search most likely asks about.
pub fn selection_first(hits: &mut [Hit]) {
    hits.sort_by_key(|hit| match &hit.target {
        Target::Action(target) => !acts_on_selection(&target.spec),
        _ => true,
    });
}

/// One window's search, over what it publishes now.
pub struct WindowSearch {
    resources: Vec<ResourceSpec>,
    content: Content,
}

impl WindowSearch {
    /// Search over the `resources` and search `providers` a window
    /// publishes.
    #[must_use]
    pub fn new(
        resources: Vec<ResourceSpec>,
        providers: Vec<Box<dyn ui::search::SearchProvider>>,
    ) -> Self {
        Self {
            content: Content::new(providers, resources.clone()).browsing(),
            resources,
        }
    }

    /// Narrow every query to `scope`, one part of the app (a
    /// [`ui::search::ScopeDecl`] anchor).
    #[must_use]
    pub fn within(mut self, scope: Option<String>) -> Self {
        self.content = self.content.within(scope);
        self
    }

    /// Ask the providers whose answers were still coming; whether one
    /// arrived, so the query should run again.
    pub fn poll(&self) -> bool {
        self.content.poll()
    }

    /// Whether an answer is still coming.
    pub fn waiting(&self) -> bool {
        self.content.waiting()
    }

    /// Whether the window publishes anything to search.
    pub fn is_empty(&self) -> bool {
        self.resources.is_empty()
    }
}

impl Source for WindowSearch {
    fn id(&self) -> &'static str {
        "window"
    }

    /// What the window holds, what its providers find, and what can be
    /// done in it, each hit under its own provider.
    fn query(&self, needle: &str) -> Vec<Hit> {
        let mut hits = self.content.query(needle);
        hits.extend(
            Actions {
                resources: &self.resources,
            }
            .query(needle),
        );
        hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::{ActionSpec, EncodedAction, ExecutionLocus, OperationKind};

    fn action(id: &str, label: &str) -> ActionSpec {
        ActionSpec::immediate_encoded(
            id,
            label,
            ExecutionLocus::ClientLocal,
            EncodedAction {
                type_id: "test.action".into(),
                payload: Vec::new(),
            },
        )
    }

    fn open() -> ActionSpec {
        ActionSpec::typed_operation(
            "open",
            "Open",
            "Item to open",
            OperationKind::View,
            tilo_actions::remote::APPLICATION_OPEN_TYPE,
            vec![tilo_actions::ActionParam::required(
                "anchor",
                tilo_actions::ParamType::Text,
                "Item to open",
            )],
            |_| EncodedAction {
                type_id: tilo_actions::remote::APPLICATION_OPEN_TYPE.into(),
                payload: Vec::new(),
            },
        )
    }

    /// A notebook's own search, as the framework publishes it: its notes,
    /// every one unasked, those naming the text once it is typed, each
    /// opened at its anchor.
    struct Notes;

    const NOTES: [(&str, &str, &str); 2] = [
        ("page:1", "Groceries", "Edited today"),
        ("page:2", "Trip plan", "Edited yesterday"),
    ];

    impl ui::search::SearchProvider for Notes {
        fn name(&self) -> &'static str {
            "Notes"
        }
        fn search(&self, query: &str) -> Vec<ui::search::SearchResult> {
            let typed = query.trim().to_lowercase();
            NOTES
                .iter()
                .filter(|(_, title, _)| title.to_lowercase().contains(&typed))
                .map(|(anchor, title, detail)| {
                    ui::search::SearchResult::new("Notes", *title, *anchor).with_subtitle(*detail)
                })
                .collect()
        }
        fn open_action(&self) -> Option<tilo_actions::ActionAddress> {
            Some(tilo_actions::ActionAddress::new("notes.notebook", "open"))
        }
    }

    /// A notebook window: the notebook, two notes inside it, each note with
    /// an action of its own, and the notebook's own actions, one acting on
    /// the selection.
    fn notebook() -> Vec<ResourceSpec> {
        let note = |id: &str, label: &str| {
            ResourceSpec::new(id, "notes.page", label)
                .with_action(action("rename", "Rename note"))
                .with_action({
                    let mut open = action("open-note", "Open note");
                    open.type_mut().kind = OperationKind::View;
                    open
                })
        };
        let mut notebook = ResourceSpec::new("notes.notebook", "notes.notebook", "Notebook")
            .with_action(open())
            .with_action(action("new-note", "New note").with_icon("file-plus"))
            .with_action(
                action("bold", "Bold").requires(tilo_actions::ContextRequirement::Selection),
            );
        notebook.children = vec![note("page:1", "Groceries"), note("page:2", "Trip plan")];
        // The toolkit's mirror of a segmented control the window shows.
        let control = ResourceSpec::new("tilo.control:view-mode", "tilo.control.segmented", "view")
            .with_action(action("select", "Select"));
        vec![notebook, control]
    }

    fn search(needle: &str) -> Vec<Hit> {
        WindowSearch::new(notebook(), vec![Box::new(Notes)]).query(needle)
    }

    #[test]
    fn unasked_a_window_lists_what_it_holds_then_what_it_can_do() {
        let hits = search("");
        let rows: Vec<(&ResourceKind, &str, Option<&str>)> = hits
            .iter()
            .map(|hit| (&hit.kind, hit.title.as_str(), hit.subtitle.as_deref()))
            .collect();
        assert_eq!(
            rows,
            [
                (&ResourceKind::Note, "Groceries", Some("Edited today")),
                (&ResourceKind::Note, "Trip plan", Some("Edited yesterday")),
                (&ResourceKind::Action, "New note", None),
                (&ResourceKind::Action, "Bold", None),
            ],
            "the notes as the app lists them, then the notebook's own actions; \
             never the opening of a hit, an action once per note, nor a control's mirror"
        );
    }

    #[test]
    fn what_it_holds_opens_through_the_windows_open_at_it() {
        let hits = search("groc");
        let Some(Target::Action(target)) = hits
            .iter()
            .find(|hit| hit.title == "Groceries" && hit.kind == ResourceKind::Note)
            .map(|hit| &hit.target)
        else {
            panic!("the note is found by its name: {hits:?}");
        };
        assert_eq!(
            target.address(),
            tilo_actions::ActionAddress::new("notes.notebook", "open")
        );
        assert_eq!(target.preset, Some(("anchor".into(), "page:1".into())));
    }

    #[test]
    fn an_items_action_answers_to_the_item_and_names_it_only_where_needed() {
        let hits = search("trip");
        let actions: Vec<(&str, Option<&str>)> = hits
            .iter()
            .filter(|hit| hit.kind == ResourceKind::Action)
            .map(|hit| (hit.title.as_str(), hit.subtitle.as_deref()))
            .collect();
        assert_eq!(actions, [("Rename note", Some("Trip plan"))]);
    }

    #[test]
    fn an_action_wears_its_own_icon() {
        let hits = search("");
        let icon = |title: &str| {
            hits.iter()
                .find(|hit| hit.title == title)
                .map(|hit| hit.icon)
        };
        assert_eq!(icon("New note"), Some(Some(ui::icons::Icon::FilePlus)));
        assert_eq!(icon("Bold"), Some(None), "one without wears its app's");
    }

    #[test]
    fn actions_on_the_selection_lead_the_others() {
        let mut actions: Vec<Hit> = search("")
            .into_iter()
            .filter(|hit| hit.kind == ResourceKind::Action)
            .collect();
        selection_first(&mut actions);
        let titles: Vec<&str> = actions.iter().map(|hit| hit.title.as_str()).collect();
        assert_eq!(titles, ["Bold", "New note"]);
    }

    #[test]
    fn the_windows_own_name_finds_none_of_its_actions() {
        let hits = search("notebook");
        assert!(
            !hits.iter().any(|hit| hit.kind == ResourceKind::Action),
            "every action of the window is the notebook's: {hits:?}"
        );
    }

    fn unavailable(mut spec: ActionSpec, code: &str) -> ActionSpec {
        spec.type_mut().contract.availability = tilo_actions::Availability::Unavailable {
            code: code.into(),
            reason: code.into(),
        };
        spec
    }

    fn typed(id: &str, label: &str, params: Vec<tilo_actions::ActionParam>) -> ActionSpec {
        ActionSpec::typed(
            id,
            label,
            label,
            ExecutionLocus::ClientLocal,
            params,
            |_| EncodedAction {
                type_id: "test.action".into(),
                payload: Vec::new(),
            },
        )
    }

    fn text(name: &'static str) -> tilo_actions::ActionParam {
        tilo_actions::ActionParam::required(name, tilo_actions::ParamType::Text, "")
    }

    /// A file manager's window as it stands: a folder shown that is not a
    /// favourite, one file selected, and the actions it publishes, some of
    /// which cannot run now or are not for a person.
    fn folder(selected: Option<&str>) -> Vec<ResourceSpec> {
        let selection = |id: &str, label: String| {
            let spec = action(id, &label).requires(tilo_actions::ContextRequirement::Selection);
            match selected {
                Some(_) => spec,
                None => unavailable(spec, "selection_empty"),
            }
        };
        let name = selected.unwrap_or_default();
        vec![
            ResourceSpec::new("files.folder", "files.folder", "Files")
                .with_action(action("refresh", "Refresh folder"))
                // A toggle pair: the folder is no favourite, so only adding
                // applies.
                .with_action(action("add-favorite", "Add to Favorites"))
                .with_action(unavailable(
                    action("remove-favorite", "Remove from Favorites"),
                    "not_a_favorite",
                ))
                .with_action(selection(
                    "rename-selected",
                    format!("Rename \u{201c}{name}\u{201d}"),
                ))
                .with_action(selection(
                    "trash-selected",
                    format!("Move \u{201c}{name}\u{201d} to Trash"),
                ))
                // What agents and scripts call, naming every argument.
                .with_action(
                    typed(
                        "rename",
                        "Rename",
                        vec![text("folder"), text("entry"), text("name")],
                    )
                    .for_audience(tilo_actions::Audience::Agents),
                )
                .with_action(
                    action("replace-range", "Replace range")
                        .for_audience(tilo_actions::Audience::Agents),
                )
                // Asks what a row can collect: one line.
                .with_action(typed("new-folder", "New folder", vec![text("name")]))
                // Asks more than a row can collect, though offered to people.
                .with_action(typed(
                    "copy-into",
                    "Copy entries to folder",
                    vec![text("entries"), text("to")],
                ))
                .with_action(typed(
                    "select-entries",
                    "Select entries",
                    vec![tilo_actions::ActionParam::required(
                        "entries",
                        tilo_actions::ParamType::Array,
                        "",
                    )],
                )),
        ]
    }

    fn action_titles(resources: Vec<ResourceSpec>, needle: &str) -> Vec<String> {
        let mut hits: Vec<Hit> = WindowSearch::new(resources, Vec::new())
            .query(needle)
            .into_iter()
            .filter(|hit| hit.kind == ResourceKind::Action)
            .collect();
        selection_first(&mut hits);
        hits.into_iter().map(|hit| hit.title).collect()
    }

    #[test]
    fn only_what_can_run_now_is_listed() {
        assert_eq!(
            action_titles(folder(None), ""),
            ["Refresh folder", "Add to Favorites", "New folder"],
            "with nothing selected: no action on the selection, one side of the \
             favourite toggle, nothing asking more than one line, nothing for agents"
        );
        for needle in ["remove", "favorites", "rename", "trash", "copy", "select"] {
            let titles = action_titles(folder(None), needle);
            assert!(
                titles.iter().all(|title| title == "Add to Favorites"),
                "{needle:?} finds nothing that cannot run now: {titles:?}"
            );
        }
    }

    #[test]
    fn actions_on_the_selection_name_it_and_lead() {
        assert_eq!(
            action_titles(folder(Some("report.pdf")), ""),
            [
                "Rename \u{201c}report.pdf\u{201d}",
                "Move \u{201c}report.pdf\u{201d} to Trash",
                "Refresh folder",
                "Add to Favorites",
                "New folder",
            ]
        );
    }

    #[test]
    fn agent_only_actions_never_reach_a_persons_search() {
        for needle in ["", "rename", "replace", "range", "files"] {
            let hits = WindowSearch::new(folder(Some("report.pdf")), Vec::new()).query(needle);
            for hit in &hits {
                let Target::Action(target) = &hit.target else {
                    continue;
                };
                assert!(
                    target.spec.reaches_people(),
                    "{needle:?} lists {:?}, which is for agents",
                    hit.title
                );
            }
        }
        // On an item too: an item's action for agents does not answer to
        // the item's name.
        let mut notebook = notebook();
        notebook[0].children[0] = notebook[0].children[0].clone().with_action(
            action("read-body", "Read note body").for_audience(tilo_actions::Audience::Agents),
        );
        let titles: Vec<String> = WindowSearch::new(notebook, vec![Box::new(Notes)])
            .query("groceries")
            .into_iter()
            .filter(|hit| hit.kind == ResourceKind::Action)
            .map(|hit| hit.title)
            .collect();
        assert_eq!(titles, ["Rename note"]);
    }

    /// A notebook whose own search lists its notes in the order the app
    /// keeps them, which is not their alphabetical order.
    struct Arranged;

    impl ui::search::SearchProvider for Arranged {
        fn name(&self) -> &'static str {
            "Notes"
        }
        fn search(&self, query: &str) -> Vec<ui::search::SearchResult> {
            let mut found: Vec<ui::search::SearchResult> = Notes.search(query);
            found.reverse();
            found
        }
        fn open_action(&self) -> Option<tilo_actions::ActionAddress> {
            Some(tilo_actions::ActionAddress::new("notes.notebook", "open"))
        }
    }

    #[test]
    fn what_the_window_holds_keeps_the_apps_order() {
        let titles: Vec<String> = WindowSearch::new(notebook(), vec![Box::new(Arranged)])
            .query("")
            .into_iter()
            .filter(|hit| hit.kind == ResourceKind::Note)
            .map(|hit| hit.title)
            .collect();
        assert_eq!(
            titles,
            ["Trip plan", "Groceries"],
            "as the app arranged them"
        );
    }

    #[test]
    fn rows_never_read_as_ids() {
        for needle in ["", "n", "page", "notes"] {
            for hit in search(needle) {
                let row = tilo_actions::ListedRow {
                    title: hit.title.clone(),
                    subtitle: hit.subtitle.clone(),
                };
                assert!(
                    tilo_actions::listed_row_problems(&row).is_empty()
                        && !hit.title.contains(':')
                        && !hit.title.contains('.'),
                    "{needle:?}: {row:?}"
                );
            }
        }
    }
}
