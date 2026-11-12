//! Records held by the workspaces on this device, found by name.
//!
//! A record this workspace holds opens in its app when its row is chosen,
//! as a link to it opens it ([`crate::record_launch`]): the window already
//! on it comes forward, else its app opens on it.
//!
//! A workspace starts empty. Moving a record into it costs one step: type
//! the record's name, the row says which workspace it lives in, and Enter
//! **brings it here**.
//!
//! Bringing is referencing, not copying: the row's address names the
//! record's id and kind, and what it runs adds this workspace's reference
//! to that record in the workspace's record index — no bytes move and no id
//! is minted. Copying a record is a different, deliberate verb
//! (`ui::record::index::duplicate`).
//!
//! The rows come from the workspaces' record indexes: the source only ranks
//! the list a host collected, so it never reads a workspace's state itself.

use ui::record::{RecordCheckout, RecordId, RecordKind};
use ui::search::system::{MAX_PROVIDER_SCORE, ResourceKind};

use crate::model::{DESKTOP_APP, Hit, InvokeTarget, LaunchPlace, Source, Target, record_launch};

/// The resource the session answers for the records it holds, and the
/// action in it naming `record` as held by the workspace it runs in.
pub const RECORDS_RESOURCE: &str = "desktop:records";
pub const BRING_HERE: &str = "bring-here";
/// The parameters carrying what is brought here: the record's id, its kind
/// and the app whose record index names it.
pub const RECORD_PARAM: &str = "record";
pub const KIND_PARAM: &str = "kind";
pub const PACKAGE_PARAM: &str = "package";

/// Rows one query contributes; a long tail here only costs memory, not the
/// palette's shared row budget.
const MAX_RESULTS: usize = 20;
/// Records the empty palette lists: the few of the other workspaces this
/// device holds, each one Enter away from here. The rest are a typed name
/// away, so the list stays short enough to read unasked.
const LISTED: usize = 6;

/// A record a workspace holds, as that workspace's record index names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeldRecord {
    /// The record's identity, as the collaboration wire carries it.
    pub id: String,
    /// What kind of record it is; empty when the index does not say.
    pub kind: String,
    /// What the index says people call it (a PDF's file name), when its app
    /// names it: a minted id names nothing a person would type.
    pub name: Option<String>,
    /// The app whose record index names it: what the reference is written
    /// into when the record is brought here.
    pub package: String,
    /// Whether the workspace search was opened in holds it.
    pub here: bool,
    /// The name of the workspace holding it, when that is another one; a
    /// record of a workspace this device no longer shows has no name.
    pub workspace: Option<String>,
}

impl HeldRecord {
    /// The record, as its index names it.
    #[must_use]
    pub fn checkout(&self) -> Option<RecordCheckout> {
        Some(RecordCheckout::new(
            RecordId::parse(&self.id)?,
            RecordKind::new(self.kind.clone()),
        ))
    }

    /// What people call the record: the name its index gives it, else what
    /// its identity says. That names a file when it is one: a file record's
    /// leaf is its name, and a minted id (`uuid:…`) has none, so its kind
    /// stands for it.
    #[must_use]
    pub fn title(&self) -> String {
        if let Some(name) = self.name.as_deref().map(str::trim)
            && !name.is_empty()
        {
            return name.to_owned();
        }
        let Some(id) = RecordId::parse(&self.id) else {
            return self.id.clone();
        };
        if let Some(path) = id.file_path() {
            return path.file_name().map_or_else(
                || path.display().to_string(),
                |name| name.to_string_lossy().to_string(),
            );
        }
        if let Some(rest) = self.id.strip_prefix("derived:")
            && let Some((role, target)) = rest.split_once('@')
        {
            return format!("{role} of {target}");
        }
        if self.kind.is_empty() {
            self.id.clone()
        } else {
            self.kind.clone()
        }
    }

    /// The line under the title: where this record lives. Not decoration —
    /// without it, two workspaces' `notes.md` are identical rows and
    /// bringing one here is a guess.
    fn facts(&self) -> Vec<String> {
        let where_it_lives = if self.here {
            crate::i18n::t("search-record-in-this-workspace")
        } else {
            match &self.workspace {
                Some(name) => crate::i18n::t_args(
                    "search-record-in-workspace",
                    &[("workspace", name.as_str().into())],
                ),
                // No workspace for this record (one this device stopped
                // showing, or whose name it does not know): say so rather
                // than print a workspace id.
                None => crate::i18n::t("search-record-in-another-workspace"),
            }
        };
        let mut facts = vec![where_it_lives];
        match RecordId::parse(&self.id).and_then(|id| id.file_path()) {
            Some(path) => facts.push(display_path(&path)),
            None if !self.kind.is_empty() => facts.push(self.kind.clone()),
            None => {}
        }
        facts
    }
}

