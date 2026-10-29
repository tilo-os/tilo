//! A fixed desktop to search: what the launcher shows in a capture, a test
//! and its own window, where no shell lends it the real one.
//!
//! It answers the way the shell's source does, over a handful of apps,
//! windows, files and actions: the launcher's sections before anything is
//! typed, results grouped by kind as the person types, the filter chips
//! for the kinds found, an action's arguments asked one step at a time, a
//! file's other actions, a value stepped in place, and one scope widened
//! to the next.

use ui::icons::Icon;

use crate::source::{
    ActivationModifier, Answer, ArgumentSpec, Ask, CommandEntry, LauncherSource, ResourceKind,
    Search, SearchScope,
};

/// Row id of a value an argument step offers.
const CHOICE: &str = "choice:";
/// Row id of one of a row's other actions.
const OTHER: &str = "other:";

/// One row of the fixed desktop.
struct Row {
    id: &'static str,
    kind: ResourceKind,
    title: &'static str,
    detail: &'static str,
    icon: Icon,
    /// The launcher section it shows in before anything is typed.
    section: Option<&'static str>,
    /// What choosing it asks first.
    asks: Asks,
}

#[derive(Clone, Copy, PartialEq)]
enum Asks {
    Nothing,
    /// A field of this prompt.
    Field(&'static str),
    /// One of these values.
    Choices(&'static [&'static str]),
}

#[allow(
    clippy::too_many_lines,
    reason = "one row per line of the fixed desktop, read top to bottom"
)]
fn rows() -> Vec<Row> {
    use ResourceKind::{Action, Application, File, Folder, Tab};
    let row = |id, kind, title, detail, icon, section| Row {
        id,
        kind,
        title,
        detail,
        icon,
        section,
        asks: Asks::Nothing,
    };
    vec![
        row(
            "window:design-review",
            Tab,
            "Design review",
            "Maya and Theo are here",
            Icon::Users,
            Some("Happening now"),
        ),
        row(
            "file:roadmap",
            File,
            "Roadmap 2027.md",
            "~/Documents/Planning",
            Icon::FileText,
            Some("Continue"),
        ),
        row(
            "app:notes",
            Application,
            "Notes",
            "Write and organise notes",
            Icon::FileText,
            Some("Apps"),
        ),
        row(
            "app:calendar",
            Application,
            "Calendar",
            "Your days and meetings",
            Icon::Calendar,
            Some("Apps"),
        ),
        row(
            "app:files",
            Application,
            "Files",
            "Browse your documents",
            Icon::Folder,
            Some("Apps"),
        ),
        row(
            "app:terminal",
            Application,
            "Terminal",
            "A shell on this device",
            Icon::Terminal,
            Some("Apps"),
        ),
        row(
            "app:music",
            Application,
            "Music",
            "Play your library",
            Icon::Music,
            None,
        ),
        row(
            "window:notes-standup",
            Tab,
            "Standup notes",
            "Open in this workspace",
            Icon::FileText,
            None,
        ),
        row(
            "file:notes-q3",
            File,
            "Q3 notes.pdf",
            "~/Documents/Reports",
            Icon::FileText,
            None,
        ),
        row(
            "folder:notes",
            Folder,
            "Notes",
            "~/Documents",
            Icon::Folder,
            None,
        ),
        Row {
            asks: Asks::Field("Name the new note"),
            ..row(
                "action:new-note",
                Action,
                "New note",
                "Notes",
                Icon::Plus,
                None,
            )
        },
        Row {
            asks: Asks::Choices(&["Maya", "Theo", "Sam"]),
            ..row(
                "action:share-notes",
                Action,
                "Share notes with…",
                "Notes",
                Icon::Users,
                None,
            )
        },
        row(
            "action:volume",
            Action,
            "Volume",
            "40%",
            Icon::Volume2,
            None,
        ),
    ]
}

