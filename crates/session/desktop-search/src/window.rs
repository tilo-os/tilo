//! One window's own search: what the window holds and what can be done in
//! it, read from the window itself.
//!
//! An app's search (its search control, the window bar's "Search <app>")
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
//! - Actions that act on what the person selected lead the other actions.

use tilo_actions::{ActionSpec, ContextRequirement, FlatAction, Resource};
use ui::search::system::{MAX_PROVIDER_SCORE, ResourceKind};

use crate::model::{ActionTarget, Hit, Source, Target};
use crate::providers::Content;
use crate::rank;

/// The provider id of the window's actions.
pub const ACTIONS: &str = "window-actions";

/// Whether a person may pick `spec` from a palette now: offered to people,
/// available, and not the opening of a search hit (which a hit of the
/// app's own search stands for).
fn offered(spec: &ActionSpec) -> bool {
    spec.reaches_people()
        && spec.contract.availability.is_available()
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
    resources: &'a [Resource],
}

impl Source for Actions<'_> {
    fn id(&self) -> &'static str {
        ACTIONS
    }

    fn query(&self, needle: &str) -> Vec<Hit> {
        let flat: Vec<FlatAction<'_>> = tilo_actions::flatten(self.resources)
            .into_iter()
            .filter(|flat| offered(flat.action) && !mirrors_a_control(&flat.resource_kind))
            .collect();
        // Two items offering the same action read alike: only then is the
        // item named under each.
        let mut rows: std::collections::HashMap<tilo_actions::ListedRow, usize> =
            std::collections::HashMap::new();
        for flat in &flat {
            let count = rows.entry(flat.row()).or_default();
            *count = count.saturating_add(1);
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
                if rows.get(&row).is_some_and(|count| *count > 1) {
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
                    rank::action_match_score(
                        needle,
                        &row.title,
                        &flat.action.keywords,
                        &flat.resource_label,
                    )?
                } else {
                    rank::item_action_match_score(needle, &flat.resource_label)?
                };
                Some(Hit {
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
    resources: Vec<Resource>,
    content: Content,
}

impl WindowSearch {
    /// Search over the `resources` and search `providers` a window
    /// publishes.
    #[must_use]
    pub fn new(
        resources: Vec<Resource>,
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
    fn notebook() -> Vec<Resource> {
        let note = |id: &str, label: &str| {
            Resource::new(id, "notes.page", label).with_action(action("rename", "Rename note"))
        };
        let mut notebook = Resource::new("notes.notebook", "notes.notebook", "Notebook")
            .with_action(open())
            .with_action(action("new-note", "New note").with_icon("file-plus"))
            .with_action(
                action("bold", "Bold").requires(tilo_actions::ContextRequirement::Selection),
            );
        notebook.children = vec![note("page:1", "Groceries"), note("page:2", "Trip plan")];
        // The toolkit's mirror of a segmented control the window shows.
        let control = Resource::new("tilo.control:view-mode", "tilo.control.segmented", "view")
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