/// `~`-shortened path for a row's second line: a file's whole path under the
/// person's home crowds out everything the row has to say.
fn display_path(path: &std::path::Path) -> String {
    if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from)
        && !home.as_os_str().is_empty()
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return format!("~/{}", rest.display());
    }
    path.display().to_string()
}

/// The records the workspaces hold, offered in the workspace of `workspace`.
pub struct Records {
    /// The workspace search was opened in: where a chosen row brings its
    /// record, and which rows say "in this workspace".
    workspace: String,
    held: Vec<HeldRecord>,
}

impl Records {
    #[must_use]
    pub fn new(workspace: impl Into<String>, held: Vec<HeldRecord>) -> Self {
        Self {
            workspace: workspace.into(),
            held,
        }
    }

    /// The row of `record`, ranked `score`: one this workspace holds opens in its app, one of
    /// another workspace is brought here. A file is the exception: bringing it here opens it as
    /// the document it is, wherever it was held.
    fn hit(&self, record: &HeldRecord, score: i32) -> Option<Hit> {
        let facts = record.facts();
        let checkout = record.checkout()?;
        let target = if record.here && checkout.id.file_path().is_none() {
            Target::Launch {
                app: record_launch(&record.package, checkout.id.route()),
                place: LaunchPlace::Focus,
            }
        } else {
            Target::Invoke(Box::new(
                InvokeTarget::action(DESKTOP_APP, &self.workspace, RECORDS_RESOURCE, BRING_HERE)
                    .with_argument(RECORD_PARAM, record.id.clone())
                    .with_argument(KIND_PARAM, checkout.kind.as_str())
                    .with_argument(PACKAGE_PARAM, record.package.clone()),
            ))
        };
        Some(Hit {
            row: None,
            preview: None,
            faces: Vec::new(),
            outcome: None,
            kind: ResourceKind::Record,
            provider: self.id().into(),
            id: record.id.clone(),
            title: record.title(),
            subtitle: Some(facts.join(", ")),
            facts,
            score,
            icon: None,
            target,
        })
    }

    /// The records of the other workspaces, each saying where it lives, in
    /// the order a person reads them: first those of a workspace this desk
    /// shows, by name, then those whose workspace it cannot name. They stand
    /// in this order while nothing is typed, and what the person brought
    /// here before rises above them.
    fn browse(&self) -> Vec<Hit> {
        let mut records: Vec<&HeldRecord> =
            self.held.iter().filter(|record| !record.here).collect();
        records.sort_by(|a, b| {
            b.workspace
                .is_some()
                .cmp(&a.workspace.is_some())
                .then_with(|| a.title().cmp(&b.title()))
        });
        records
            .into_iter()
            .take(LISTED)
            .enumerate()
            .filter_map(|(at, record)| {
                let at = i32::try_from(at).unwrap_or(MAX_PROVIDER_SCORE);
                self.hit(record, MAX_PROVIDER_SCORE.saturating_sub(at))
            })
            .collect()
    }
}