/// The kinds in the order the results list them.
fn kind_rank(kind: &ResourceKind) -> usize {
    match kind {
        ResourceKind::Application => 0,
        ResourceKind::Tab => 1,
        ResourceKind::Action => 2,
        ResourceKind::File => 3,
        ResourceKind::Folder => 4,
        _ => 5,
    }
}

/// A step collecting an argument: the query the results showed, given back
/// when the step is left.
struct Steps {
    query: String,
}

/// The fixed desktop, answering as the shell's source does.
pub struct FixtureSource {
    search: Search,
    scope: SearchScope,
    volume: u8,
    steps: Option<Steps>,
    /// The row whose other actions are listed, and the query to give back.
    others: Option<(&'static str, String)>,
    field_only: bool,
}

impl Default for FixtureSource {
    fn default() -> Self {
        Self::new()
    }
}

impl FixtureSource {
    pub fn new() -> Self {
        Self {
            search: Search::default(),
            scope: SearchScope::Workspace,
            volume: 40,
            steps: None,
            others: None,
            field_only: false,
        }
    }

    /// The field alone until something is typed, as a held Super shows it.
    #[must_use]
    pub fn field_only(mut self) -> Self {
        self.field_only = true;
        self
    }

    fn entry(&self, row: &Row) -> CommandEntry {
        let detail = if row.id == "action:volume" {
            format!("{}%", self.volume)
        } else {
            row.detail.to_owned()
        };
        let mut entry = CommandEntry::new(row.id, row.title)
            .icon(row.icon)
            .description(detail);
        if matches!(row.kind, ResourceKind::File | ResourceKind::Folder) {
            entry = entry.path_description();
        }
        if row.id == "action:volume" {
            entry = entry.adjustable();
        }
        if matches!(row.kind, ResourceKind::Application | ResourceKind::File) {
            entry = entry
                .alternate(ActivationModifier::Shift, "Open as a tab here")
                .alternate(ActivationModifier::Ctrl, "Open beside");
        }
        if row.kind == ResourceKind::File {
            entry = entry.alternate(ActivationModifier::Alt, "More actions");
        }
        entry
    }

    fn scoped(&self, row: &Row) -> bool {
        match self.scope {
            // Notes is the app in front: its own windows and actions.
            SearchScope::ActiveApp => {
                row.detail.starts_with("Notes") || row.id.starts_with("action:new-note")
            }
            SearchScope::Workspace | SearchScope::All => true,
        }
    }

    fn results(&self, answer: &mut Answer) {
        let query = self.search.query.trim().to_lowercase();
        let rows = rows();
        answer.narrows = false;
        answer.field = None;
        answer.status = None;
        answer.field_only = self.field_only && query.is_empty();
        answer.placeholder = match self.scope {
            SearchScope::ActiveApp => "Search Notes…".into(),
            _ => "Search apps, windows, files and actions…".into(),
        };
        answer.scope = self.scope;
        answer.scope_token = match self.scope {
            SearchScope::ActiveApp => Some("Notes".into()),
            SearchScope::Workspace => Some("This workspace".into()),
            SearchScope::All => None,
        };
        answer.scope_token_action =
            (self.scope == SearchScope::Workspace).then(|| "Search every workspace".into());
        if query.is_empty() && self.search.kinds.is_empty() && self.scope != SearchScope::ActiveApp
        {
            answer.rows = rows
                .iter()
                .filter_map(|row| {
                    let section = row.section?;
                    let mut entry = self.entry(row).group(section);
                    entry.section = Some(section.to_owned());
                    Some(entry)
                })
                .collect();
            let mut all = CommandEntry::new("all-apps", "All apps")
                .icon(Icon::LayoutGrid)
                .group("Apps");
            all.section = Some("Apps".into());
            answer.rows.push(all);
            answer.kinds.clear();
            answer.band = false;
            return;
        }
        let mut found: Vec<&Row> = rows
            .iter()
            .filter(|row| self.scoped(row))
            .filter(|row| {
                query.is_empty()
                    || row.title.to_lowercase().contains(&query)
                    || row.detail.to_lowercase().contains(&query)
            })
            .collect();
        found.sort_by_key(|row| kind_rank(&row.kind));
        let mut kinds: Vec<(ResourceKind, usize)> = Vec::new();
        for row in &found {
            match kinds.iter_mut().find(|(kind, _)| *kind == row.kind) {
                Some((_, count)) => *count = count.saturating_add(1),
                None => kinds.push((row.kind.clone(), 1)),
            }
        }
        kinds.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        answer.kinds = kinds
            .into_iter()
            .map(|(kind, _)| {
                let label = kind.label().to_owned();
                (kind, label)
            })
            .collect();
        for kind in &self.search.kinds {
            if !answer.kinds.iter().any(|(shown, _)| shown == kind) {
                answer.kinds.push((kind.clone(), kind.label().to_owned()));
            }
        }
        answer.band =
            !self.search.kinds.is_empty() || (!query.is_empty() && answer.kinds.len() > 1);
        answer.rows = found
            .into_iter()
            .filter(|row| self.search.kinds.is_empty() || self.search.kinds.contains(&row.kind))
            .map(|row| self.entry(row).group(row.kind.label()))
            .collect();
    }

    fn row(id: &str) -> Option<Row> {
        rows().into_iter().find(|row| row.id == id)
    }

    fn step(&mut self, row: &Row, answer: &mut Answer) {
        answer.band = false;
        answer.status = None;
        match row.asks {
            Asks::Field(prompt) => {
                answer.rows.clear();
                answer.narrows = false;
                answer.field = Some((
                    format!("param:{}", row.id),
                    ArgumentSpec {
                        prompt: prompt.into(),
                        placeholder: "Title".into(),
                        prefill: None,
                        ..ArgumentSpec::default()
                    },
                ));
            }
            Asks::Choices(values) => {
                answer.field = None;
                answer.narrows = true;
                answer.query = Some(String::new());
                answer.placeholder = "Who to share with".into();
                answer.rows = values
                    .iter()
                    .enumerate()
                    .map(|(at, name)| {
                        CommandEntry::new(format!("{CHOICE}{at}"), *name)
                            .group(row.title)
                            .avatar(*name, name.to_lowercase())
                    })
                    .collect();
            }
            Asks::Nothing => {}
        }
        self.steps = Some(Steps {
            query: std::mem::take(&mut self.search.query),
        });
    }

    fn others(&mut self, row: &Row, answer: &mut Answer) {
        answer.band = false;
        answer.narrows = true;
        answer.field = None;
        answer.query = Some(String::new());
        answer.placeholder = row.title.into();
        answer.rows = [
            "Open as a tab here",
            "Open beside",
            "Show in folder",
            "Copy path",
        ]
        .into_iter()
        .enumerate()
        .map(|(at, label)| CommandEntry::new(format!("{OTHER}{at}"), label).group(row.title))
        .collect();
        self.others = Some((row.id, std::mem::take(&mut self.search.query)));
    }