impl Source for Records {
    fn id(&self) -> &'static str {
        "records"
    }

    fn browse_rank(&self) -> u8 {
        crate::model::BROWSE_RANK_RECORDS
    }

    fn name(&self) -> String {
        crate::i18n::t("search-source-records")
    }

    /// The browse set is what the other workspaces hold, not what this one
    /// does: what the person worked in here is where they left their windows
    /// ([`crate::providers::places::Continue`]) and in what the open apps
    /// publish. A record of this workspace is a typed name away, and opens.
    fn query(&self, needle: &str) -> Vec<Hit> {
        if needle.trim().is_empty() {
            return self.browse();
        }
        let mut hits: Vec<Hit> = self
            .held
            .iter()
            .filter_map(|record| {
                let score = crate::rank::record_match_score(needle, &record.title(), &record.id)?;
                self.hit(record, score)
            })
            .collect();
        hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.title.cmp(&b.title)));
        hits.truncate(MAX_RESULTS);
        hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(id: &str, kind: &str, here: bool, workspace: Option<&str>) -> HeldRecord {
        HeldRecord {
            id: id.into(),
            kind: kind.into(),
            name: None,
            package: "so.tilo.notes".into(),
            here,
            workspace: workspace.map(str::to_owned),
        }
    }

    /// A record with a minted id is found by the name its index gives it,
    /// which is its row's title: an id names nothing a person types.
    #[test]
    fn a_minted_record_is_found_by_the_name_its_index_gives_it() {
        let mut form = held(
            "uuid:6d1c1f36-0000-4000-8000-0000000000aa",
            "pdf.document",
            true,
            None,
        );
        form.name = Some("application.pdf".into());
        form.package = "so.tilo.pdf-viewer".into();
        let records = Records::new("here", vec![form]);
        let hits = records.query("application");
        let hit = hits.first().expect("the form is found by its name");
        assert_eq!(hit.title, "application.pdf");
        assert_eq!(
            hit.target,
            Target::Launch {
                app: record_launch(
                    "so.tilo.pdf-viewer",
                    "uuid:6d1c1f36-0000-4000-8000-0000000000aa"
                ),
                place: LaunchPlace::Focus,
            },
            "a record this workspace holds opens in its app, as a link to it does"
        );
        assert!(records.query("passport").is_empty());
    }

    #[test]
    fn a_record_of_another_workspace_is_found_by_name_and_brought_here() {
        let records = Records::new(
            "here",
            vec![held(
                "file:/work/Quarterly plan.odt",
                "document",
                false,
                Some("Lounge"),
            )],
        );
        let hits = records.query("quarterly");
        assert_eq!(hits.len(), 1);
        let hit = &hits[0];
        assert_eq!(hit.title, "Quarterly plan.odt");
        assert_eq!(hit.kind, ResourceKind::Record);
        assert_eq!(
            hit.facts[0], "in Lounge",
            "the row says which workspace holds it"
        );
        let Target::Invoke(target) = &hit.target else {
            panic!("a record is brought here by address");
        };
        assert_eq!(target.app, DESKTOP_APP);
        assert_eq!(
            target.address,
            tilo_actions::ActionAddress::new(RECORDS_RESOURCE, BRING_HERE)
        );
        assert_eq!(
            target.arguments[RECORD_PARAM], "file:/work/Quarterly plan.odt",
            "the record travels as its own id: no bytes, no new identity"
        );
        assert_eq!(
            target.arguments[KIND_PARAM], "tilo.rich-text",
            "the row carries the kind as the index keeps it: a former name of \
             a kind is the kind it names"
        );
        assert_eq!(target.arguments[PACKAGE_PARAM], "so.tilo.notes");
        assert_eq!(
            target.workspace, "here",
            "and it is brought into this workspace"
        );
        assert!(records.query("zzz").is_empty());
    }

    /// Nothing typed lists the records of the other workspaces, each saying
    /// where it lives, and never one this workspace already holds: its row
    /// has no reference to add, so choosing it would do nothing.
    #[test]
    fn nothing_typed_lists_the_records_of_the_other_workspaces() {
        let records = Records::new(
            "here",
            vec![
                held("file:/work/notes.md", "document", true, None),
                held(
                    "file:/work/Quarterly plan.odt",
                    "document",
                    false,
                    Some("Lounge"),
                ),
                held("file:/other/budget.ods", "document", false, None),
            ],
        );
        let hits = records.query("");
        let titles: Vec<&str> = hits.iter().map(|hit| hit.title.as_str()).collect();
        assert_eq!(
            titles,
            ["Quarterly plan.odt", "budget.ods"],
            "the named workspace's record first, this workspace's never"
        );
        assert_eq!(hits[0].facts[0], "in Lounge");
        assert_eq!(hits[1].facts[0], "in another workspace");
        assert!(
            hits[0].score > hits[1].score,
            "they stand in this order while nothing is typed"
        );
    }

    #[test]
    fn a_record_held_here_says_so_and_a_kind_names_a_minted_one() {
        let records = Records::new(
            "here",
            vec![
                held("file:/work/notes.md", "document", true, None),
                held(
                    "uuid:6c0a2f2e-0d5f-4d3e-9f5e-2b8b0b0c9d11",
                    "note",
                    false,
                    None,
                ),
            ],
        );
        let here = records.query("notes");
        assert_eq!(here[0].facts[0], "in this workspace");
        // The minted record's own identity, which no other row carries.
        let minted = records.query("6c0a2f2e");
        assert_eq!(minted.len(), 1);
        assert_eq!(
            minted[0].title, "note",
            "a minted id has no name but its kind"
        );
        assert_eq!(minted[0].facts[0], "in another workspace");
    }
}