    fn back(&mut self, answer: &mut Answer) {
        let query = match (self.steps.take(), self.others.take()) {
            (Some(steps), _) => steps.query,
            (None, Some((_, query))) => query,
            (None, None) => return,
        };
        self.search.query.clone_from(&query);
        answer.query = Some(query);
        self.results(answer);
    }
}

impl LauncherSource for FixtureSource {
    fn answer(&mut self, ask: Ask, answer: &mut Answer) {
        match ask {
            Ask::Open { scope, query } => {
                self.scope = scope;
                self.steps = None;
                self.others = None;
                self.search = Search {
                    query: query.unwrap_or_default(),
                    kinds: Vec::new(),
                };
                answer.query = Some(self.search.query.clone());
                self.results(answer);
            }
            Ask::Search(search) => {
                if self.steps.is_some() || self.others.is_some() {
                    return;
                }
                self.search = search;
                self.results(answer);
            }
            Ask::Widen => {
                self.scope = match self.scope {
                    SearchScope::ActiveApp => SearchScope::Workspace,
                    SearchScope::Workspace | SearchScope::All => SearchScope::All,
                };
                self.results(answer);
            }
            Ask::Choose { row, modifier } => {
                if row == "all-apps" {
                    self.search.kinds = vec![ResourceKind::Application];
                    self.results(answer);
                    return;
                }
                if row.starts_with(CHOICE) || row.starts_with(OTHER) {
                    self.steps = None;
                    self.others = None;
                    answer.closed = true;
                    return;
                }
                let Some(found) = Self::row(&row) else {
                    return;
                };
                if modifier == Some(ActivationModifier::Alt) && found.kind == ResourceKind::File {
                    self.others(&found, answer);
                } else if found.asks == Asks::Nothing {
                    answer.closed = true;
                } else {
                    self.step(&found, answer);
                }
            }
            Ask::Argument(text) => {
                if text.trim().is_empty() {
                    answer.field = answer.field.take().map(|(id, mut spec)| {
                        spec.prompt = "A note needs a name".into();
                        (id, spec)
                    });
                    return;
                }
                self.steps = None;
                answer.closed = true;
            }
            Ask::Adjust { row, delta } => {
                if row == "action:volume" {
                    self.volume = if delta > 0 {
                        self.volume.saturating_add(5).min(100)
                    } else {
                        self.volume.saturating_sub(5)
                    };
                    self.results(answer);
                }
            }
            Ask::Back => self.back(answer),
            // The surface keeps the person's choice itself.
            Ask::Select { .. } => {}
            Ask::Dismissed => {
                self.steps = None;
                self.others = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answered(source: &mut FixtureSource, ask: Ask) -> Answer {
        let mut answer = Answer::default();
        source.answer(ask, &mut answer);
        answer
    }

    fn search(query: &str) -> Ask {
        Ask::Search(Search {
            query: query.into(),
            kinds: Vec::new(),
        })
    }

    #[test]
    fn before_anything_is_typed_the_launcher_shows_its_sections() {
        let mut source = FixtureSource::new();
        let answer = answered(
            &mut source,
            Ask::Open {
                scope: SearchScope::Workspace,
                query: None,
            },
        );
        let sections: Vec<_> = answer
            .rows
            .iter()
            .filter_map(|row| row.section.as_deref())
            .collect();
        assert_eq!(sections.first(), Some(&"Happening now"));
        assert_eq!(
            answer.rows.last().map(|row| row.id.as_str()),
            Some("all-apps")
        );
        assert!(!answer.band, "no chips before a search");
    }

    #[test]
    fn a_search_groups_its_hits_and_offers_their_kinds() {
        let mut source = FixtureSource::new();
        let answer = answered(&mut source, search("notes"));
        assert!(answer.rows.len() > 3);
        assert!(answer.band);
        assert!(answer.kinds.len() > 1);
        let narrowed = answered(
            &mut source,
            Ask::Search(Search {
                query: "notes".into(),
                kinds: vec![ResourceKind::File],
            }),
        );
        assert!(narrowed.rows.iter().all(|row| row.id.starts_with("file:")));
    }

    #[test]
    fn an_action_asks_for_its_argument_and_back_gives_the_query_back() {
        let mut source = FixtureSource::new();
        answered(&mut source, search("new"));
        let step = answered(
            &mut source,
            Ask::Choose {
                row: "action:new-note".into(),
                modifier: None,
            },
        );
        assert!(step.field.is_some());
        let back = answered(&mut source, Ask::Back);
        assert_eq!(back.query.as_deref(), Some("new"));
        assert!(back.field.is_none());
    }
}
